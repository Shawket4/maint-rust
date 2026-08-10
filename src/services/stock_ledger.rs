//! Stock debit ledger — the one place consumption is recorded.
//!
//! Used by two callers: the REST `/stock/debit` endpoint (legacy/manual path)
//! and the sync engine's side effects (a mount or oil change applying via
//! `/sync/push`). Both are idempotent on `ref_id` — the consuming event's UUID —
//! so a replayed op or a double-calling client can never double-decrement.
//!
//! On-hand may go negative: an offline oversell is a physical-count correction,
//! not an integrity failure (§4 of the rebuild plan). Nothing here blocks.

use serde_json::json;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::ApiResult;
use crate::handlers::AppState;

#[derive(Debug, Clone)]
pub struct DebitSpec {
    /// "tire_debit" | "oil_debit"
    pub kind: &'static str,
    pub brand: Option<String>,
    pub model: Option<String>,
    pub size: Option<String>,
    pub oil_type: Option<String>,
    pub qty: Option<i32>,
    pub liters: Option<f64>,
    /// The consuming event's UUID (tire id / oil_change id) — idempotency key.
    pub ref_id: Uuid,
    pub work_order_id: Option<Uuid>,
}

/// Record a movement and decrement the mirrored cache, inside the caller's
/// transaction. Returns false (and does nothing) when `ref_id` was already
/// recorded.
pub async fn record_debit(
    tx: &mut Transaction<'_, Postgres>,
    d: &DebitSpec,
) -> ApiResult<bool> {
    // Exactly-once, race-free: the unique index on ref_id (0018) + ON CONFLICT
    // DO NOTHING means a concurrent/retried debit with the same ref_id inserts
    // nothing and RETURNING yields no row — so we skip the decrement too.
    let inserted: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO stock_movements (kind, brand, model, size, oil_type, qty, liters, work_order_id, ref_id) \
         VALUES ($1::stock_move_kind,$2,$3,$4,$5,$6,$7,$8,$9) \
         ON CONFLICT (ref_id) WHERE ref_id IS NOT NULL DO NOTHING RETURNING id",
    )
    .bind(d.kind)
    .bind(&d.brand)
    .bind(&d.model)
    .bind(&d.size)
    .bind(&d.oil_type)
    .bind(d.qty)
    .bind(d.liters)
    .bind(d.work_order_id)
    .bind(d.ref_id)
    .fetch_optional(&mut **tx)
    .await?;
    if inserted.is_none() {
        return Ok(false);
    }

    match d.kind {
        "tire_debit" => {
            sqlx::query(
                "UPDATE tire_stock_cache SET on_hand_qty = on_hand_qty - $1 \
                 WHERE brand = $2 AND model = COALESCE($3,'') AND size = COALESCE($4,'')",
            )
            .bind(d.qty.unwrap_or(0))
            .bind(&d.brand)
            .bind(&d.model)
            .bind(&d.size)
            .execute(&mut **tx)
            .await?;
        }
        "oil_debit" => {
            sqlx::query(
                "UPDATE oil_stock_cache SET liters_on_hand = liters_on_hand - $1 WHERE oil_type = $2",
            )
            .bind(d.liters.unwrap_or(0.0))
            .bind(&d.oil_type)
            .execute(&mut **tx)
            .await?;
        }
        _ => {}
    }
    Ok(true)
}

/// Best-effort push of recorded movements up to Falcon (idempotent upstream on
/// ref_id). Never fails the caller — an unreachable Falcon leaves the movement
/// un-mirrored, visible via `mirrored_to_falcon_at IS NULL`, for later retry.
pub async fn mirror_debits(state: &AppState, token: &str, refs: &[Uuid]) {
    for ref_id in refs {
        let row: Option<(serde_json::Value,)> = match sqlx::query_as(
            "SELECT to_jsonb(m) FROM stock_movements m WHERE ref_id = $1 AND mirrored_to_falcon_at IS NULL",
        )
        .bind(ref_id)
        .fetch_optional(&state.pool)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "mirror_debits: movement lookup failed");
                continue;
            }
        };
        let Some((m,)) = row else { continue };
        let payload = json!({
            "kind": m.get("kind"), "brand": m.get("brand"), "model": m.get("model"),
            "size": m.get("size"), "oil_type": m.get("oil_type"),
            "qty": m.get("qty"), "liters": m.get("liters"),
            "ref_id": m.get("ref_id"), "work_order_id": m.get("work_order_id"),
        });
        match state.falcon.post_json("/api/maint-stock/debit", token, &payload).await {
            // Any 2xx is an ack — requiring exactly 200 left 201/204 rows
            // permanently un-mirrored.
            Ok((s, _)) if (200..300).contains(&s) => {
                let _ = sqlx::query(
                    "UPDATE stock_movements SET mirrored_to_falcon_at = now() WHERE ref_id = $1",
                )
                .bind(ref_id)
                .execute(&state.pool)
                .await;
            }
            Ok((s, _)) => tracing::warn!(status = s, "falcon debit non-2xx (will reconcile later)"),
            Err(e) => tracing::warn!(error = %e, "falcon debit push failed (offline; reconcile later)"),
        }
    }
}

/// The reconciler the `mirrored_to_falcon_at IS NULL` marker exists for:
/// replay every movement whose one post-commit mirror attempt failed. Called
/// from the push handler (any device syncing retries the backlog) and before
/// stock refreshes (so Falcon's counts include our debits before we overwrite
/// the local cache with them). Best-effort, idempotent upstream on ref_id.
pub async fn mirror_unmirrored(state: &AppState, token: &str) {
    let refs: Vec<(Uuid,)> = match sqlx::query_as(
        "SELECT ref_id FROM stock_movements WHERE mirrored_to_falcon_at IS NULL \
         ORDER BY created_at ASC LIMIT 100",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "mirror_unmirrored: backlog lookup failed");
            return;
        }
    };
    if refs.is_empty() {
        return;
    }
    let refs: Vec<Uuid> = refs.into_iter().map(|t| t.0).collect();
    mirror_debits(state, token, &refs).await;
}
