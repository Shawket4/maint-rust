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
pub fn writable_columns(entity: EntityType) -> &'static [&'static str] {
    match entity {
        EntityType::WorkOrders => &[
            "id", "vehicle_id", "status", "title", "odometer_at_open",
            "opened_at", "closed_at", "notes_ar", "notes_en",
        ],
        EntityType::WorkOrderTasks => &[
            "id", "work_order_id", "kind", "category_id", "category_name_ar",
            "description_ar", "description_en", "is_done", "done_at", "sort_order",
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
        EntityType::AttendanceWorkers => &[
            "id", "name", "job", "phone", "falcon_employee_id",
            "joined_on", "left_on", "sort_order", "notes",
        ],
        EntityType::AttendanceDays => &[
            "id", "worker_id", "work_date", "status", "check_in_at", "check_out_at", "note",
        ],
        EntityType::BatteryLayouts => &["vehicle_id", "slots"],
        EntityType::Batteries => &[
            "id", "brand", "model", "capacity_ah", "voltage", "cca",
            "serial", "date_code", "warranty_months", "status", "origin", "notes_ar",
        ],
        EntityType::BatteryInstalls => &[
            "id", "battery_id", "vehicle_id", "slot", "work_order_id",
            "installed_at", "installed_odometer",
            "removed_at", "removed_odometer", "removal_reason", "removal_outcome",
            "removal_work_order_id", "notes",
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
        (EntityType::AttendanceDays, "status") => "::attendance_status",
        (EntityType::Batteries, "status") => "::battery_status",
        (EntityType::Batteries, "origin") => "::battery_origin",
        (EntityType::BatteryInstalls, "removal_reason") => "::battery_removal_reason",
        (EntityType::BatteryInstalls, "removal_outcome") => "::battery_status",
        // UUID-typed PKs and FKs need an explicit cast when extracted via ->>
        (_, "id") if entity.pk_column() == "id" => "::uuid",
        (_, "work_order_id") => "::uuid",
        (_, "dismount_work_order_id") => "::uuid",
        (_, "parent_tire_id") => "::uuid",
        (_, "tire_id") => "::uuid",
        (_, "position_id") => "::uuid",
        (_, "layout_id") => "::uuid",
        (_, "axle_id") => "::uuid",
        (_, "worker_id") => "::uuid",
        (_, "battery_id") => "::uuid",
        (_, "removal_work_order_id") => "::uuid",
        // Dates
        (_, "work_date") | (_, "joined_on") | (_, "left_on") => "::date",
        // Integer columns
        (_, "vehicle_id") | (_, "odometer") | (_, "odometer_at_open") |
        (_, "total_km") | (_, "section_index") | (_, "spare_index") |
        (_, "sort_order") | (_, "mounted_odometer") | (_, "dismounted_odometer") |
        (_, "retread_count") | (_, "superseded_km") |
        (_, "falcon_employee_id") | (_, "slot") | (_, "slots") |
        (_, "capacity_ah") | (_, "voltage") | (_, "cca") | (_, "warranty_months") |
        (_, "installed_odometer") | (_, "removed_odometer") => "::integer",
        // Numeric/Decimal columns
        (_, "liters") => "::numeric",
        // Timestamptz columns
        (_, "set_at") | (_, "performed_at") | (_, "occurred_at") |
        (_, "opened_at") | (_, "closed_at") |
        (_, "mounted_at") | (_, "dismounted_at") | (_, "done_at") |
        (_, "check_in_at") | (_, "check_out_at") |
        (_, "installed_at") | (_, "removed_at") => "::timestamptz",
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
    if entity.is_singleton() {
        let pk = entity.pk_column();
        q.push_str(&format!(" ON CONFLICT ({pk}) DO UPDATE SET "));
        let mut updates = Vec::new();
        for col in col_list.iter() {
            if *col == pk { continue; }
            updates.push(format!("{col} = EXCLUDED.{col}"));
        }
        updates.push("updated_at = now()".to_string());
        updates.push(format!("sync_version = {table}.sync_version + 1"));
        if entity.has_deleted_at() {
            // A re-insert of a tombstoned singleton is a resurrection. Without
            // this, the upsert acks "applied" while the row stays soft-deleted
            // and invisible to every view that filters deleted_at.
            updates.push("deleted_at = NULL".to_string());
        }
        q.push_str(&updates.join(", "));
    }
    q.push_str(" RETURNING sync_version");
    let (sv,): (i64,) = sqlx::query_as(&q)
        .bind(payload)
        .bind(user_id)
        .fetch_one(&mut **tx)
        .await
        ?;
    Ok(sv)
}

/// Guarded update: the expected sync_version is part of the UPDATE's WHERE
/// clause, not just a pre-read compare. The pre-read runs on a plain SELECT
/// (no FOR UPDATE), so under READ COMMITTED a concurrent batch can commit
/// between the read and this statement — the predicate makes that window
/// lose cleanly (0 rows → caller reports Conflict) instead of silently
/// overwriting the winner.
///
/// Returns None when no row matched (version raced or row vanished).
pub async fn update_from_payload(
    tx: &mut Transaction<'_, Postgres>,
    entity: EntityType,
    entity_id: &str,
    payload: &Value,
    user_id: i64,
    expected_sync_version: i64,
) -> ApiResult<Option<i64>> {
    let table = entity.table_name();
    let pk = entity.pk_column();
    let pk_cast = entity.pk_cast();
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
        "UPDATE {table} SET {set_sql} WHERE {pk} = $3{pk_cast} AND sync_version = $4 RETURNING sync_version"
    );
    let sv: Option<(i64,)> = sqlx::query_as(&q)
        .bind(payload)
        .bind(user_id)
        .bind(entity_id)
        .bind(expected_sync_version)
        .fetch_optional(&mut **tx)
        .await
        ?;
    Ok(sv.map(|t| t.0))
}

/// Version-guarded tombstone. None = the guard matched no row.
pub async fn soft_delete(
    tx: &mut Transaction<'_, Postgres>,
    entity: EntityType,
    entity_id: &str,
    user_id: i64,
    expected_sync_version: i64,
) -> ApiResult<Option<i64>> {
    let table = entity.table_name();
    let pk = entity.pk_column();
    let cast = entity.pk_cast();
    let q = format!(
        "UPDATE {table}
            SET deleted_at = now(), updated_by_user_id = $1, sync_version = sync_version + 1
          WHERE {pk} = $2{cast} AND sync_version = $3
          RETURNING sync_version"
    );
    let sv: Option<(i64,)> = sqlx::query_as(&q)
        .bind(user_id)
        .bind(entity_id)
        .bind(expected_sync_version)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(sv.map(|t| t.0))
}

/// Does the stored row already hold everything this payload would write?
/// Used only for clients without op ids, to tell a redelivered update (the
/// ack was lost) from a real conflict.
pub fn payload_matches_row(entity: EntityType, payload: &Value, row: &Value) -> bool {
    let pk = entity.pk_column();
    writable_columns(entity).iter().filter(|c| **c != pk).all(|col| match payload.get(col) {
        None => true,
        Some(sent) => loosely_equal(sent, row.get(col).unwrap_or(&Value::Null)),
    })
}

fn loosely_equal(a: &Value, b: &Value) -> bool {
    use chrono::DateTime;
    let num = |v: &Value| -> Option<f64> {
        match v {
            Value::Number(n) => n.as_f64(),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    };
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::String(x), Value::String(y)) => {
            if x == y {
                return true;
            }
            match (DateTime::parse_from_rfc3339(x), DateTime::parse_from_rfc3339(y)) {
                (Ok(tx), Ok(ty)) => (tx.timestamp_millis() - ty.timestamp_millis()).abs() < 2,
                _ => match (x.trim().parse::<f64>(), y.trim().parse::<f64>()) {
                    (Ok(fx), Ok(fy)) => (fx - fy).abs() < 1e-9,
                    _ => x.eq_ignore_ascii_case(y) && uuid::Uuid::parse_str(x).is_ok(),
                },
            }
        }
        _ => match (num(a), num(b)) {
            (Some(x), Some(y)) => (x - y).abs() < 1e-9,
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replayed_update_is_recognised() {
        let row = json!({"id": "a", "work_order_id": "w", "is_done": true, "sort_order": 3,
                          "done_at": "2026-11-01T10:00:00.000+00:00", "description_ar": "x", "category_id": null});
        let same = json!({"is_done": 1, "sort_order": "3", "done_at": "2026-11-01T10:00:00Z",
                           "description_ar": "x", "category_id": null});
        assert!(payload_matches_row(EntityType::WorkOrderTasks, &same, &row));
        let other = json!({"is_done": false});
        assert!(!payload_matches_row(EntityType::WorkOrderTasks, &other, &row));
        let other = json!({"category_id": "brakes"});
        assert!(!payload_matches_row(EntityType::WorkOrderTasks, &other, &row));
    }
}
