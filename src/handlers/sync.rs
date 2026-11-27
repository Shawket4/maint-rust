use actix_web::{get, post, web, HttpRequest, HttpResponse};
use chrono::{TimeZone, Utc};
use serde_json::{json, Value};

use crate::auth::middleware::BearerToken;
use crate::auth::claims::level;
use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};
use crate::handlers::AppState;
use crate::services::stock_ledger;
use crate::services::sync_engine::{
    apply_push_batch, fetch_record, pull_rows, EntityType, PullQuery, PullResponse, PushBody,
};

#[post("/sync/push")]
async fn sync_push(
    state: web::Data<AppState>,
    claims: AuthClaims,
    token: BearerToken,
    body: web::Json<PushBody>,
) -> ApiResult<HttpResponse> {
    claims.require(level::USE, "syncing")?;
    let (resp, stock_moved) = apply_push_batch(&state.pool, &body.into_inner(), claims.user_id).await?;
    // Stock movements go up to Falcon in the background. Doing it here made
    // the client wait on up to 100 upstream calls and time out on a push that
    // had already committed.
    if stock_moved {
        stock_ledger::spawn_mirror(&state, &token.0);
    }
    Ok(HttpResponse::Ok().json(resp))
}

/// Streams that no longer exist. Garage apps from before they went still ask
/// for them on every pull; an empty stream keeps those pulls (and the pushes
/// queued behind them) working until the app is updated.
const RETIRED_STREAMS: &[&str] = &["attendance_settings"];

#[get("/sync/pull")]
async fn sync_pull(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    req: HttpRequest,
) -> ApiResult<HttpResponse> {
    claims.require(level::USE, "syncing")?;
    let raw = web::Query::<Vec<(String, String)>>::from_query(req.query_string()).map(|q| q.into_inner()).unwrap_or_default();
    if raw.iter().any(|(k, v)| k == "entity_type" && RETIRED_STREAMS.contains(&v.as_str())) {
        return Ok(HttpResponse::Ok().json(json!({ "rows": [], "has_more": false })));
    }
    let q = web::Query::<PullQuery>::from_query(req.query_string()).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let since = q.since.unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap());
    let limit = q.limit.clamp(1, 1000);
    let resp: PullResponse =
        pull_rows(pool.get_ref(), q.entity_type, since, q.after_id.as_deref(), limit).await?;
    Ok(HttpResponse::Ok().json(resp))
}

/// One synced row by entity type + PK, with the sync_version a follow-up
/// update/delete must send. The advisor uses it for the before-state of a
/// proposed change. Same table/PK map as push/pull; soft-deleted rows are
/// returned as-is (their `deleted_at` is set).
#[get("/records/{entity_type}/{entity_id}")]
async fn get_record(
    pool: web::Data<PgPool>,
    path: web::Path<(String, String)>,
) -> ApiResult<HttpResponse> {
    let (entity_type, entity_id) = path.into_inner();
    let entity: EntityType = serde_json::from_value(Value::String(entity_type.clone()))
        .map_err(|_| ApiError::BadRequest(format!("unknown entity_type: {entity_type}")))?;
    let row = fetch_record(pool.get_ref(), entity, &entity_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("{entity_type} {entity_id}")))?;
    let sync_version = row.get("sync_version").and_then(Value::as_i64);
    Ok(HttpResponse::Ok().json(json!({
        "entity_type": entity,
        "entity_id": entity_id,
        "row": row,
        "sync_version": sync_version,
    })))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(sync_push).service(sync_pull).service(get_record);
}
