use actix_web::{get, post, web, HttpResponse, Scope};
use chrono::{TimeZone, Utc};

use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::ApiResult;
use crate::services::sync_engine::{
    apply_push_operation, pull_rows, PullQuery, PullResponse, PushBody, PushResponse,
};

#[post("/sync/push")]
async fn sync_push(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    body: web::Json<PushBody>,
) -> ApiResult<HttpResponse> {
    let body = body.into_inner();

    // Per §7.2: each operation is independent — applied / conflict / error per row.
    // We apply all in one transaction so that a transport-level failure mid-batch
    // doesn't leave a partial state. Per-op conflicts/errors do NOT roll back the batch.
    let mut tx = pool.begin().await?;
    let mut results = Vec::with_capacity(body.operations.len());
    for op in &body.operations {
        let r = apply_push_operation(&mut tx, op, claims.user_id).await?;
        results.push(r);
    }
    tx.commit().await?;

    Ok(HttpResponse::Ok().json(PushResponse { results }))
}

#[get("/sync/pull")]
async fn sync_pull(
    pool: web::Data<PgPool>,
    q: web::Query<PullQuery>,
) -> ApiResult<HttpResponse> {
    let since = q.since.unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap());
    let limit = q.limit.clamp(1, 1000);
    let resp: PullResponse = pull_rows(pool.get_ref(), q.entity_type, since, limit).await?;
    Ok(HttpResponse::Ok().json(resp))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(sync_push).service(sync_pull);
}
