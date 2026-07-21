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

        // A mount marks the tire mounted.
        (EntityType::TireAssignments, SyncOperation::Insert) => {
            if let Some(tire_id) = payload
                .get("tire_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
            {
                sqlx::query(
                    "UPDATE tires SET status = 'mounted', updated_at = now(), \
                     sync_version = sync_version + 1 WHERE id = $1 AND status <> 'mounted'",
                )
                .bind(tire_id)
                .execute(&mut **tx)
                .await?;
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
                let row: Option<(Uuid, Option<i32>, Option<i32>, Option<String>, bool)> =
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
                if let Some((tire_id, mounted_km, dismounted_km, reason, is_spare)) = row {
                    let status = dismount_status(reason.as_deref().unwrap_or("other"));
                    let delta = match (mounted_km, dismounted_km, is_spare) {
                        (Some(m), Some(d), false) if d > m => d - m,
                        _ => 0,
                    };
                    sqlx::query(
                        "UPDATE tires SET status = $2::tire_status, \
                                total_km = COALESCE(total_km, 0) + $3, \
                                updated_at = now(), sync_version = sync_version + 1 \
                          WHERE id = $1",
                    )
                    .bind(tire_id)
                    .bind(status)
                    .bind(delta)
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
