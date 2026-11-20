//! Old service-invoice lines → `legacy_service_lines` (migration 0025).
//!
//! Reads Falcon's `service_invoices` + `inspection_items` through the
//! read-only pool and keeps a local copy of every live line with the
//! maintenance type it was recognised as. Safe to run any number of times:
//! lines are upserted by Falcon's item id, lines Falcon no longer has are
//! removed, and every line's type is recomputed (so a new keyword or a review
//! decision applies to history at once).

use sqlx::PgPool;

use crate::error::ApiResult;

#[derive(Debug, Default, serde::Serialize)]
pub struct ImportSummary {
    pub lines: i64,
    pub recognised: i64,
    pub unrecognised: i64,
    pub removed: i64,
}

pub type FalconLine = (i64, i64, i64, String, chrono::DateTime<chrono::Utc>, Option<i64>);

pub async fn import(pool: &PgPool, falcon: &PgPool) -> ApiResult<ImportSummary> {
    let lines: Vec<FalconLine> = sqlx::query_as(
        "SELECT ii.id::bigint, si.id::bigint, si.car_id::bigint, ii.service, si.date::timestamptz, si.meter_reading::bigint \
           FROM inspection_items ii \
           JOIN service_invoices si ON si.id = ii.service_invoice_id \
          WHERE ii.deleted_at IS NULL AND si.deleted_at IS NULL \
            AND si.car_id > 0 AND btrim(COALESCE(ii.service, '')) <> ''",
    )
    .fetch_all(falcon)
    .await?;
    store(pool, lines).await
}

/// The same import through Falcon's own API (`GET /api/service-invoices`, in
/// the caller's name), for a server that has no direct line to Falcon's
/// database.
pub async fn import_via_api(pool: &PgPool, falcon: &crate::falcon::FalconClient, token: &str) -> ApiResult<ImportSummary> {
    let v = falcon.get_json(crate::falcon::invoices::FALCON_INVOICES_PATH, token).await?;
    let invoices = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| crate::error::ApiError::Unavailable("Falcon did not answer with a list of invoices".into()))?;
    store(pool, lines_from_api(invoices)).await
}

/// Falcon's JSON → lines. gorm's own fields are capitalised (`ID`, `DeletedAt`).
pub fn lines_from_api(invoices: &[serde_json::Value]) -> Vec<FalconLine> {
    let mut out = Vec::new();
    for inv in invoices {
        if !inv.get("DeletedAt").map(|d| d.is_null()).unwrap_or(true) {
            continue;
        }
        let (Some(invoice), Some(car)) = (inv.get("ID").and_then(|x| x.as_i64()), inv.get("car_id").and_then(|x| x.as_i64())) else {
            continue;
        };
        let Some(date) = inv
            .get("date")
            .and_then(|d| d.as_str())
            .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
            .map(|d| d.with_timezone(&chrono::Utc))
        else {
            continue;
        };
        if car <= 0 {
            continue;
        }
        let km = inv.get("meter_reading").and_then(|x| x.as_i64());
        for item in inv.get("inspection_items").and_then(|i| i.as_array()).map(Vec::as_slice).unwrap_or(&[]) {
            if !item.get("DeletedAt").map(|d| d.is_null()).unwrap_or(true) {
                continue;
            }
            let (Some(id), Some(service)) = (item.get("ID").and_then(|x| x.as_i64()), item.get("service").and_then(|x| x.as_str())) else {
                continue;
            };
            if service.trim().is_empty() {
                continue;
            }
            out.push((id, invoice, car, service.to_string(), date, km));
        }
    }
    out
}

/// Without the database link, read the invoices through the API at most once
/// an hour, on the back of a signed-in request. Returns at once.
pub fn refresh_via_api_if_stale(state: &crate::handlers::AppState, token: &str) {
    use std::sync::atomic::{AtomicI64, Ordering};
    static LAST: AtomicI64 = AtomicI64::new(0);
    if state.falcon_pool.is_some() {
        return;
    }
    let now = chrono::Utc::now().timestamp();
    let last = LAST.load(Ordering::Relaxed);
    if now - last < 3600 || LAST.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        return;
    }
    let (pool, falcon, token) = (state.pool.clone(), state.falcon.clone(), token.to_string());
    actix_web::rt::spawn(async move {
        match import_via_api(&pool, &falcon, &token).await {
            Ok(s) => tracing::info!(lines = s.lines, recognised = s.recognised, "legacy service history imported through the API"),
            Err(e) => {
                // Let the next request try again rather than wait an hour.
                LAST.store(0, Ordering::Relaxed);
                tracing::warn!(error = %e, "legacy service history import through the API failed");
            }
        }
    });
}

async fn store(pool: &PgPool, lines: Vec<FalconLine>) -> ApiResult<ImportSummary> {
    let mut tx = pool.begin().await?;
    let ids: Vec<i64> = lines.iter().map(|l| l.0).collect();
    for (item, invoice, car, service, date, km) in &lines {
        // A meter reading that cannot be an odometer is no reading.
        let km = km.filter(|k| (1..=9_999_999).contains(k)).map(|k| k as i32);
        let Ok(car) = i32::try_from(*car) else { continue };
        sqlx::query(
            "INSERT INTO legacy_service_lines (falcon_item_id, falcon_invoice_id, vehicle_id, service, norm_text, done_at, km) \
             VALUES ($1, $2, $3, $4, maint_norm($4), $5, $6) \
             ON CONFLICT (falcon_item_id) DO UPDATE SET \
                falcon_invoice_id = EXCLUDED.falcon_invoice_id, vehicle_id = EXCLUDED.vehicle_id, \
                service = EXCLUDED.service, norm_text = EXCLUDED.norm_text, done_at = EXCLUDED.done_at, km = EXCLUDED.km",
        )
        .bind(item)
        .bind(invoice)
        .bind(car)
        .bind(service.trim())
        .bind(date)
        .bind(km)
        .execute(&mut *tx)
        .await?;
    }
    // An empty answer is far more likely a fault than an emptied Falcon.
    let removed = if ids.is_empty() {
        0
    } else {
        sqlx::query("DELETE FROM legacy_service_lines WHERE falcon_item_id <> ALL($1)")
            .bind(&ids)
            .execute(&mut *tx)
            .await?
            .rows_affected() as i64
    };
    reclassify(&mut tx).await?;
    let (total, recognised): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COUNT(category_id) FROM legacy_service_lines").fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(ImportSummary { lines: total, recognised, unrecognised: total - recognised, removed })
}

/// Apply the current keywords and review decisions to every stored line.
pub async fn reclassify(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> ApiResult<()> {
    sqlx::query(
        "UPDATE legacy_service_lines l SET category_id = c.cat \
           FROM (SELECT DISTINCT norm_text, maint_legacy_category(norm_text) AS cat FROM legacy_service_lines) c \
          WHERE c.norm_text = l.norm_text AND l.category_id IS DISTINCT FROM c.cat",
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// One person's name and permission, as Falcon just reported them.
pub async fn remember_user(pool: &PgPool, id: i64, name: &str, permission: i32) -> ApiResult<()> {
    let name = name.trim();
    sqlx::query(
        "INSERT INTO users_cache (id, name, permission, fetched_at) VALUES ($1, NULLIF($2, ''), $3, now())          ON CONFLICT (id) DO UPDATE SET name = COALESCE(EXCLUDED.name, users_cache.name),                permission = EXCLUDED.permission, fetched_at = now()",
    )
    .bind(id)
    .bind(name)
    .bind(permission)
    .execute(pool)
    .await?;
    Ok(())
}

/// Names and permissions of every Falcon user, into `users_cache`.
pub async fn import_users(pool: &PgPool, falcon: &PgPool) -> ApiResult<u64> {
    let users: Vec<(i64, Option<String>, Option<i64>)> =
        sqlx::query_as("SELECT id::bigint, name::text, permission::bigint FROM users").fetch_all(falcon).await?;
    let mut n = 0;
    for (id, name, permission) in users {
        remember_user(pool, id, name.as_deref().unwrap_or(""), permission.unwrap_or(0) as i32).await?;
        n += 1;
    }
    Ok(n)
}

/// Import at start-up and every hour, when the Falcon database is configured.
pub fn spawn_periodic(pool: PgPool, falcon: Option<PgPool>) {
    let Some(falcon) = falcon else { return };
    actix_web::rt::spawn(async move {
        loop {
            if let Err(e) = import_users(&pool, &falcon).await {
                tracing::warn!(error = %e, "user names import failed");
            }
            match import(&pool, &falcon).await {
                Ok(s) => tracing::info!(lines = s.lines, recognised = s.recognised, unrecognised = s.unrecognised, "legacy service history imported"),
                Err(e) => tracing::warn!(error = %e, "legacy service history import failed"),
            }
            actix_web::rt::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn falcon_json_becomes_lines() {
        let invoices = vec![
            json!({"ID": 10, "DeletedAt": null, "car_id": 4, "date": "2026-03-01T00:00:00+02:00", "meter_reading": 120000,
                   "inspection_items": [
                       {"ID": 100, "DeletedAt": null, "service": "تغيير زيت موتور"},
                       {"ID": 101, "DeletedAt": "2026-03-02T00:00:00Z", "service": "اتمسح"},
                       {"ID": 102, "DeletedAt": null, "service": "   "}]}),
            json!({"ID": 11, "DeletedAt": "2026-04-01T00:00:00Z", "car_id": 4, "date": "2026-03-01T00:00:00Z",
                   "inspection_items": [{"ID": 103, "service": "فرامل"}]}),
            json!({"ID": 12, "car_id": 0, "date": "2026-03-01T00:00:00Z", "inspection_items": [{"ID": 104, "service": "فرامل"}]}),
            json!({"ID": 13, "car_id": 5, "date": "not a date", "inspection_items": [{"ID": 105, "service": "فرامل"}]}),
            json!({"ID": 14, "car_id": 5, "date": "2026-05-01T10:00:00Z"}),
        ];
        let lines = lines_from_api(&invoices);
        assert_eq!(lines.len(), 1, "deleted, blank, car-less and undated rows are left out");
        assert_eq!((lines[0].0, lines[0].1, lines[0].2, lines[0].5), (100, 10, 4, Some(120000)));
        assert_eq!(lines[0].4.to_rfc3339(), "2026-02-28T22:00:00+00:00");
    }
}
