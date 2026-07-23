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
        EntityType::WorkOrders => &[
            "id", "vehicle_id", "status", "title", "odometer_at_open",
            "opened_at", "closed_at", "notes_ar", "notes_en",
        ],
        EntityType::WorkOrderTasks => &[
            "id", "work_order_id", "kind", "category_id",
            "description_ar", "description_en", "is_done", "sort_order",
        ],
        EntityType::Tires => &[
            "id", "dot_code", "brand", "model", "size",
            "status", "origin", "total_km", "parent_tire_id", "retread_count",
            "notes_ar", "notes_en",
        ],
        EntityType::TireAssignments => &[
            "id", "tire_id", "position_id", "vehicle_id", "work_order_id",
            "mounted_at", "mounted_odometer", "mount_reason",
            "dismounted_at", "dismounted_odometer", "dismount_reason",
            "dismount_work_order_id", "notes",
        ],
        EntityType::TireStatusEvents => &[
            "id", "tire_id", "event_type", "work_order_id",
            "from_status", "to_status", "notes", "occurred_at",
        ],
        EntityType::OilChanges => &[
            "id", "work_order_id", "vehicle_id", "oil_type", "liters",
            "odometer", "flagged", "notes", "performed_at",
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
        EntityType::VehicleClassAssignments => &["vehicle_id", "class_id"],
        EntityType::VehicleOdometerOverrides => &[
            "vehicle_id", "odometer", "set_at", "notes", "superseded_km",
        ],
    }
}

/// Columns that need explicit Postgres casts because their type isn't trivially inferable from JSON.
/// Returns either an empty string or "::<type>".
fn pg_cast(entity: EntityType, col: &str) -> &'static str {
    match (entity, col) {
        // Enums
        (EntityType::WorkOrders, "status") => "::work_order_status",
        (EntityType::WorkOrderTasks, "kind") => "::wo_task_kind",
        (EntityType::Tires, "status") => "::tire_status",
        (EntityType::Tires, "origin") => "::tire_origin",
        (EntityType::TireAssignments, "mount_reason") => "::mount_reason",
        (EntityType::TireAssignments, "dismount_reason") => "::dismount_reason",
        (EntityType::TireStatusEvents, "event_type") => "::tire_event_type",
        (EntityType::TireStatusEvents, "from_status") => "::tire_status",
        (EntityType::TireStatusEvents, "to_status") => "::tire_status",
        (EntityType::ChassisAxles, "section") => "::chassis_section",
        (EntityType::ChassisAxles, "axle_type") => "::axle_type",
        (EntityType::ChassisPositions, "side") => "::position_side",
        (EntityType::ChassisPositions, "depth") => "::position_depth",
        // UUID-typed PKs and FKs need an explicit cast when extracted via ->>
        (_, "id") if entity.pk_column() == "id" => "::uuid",
        (_, "work_order_id") => "::uuid",
        (_, "dismount_work_order_id") => "::uuid",
        (_, "parent_tire_id") => "::uuid",
        (_, "tire_id") => "::uuid",
        (_, "position_id") => "::uuid",
        (_, "layout_id") => "::uuid",
        (_, "axle_id") => "::uuid",
        // Integer columns
        (_, "vehicle_id") | (_, "odometer") | (_, "odometer_at_open") |
        (_, "total_km") | (_, "section_index") | (_, "spare_index") |
        (_, "sort_order") | (_, "mounted_odometer") | (_, "dismounted_odometer") |
        (_, "retread_count") | (_, "superseded_km") => "::integer",
        // Numeric/Decimal columns
        (_, "liters") => "::numeric",
        // Timestamptz columns
        (_, "set_at") | (_, "performed_at") | (_, "occurred_at") |
        (_, "opened_at") | (_, "closed_at") |
        (_, "mounted_at") | (_, "dismounted_at") => "::timestamptz",
        // Boolean columns
        (_, "is_done") | (_, "is_steering") | (_, "is_lifted") |
        (_, "is_spare") | (_, "flagged") => "::boolean",
        _ => "",
    }
}

/// JSONB-typed columns are extracted via -> (preserving JSON), all others via ->> (text → cast).
fn is_jsonb(_entity: EntityType, _col: &str) -> bool {
    false
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
        if payload.get(col).is_none() {
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
    if entity.upserts_on_conflict() {
        q.push_str(" ON CONFLICT (vehicle_id) DO UPDATE SET ");
        let mut updates = Vec::new();
        for col in col_list.iter() {
            if *col == "vehicle_id" { continue; }
            updates.push(format!("{col} = EXCLUDED.{col}"));
        }
        updates.push("updated_at = now()".to_string());
        updates.push(format!("sync_version = {table}.sync_version + 1"));
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
        if payload.get(col).is_none() {
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
