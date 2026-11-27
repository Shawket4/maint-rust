//! What an applied row MEANS: guards before it is written, derivations after.
//!
//! - **Guards** (`before_*`): cross-row rules a column constraint cannot
//!   express — a mount must target a live position on the same vehicle, a
//!   mounted tire cannot be deleted or sent to repair, a work order with
//!   recorded work cannot be deleted.
//! - **Tire status** is a projection of the assignment ledger and the status
//!   events, recomputed here by ONE function (`rederive_tire_status`). Clients
//!   never write it.
//! - **Stock** follows consumption exactly: an oil change or a shipment tire
//!   debits; editing or deleting one posts the compensating movement.
//!
//! Everything runs inside the op's savepoint.

use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::ApiResult;
use crate::services::rules::{self, codes};
use crate::services::stock_ledger::{self, Movement};
use crate::services::sync_engine::{EntityType, SyncOperation};

type Reject = Option<(&'static str, String)>;

fn uuid_at(v: &Value, key: &str) -> Option<Uuid> {
    v.get(key).and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok())
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn is_open(row: &Value) -> bool {
    row.get("dismounted_at").map(Value::is_null).unwrap_or(true)
}

/// Namespace for derived movement ids: (event id, purpose) → a stable UUID, so
/// a replayed correction can never post its compensation twice.
fn derived_ref(event: Uuid, purpose: &str) -> Uuid {
    Uuid::new_v5(&event, purpose.as_bytes())
}

// ═══════════════════════════════ guards ═════════════════════════════════════

pub async fn before_insert(
    tx: &mut Transaction<'_, Postgres>,
    entity: EntityType,
    payload: &mut Value,
) -> ApiResult<Reject> {
    match entity {
        EntityType::TireAssignments => {
            let (Some(tire_id), Some(position_id)) = (uuid_at(payload, "tire_id"), uuid_at(payload, "position_id"))
            else {
                return Ok(Some((codes::VALIDATION, "tire_id and position_id are required".into())));
            };
            let pos: Option<(Option<i32>, bool)> = sqlx::query_as(
                "SELECT l.vehicle_id, (p.deleted_at IS NOT NULL OR l.deleted_at IS NOT NULL) \
                   FROM chassis_positions p JOIN chassis_layouts l ON l.id = p.layout_id \
                  WHERE p.id = $1",
            )
            .bind(position_id)
            .fetch_optional(&mut **tx)
            .await?;
            let Some((pos_vehicle, pos_deleted)) = pos else {
                return Ok(Some((codes::MISSING_REFERENCE, "the wheel position does not exist on the server".into())));
            };
            let vehicle_id = payload.get("vehicle_id").and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()));
            if pos_vehicle.map(i64::from) != vehicle_id {
                return Ok(Some((codes::POSITION_WRONG_VEHICLE, "that wheel position belongs to another vehicle".into())));
            }
            let tire: Option<(String, bool)> =
                sqlx::query_as("SELECT status::text, deleted_at IS NOT NULL FROM tires WHERE id = $1 FOR UPDATE")
                    .bind(tire_id)
                    .fetch_optional(&mut **tx)
                    .await?;
            let Some((tire_status, tire_deleted)) = tire else {
                return Ok(Some((codes::MISSING_REFERENCE, "the tire does not exist on the server".into())));
            };
            if is_open(payload) {
                // A live mount needs a live position and a tire that is
                // actually available. (History rows — already closed — may
                // describe a position or a tire that has since gone.)
                if pos_deleted {
                    return Ok(Some((codes::MISSING_REFERENCE, "the wheel position was deleted".into())));
                }
                if tire_deleted || tire_status != "in_stock_used" {
                    return Ok(Some((
                        codes::TIRE_NOT_AVAILABLE,
                        format!("the tire is not available to mount (it is {tire_status})"),
                    )));
                }
            }
            Ok(None)
        }
        EntityType::TireStatusEvents => {
            let Some(tire_id) = uuid_at(payload, "tire_id") else {
                return Ok(Some((codes::VALIDATION, "tire_id is required".into())));
            };
            let tire: Option<(String,)> =
                sqlx::query_as("SELECT status::text FROM tires WHERE id = $1 AND deleted_at IS NULL FOR UPDATE")
                    .bind(tire_id)
                    .fetch_optional(&mut **tx)
                    .await?;
            let Some((status,)) = tire else {
                return Ok(Some((codes::MISSING_REFERENCE, "the tire does not exist on the server".into())));
            };
            if status == "mounted" {
                return Ok(Some((codes::TIRE_MOUNTED, "the tire is mounted; dismount it first".into())));
            }
            // The server knows where the tire really was; the retread counter
            // must not trust a client's claim.
            payload["from_status"] = Value::String(status);
            Ok(None)
        }
        EntityType::BatteryInstalls => {
            let Some(battery_id) = uuid_at(payload, "battery_id") else {
                return Ok(Some((codes::VALIDATION, "battery_id is required".into())));
            };
            if let Some(reject) = live_work_order(tx, payload, "work_order_id").await? {
                return Ok(Some(reject));
            }
            let battery: Option<(String, bool)> =
                sqlx::query_as("SELECT status::text, deleted_at IS NOT NULL FROM batteries WHERE id = $1 FOR UPDATE")
                    .bind(battery_id)
                    .fetch_optional(&mut **tx)
                    .await?;
            let Some((status, deleted)) = battery else {
                return Ok(Some((codes::MISSING_REFERENCE, "the battery does not exist on the server".into())));
            };
            if payload.get("removed_at").map(Value::is_null).unwrap_or(true) && (deleted || status != "in_stock_used") {
                return Ok(Some((
                    codes::BATTERY_NOT_AVAILABLE,
                    format!("the battery is not available to fit (it is {status})"),
                )));
            }
            Ok(None)
        }
        EntityType::WorkOrderTasks | EntityType::OilChanges => {
            // A child of a deleted work order would be invisible everywhere.
            if let Some(wo) = uuid_at(payload, "work_order_id") {
                let deleted: Option<(bool,)> =
                    sqlx::query_as("SELECT deleted_at IS NOT NULL FROM work_orders WHERE id = $1")
                        .bind(wo)
                        .fetch_optional(&mut **tx)
                        .await?;
                match deleted {
                    None => {
                        return Ok(Some((codes::MISSING_REFERENCE, "the work order does not exist on the server".into())))
                    }
                    Some((true,)) => return Ok(Some((codes::DELETED, "the work order was deleted".into()))),
                    _ => {}
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

pub async fn before_update(
    tx: &mut Transaction<'_, Postgres>,
    entity: EntityType,
    _entity_id: &str,
    payload: &Value,
    row_before: &Value,
) -> ApiResult<Reject> {
    if entity == EntityType::Batteries && payload.get("status").is_some() {
        if let Some(id) = row_before.get("id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok()) {
            if battery_fitted(tx, id).await? {
                return Ok(Some((codes::BATTERY_INSTALLED, "the battery is fitted; take it off first".into())));
            }
        }
    }
    let immutable: &[&str] = match entity {
        // An assignment is a fact about one tire on one position. Moving it is
        // a dismount + a mount, never an edit. The same for a battery in a slot.
        EntityType::TireAssignments => &["tire_id", "position_id", "vehicle_id"],
        EntityType::BatteryInstalls => &["battery_id", "vehicle_id", "slot"],
        _ => &[],
    };
    for &k in immutable {
        if let Some(sent) = payload.get(k) {
            let same = match (sent, row_before.get(k)) {
                (a, Some(b)) => a.to_string().trim_matches('"').eq_ignore_ascii_case(b.to_string().trim_matches('"')),
                _ => false,
            };
            if !same {
                let what = if entity == EntityType::BatteryInstalls { "a fitted battery" } else { "a mount" };
                return Ok(Some((codes::VALIDATION, format!("{k} of {what} cannot be changed"))));
            }
        }
    }
    Ok(None)
}

pub async fn before_delete(
    tx: &mut Transaction<'_, Postgres>,
    entity: EntityType,
    entity_id: &str,
    _row_before: &Value,
) -> ApiResult<Reject> {
    let Ok(id) = Uuid::parse_str(entity_id) else { return Ok(None) };
    match entity {
        EntityType::Tires => {
            let (mounted,): (bool,) = sqlx::query_as(
                "SELECT EXISTS (SELECT 1 FROM tire_assignments \
                  WHERE tire_id = $1 AND dismounted_at IS NULL AND deleted_at IS NULL)",
            )
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
            if mounted {
                return Ok(Some((codes::TIRE_MOUNTED, "the tire is mounted; dismount it first".into())));
            }
        }
        EntityType::Batteries => {
            if battery_fitted(tx, id).await? {
                return Ok(Some((codes::BATTERY_INSTALLED, "the battery is fitted; take it off first".into())));
            }
        }
        EntityType::WorkOrders => {
            // Tasks go with the work order; recorded tire, battery and oil
            // work does not — it changed stock and must be undone first.
            let (busy,): (bool,) = sqlx::query_as(
                "SELECT EXISTS (SELECT 1 FROM oil_changes WHERE work_order_id = $1 AND deleted_at IS NULL) \
                     OR EXISTS (SELECT 1 FROM tire_assignments \
                                 WHERE (work_order_id = $1 OR dismount_work_order_id = $1) AND deleted_at IS NULL) \
                     OR EXISTS (SELECT 1 FROM battery_installs \
                                 WHERE (work_order_id = $1 OR removal_work_order_id = $1) AND deleted_at IS NULL)",
            )
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
            if busy {
                return Ok(Some((
                    codes::WORK_ORDER_NOT_EMPTY,
                    "the work order has tire, battery or oil work recorded; remove that first".into(),
                )));
            }
        }
        EntityType::AttendanceWorkers => {
            // Marked days stay in their months: a worker who left gets a
            // leaving date, not a delete (another device may have marked days
            // this one has not seen yet).
            let (has_days,): (bool,) = sqlx::query_as(
                "SELECT EXISTS (SELECT 1 FROM attendance_days WHERE worker_id = $1 AND deleted_at IS NULL)",
            )
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
            if has_days {
                return Ok(Some((codes::WORKER_HAS_DAYS, "this worker has marked days; set the day they left".into())));
            }
        }
        EntityType::ChassisPositions | EntityType::ChassisAxles | EntityType::ChassisLayouts => {
            let filter = match entity {
                EntityType::ChassisPositions => "p.id = $1",
                EntityType::ChassisAxles => "p.axle_id = $1",
                _ => "p.layout_id = $1",
            };
            let (in_use,): (bool,) = sqlx::query_as(&format!(
                "SELECT EXISTS (SELECT 1 FROM tire_assignments ta \
                   JOIN chassis_positions p ON p.id = ta.position_id \
                  WHERE {filter} AND ta.dismounted_at IS NULL AND ta.deleted_at IS NULL)"
            ))
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
            if in_use {
                return Ok(Some((codes::CHASSIS_IN_USE, "tires are mounted there; dismount them first".into())));
            }
        }
        _ => {}
    }
    Ok(None)
}

/// A work order a row hangs off must exist and be live.
async fn live_work_order(tx: &mut Transaction<'_, Postgres>, payload: &Value, key: &str) -> ApiResult<Reject> {
    let Some(wo) = uuid_at(payload, key) else { return Ok(None) };
    let deleted: Option<(bool,)> = sqlx::query_as("SELECT deleted_at IS NOT NULL FROM work_orders WHERE id = $1")
        .bind(wo)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(match deleted {
        None => Some((codes::MISSING_REFERENCE, "the work order does not exist on the server".into())),
        Some((true,)) => Some((codes::DELETED, "the work order was deleted".into())),
        _ => None,
    })
}

/// Is the battery in a slot right now?
async fn battery_fitted(tx: &mut Transaction<'_, Postgres>, battery_id: Uuid) -> ApiResult<bool> {
    let (fitted,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM battery_installs \
          WHERE battery_id = $1 AND removed_at IS NULL AND deleted_at IS NULL)",
    )
    .bind(battery_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(fitted)
}

// ═══════════════════════════ derivations ════════════════════════════════════

/// The one rule for `tires.status`:
///   * an open assignment ⇒ mounted;
///   * otherwise a tire still marked mounted takes the status its last
///     dismount implies;
///   * otherwise it keeps what the last status event gave it.
/// Bumps sync_version only when the status really changes.
pub async fn rederive_tire_status(tx: &mut Transaction<'_, Postgres>, tire_id: Uuid) -> ApiResult<()> {
    sqlx::query(
        r#"
        WITH open AS (
            SELECT 1 FROM tire_assignments
             WHERE tire_id = $1 AND dismounted_at IS NULL AND deleted_at IS NULL
        ),
        last_closed AS (
            SELECT dismount_reason::text AS reason FROM tire_assignments
             WHERE tire_id = $1 AND dismounted_at IS NOT NULL AND deleted_at IS NULL
             ORDER BY dismounted_at DESC, id LIMIT 1
        ),
        want AS (
            SELECT CASE
                     WHEN EXISTS (SELECT 1 FROM open) THEN 'mounted'
                     WHEN t.status = 'mounted' THEN
                       CASE (SELECT reason FROM last_closed)
                         WHEN 'worn' THEN 'scrapped' WHEN 'damaged' THEN 'scrapped'
                         WHEN 'retread' THEN 'retreading' WHEN 'puncture' THEN 'in_repair'
                         ELSE 'in_stock_used' END
                     ELSE t.status::text
                   END AS status
              FROM tires t WHERE t.id = $1
        )
        UPDATE tires t
           SET status = want.status::tire_status, sync_version = t.sync_version + 1
          FROM want
         WHERE t.id = $1 AND t.status::text <> want.status
        "#,
    )
    .bind(tire_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn set_tire_status(tx: &mut Transaction<'_, Postgres>, tire_id: Uuid, status: &str) -> ApiResult<()> {
    sqlx::query(
        "UPDATE tires SET status = $2::tire_status, sync_version = sync_version + 1 \
          WHERE id = $1 AND status::text <> $2",
    )
    .bind(tire_id)
    .bind(status)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The one rule for `batteries.status`, as for tires:
///   * an open install ⇒ installed;
///   * otherwise a battery still marked installed takes the outcome of its
///     last removal (no install left at all: back in stock as used);
///   * otherwise it keeps what it was given by hand.
pub async fn rederive_battery_status(tx: &mut Transaction<'_, Postgres>, battery_id: Uuid) -> ApiResult<()> {
    sqlx::query(
        r#"
        WITH want AS (
            SELECT CASE
                     WHEN EXISTS (SELECT 1 FROM battery_installs
                                   WHERE battery_id = $1 AND removed_at IS NULL AND deleted_at IS NULL) THEN 'installed'
                     WHEN b.status = 'installed' THEN
                       COALESCE((SELECT removal_outcome::text FROM battery_installs
                                  WHERE battery_id = $1 AND removed_at IS NOT NULL AND deleted_at IS NULL
                                  ORDER BY removed_at DESC, id LIMIT 1), 'in_stock_used')
                     ELSE b.status::text
                   END AS status
              FROM batteries b WHERE b.id = $1
        )
        UPDATE batteries b
           SET status = want.status::battery_status, sync_version = b.sync_version + 1
          FROM want
         WHERE b.id = $1 AND b.status::text <> want.status
        "#,
    )
    .bind(battery_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn set_battery_status(tx: &mut Transaction<'_, Postgres>, battery_id: Uuid, status: &str) -> ApiResult<()> {
    sqlx::query(
        "UPDATE batteries SET status = $2::battery_status, sync_version = sync_version + 1 \
          WHERE id = $1 AND status::text <> $2",
    )
    .bind(battery_id)
    .bind(status)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// One unit of the battery's product, in or out of the shelf count. The spec
/// rides in `size` ("100Ah 12V") so movement lists read without a join.
async fn battery_movement(
    tx: &mut Transaction<'_, Postgres>,
    kind: &'static str,
    battery_id: Uuid,
    ref_id: Uuid,
    user_id: i64,
    note: &str,
) -> ApiResult<Option<Movement>> {
    let row: Option<(Option<Uuid>, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT b.battery_product_id, b.brand, b.model, \
                NULLIF(concat_ws(' ', b.capacity_ah::text || 'Ah', b.voltage::text || 'V'), '') \
           FROM batteries b WHERE b.id = $1",
    )
    .bind(battery_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((product, brand, model, spec)) = row else { return Ok(None) };
    Ok(Some(Movement {
        kind,
        brand: Some(brand),
        model,
        size: spec,
        oil_type: None,
        qty: Some(1),
        liters: None,
        ref_id,
        work_order_id: None,
        user_id: Some(user_id),
        note: Some(note.to_string()),
        battery_product_id: product,
    }))
}

fn oil_movement(kind: &'static str, row: &Value, ref_id: Uuid, user_id: i64, note: &str) -> Option<Movement> {
    let liters = row.get("liters").and_then(rules::as_f64)?;
    Some(Movement {
        kind,
        brand: None,
        model: None,
        size: None,
        oil_type: str_at(row, "oil_type").map(String::from),
        qty: None,
        liters: Some(liters),
        ref_id,
        work_order_id: uuid_at(row, "work_order_id"),
        user_id: Some(user_id),
        note: Some(note.to_string()),
        battery_product_id: None,
    })
}

fn tire_movement(kind: &'static str, row: &Value, ref_id: Uuid, user_id: i64, note: &str) -> Movement {
    Movement {
        kind,
        brand: str_at(row, "brand").map(String::from),
        model: str_at(row, "model").map(String::from),
        size: str_at(row, "size").map(String::from),
        oil_type: None,
        qty: Some(1),
        liters: None,
        ref_id,
        work_order_id: None,
        user_id: Some(user_id),
        note: Some(note.to_string()),
        battery_product_id: None,
    }
}

/// Run after a successful apply. `row_before` is the row before this op
/// (None for a fresh insert). Returns true when a stock movement was recorded.
pub async fn after_apply(
    tx: &mut Transaction<'_, Postgres>,
    entity: EntityType,
    operation: SyncOperation,
    entity_id: &str,
    payload: &Value,
    row_before: Option<&Value>,
    user_id: i64,
) -> ApiResult<bool> {
    let mut moved = false;
    let id = Uuid::parse_str(entity_id).ok();
    match (entity, operation) {
        // ── oil: consumption follows the stored row, edits included ─────────
        (EntityType::OilChanges, SyncOperation::Insert) => {
            if let Some(m) = id.and_then(|id| oil_movement("oil_debit", payload, id, user_id, "oil change")) {
                moved |= stock_ledger::record(tx, &m).await?;
            }
        }
        (EntityType::OilChanges, SyncOperation::Update) => {
            let (Some(id), Some(before)) = (id, row_before) else { return Ok(false) };
            let after: Option<(Value,)> = sqlx::query_as("SELECT to_jsonb(o) FROM oil_changes o WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut **tx)
                .await?;
            let Some((after,)) = after else { return Ok(false) };
            let changed = before.get("liters").and_then(rules::as_f64) != after.get("liters").and_then(rules::as_f64)
                || str_at(before, "oil_type") != str_at(&after, "oil_type");
            if changed {
                // Give back what the old row took, take what the new row says.
                // Keyed on the new sync_version so each edit compensates once.
                let sv = after.get("sync_version").and_then(Value::as_i64).unwrap_or(0);
                if let Some(m) = oil_movement("oil_credit", before, derived_ref(id, &format!("edit-credit-{sv}")), user_id, "oil change corrected") {
                    moved |= stock_ledger::record(tx, &m).await?;
                }
                if let Some(m) = oil_movement("oil_debit", &after, derived_ref(id, &format!("edit-debit-{sv}")), user_id, "oil change corrected") {
                    moved |= stock_ledger::record(tx, &m).await?;
                }
            }
        }
        (EntityType::OilChanges, SyncOperation::Delete) => {
            if let (Some(id), Some(before)) = (id, row_before) {
                if let Some(m) = oil_movement("oil_credit", before, derived_ref(id, "delete-credit"), user_id, "oil change removed") {
                    moved |= stock_ledger::record(tx, &m).await?;
                }
            }
        }

        // ── tires: a shipment tire is one unit out of fungible stock ────────
        (EntityType::Tires, SyncOperation::Insert) => {
            if let (Some(id), Some("shipment")) = (id, str_at(payload, "origin")) {
                moved |= stock_ledger::record(tx, &tire_movement("tire_debit", payload, id, user_id, "new tire fitted")).await?;
            }
        }
        (EntityType::Tires, SyncOperation::Delete) => {
            if let (Some(id), Some(before)) = (id, row_before) {
                if str_at(before, "origin") == Some("shipment") {
                    let m = tire_movement("tire_credit", before, derived_ref(id, "delete-credit"), user_id, "tire record removed");
                    moved |= stock_ledger::record(tx, &m).await?;
                }
            }
        }

        // ── assignments drive tire status ───────────────────────────────────
        (EntityType::TireAssignments, SyncOperation::Insert) => {
            if let Some(tire_id) = uuid_at(payload, "tire_id") {
                if is_open(payload) {
                    set_tire_status(tx, tire_id, "mounted").await?;
                } else {
                    // History arriving already closed: it only decides the
                    // status when nothing newer does.
                    rederive_closed(tx, tire_id).await?;
                }
            }
        }
        (EntityType::TireAssignments, SyncOperation::Update) => {
            let Some(before) = row_before else { return Ok(false) };
            if let Some(tire_id) = uuid_at(before, "tire_id") {
                let was_open = is_open(before);
                let now_open = match payload.get("dismounted_at") {
                    Some(v) => v.is_null(),
                    None => was_open,
                };
                if was_open && !now_open {
                    let reason = str_at(payload, "dismount_reason").unwrap_or("other");
                    set_tire_status(tx, tire_id, rules::dismount_status(reason)).await?;
                } else if !was_open && !now_open && payload.get("dismount_reason").is_some() {
                    // The reason of the LAST dismount was corrected (worn →
                    // rotation undoes an accidental scrap).
                    rederive_closed(tx, tire_id).await?;
                }
                rederive_tire_status(tx, tire_id).await?;
            }
        }
        (EntityType::TireAssignments, SyncOperation::Delete) => {
            if let Some(tire_id) = row_before.and_then(|r| uuid_at(r, "tire_id")) {
                rederive_tire_status(tx, tire_id).await?;
            }
        }

        // ── lifecycle events (repair, retread, restock, scrap) ──────────────
        (EntityType::TireStatusEvents, SyncOperation::Insert) => {
            if let (Some(tire_id), Some(to_status)) = (uuid_at(payload, "tire_id"), str_at(payload, "to_status")) {
                set_tire_status(tx, tire_id, to_status).await?;
                if str_at(payload, "event_type") == Some("restock") && str_at(payload, "from_status") == Some("retreading") {
                    sqlx::query(
                        "UPDATE tires SET retread_count = retread_count + 1, sync_version = sync_version + 1 WHERE id = $1",
                    )
                    .bind(tire_id)
                    .execute(&mut **tx)
                    .await?;
                }
            }
        }

        // ── batteries: one from stock is one unit off the shelf count ───────
        (EntityType::Batteries, SyncOperation::Insert) => {
            if let (Some(id), Some("stock")) = (id, str_at(payload, "origin")) {
                if let Some(m) = battery_movement(tx, "battery_debit", id, id, user_id, "new battery fitted").await? {
                    moved |= stock_ledger::record(tx, &m).await?;
                }
            }
        }
        (EntityType::Batteries, SyncOperation::Delete) => {
            if let (Some(id), Some(before)) = (id, row_before) {
                if str_at(before, "origin") == Some("stock") {
                    let ref_id = derived_ref(id, "delete-credit");
                    if let Some(m) = battery_movement(tx, "battery_credit", id, ref_id, user_id, "battery record removed").await? {
                        moved |= stock_ledger::record(tx, &m).await?;
                    }
                }
            }
        }

        // ── battery installs drive battery status ───────────────────────────
        (EntityType::BatteryInstalls, SyncOperation::Insert) => {
            if let Some(battery_id) = uuid_at(payload, "battery_id") {
                if payload.get("removed_at").map(Value::is_null).unwrap_or(true) {
                    set_battery_status(tx, battery_id, "installed").await?;
                } else {
                    rederive_battery_status(tx, battery_id).await?;
                }
            }
        }
        (EntityType::BatteryInstalls, SyncOperation::Update) => {
            let Some(before) = row_before else { return Ok(false) };
            if let Some(battery_id) = uuid_at(before, "battery_id") {
                let was_open = before.get("removed_at").map(Value::is_null).unwrap_or(true);
                let now_open = match payload.get("removed_at") {
                    Some(v) => v.is_null(),
                    None => was_open,
                };
                if was_open && !now_open {
                    let outcome = str_at(payload, "removal_outcome").unwrap_or("in_stock_used");
                    set_battery_status(tx, battery_id, outcome).await?;
                } else if !was_open && now_open {
                    set_battery_status(tx, battery_id, "installed").await?;
                }
                rederive_battery_status(tx, battery_id).await?;
            }
        }
        (EntityType::BatteryInstalls, SyncOperation::Delete) => {
            if let Some(battery_id) = row_before.and_then(|r| uuid_at(r, "battery_id")) {
                rederive_battery_status(tx, battery_id).await?;
            }
        }

        // ── a deleted work order takes its tasks with it ────────────────────
        (EntityType::WorkOrders, SyncOperation::Delete) => {
            if let Some(id) = id {
                sqlx::query(
                    "UPDATE work_order_tasks SET deleted_at = now(), updated_by_user_id = $2, \
                            sync_version = sync_version + 1 \
                      WHERE work_order_id = $1 AND deleted_at IS NULL",
                )
                .bind(id)
                .bind(user_id)
                .execute(&mut **tx)
                .await?;
            }
        }
        _ => {}
    }
    Ok(moved)
}

/// For a tire with NO open assignment: take the status implied by its most
/// recent dismount, unless a status event happened after it.
async fn rederive_closed(tx: &mut Transaction<'_, Postgres>, tire_id: Uuid) -> ApiResult<()> {
    sqlx::query(
        r#"
        WITH last_closed AS (
            SELECT dismount_reason::text AS reason, dismounted_at FROM tire_assignments
             WHERE tire_id = $1 AND dismounted_at IS NOT NULL AND deleted_at IS NULL
             ORDER BY dismounted_at DESC, id LIMIT 1
        ),
        want AS (
            SELECT CASE reason
                     WHEN 'worn' THEN 'scrapped' WHEN 'damaged' THEN 'scrapped'
                     WHEN 'retread' THEN 'retreading' WHEN 'puncture' THEN 'in_repair'
                     ELSE 'in_stock_used' END AS status
              FROM last_closed lc
             WHERE NOT EXISTS (SELECT 1 FROM tire_status_events e
                                WHERE e.tire_id = $1 AND e.deleted_at IS NULL
                                  AND e.occurred_at > lc.dismounted_at)
               AND NOT EXISTS (SELECT 1 FROM tire_assignments o
                                WHERE o.tire_id = $1 AND o.dismounted_at IS NULL AND o.deleted_at IS NULL)
        )
        UPDATE tires t
           SET status = want.status::tire_status, sync_version = t.sync_version + 1
          FROM want
         WHERE t.id = $1 AND t.status::text <> want.status
        "#,
    )
    .bind(tire_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
