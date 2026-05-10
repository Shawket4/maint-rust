//! Generic JSON → SQL upsert helpers used by the sync engine.
//!
//! The sync push protocol (§7.2) is generic across entity types. Rather than
//! hand-writing a switch with a custom INSERT for each table, we list the
//! mutable column set per entity and build the SQL dynamically.
//!
//! Audit columns and `sync_version` are always server-assigned. Any client-supplied
//! values for those are ignored.

use serde_json::Value;
use sqlx::{Postgres, Transaction};

use crate::error::{ApiError, ApiResult};
use crate::services::sync_engine::EntityType;

/// Columns the client may supply via sync. Excludes audit columns + sync_version + timestamps.
fn writable_columns(entity: EntityType) -> &'static [&'static str] {
    match entity {
        EntityType::MaintenanceTemplates => &[
            "id", "vehicle_id", "category_id", "name_ar", "name_en",
            "notes_ar", "notes_en", "trigger_type", "interval_km", "interval_days",
            "lead_warn_km", "lead_warn_days", "is_active",
        ],
        EntityType::MaintenanceRecords => &[
            "id", "template_id", "vehicle_id", "category_id",
            "performed_at", "odometer_at_service",
            "next_due_at", "next_due_km",
            "cost", "vendor", "performed_by", "notes", "attachments",
        ],
        EntityType::ChassisLayouts => &["id", "vehicle_id", "name_ar", "name_en"],
        EntityType::ChassisAxles => &[
            "id", "layout_id", "section", "section_index", "axle_type",
            "label_ar", "label_en", "is_steering", "is_lifted",
        ],
        EntityType::ChassisPositions => &[
            "id", "layout_id", "axle_id", "side", "depth",
            "is_spare", "spare_index", "position_code",
        ],
        EntityType::Tires => &[
            "id", "dot_code", "internal_serial", "brand", "model",
            "purchase_date", "purchase_cost", "supplier",
            "production_week", "production_year", "production_date",
            "is_retread", "retread_count", "parent_tire_id",
            "status", "stock_location", "scrap_reason", "scrap_date", "notes",
        ],
        EntityType::TireAssignments => &[
            "id", "tire_id", "position_id", "vehicle_id",
            "mounted_at", "mounted_odometer", "dismounted_at", "dismounted_odometer",
            "mount_reason", "dismount_reason", "notes",
        ],
        EntityType::VehicleOdometerOverrides => &[
            "vehicle_id", "odometer", "set_at", "notes",
        ],
    }
}

/// Columns that need explicit Postgres casts because their type isn't trivially inferable from JSON.
/// Returns either an empty string or "::<type>".
fn pg_cast(entity: EntityType, col: &str) -> &'static str {
    match (entity, col) {
        (EntityType::MaintenanceTemplates, "trigger_type") => "::maintenance_trigger",
        (EntityType::ChassisAxles, "section") => "::chassis_section",
        (EntityType::ChassisAxles, "axle_type") => "::axle_type",
        (EntityType::ChassisPositions, "side") => "::position_side",
        (EntityType::ChassisPositions, "depth") => "::position_depth",
        (EntityType::Tires, "status") => "::tire_status",
        (EntityType::TireAssignments, "mount_reason") => "::mount_reason",
        (EntityType::TireAssignments, "dismount_reason") => "::mount_reason",
        // UUID-typed PKs and FKs need an explicit cast when extracted via ->>
        (_, "id") if entity.pk_column() == "id" => "::uuid",
        (EntityType::MaintenanceRecords, "template_id") => "::uuid",
        (EntityType::ChassisAxles, "layout_id") => "::uuid",
        (EntityType::ChassisPositions, "layout_id") => "::uuid",
        (EntityType::ChassisPositions, "axle_id") => "::uuid",
        (EntityType::Tires, "parent_tire_id") => "::uuid",
        (EntityType::TireAssignments, "tire_id") => "::uuid",
        (EntityType::TireAssignments, "position_id") => "::uuid",
        // Integer columns
        (_, "vehicle_id") | (_, "odometer") | (_, "odometer_at_service") |
        (_, "next_due_km") | (_, "interval_km") | (_, "interval_days") |
        (_, "lead_warn_km") | (_, "lead_warn_days") | (_, "section_index") |
        (_, "spare_index") | (_, "mounted_odometer") | (_, "dismounted_odometer") |
        (_, "production_week") | (_, "production_year") | (_, "retread_count") => "::integer",
        // Numeric/Decimal columns
        (_, "cost") | (_, "purchase_cost") => "::numeric",
        // Date columns
        (_, "purchase_date") | (_, "production_date") | (_, "scrap_date") => "::date",
        // Timestamptz columns
        (_, "set_at") | (_, "performed_at") | (_, "next_due_at") |
        (_, "mounted_at") | (_, "dismounted_at") => "::timestamptz",
        // Boolean columns
        (_, "is_active") | (_, "is_steering") | (_, "is_lifted") |
        (_, "is_spare") | (_, "is_retread") => "::boolean",
        // JSONB columns use the value-as-jsonb extraction below; no cast needed
        _ => "",
    }
}

/// JSONB-typed columns are extracted via -> (preserving JSON), all others via ->> (text → cast).
fn is_jsonb(entity: EntityType, col: &str) -> bool {
    matches!(
        (entity, col),
        (EntityType::MaintenanceRecords, "attachments")
    )
}

pub async fn insert_from_payload(
    tx: &mut Transaction<'_, Postgres>,
    entity: EntityType,
    payload: &Value,
    user_id: i64,
) -> ApiResult<i64> {
    let table = entity.table_name();
    let cols = writable_columns(entity);

    let mut col_list: Vec<&str> = Vec::new();
    let mut value_exprs: Vec<String> = Vec::new();

    for col in cols {
        if !payload.get(col).is_some() {
            continue;
        }
        col_list.push(col);
        let cast = pg_cast(entity, col);
        if is_jsonb(entity, col) {
            value_exprs.push(format!("($1::jsonb -> '{col}')"));
        } else {
            value_exprs.push(format!("($1::jsonb ->> '{col}'){cast}"));
        }
    }

    // Always force audit columns from server side
    col_list.push("created_by_user_id");
    value_exprs.push("$2".to_string());
    col_list.push("updated_by_user_id");
    value_exprs.push("$2".to_string());

    let cols_sql = col_list.join(", ");
    let vals_sql = value_exprs.join(", ");
    let mut q = format!(
        "INSERT INTO {table} ({cols_sql}) VALUES ({vals_sql})"
    );
    if entity == EntityType::VehicleOdometerOverrides {
        q.push_str(" ON CONFLICT (vehicle_id) DO UPDATE SET ");
        let mut updates = Vec::new();
        for col in col_list.iter() {
            if *col == "vehicle_id" { continue; }
            updates.push(format!("{col} = EXCLUDED.{col}"));
        }
        updates.push("updated_at = now()".to_string());
        updates.push("sync_version = vehicle_odometer_overrides.sync_version + 1".to_string());
        q.push_str(&updates.join(", "));
    }
    q.push_str(" RETURNING sync_version");
    let (sv,): (i64,) = sqlx::query_as(&q)
        .bind(payload)
        .bind(user_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_check_violation() => {
                ApiError::BadRequest(format!("check constraint failed: {}", db.message()))
            }
            sqlx::Error::Database(db) if db.is_foreign_key_violation() => {
                ApiError::BadRequest(format!("foreign key violation: {}", db.message()))
            }
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                ApiError::Conflict(format!("unique violation: {}", db.message()))
            }
            _ => ApiError::Db(e),
        })?;
    Ok(sv)
}

pub async fn update_from_payload(
    tx: &mut Transaction<'_, Postgres>,
    entity: EntityType,
    entity_id: &str,
    payload: &Value,
    user_id: i64,
) -> ApiResult<i64> {
    let table = entity.table_name();
    let pk = entity.pk_column();
    let cols = writable_columns(entity);

    let mut sets: Vec<String> = Vec::new();
    for col in cols {
        if *col == pk {
            continue;
        }
        if !payload.get(col).is_some() {
            continue;
        }
        let cast = pg_cast(entity, col);
        if is_jsonb(entity, col) {
            sets.push(format!("{col} = ($1::jsonb -> '{col}')"));
        } else {
            sets.push(format!("{col} = ($1::jsonb ->> '{col}'){cast}"));
        }
    }
    if sets.is_empty() {
        return Err(ApiError::BadRequest("no updatable fields in payload".into()));
    }
    sets.push("updated_at = now()".to_string());
    sets.push("updated_by_user_id = $2".to_string());
    sets.push("sync_version = sync_version + 1".to_string());

    let set_sql = sets.join(", ");
    let q = format!(
        "UPDATE {table} SET {set_sql} WHERE {pk}::text = $3 RETURNING sync_version"
    );
    let (sv,): (i64,) = sqlx::query_as(&q)
        .bind(payload)
        .bind(user_id)
        .bind(entity_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_check_violation() => {
                ApiError::BadRequest(format!("check constraint failed: {}", db.message()))
            }
            sqlx::Error::Database(db) if db.is_foreign_key_violation() => {
                ApiError::BadRequest(format!("foreign key violation: {}", db.message()))
            }
            _ => ApiError::Db(e),
        })?;
    Ok(sv)
}
