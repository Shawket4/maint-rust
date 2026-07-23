//! Domain derivations applied alongside sync ops.
//!
//! The sync engine stays generic (rows in, rows out); this module is the seam
//! that knows what an applied row *means*. Three families:
//!
//! - **Authority**: the 34–36 L oil-flag band is recomputed here; the client's
//!   value is overwritten. The rule lives in exactly one place.
//! - **Stock**: consumption debits the mirrored ledger (idempotent on the
//!   event UUID), mirrored up to Falcon after the batch commits.
//! - **Projections**: `tires.status` and `tires.total_km` are derived from
//!   assignment ops. The client must NEVER push a competing `tires` update for
//!   these fields — it would race the server's own sync_version bump and
//!   dead-letter a legitimate write. Clients apply the same derivations to
//!   their local mirror without enqueueing them.

use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::ApiResult;
use crate::services::stock_ledger::{record_debit, DebitSpec};
use crate::services::sync_engine::{EntityType, PushOperation, SyncOperation};

pub const OIL_FLAG_MIN: f64 = 34.0;
pub const OIL_FLAG_MAX: f64 = 36.0;

pub fn oil_flagged(liters: f64) -> bool {
    !(OIL_FLAG_MIN..=OIL_FLAG_MAX).contains(&liters)
}

/// Dismount reason → the tire's status after it comes off.
/// worn/damaged leave inventory; puncture goes to repair; retread to the
/// retreader; rotation/other return to the used pool.
pub fn dismount_status(reason: &str) -> &'static str {
    match reason {
        "worn" | "damaged" => "scrapped",
        "retread" => "retreading",
        "puncture" => "in_repair",
        _ => "in_stock_used",
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Rewrite server-owned derived fields before an INSERT.
pub fn normalize_insert(entity: EntityType, payload: &mut Value) {
    if entity == EntityType::OilChanges {
        if let Some(l) = payload.get("liters").and_then(as_f64) {
            payload["flagged"] = Value::Bool(oil_flagged(l));
        }
    }
}

/// Rewrite server-owned derived fields before an UPDATE. The oil flag is
/// server authority: a client-sent `flagged` is stripped, and when the update
/// touches `liters` the flag is recomputed from the new value — otherwise an
/// update from 35 L to 50 L would sail through still unflagged.
///
/// NOTE (accepted drift): editing or deleting an oil change does NOT adjust
/// the stock ledger — movements are append-only and mirrored to Falcon, whose
/// debit endpoint has no compensation contract. Corrections are entered as
/// manual stock credits on the dashboard.
pub fn normalize_update(entity: EntityType, payload: &mut Value) {
    if entity == EntityType::OilChanges {
        if let Some(obj) = payload.as_object_mut() {
            obj.remove("flagged");
        }
        if let Some(l) = payload.get("liters").and_then(as_f64) {
            payload["flagged"] = Value::Bool(oil_flagged(l));
        }
    }
}

/// Run inside the op's savepoint after a successful apply.
/// `row_before` is the row as it existed before this op (None for inserts).
/// Returns the ref_ids of stock movements recorded, for post-commit mirroring.
pub async fn after_apply(
    tx: &mut Transaction<'_, Postgres>,
    op: &PushOperation,
    payload: &Value,
    row_before: Option<&Value>,
) -> ApiResult<Vec<Uuid>> {
    let mut debits = Vec::new();
    match (op.entity_type, op.operation) {
        // An oil change consumes liters from the mirrored ledger.
        (EntityType::OilChanges, SyncOperation::Insert) => {
            let Ok(ref_id) = Uuid::parse_str(&op.entity_id) else { return Ok(debits) };
            let spec = DebitSpec {
                kind: "oil_debit",
                brand: None,
                model: None,
                size: None,
                oil_type: payload.get("oil_type").and_then(|v| v.as_str()).map(String::from),
                qty: None,
                liters: payload.get("liters").and_then(as_f64),
                ref_id,
                work_order_id: payload
                    .get("work_order_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok()),
            };
            if record_debit(tx, &spec).await? {
                debits.push(ref_id);
            }
        }

        // A NEW tire asset minted from fungible stock consumes one unit.
        // Legacy onboarding (origin = "legacy") has no stock effect.
        (EntityType::Tires, SyncOperation::Insert) => {
            if payload.get("origin").and_then(|v| v.as_str()) == Some("shipment") {
                let Ok(ref_id) = Uuid::parse_str(&op.entity_id) else { return Ok(debits) };
                let spec = DebitSpec {
                    kind: "tire_debit",
                    brand: payload.get("brand").and_then(|v| v.as_str()).map(String::from),
                    model: payload.get("model").and_then(|v| v.as_str()).map(String::from),
                    size: payload.get("size").and_then(|v| v.as_str()).map(String::from),
                    oil_type: None,
                    qty: Some(1),
                    liters: None,
                    ref_id,
                    work_order_id: None,
                };
                if record_debit(tx, &spec).await? {
                    debits.push(ref_id);
                }
            }
        }

        // A mount marks the tire mounted — unless the inserted assignment is
        // ALREADY CLOSED (legacy onboarding history, or a client outbox that
        // coalesced mount+dismount into one insert): then the dismount
        // derivation applies instead, or the tire would stick 'mounted'.
        (EntityType::TireAssignments, SyncOperation::Insert) => {
            if let Some(tire_id) = payload
                .get("tire_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
            {
                let closed = payload
                    .get("dismounted_at")
                    .map(|v| !v.is_null())
                    .unwrap_or(false);
                if closed {
                    let reason = payload
                        .get("dismount_reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("other");
                    sqlx::query(
                        "UPDATE tires SET status = $2::tire_status, updated_at = now(), \
                         sync_version = sync_version + 1 WHERE id = $1",
                    )
                    .bind(tire_id)
                    .bind(dismount_status(reason))
                    .execute(&mut **tx)
                    .await?;
                } else {
                    sqlx::query(
                        "UPDATE tires SET status = 'mounted', updated_at = now(), \
                         sync_version = sync_version + 1 WHERE id = $1 AND status <> 'mounted'",
                    )
                    .bind(tire_id)
                    .execute(&mut **tx)
                    .await?;
                }
            }
        }

        // A lifecycle event (repair out/in, retread, restock, scrap) DERIVES
        // the tire's status server-side. The client enqueues only the event —
        // never a competing `tires` update, which would carry a base
        // sync_version made stale by these very derivations and dead-letter.
        (EntityType::TireStatusEvents, SyncOperation::Insert) => {
            if let (Some(tire_id), Some(to_status)) = (
                payload
                    .get("tire_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok()),
                payload.get("to_status").and_then(|v| v.as_str()),
            ) {
                // Never override 'mounted' from an event — mount state is the
                // assignment projection's job.
                if to_status != "mounted" {
                    sqlx::query(
                        "UPDATE tires SET status = $2::tire_status, updated_at = now(), \
                         sync_version = sync_version + 1 \
                          WHERE id = $1 AND status <> 'mounted' AND status <> $2::tire_status",
                    )
                    .bind(tire_id)
                    .bind(to_status)
                    .execute(&mut **tx)
                    .await?;
                }
                // A restock FROM the retreader completes a retread — the
                // lineage counter moves here (it was schema-only before:
                // nothing ever wrote retread_count).
                let event_type = payload.get("event_type").and_then(|v| v.as_str());
                let from_status = payload.get("from_status").and_then(|v| v.as_str());
                if event_type == Some("restock") && from_status == Some("retreading") {
                    sqlx::query(
                        "UPDATE tires SET retread_count = COALESCE(retread_count,0) + 1, \
                         updated_at = now(), sync_version = sync_version + 1 WHERE id = $1",
                    )
                    .bind(tire_id)
                    .execute(&mut **tx)
                    .await?;
                }
            }
        }

        // A dismount (dismounted_at transitioning NULL → set) derives the
        // tire's next status from the reason and accrues km — road positions
        // only; a tire riding as a spare doesn't roll.
        (EntityType::TireAssignments, SyncOperation::Update) => {
            let was_open = row_before
                .and_then(|r| r.get("dismounted_at"))
                .map(|v| v.is_null())
                .unwrap_or(false);
            let closes = payload.get("dismounted_at").map(|v| !v.is_null()).unwrap_or(false);
            if was_open && closes {
                type ClosedAssignmentRow = (Uuid, Option<i32>, Option<i32>, Option<String>, bool);
                let row: Option<ClosedAssignmentRow> =
                    sqlx::query_as(
                        "SELECT ta.tire_id, ta.mounted_odometer, ta.dismounted_odometer, \
                                ta.dismount_reason::text, COALESCE(cp.is_spare, false) \
                           FROM tire_assignments ta \
                           LEFT JOIN chassis_positions cp ON cp.id = ta.position_id \
                          WHERE ta.id::text = $1",
                    )
                    .bind(&op.entity_id)
                    .fetch_optional(&mut **tx)
                    .await?;
                if let Some((tire_id, _mounted_km, _dismounted_km, reason, _is_spare)) = row {
                    // Status only. Lifetime km is DERIVED from the assignment
                    // history (0015) — never accrued into tires.total_km.
                    let status = dismount_status(reason.as_deref().unwrap_or("other"));
                    sqlx::query(
                        "UPDATE tires SET status = $2::tire_status, \
                                updated_at = now(), sync_version = sync_version + 1 \
                          WHERE id = $1",
                    )
                    .bind(tire_id)
                    .bind(status)
                    .execute(&mut **tx)
                    .await?;
                }
            }
        }

        // Tombstoning an OPEN assignment un-mounts the tire — otherwise the
        // partial unique indexes free the position for re-use while the tire
        // itself stays 'mounted' forever.
        (EntityType::TireAssignments, SyncOperation::Delete) => {
            let was_open = row_before
                .and_then(|r| r.get("dismounted_at"))
                .map(|v| v.is_null())
                .unwrap_or(false);
            if was_open {
                if let Some(tire_id) = row_before
                    .and_then(|r| r.get("tire_id"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                {
                    sqlx::query(
                        "UPDATE tires SET status = 'in_stock_used', updated_at = now(), \
                         sync_version = sync_version + 1 WHERE id = $1 AND status = 'mounted'",
                    )
                    .bind(tire_id)
                    .execute(&mut **tx)
                    .await?;
                }
            }
        }

        _ => {}
    }
    Ok(debits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oil_flag_band_is_inclusive() {
        assert!(oil_flagged(33.9));
        assert!(!oil_flagged(34.0));
        assert!(!oil_flagged(35.0));
        assert!(!oil_flagged(36.0));
        assert!(oil_flagged(36.1));
        assert!(oil_flagged(0.0));
    }

    #[test]
    fn dismount_reason_maps_to_status() {
        assert_eq!(dismount_status("worn"), "scrapped");
        assert_eq!(dismount_status("damaged"), "scrapped");
        assert_eq!(dismount_status("retread"), "retreading");
        assert_eq!(dismount_status("puncture"), "in_repair");
        assert_eq!(dismount_status("rotation"), "in_stock_used");
        assert_eq!(dismount_status("other"), "in_stock_used");
        assert_eq!(dismount_status("garbage"), "in_stock_used");
    }

    #[test]
    fn normalize_insert_overrides_client_flag() {
        let mut p = serde_json::json!({"liters": 50.0, "flagged": false});
        normalize_insert(EntityType::OilChanges, &mut p);
        assert_eq!(p["flagged"], serde_json::Value::Bool(true));

        let mut p = serde_json::json!({"liters": "35", "flagged": true});
        normalize_insert(EntityType::OilChanges, &mut p);
        assert_eq!(p["flagged"], serde_json::Value::Bool(false));
    }
}
