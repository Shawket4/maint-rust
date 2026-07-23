use actix_web::{get, post, web, HttpResponse};
use chrono::{TimeZone, Utc};

use crate::auth::middleware::BearerToken;
use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::ApiResult;
use crate::handlers::AppState;
use crate::services::stock_ledger;
use crate::services::sync_engine::{apply_push_batch, pull_rows, PullQuery, PullResponse, PushBody};

#[post("/sync/push")]
async fn sync_push(
    state: web::Data<AppState>,
    claims: AuthClaims,
    token: BearerToken,
    body: web::Json<PushBody>,
) -> ApiResult<HttpResponse> {
    // Per §7.2 each operation is independent: applied / conflict / error per row.
    // apply_push_batch wraps every op in a SAVEPOINT so a constraint violation
    // rolls back only its own op instead of aborting the whole batch.
    let (resp, _debit_refs) =
        apply_push_batch(&state.pool, &body.into_inner(), claims.user_id).await?;
    // Stock movements derived while applying (oil changes, mounts from stock)
    // are mirrored up to Falcon AFTER the commit — best-effort, idempotent on
    // ref_id. The unmirrored sweep also replays any backlog whose earlier
    // mirror attempt failed (this call IS the "retried later" the marker
    // column promises — one indexed SELECT when the backlog is empty; the
    // fresh refs from this batch are part of the unmirrored set).
    stock_ledger::mirror_unmirrored(&state, &token.0).await;
    Ok(HttpResponse::Ok().json(resp))
}

#[get("/sync/pull")]
async fn sync_pull(
    pool: web::Data<PgPool>,
    q: web::Query<PullQuery>,
) -> ApiResult<HttpResponse> {
    let since = q.since.unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap());
    let limit = q.limit.clamp(1, 1000);
    let resp: PullResponse =
        pull_rows(pool.get_ref(), q.entity_type, since, q.after_id.as_deref(), limit).await?;
    Ok(HttpResponse::Ok().json(resp))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(sync_push).service(sync_pull);
}
