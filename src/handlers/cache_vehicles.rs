use actix_web::{get, web, HttpResponse};
use chrono::Utc;
use serde_json::json;

use crate::auth::middleware::BearerToken;
use crate::error::ApiResult;
use crate::falcon::cars::{
    fetch_cars_cached, project_vehicles_and_drivers, upsert_cars_into_cache,
};
use crate::handlers::AppState;

#[get("/cache/vehicles")]
async fn get_cache_vehicles(
    state: web::Data<AppState>,
    token: BearerToken,
) -> ApiResult<HttpResponse> {
    let raw = fetch_cars_cached(
        &state.falcon,
        &state.cache,
        &token.0,
        state.falcon_cars_ttl,
    )
    .await?;
    let (mut vehicles, drivers) = project_vehicles_and_drivers(&raw);

    // Mirror into Postgres for the LATERAL joins in views (vehicles_cache).
    // Best-effort: if DB write fails, we still respond from Falcon.
    if let Err(e) = upsert_cars_into_cache(&state.pool, &mut vehicles, &drivers).await {
        tracing::warn!(error = %e, "failed to upsert vehicles into cache");
    }

    Ok(HttpResponse::Ok().json(json!({
        "vehicles": vehicles,
        "drivers": drivers,
        "fetched_at": Utc::now(),
    })))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(get_cache_vehicles);
}
