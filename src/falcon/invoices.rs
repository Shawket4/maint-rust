use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use super::cache::CacheManager;
use super::client::FalconClient;
use crate::error::ApiResult;

pub const FALCON_INVOICES_PATH: &str = "/api/service-invoices";
pub const FALCON_INVOICE_SEARCH_PATH: &str = "/api/service-invoices/search";

fn cache_key_invoices(page: u32, limit: u32) -> String {
    format!("falcon:invoices:p{page}:l{limit}")
}
fn cache_key_search(query: &str, car_id: Option<i32>, page: u32, limit: u32) -> String {
    // Simple sanitization — colons and whitespace would mangle the keyspace
    let q = query.replace([':', ' ', '\n', '\r', '\t'], "_");
    match car_id {
        Some(cid) => format!("falcon:invoices:search:{q}:c{cid}:p{page}:l{limit}"),
        None => format!("falcon:invoices:search:{q}:p{page}:l{limit}"),
    }
}

pub async fn fetch_invoices_cached(
    falcon: &FalconClient,
    cache: &CacheManager,
    token: &str,
    page: u32,
    limit: u32,
    ttl_seconds: u64,
) -> ApiResult<Value> {
    let key = cache_key_invoices(page, limit);
    if let Some(hit) = cache.get_json::<Value>(&key).await? {
        return Ok(hit);
    }
    let path = format!("{FALCON_INVOICES_PATH}?page={page}&limit={limit}");
    let v = falcon.get_json(&path, token).await?;
    cache.set_json(&key, &v, ttl_seconds).await?;
    Ok(v)
}

#[allow(clippy::too_many_arguments)] // thin passthrough of the Falcon search params
pub async fn search_invoices(
    falcon: &FalconClient,
    cache: &CacheManager,
    token: &str,
    query: &str,
    car_id: Option<i32>,
    page: u32,
    limit: u32,
    ttl_seconds: u64,
) -> ApiResult<Value> {
    let key = cache_key_search(query, car_id, page, limit);
    if let Some(hit) = cache.get_json::<Value>(&key).await? {
        return Ok(hit);
    }
    let qenc = urlencoding(query);
    let mut path = format!("{FALCON_INVOICE_SEARCH_PATH}?query={qenc}&page={page}&limit={limit}");
    if let Some(cid) = car_id {
        use std::fmt::Write;
        write!(&mut path, "&car_id={cid}").unwrap();
    }
    let v = falcon.get_json(&path, token).await?;
    // Search results have shorter TTL because they're query-dependent and rare
    cache.set_json(&key, &v, ttl_seconds.min(15)).await?;
    Ok(v)
}

fn urlencoding(s: &str) -> String {
    // Conservative percent-encoding for the query parameter
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

/// Walk pages of /api/service-invoices, upserting into cache tables.
/// Returns count of invoices processed. Stops when first fully-known page is encountered.
pub async fn sync_invoices_into_cache(
    falcon: &FalconClient,
    cache: &CacheManager,
    pool: &PgPool,
    token: &str,
    full: bool,
    ttl_seconds: u64,
) -> ApiResult<u64> {
    let limit: u32 = if full { 100 } else { 20 };
    let mut page: u32 = 1;
    let mut processed: u64 = 0;
    let mut total_pages: u32 = 1;

    loop {
        let resp = fetch_invoices_cached(falcon, cache, token, page, limit, ttl_seconds).await?;
        let data = resp
            .get("data")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if data.is_empty() {
            break;
        }
        if let Some(p) = resp.get("pagination") {
            if let Some(tp) = p.get("totalPages").and_then(|x| x.as_i64()) {
                total_pages = tp as u32;
            }
        }

        let (inserted, all_known) = upsert_invoices_page(pool, &data).await?;
        processed += inserted;

        // Incremental: when we hit a page where everything was already known, stop.
        if !full && all_known {
            break;
        }

        if page >= total_pages {
            break;
        }
        page += 1;
    }
    Ok(processed)
}

/// Upsert a page of invoices + their nested items.
/// Returns (rows_processed, all_were_already_known).
async fn upsert_invoices_page(pool: &PgPool, page_rows: &[Value]) -> ApiResult<(u64, bool)> {
    if page_rows.is_empty() {
        return Ok((0, true));
    }
    let mut tx = pool.begin().await?;
    let mut all_known = true;

    for inv in page_rows {
        let Some(id) = inv.get("id").and_then(|x| x.as_i64()).map(|x| x as i32).or_else(|| {
            inv.get("ID").and_then(|x| x.as_i64()).map(|x| x as i32)
        }) else {
            continue;
        };
        let car_id = inv.get("car_id").and_then(|x| x.as_i64()).map(|x| x as i32);
        let Some(car_id) = car_id.or_else(|| {
            inv.get("car")
                .and_then(|c| c.get("id"))
                .and_then(|x| x.as_i64())
                .map(|x| x as i32)
        }) else {
            continue;
        };
        let driver_name = inv.get("driver_name").and_then(|x| x.as_str()).map(str::to_string);
        let date = inv
            .get("date")
            .and_then(|x| x.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));
        let meter_reading = inv.get("meter_reading").and_then(|x| x.as_i64()).map(|x| x as i32);
        let plate_number = inv.get("plate_number").and_then(|x| x.as_str()).map(str::to_string);
        let supervisor = inv.get("supervisor").and_then(|x| x.as_str()).map(str::to_string);
        let operating_region = inv
            .get("operating_region")
            .and_then(|x| x.as_str())
            .map(str::to_string);

        // "Known" = exists AND unchanged. Existence alone made the
        // incremental sweep stop at the first page of familiar ids, so a
        // Falcon-side edit to any invoice older than the newest page was
        // never re-fetched. Comparing raw_payload keeps the sweep walking
        // past pages that contain upstream edits. (serde_json object
        // equality is key-order-insensitive, so jsonb normalization does
        // not cause false mismatches.)
        let existed: Option<(Value,)> =
            sqlx::query_as("SELECT raw_payload FROM service_invoices_cache WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        match &existed {
            None => all_known = false,
            Some((stored,)) if stored != inv => all_known = false,
            _ => {}
        }

        sqlx::query(
            r#"
            INSERT INTO service_invoices_cache (
                id, car_id, driver_name, date, meter_reading,
                plate_number, supervisor, operating_region,
                raw_payload, fetched_at
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9, now())
            ON CONFLICT (id) DO UPDATE SET
                car_id = EXCLUDED.car_id,
                driver_name = EXCLUDED.driver_name,
                date = EXCLUDED.date,
                meter_reading = EXCLUDED.meter_reading,
                plate_number = EXCLUDED.plate_number,
                supervisor = EXCLUDED.supervisor,
                operating_region = EXCLUDED.operating_region,
                raw_payload = EXCLUDED.raw_payload,
                fetched_at = now()
            "#,
        )
        .bind(id)
        .bind(car_id)
        .bind(driver_name)
        .bind(date)
        .bind(meter_reading)
        .bind(plate_number)
        .bind(supervisor)
        .bind(operating_region)
        .bind(inv)
        .execute(&mut *tx)
        .await?;

        // Items
        if let Some(items) = inv.get("inspection_items").and_then(|v| v.as_array()) {
            for it in items {
                let Some(item_id) =
                    it.get("id").and_then(|x| x.as_i64()).map(|x| x as i32).or_else(|| {
                        it.get("ID").and_then(|x| x.as_i64()).map(|x| x as i32)
                    })
                else {
                    continue;
                };
                let service = it.get("service").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let notes = it.get("notes").and_then(|x| x.as_str()).map(str::to_string);
                let item_order = it.get("item_order").and_then(|x| x.as_i64()).map(|x| x as i32);

                sqlx::query(
                    r#"
                    INSERT INTO service_invoice_items_cache (
                        id, service_invoice_id, service, notes, item_order, raw_payload, fetched_at
                    ) VALUES ($1,$2,$3,$4,$5,$6, now())
                    ON CONFLICT (id) DO UPDATE SET
                        service_invoice_id = EXCLUDED.service_invoice_id,
                        service = EXCLUDED.service,
                        notes = EXCLUDED.notes,
                        item_order = EXCLUDED.item_order,
                        raw_payload = EXCLUDED.raw_payload,
                        fetched_at = now()
                    "#,
                )
                .bind(item_id)
                .bind(id)
                .bind(service)
                .bind(notes)
                .bind(item_order)
                .bind(it)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await?;

    let inserted = page_rows.len() as u64;
    Ok((inserted, all_known))
}

