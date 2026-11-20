//! Stock movement ledger — the one place a movement is recorded.
//!
//! Falcon owns the authoritative counts; `tire_stock_cache` / `oil_stock_cache`
//! mirror them. A movement recorded here adjusts the mirror immediately and is
//! pushed to Falcon afterwards (idempotent upstream on `ref_id`).
//!
//! * `ref_id` is unique: a replayed op records nothing twice.
//! * On-hand may go negative — an offline oversell is a count to correct, not
//!   a write to refuse.
//! * The Falcon push never runs on a request path: `spawn_mirror` does it in
//!   the background, one sweep at a time.

use std::sync::atomic::Ordering;

use serde_json::json;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::ApiResult;
use crate::handlers::AppState;

#[derive(Debug, Clone)]
pub struct Movement {
    /// tire_debit | tire_credit | oil_debit | oil_credit
    pub kind: &'static str,
    pub brand: Option<String>,
    pub model: Option<String>,
    pub size: Option<String>,
    pub oil_type: Option<String>,
    pub qty: Option<i32>,
    pub liters: Option<f64>,
    /// Idempotency key (the consuming event's UUID, or one derived from it).
    pub ref_id: Uuid,
    pub work_order_id: Option<Uuid>,
    pub user_id: Option<i64>,
    pub note: Option<String>,
}

/// Record a movement and adjust the mirrored on-hand, inside the caller's
/// transaction. Returns false (and changes nothing) when `ref_id` was already
/// recorded.
pub async fn record(tx: &mut Transaction<'_, Postgres>, m: &Movement) -> ApiResult<bool> {
    let inserted: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO stock_movements \
            (kind, brand, model, size, oil_type, qty, liters, work_order_id, ref_id, user_id, note) \
         VALUES ($1::stock_move_kind,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) \
         ON CONFLICT (ref_id) WHERE ref_id IS NOT NULL DO NOTHING RETURNING id",
    )
    .bind(m.kind)
    .bind(&m.brand)
    .bind(&m.model)
    .bind(&m.size)
    .bind(&m.oil_type)
    .bind(m.qty)
    .bind(m.liters)
    .bind(m.work_order_id)
    .bind(m.ref_id)
    .bind(m.user_id)
    .bind(&m.note)
    .fetch_optional(&mut **tx)
    .await?;
    if inserted.is_none() {
        return Ok(false);
    }

    let sign = if m.kind.ends_with("_credit") { 1 } else { -1 };
    if m.kind.starts_with("tire_") {
        if let Some(brand) = m.brand.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
            // Upsert: a debit against a line the mirror has not seen yet must
            // still show (as a negative), not vanish.
            sqlx::query(
                "INSERT INTO tire_stock_cache (brand, model, size, on_hand_qty) \
                 VALUES ($1, COALESCE($2,''), COALESCE($3,''), $4) \
                 ON CONFLICT (brand, model, size) DO UPDATE \
                    SET on_hand_qty = tire_stock_cache.on_hand_qty + EXCLUDED.on_hand_qty",
            )
            .bind(brand)
            .bind(m.model.as_deref().map(str::trim))
            .bind(m.size.as_deref().map(str::trim))
            .bind(sign * m.qty.unwrap_or(0))
            .execute(&mut **tx)
            .await?;
        }
    } else if let Some(oil) = m.oil_type.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
        sqlx::query(
            "INSERT INTO oil_stock_cache (oil_type, liters_on_hand) VALUES ($1, $2) \
             ON CONFLICT (oil_type) DO UPDATE \
                SET liters_on_hand = oil_stock_cache.liters_on_hand + EXCLUDED.liters_on_hand",
        )
        .bind(oil)
        .bind(f64::from(sign) * m.liters.unwrap_or(0.0))
        .execute(&mut **tx)
        .await?;
    }
    Ok(true)
}

/// Push every un-mirrored movement to Falcon, in the background. At most one
/// sweep runs at a time; a second call while one is running is a no-op (the
/// running sweep, or the next trigger, picks the new rows up).
pub fn spawn_mirror(state: &AppState, token: &str) {
    if state.mirror_busy.swap(true, Ordering::AcqRel) {
        return;
    }
    let state = state.clone();
    let token = token.to_string();
    actix_web::rt::spawn(async move {
        mirror_backlog(&state, &token).await;
        state.mirror_busy.store(false, Ordering::Release);
    });
}

/// One sweep. A movement Falcon refuses for good (4xx) is parked with the
/// reason instead of retrying forever in front of everything else.
pub async fn mirror_backlog(state: &AppState, token: &str) {
    let rows: Vec<(Uuid, serde_json::Value)> = match sqlx::query_as(
        "SELECT id, to_jsonb(m) FROM stock_movements m \
          WHERE mirrored_to_falcon_at IS NULL AND mirror_error IS NULL AND ref_id IS NOT NULL \
          ORDER BY created_at ASC, id ASC LIMIT 200",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "stock mirror: backlog lookup failed");
            return;
        }
    };
    for (id, m) in rows {
        let payload = json!({
            "kind": m.get("kind"), "brand": m.get("brand"), "model": m.get("model"),
            "size": m.get("size"), "oil_type": m.get("oil_type"),
            "qty": m.get("qty"), "liters": m.get("liters"),
            "ref_id": m.get("ref_id"), "work_order_id": m.get("work_order_id"),
        });
        match state.falcon.post_json("/api/maint-stock/debit", token, &payload).await {
            Ok((s, _)) if (200..300).contains(&s) => {
                let _ = sqlx::query("UPDATE stock_movements SET mirrored_to_falcon_at = now() WHERE id = $1")
                    .bind(id)
                    .execute(&state.pool)
                    .await;
            }
            // Our caller's token being rejected says nothing about the row.
            Ok((s, _)) if s == 401 || s == 403 || s == 408 || s == 429 => {
                tracing::warn!(status = s, "stock mirror: Falcon refused the caller; stopping this sweep");
                return;
            }
            Ok((s, body)) if (400..500).contains(&s) => {
                let reason = body
                    .get("error")
                    .or_else(|| body.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("rejected")
                    .chars()
                    .take(300)
                    .collect::<String>();
                tracing::error!(status = s, movement = %id, reason = %reason, "stock mirror: Falcon rejected a movement");
                let _ = sqlx::query(
                    "UPDATE stock_movements SET mirror_error = $2, mirror_attempts = mirror_attempts + 1 WHERE id = $1",
                )
                .bind(id)
                .bind(format!("{s}: {reason}"))
                .execute(&state.pool)
                .await;
            }
            Ok((s, _)) => {
                tracing::warn!(status = s, "stock mirror: Falcon unavailable; will retry");
                let _ = sqlx::query("UPDATE stock_movements SET mirror_attempts = mirror_attempts + 1 WHERE id = $1")
                    .bind(id)
                    .execute(&state.pool)
                    .await;
                return;
            }
            Err(e) => {
                tracing::warn!(error = %e, "stock mirror: Falcon unreachable; will retry");
                return;
            }
        }
    }
}
