use actix_web::{get, web, HttpResponse};
use serde::Deserialize;

use crate::auth::middleware::BearerToken;
use crate::error::ApiResult;
use crate::falcon::invoices::{fetch_invoices_cached, search_invoices};
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
        q.page.max(1),
        q.limit.clamp(1, 200),
        state.falcon_invoices_ttl,
    )
    .await?;
    Ok(HttpResponse::Ok().json(v))
}

#[derive(Deserialize)]
struct SearchQuery {
    query: String,
    car_id: Option<i32>,
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
    let query = q.query.trim();
    if query.is_empty() || query.chars().count() > 100 {
        return Err(crate::error::ApiError::BadRequest("the search text must be 1 to 100 characters".into()));
    }
    let v = search_invoices(
        &state.falcon,
        &state.cache,
        &token.0,
        query,
        q.car_id,
        q.page.max(1),
        q.limit.clamp(1, 200),
        state.falcon_invoices_ttl,
    )
    .await?;
    Ok(HttpResponse::Ok().json(v))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(search_invoices_handler)
        .service(get_invoices);
}
