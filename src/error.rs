use actix_web::{
    http::{header::ContentType, StatusCode},
    HttpResponse, ResponseError,
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("forbidden: {0}")]
    #[allow(dead_code)]
    Forbidden(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("conflict: {0}")]
    Conflict(String),

    /// Sync version conflict — body carries the server's current row JSON.
    #[error("sync version conflict")]
    SyncConflict {
        entity_id: String,
        server_row: serde_json::Value,
    },

    #[error("upstream Falcon error ({status}): {body}")]
    UpstreamFalcon { status: u16, body: String },

    #[error("upstream timeout")]
    UpstreamTimeout,

    #[error("upstream auth failure")]
    UpstreamAuth,

    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("internal error: {0}")]
    #[allow(dead_code)]
    Internal(String),

    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}

impl ApiError {
    fn error_code(&self) -> &'static str {
        match self {
            Self::Unauthorized(_) => "unauthorized",
            Self::Forbidden(_) => "forbidden",
            Self::NotFound(_) => "not_found",
            Self::BadRequest(_) => "bad_request",
            Self::Conflict(_) => "conflict",
            Self::SyncConflict { .. } => "sync_conflict",
            Self::UpstreamFalcon { .. } => "upstream_falcon",
            Self::UpstreamTimeout => "upstream_timeout",
            Self::UpstreamAuth => "upstream_auth",
            Self::Db(e) if db_client_fault(e) => "bad_request",
            Self::Db(_) => "db_error",
            Self::Redis(_) => "redis_error",
            Self::Reqwest(_) => "reqwest_error",
            Self::Internal(_) => "internal",
            Self::Anyhow(_) => "internal",
        }
    }
}

/// A DB error whose SQLSTATE says the CLIENT sent bad data (class 22 data
/// exception — e.g. 22021 NUL byte / bad encoding; class 23 integrity —
/// FK/check/not-null). These must surface as 4xx, never a 500 `db_error`.
/// (Unique violations, 23505, are excluded: the sync path maps those to a
/// 409 Conflict deliberately, and a raw one is closer to a conflict than a
/// bad request.) Discovered by API fuzzing: every one of these used to leak
/// a 500 with a raw Postgres message.
fn db_client_fault(e: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db) = e {
        if let Some(code) = db.code() {
            let class = &code[..2.min(code.len())];
            return (class == "22" || class == "23") && code.as_ref() != "23505";
        }
    }
    false
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Unauthorized(_) | Self::UpstreamAuth => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Conflict(_) | Self::SyncConflict { .. } => StatusCode::CONFLICT,
            Self::UpstreamFalcon { status, .. } => {
                StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
            }
            Self::UpstreamTimeout => StatusCode::GATEWAY_TIMEOUT,
            Self::Db(e) if db_client_fault(e) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_response(&self) -> HttpResponse {
        let status = self.status_code();
        let body = match self {
            Self::SyncConflict {
                entity_id,
                server_row,
            } => json!({
                "error": "sync_conflict",
                "message": "client sync_version is stale",
                "entity_id": entity_id,
                "server_row": server_row,
            }),
            _ => json!({
                "error": self.error_code(),
                "message": self.to_string(),
            }),
        };

        // Log non-client errors loudly
        match self {
            Self::Db(e) if db_client_fault(e) => {
                tracing::debug!(error = %e, "client-fault db error (4xx)")
            }
            Self::Db(e) => tracing::error!(error = %e, "db error"),
            Self::Redis(e) => tracing::warn!(error = %e, "redis error"),
            Self::Reqwest(e) => tracing::warn!(error = %e, "upstream reqwest error"),
            Self::Internal(m) => tracing::error!(message = %m, "internal error"),
            Self::Anyhow(e) => tracing::error!(error = %e, "anyhow error"),
            _ => {}
        }

        HttpResponse::build(status)
            .content_type(ContentType::json())
            .body(body.to_string())
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;
