//! The AI audit trail, read side (rows are written by auth::ai_audit).
//!
//! The advisor probes `GET /ai-audit?limit=1` before every apply and treats
//! any 2xx as "audit deployed"; the owner reads the trail through it too.

use actix_web::{get, web, HttpResponse};
use serde::Deserialize;
use serde_json::Value;

use crate::auth::ai_audit::AI_WRITE_PERMISSION;
use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 500;

#[derive(Debug, Deserialize)]
struct AuditQuery {
    limit: Option<i64>,
    suggestion_id: Option<String>,
}

/// Newest first, optionally narrowed to one suggestion.
#[get("/ai-audit")]
async fn list_ai_audit(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    q: web::Query<AuditQuery>,
) -> ApiResult<HttpResponse> {
    if claims.permission < AI_WRITE_PERMISSION {
        return Err(ApiError::Forbidden(
            "the AI audit log requires permission >= 4".into(),
        ));
    }
    let limit = match q.limit {
        None => DEFAULT_LIMIT,
        Some(n) if n >= 1 => n.min(MAX_LIMIT),
        Some(_) => {
            return Err(ApiError::BadRequest(
                "limit must be a positive integer".into(),
            ))
        }
    };
    let suggestion = match q.suggestion_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(s) => Some(
            uuid::Uuid::parse_str(s)
                .map_err(|_| ApiError::BadRequest("suggestion_id must be a uuid".into()))?,
        ),
    };
    let rows: Vec<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(a) FROM ai_audit_log a \
         WHERE ($2::uuid IS NULL OR a.suggestion_id = $2) \
         ORDER BY a.created_at DESC, a.id DESC LIMIT $1",
    )
    .bind(limit)
    .bind(suggestion)
    .fetch_all(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(rows.into_iter().map(|(v,)| v).collect::<Vec<_>>()))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(list_ai_audit);
}
