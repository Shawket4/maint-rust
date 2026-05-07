use actix_web::{get, web, HttpResponse, Scope};
use serde::Deserialize;
use serde_json::json;

use crate::auth::middleware::BearerToken;
use crate::error::ApiResult;
use crate::falcon::invoices::{
    fetch_invoices_cached, search_invoices, sync_invoices_into_cache,
};
use crate::handlers::AppState;

#[derive(Deserialize)]
struct PageQuery {
    #[serde(default = "default_page")]
    page: u32,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_page() -> u32 {
    1
}
fn default_limit() -> u32 {
    20
}

#[get("/cache/service-invoices")]
async fn get_invoices(
    state: web::Data<AppState>,
    token: BearerToken,
    q: web::Query<PageQuery>,
) -> ApiResult<HttpResponse> {
    let v = fetch_invoices_cached(
        &state.falcon,
        &state.cache,
        &token.0,
        q.page,
        q.limit,
        state.falcon_invoices_ttl,
    )
    .await?;
    Ok(HttpResponse::Ok().json(v))
}

#[derive(Deserialize)]
struct SyncQuery {
    /// "true" forces a full walk; default is incremental.
    #[serde(default)]
    full: bool,
}

#[get("/cache/service-invoices/sync")]
async fn sync_invoices(
    state: web::Data<AppState>,
    token: BearerToken,
    q: web::Query<SyncQuery>,
) -> ApiResult<HttpResponse> {
    let count = sync_invoices_into_cache(
        &state.falcon,
        &state.cache,
        &state.pool,
        &token.0,
        q.full,
        state.falcon_invoices_ttl,
    )
    .await?;
    // Per §4: invalidate paged proxy cache on sync — the next /cache/service-invoices
    // request should return fresh data.
    let _ = state.cache.delete_pattern("falcon:invoices:p*").await;
    Ok(HttpResponse::Ok().json(json!({
        "processed": count,
        "full": q.full,
    })))
}

#[derive(Deserialize)]
struct SearchQuery {
    query: String,
    #[serde(default = "default_page")]
    page: u32,
    #[serde(default = "default_limit")]
    limit: u32,
}

#[get("/cache/service-invoices/search")]
async fn search_invoices_handler(
    state: web::Data<AppState>,
    token: BearerToken,
    q: web::Query<SearchQuery>,
) -> ApiResult<HttpResponse> {
    let v = search_invoices(
        &state.falcon,
        &state.cache,
        &token.0,
        &q.query,
        q.page,
        q.limit,
        state.falcon_invoices_ttl,
    )
    .await?;
    Ok(HttpResponse::Ok().json(v))
}

pub fn routes() -> Scope {
    // Order matters: more-specific paths must register before less-specific ones,
    // otherwise actix's path matcher will route /sync to the parameterized handler.
    web::scope("")
        .service(sync_invoices)
        .service(search_invoices_handler)
        .service(get_invoices)
}
