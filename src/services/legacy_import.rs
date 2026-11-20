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

type FalconLine = (i64, i64, i64, String, chrono::DateTime<chrono::Utc>, Option<i64>);

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

/// Import at start-up and every hour, when the Falcon database is configured.
pub fn spawn_periodic(pool: PgPool, falcon: Option<PgPool>) {
    let Some(falcon) = falcon else { return };
    actix_web::rt::spawn(async move {
        loop {
            match import(&pool, &falcon).await {
                Ok(s) => tracing::info!(lines = s.lines, recognised = s.recognised, unrecognised = s.unrecognised, "legacy service history imported"),
                Err(e) => tracing::warn!(error = %e, "legacy service history import failed"),
            }
            actix_web::rt::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });
}
