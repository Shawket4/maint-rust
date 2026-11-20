
use serde_json::Value;


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
