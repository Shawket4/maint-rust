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
    let (resp, debit_refs) =
        apply_push_batch(&state.pool, &body.into_inner(), claims.user_id).await?;
    // Stock movements derived while applying (oil changes, mounts from stock)
    // are mirrored up to Falcon AFTER the commit — best-effort, idempotent on
    // ref_id, retried later via `mirrored_to_falcon_at IS NULL` when it fails.
    if !debit_refs.is_empty() {
        stock_ledger::mirror_debits(&state, &token.0, &debit_refs).await;
    }
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
