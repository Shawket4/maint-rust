//! Input rules for synced rows: what a client may send, and what the server
//! decides for itself.
//!
//! The sync path is a generic row engine, so this is the one place a payload
//! is checked before it reaches SQL. Three jobs:
//!   * bounds (odometers, liters, text length, timestamps);
//!   * server-owned fields are stripped or recomputed (oil flag, tire status,
//!     work-order closed_at, task done_at);
//!   * database errors become stable codes with messages that leak no SQL.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value};

use crate::services::sync_engine::{EntityType, SyncOperation};

/// Stable machine codes carried on a per-op `error` result. The desktop
/// client maps these to Arabic/English text; never rename one.
pub mod codes {
    pub const VALIDATION: &str = "validation";
    pub const NOT_FOUND: &str = "not_found";
    pub const DELETED: &str = "deleted";
    pub const CONFLICT: &str = "conflict";
    pub const INTERNAL: &str = "internal";
    pub const GROUP_ROLLED_BACK: &str = "group_rolled_back";
    pub const MISSING_REFERENCE: &str = "missing_reference";
    pub const POSITION_OCCUPIED: &str = "position_occupied";
    pub const TIRE_ALREADY_MOUNTED: &str = "tire_already_mounted";
    pub const TIRE_NOT_AVAILABLE: &str = "tire_not_available";
    pub const TIRE_MOUNTED: &str = "tire_mounted";
    pub const POSITION_WRONG_VEHICLE: &str = "position_wrong_vehicle";
    pub const CHASSIS_EXISTS: &str = "chassis_exists";
    pub const CHASSIS_IN_USE: &str = "chassis_in_use";
    pub const WORK_ORDER_NOT_EMPTY: &str = "work_order_not_empty";
    pub const DUPLICATE: &str = "duplicate";
    pub const WORKER_HAS_DAYS: &str = "worker_has_days";
}

pub const OIL_FLAG_MIN: f64 = 34.0;
pub const OIL_FLAG_MAX: f64 = 36.0;
pub const MAX_ODOMETER: i64 = 9_999_999;
pub const MAX_LITERS: f64 = 1000.0;
const MAX_TEXT: usize = 2000;
/// A device clock this far ahead is wrong, not prophetic.
const FUTURE_TOLERANCE_MINUTES: i64 = 5;

pub fn oil_flagged(liters: f64) -> bool {
    !(OIL_FLAG_MIN..=OIL_FLAG_MAX).contains(&liters)
}

/// Dismount reason → the tire's status once it is off the truck.
pub fn dismount_status(reason: &str) -> &'static str {
    match reason {
        "worn" | "damaged" => "scrapped",
        "retread" => "retreading",
        "puncture" => "in_repair",
        _ => "in_stock_used",
    }
}

/// Postgres text and jsonb both reject U+0000.
pub fn json_has_nul(v: &Value) -> bool {
    match v {
        Value::String(s) => s.contains('\0'),
        Value::Array(a) => a.iter().any(json_has_nul),
        Value::Object(o) => o.keys().any(|k| k.contains('\0')) || o.values().any(json_has_nul),
        _ => false,
    }
}

pub fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn as_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

const ODOMETER_COLS: &[&str] =
    &["odometer", "odometer_at_open", "mounted_odometer", "dismounted_odometer", "superseded_km", "total_km"];
const TIMESTAMP_COLS: &[&str] = &[
    "set_at", "performed_at", "occurred_at", "opened_at", "closed_at", "mounted_at", "dismounted_at", "done_at",
    "check_in_at", "check_out_at",
];

/// Namespace of attendance day ids: a day's id is the UUID v5 of
/// "<worker_id>:<YYYY-MM-DD>" in it. The garage app holds the same constant.
pub const ATTENDANCE_DAY_NAMESPACE: uuid::Uuid = uuid::Uuid::from_u128(0x510d161b_3155_4560_9a62_cd726bbed9e9);

/// What a worker's day can be.
pub const ATTENDANCE_STATUSES: &[&str] = &["present", "absent", "leave", "sick"];

/// The one id a worker's day may have.
pub fn attendance_day_id(worker_id: &uuid::Uuid, work_date: &str) -> uuid::Uuid {
    uuid::Uuid::new_v5(&ATTENDANCE_DAY_NAMESPACE, format!("{worker_id}:{work_date}").as_bytes())
}

/// Validate and rewrite a payload in place. `row_before` is the stored row on
/// an update. Returns a human-readable reason on rejection.
pub fn normalize(
    entity: EntityType,
    operation: SyncOperation,
    payload: &mut Value,
    row_before: Option<&Value>,
) -> Result<(), String> {
    let now = Utc::now();
    let is_insert = operation == SyncOperation::Insert;
    let Some(obj) = payload.as_object_mut() else {
        return Err("payload must be a JSON object".into());
    };

    // ── generic bounds ──────────────────────────────────────────────────────
    for (k, v) in obj.iter_mut() {
        if let Value::String(s) = v {
            if s.chars().count() > MAX_TEXT {
                return Err(format!("{k} is too long"));
            }
        }
        if ODOMETER_COLS.contains(&k.as_str()) && !v.is_null() {
            let n = as_i64(v).ok_or_else(|| format!("{k} must be a whole number"))?;
            if !(0..=MAX_ODOMETER).contains(&n) {
                return Err(format!("{k} must be between 0 and {MAX_ODOMETER}"));
            }
            *v = Value::from(n);
        }
        if TIMESTAMP_COLS.contains(&k.as_str()) && !v.is_null() {
            let s = v.as_str().ok_or_else(|| format!("{k} must be a timestamp"))?;
            let t = DateTime::parse_from_rfc3339(s)
                .map_err(|_| format!("{k} is not a valid timestamp"))?
                .with_timezone(&Utc);
            // Device clocks drive business time. A future time is clamped to
            // the server's now so one wrong PC clock cannot outrank every
            // later reading in the odometer arbitration.
            let t = if t > now + Duration::minutes(FUTURE_TOLERANCE_MINUTES) { now } else { t };
            *v = Value::String(t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        }
    }
    if let Some(v) = obj.get("vehicle_id") {
        match as_i64(v) {
            Some(n) if n > 0 && n <= i32::MAX as i64 => {}
            _ => return Err("vehicle_id must be a positive number".into()),
        }
    }

    match entity {
        EntityType::OilChanges => normalize_oil(obj, is_insert)?,
        EntityType::Tires => normalize_tire(obj, is_insert)?,
        EntityType::WorkOrders => normalize_work_order(obj, row_before, now),
        EntityType::WorkOrderTasks => normalize_task(obj, row_before, is_insert, now),
        EntityType::TireAssignments => normalize_assignment(obj, row_before)?,
        EntityType::TireStatusEvents => {
            if obj.get("to_status").and_then(Value::as_str) == Some("mounted") {
                return Err("a status event cannot mount a tire".into());
            }
        }
        EntityType::AttendanceWorkers => normalize_worker(obj, is_insert)?,
        EntityType::AttendanceDays => normalize_attendance_day(obj, row_before, is_insert)?,
        _ => {}
    }
    Ok(())
}

/// A worker needs a name; everything else is optional.
fn normalize_worker(obj: &mut Map<String, Value>, is_insert: bool) -> Result<(), String> {
    match obj.get("name") {
        Some(Value::String(n)) => {
            let n = n.trim().to_string();
            if n.is_empty() {
                return Err("a worker needs a name".into());
            }
            obj.insert("name".into(), Value::String(n));
        }
        Some(_) => return Err("a worker needs a name".into()),
        None if is_insert => return Err("a worker needs a name".into()),
        None => {}
    }
    for k in ["job", "phone", "notes"] {
        if let Some(Value::String(v)) = obj.get(k) {
            let t = v.trim();
            let v = if t.is_empty() { Value::Null } else { Value::String(t.to_string()) };
            obj.insert(k.into(), v);
        }
    }
    Ok(())
}

/// One row per worker per day, under the one id that day may have. The
/// worker and the date never change after the row exists. Only a present day
/// keeps its times. There is no "off": the garage has no days off.
fn normalize_attendance_day(
    obj: &mut Map<String, Value>,
    row_before: Option<&Value>,
    is_insert: bool,
) -> Result<(), String> {
    if is_insert {
        let worker = obj
            .get("worker_id")
            .and_then(Value::as_str)
            .and_then(|w| uuid::Uuid::parse_str(w).ok())
            .ok_or("worker_id is required")?;
        let date = obj.get("work_date").and_then(Value::as_str).ok_or("work_date is required")?;
        chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| "work_date must be YYYY-MM-DD")?;
        let id = obj.get("id").and_then(Value::as_str).and_then(|i| uuid::Uuid::parse_str(i).ok());
        if id != Some(attendance_day_id(&worker, date)) {
            return Err("an attendance day must use its worker-and-date id".into());
        }
    } else if let Some(before) = row_before {
        for k in ["worker_id", "work_date"] {
            if let Some(v) = obj.get(k) {
                if before.get(k) != Some(v) {
                    return Err(format!("{k} of an attendance day cannot change"));
                }
            }
        }
    }
    match obj.get("status") {
        Some(Value::String(s)) if ATTENDANCE_STATUSES.contains(&s.as_str()) => {}
        Some(_) => return Err(format!("status must be one of {}", ATTENDANCE_STATUSES.join(", "))),
        None if is_insert => return Err("status is required".into()),
        None => {}
    }
    if let Some(status) = obj.get("status").and_then(Value::as_str) {
        if status != "present" {
            obj.insert("check_in_at".into(), Value::Null);
            obj.insert("check_out_at".into(), Value::Null);
        }
    }
    Ok(())
}

/// The 34–36 L flag is server authority; liters must be a sane positive number.
fn normalize_oil(obj: &mut Map<String, Value>, is_insert: bool) -> Result<(), String> {
    obj.remove("flagged");
    match obj.get("liters") {
        Some(v) => {
            let l = as_f64(v).filter(|l| l.is_finite()).ok_or("liters must be a number")?;
            let l = (l * 100.0).round() / 100.0;
            if l <= 0.0 || l > MAX_LITERS {
                return Err(format!("liters must be between 0 and {MAX_LITERS}"));
            }
            obj.insert("liters".into(), Value::from(l));
            obj.insert("flagged".into(), Value::Bool(oil_flagged(l)));
        }
        None if is_insert => return Err("liters is required".into()),
        None => {}
    }
    if let Some(v) = obj.get("oil_type") {
        let t = v.as_str().map(str::trim).unwrap_or("");
        if t.is_empty() {
            return Err("oil_type is required".into());
        }
        obj.insert("oil_type".into(), Value::String(t.to_string()));
    } else if is_insert {
        return Err("oil_type is required".into());
    }
    Ok(())
}

/// `status` and `retread_count` are projections of assignments and status
/// events — never client-writable. A new tire starts in the used pool; the
/// assignment insert that follows (same group) is what mounts it.
fn normalize_tire(obj: &mut Map<String, Value>, is_insert: bool) -> Result<(), String> {
    if is_insert {
        let status = match obj.get("status").and_then(Value::as_str) {
            Some(s @ ("in_stock_used" | "in_repair" | "retreading" | "scrapped")) => s.to_string(),
            _ => "in_stock_used".to_string(),
        };
        obj.insert("status".into(), Value::String(status));
        obj.insert("retread_count".into(), Value::from(0));
        match obj.get("origin").and_then(Value::as_str) {
            Some("shipment" | "legacy") => {}
            _ => return Err("origin must be 'shipment' or 'legacy'".into()),
        }
    } else {
        obj.remove("status");
        obj.remove("retread_count");
        obj.remove("origin");
    }
    if let Some(v) = obj.get("brand") {
        let b = v.as_str().map(str::trim).unwrap_or("");
        if b.is_empty() {
            return Err("brand is required".into());
        }
        obj.insert("brand".into(), Value::String(b.to_string()));
    } else if is_insert {
        return Err("brand is required".into());
    }
    for k in ["model", "size", "dot_code"] {
        if let Some(Value::String(s)) = obj.get(k) {
            let t = s.trim().to_string();
            obj.insert(k.into(), if t.is_empty() { Value::Null } else { Value::String(t) });
        }
    }
    Ok(())
}

/// status and closed_at say the same thing; keep them saying it.
fn normalize_work_order(obj: &mut Map<String, Value>, row_before: Option<&Value>, now: DateTime<Utc>) {
    let status = obj
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            // closed_at sent alone decides the status.
            obj.get("closed_at").map(|c| if c.is_null() { "open".to_string() } else { "closed".to_string() })
        });
    let Some(status) = status else { return };
    if status == "closed" {
        let has = obj.get("closed_at").map(|v| !v.is_null()).unwrap_or(false)
            || (!obj.contains_key("closed_at")
                && row_before.and_then(|r| r.get("closed_at")).map(|v| !v.is_null()).unwrap_or(false));
        if !has {
            obj.insert(
                "closed_at".into(),
                Value::String(now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            );
        }
        obj.insert("status".into(), Value::String("closed".into()));
    } else if status == "open" {
        obj.insert("status".into(), Value::String("open".into()));
        obj.insert("closed_at".into(), Value::Null);
    }
}

/// done_at is when the work was ticked. It follows is_done.
fn normalize_task(
    obj: &mut Map<String, Value>,
    row_before: Option<&Value>,
    is_insert: bool,
    now: DateTime<Utc>,
) {
    let done = match obj.get("is_done") {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::Number(n)) => Some(n.as_i64().unwrap_or(0) != 0),
        _ => None,
    };
    let Some(done) = done.or(if is_insert { Some(false) } else { None }) else {
        obj.remove("done_at");
        return;
    };
    obj.insert("is_done".into(), Value::Bool(done));
    if !done {
        obj.insert("done_at".into(), Value::Null);
        return;
    }
    let sent = obj.get("done_at").filter(|v| !v.is_null()).cloned();
    let kept = row_before
        .filter(|r| r.get("is_done").and_then(Value::as_bool) == Some(true))
        .and_then(|r| r.get("done_at"))
        .filter(|v| !v.is_null())
        .cloned();
    let value = sent
        .or(kept)
        .unwrap_or_else(|| Value::String(now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)));
    obj.insert("done_at".into(), value);
}

fn normalize_assignment(obj: &mut Map<String, Value>, row_before: Option<&Value>) -> Result<(), String> {
    let pick = |obj: &Map<String, Value>, k: &str| -> Option<i64> {
        match obj.get(k) {
            Some(v) => as_i64(v),
            None => row_before.and_then(|r| r.get(k)).and_then(as_i64),
        }
    };
    if let (Some(m), Some(d)) = (pick(obj, "mounted_odometer"), pick(obj, "dismounted_odometer")) {
        if d < m {
            return Err("the dismount odometer is lower than the mount odometer".into());
        }
    }
    // A reason or odometer without a dismount time is not a dismount.
    let closed = match obj.get("dismounted_at") {
        Some(v) => !v.is_null(),
        None => row_before.and_then(|r| r.get("dismounted_at")).map(|v| !v.is_null()).unwrap_or(false),
    };
    if !closed {
        for k in ["dismounted_odometer", "dismount_reason", "dismount_work_order_id"] {
            if obj.contains_key(k) || obj.contains_key("dismounted_at") {
                obj.insert(k.into(), Value::Null);
            }
        }
    }
    Ok(())
}

/// A database error as a stable code + a message safe to show.
pub fn explain_db_error(e: &sqlx::Error) -> (&'static str, String) {
    let sqlx::Error::Database(db) = e else {
        return (codes::INTERNAL, "internal error".into());
    };
    let constraint = db.constraint().unwrap_or("");
    let code = db.code().map(|c| c.to_string()).unwrap_or_default();
    match code.as_str() {
        "23505" => match constraint {
            "uq_active_position" => (codes::POSITION_OCCUPIED, "that wheel position already has a tire".into()),
            "uq_active_tire" => (codes::TIRE_ALREADY_MOUNTED, "that tire is already mounted".into()),
            "uq_chassis_layouts_vehicle_live" => {
                (codes::CHASSIS_EXISTS, "this vehicle already has a chassis layout".into())
            }
            _ => (codes::DUPLICATE, "a record with the same key already exists".into()),
        },
        "23503" => (codes::MISSING_REFERENCE, "a record this change refers to does not exist on the server".into()),
        "23514" => (codes::VALIDATION, format!("a value is out of range ({constraint})")),
        "23502" => (codes::VALIDATION, "a required value is missing".into()),
        c if c.starts_with("22") => (codes::VALIDATION, "a value has the wrong format".into()),
        _ => (codes::INTERNAL, "internal error".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ins(entity: EntityType, mut p: Value) -> Result<Value, String> {
        normalize(entity, SyncOperation::Insert, &mut p, None).map(|_| p)
    }
    fn upd(entity: EntityType, mut p: Value, before: Value) -> Result<Value, String> {
        normalize(entity, SyncOperation::Update, &mut p, Some(&before)).map(|_| p)
    }

    #[test]
    fn oil_flag_band_is_inclusive() {
        assert!(oil_flagged(33.9));
        assert!(!oil_flagged(34.0));
        assert!(!oil_flagged(36.0));
        assert!(oil_flagged(36.1));
    }

    #[test]
    fn dismount_reason_maps_to_status() {
        assert_eq!(dismount_status("worn"), "scrapped");
        assert_eq!(dismount_status("damaged"), "scrapped");
        assert_eq!(dismount_status("retread"), "retreading");
        assert_eq!(dismount_status("puncture"), "in_repair");
        assert_eq!(dismount_status("rotation"), "in_stock_used");
        assert_eq!(dismount_status("garbage"), "in_stock_used");
    }

    #[test]
    fn oil_liters_are_bounded_and_the_flag_is_ours() {
        let p = ins(EntityType::OilChanges, json!({"liters": " 50", "oil_type": "15W-40", "flagged": false})).unwrap();
        assert_eq!(p["liters"], json!(50.0));
        assert_eq!(p["flagged"], json!(true));
        assert!(ins(EntityType::OilChanges, json!({"liters": -3, "oil_type": "x"})).is_err());
        assert!(ins(EntityType::OilChanges, json!({"liters": 0, "oil_type": "x"})).is_err());
        assert!(ins(EntityType::OilChanges, json!({"liters": "NaN", "oil_type": "x"})).is_err());
        assert!(ins(EntityType::OilChanges, json!({"liters": 5000, "oil_type": "x"})).is_err());
        assert!(ins(EntityType::OilChanges, json!({"oil_type": "x"})).is_err());
        assert!(ins(EntityType::OilChanges, json!({"liters": 35, "oil_type": "  "})).is_err());
        // An update that does not touch liters must not invent a flag.
        let p = upd(EntityType::OilChanges, json!({"notes": "x", "flagged": true}), json!({})).unwrap();
        assert!(p.get("flagged").is_none());
    }

    #[test]
    fn odometers_are_bounded() {
        assert!(ins(EntityType::WorkOrders, json!({"vehicle_id": 1, "odometer_at_open": -1})).is_err());
        assert!(ins(EntityType::WorkOrders, json!({"vehicle_id": 1, "odometer_at_open": 10_000_000})).is_err());
        assert!(ins(EntityType::WorkOrders, json!({"vehicle_id": 1, "odometer_at_open": "abc"})).is_err());
        let p = ins(EntityType::WorkOrders, json!({"vehicle_id": 1, "odometer_at_open": "412300"})).unwrap();
        assert_eq!(p["odometer_at_open"], json!(412300));
        assert!(ins(EntityType::WorkOrders, json!({"vehicle_id": 0})).is_err());
    }

    #[test]
    fn future_timestamps_are_clamped_and_garbage_is_refused() {
        let p = ins(
            EntityType::VehicleOdometerOverrides,
            json!({"vehicle_id": 1, "odometer": 5, "set_at": "2099-01-01T00:00:00Z"}),
        )
        .unwrap();
        let t = DateTime::parse_from_rfc3339(p["set_at"].as_str().unwrap()).unwrap();
        assert!(t.with_timezone(&Utc) <= Utc::now() + Duration::minutes(1));
        assert!(ins(
            EntityType::VehicleOdometerOverrides,
            json!({"vehicle_id": 1, "odometer": 5, "set_at": "yesterday"})
        )
        .is_err());
    }

    #[test]
    fn tire_status_is_never_client_written() {
        let p = ins(EntityType::Tires, json!({"brand": " Pirelli ", "origin": "shipment", "status": "mounted", "retread_count": 9}))
            .unwrap();
        assert_eq!(p["status"], json!("in_stock_used"));
        assert_eq!(p["retread_count"], json!(0));
        assert_eq!(p["brand"], json!("Pirelli"));
        let p = ins(EntityType::Tires, json!({"brand": "x", "origin": "legacy", "status": "scrapped"})).unwrap();
        assert_eq!(p["status"], json!("scrapped"));
        assert!(ins(EntityType::Tires, json!({"brand": "", "origin": "legacy"})).is_err());
        assert!(ins(EntityType::Tires, json!({"brand": "x", "origin": "stolen"})).is_err());
        let p = upd(EntityType::Tires, json!({"status": "scrapped", "retread_count": 4, "dot_code": " "}), json!({})).unwrap();
        assert!(p.get("status").is_none() && p.get("retread_count").is_none());
        assert_eq!(p["dot_code"], Value::Null);
    }

    #[test]
    fn work_order_status_and_closed_at_agree() {
        let p = ins(EntityType::WorkOrders, json!({"vehicle_id": 1, "status": "closed"})).unwrap();
        assert!(p["closed_at"].is_string());
        let p = ins(EntityType::WorkOrders, json!({"vehicle_id": 1, "status": "open", "closed_at": "2026-01-01T00:00:00Z"}))
            .unwrap();
        assert_eq!(p["closed_at"], Value::Null);
        let p = upd(
            EntityType::WorkOrders,
            json!({"status": "closed"}),
            json!({"closed_at": "2026-01-01T00:00:00Z"}),
        )
        .unwrap();
        assert!(p.get("closed_at").is_none(), "an existing close time is kept");
    }

    #[test]
    fn task_done_at_follows_is_done() {
        let p = ins(EntityType::WorkOrderTasks, json!({"work_order_id": "x"})).unwrap();
        assert_eq!(p["is_done"], json!(false));
        assert_eq!(p["done_at"], Value::Null);
        let p = ins(EntityType::WorkOrderTasks, json!({"is_done": true})).unwrap();
        assert!(p["done_at"].is_string());
        let p = ins(EntityType::WorkOrderTasks, json!({"is_done": 1, "done_at": "2026-03-01T10:00:00Z"})).unwrap();
        assert_eq!(p["done_at"], json!("2026-03-01T10:00:00.000Z"));
        // Re-sending a done task keeps the original time.
        let p = upd(
            EntityType::WorkOrderTasks,
            json!({"is_done": true}),
            json!({"is_done": true, "done_at": "2026-02-02T00:00:00+00:00"}),
        )
        .unwrap();
        assert_eq!(p["done_at"], json!("2026-02-02T00:00:00+00:00"));
        let p = upd(EntityType::WorkOrderTasks, json!({"is_done": false, "done_at": "2026-02-02T00:00:00Z"}), json!({}))
            .unwrap();
        assert_eq!(p["done_at"], Value::Null);
    }

    #[test]
    fn assignment_odometers_must_not_run_backwards() {
        assert!(ins(
            EntityType::TireAssignments,
            json!({"mounted_odometer": 500, "dismounted_odometer": 400, "dismounted_at": "2026-01-01T00:00:00Z"})
        )
        .is_err());
        assert!(upd(EntityType::TireAssignments, json!({"dismounted_odometer": 400}), json!({"mounted_odometer": 500}))
            .is_err());
        // Reopening clears the dismount fields.
        let p = upd(
            EntityType::TireAssignments,
            json!({"dismounted_at": null}),
            json!({"dismounted_at": "2026-01-01T00:00:00Z", "dismount_reason": "worn"}),
        )
        .unwrap();
        assert_eq!(p["dismount_reason"], Value::Null);
    }

    #[test]
    fn nul_is_found_everywhere() {
        assert!(!json_has_nul(&json!({"a": "b", "n": 1})));
        assert!(json_has_nul(&json!({"a": {"b": ["x\u{0000}"]}})));
        assert!(json_has_nul(&json!({"k\u{0000}": 1})));
    }

    proptest::proptest! {
        #[test]
        fn oil_flag_is_exactly_the_band(liters in -1.0e6f64..1.0e6) {
            proptest::prop_assert_eq!(oil_flagged(liters), !(OIL_FLAG_MIN..=OIL_FLAG_MAX).contains(&liters));
        }

        #[test]
        fn dismount_status_is_total_and_valid(reason in ".*") {
            let s = dismount_status(&reason);
            proptest::prop_assert!(["scrapped", "retreading", "in_repair", "in_stock_used"].contains(&s));
        }

        /// Whatever a client sends, normalize never panics and an accepted
        /// oil change always carries bounded liters and a consistent flag.
        #[test]
        fn accepted_oil_is_always_sane(liters in proptest::num::f64::ANY) {
            let mut p = json!({"liters": liters, "oil_type": "x", "flagged": true});
            if normalize(EntityType::OilChanges, SyncOperation::Insert, &mut p, None).is_ok() {
                let l = p["liters"].as_f64().unwrap();
                proptest::prop_assert!(l > 0.0 && l <= MAX_LITERS);
                proptest::prop_assert_eq!(p["flagged"].as_bool().unwrap(), oil_flagged(l));
            }
        }
    }
}
