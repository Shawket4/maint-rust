//! AI audit middleware — the trail of every write the AI advisor applies.
//!
//! The advisor (apex-rust) applies an approved suggestion by calling this API
//! with the APPROVER's own JWT plus `X-Via-AI` and `X-AI-Suggestion-Id`.
//! Nothing in those headers is trusted for identity: who made the change is
//! always the verified JWT user. Same semantics as FalconGo's
//! middleware/ai_audit.go:
//!
//!  - only writes (anything but GET/HEAD/OPTIONS) carrying a non-empty
//!    `X-Via-AI` are touched; every other request passes through unchanged —
//!    the desktop app never sends the header, at any permission level;
//!  - such a write needs permission >= 4, whatever the route itself asks for;
//!  - it is recorded in `ai_audit_log` (0020) with the user, the via value,
//!    the suggestion id, the request body and the response status.
//!
//! Unlike FalconGo (record after, best-effort) the row is inserted BEFORE the
//! handler runs and the request is refused with a 500 if that insert fails, so
//! no AI write can apply unaudited. The status (and the JSON response, e.g.
//! the /sync/push per-op results) is filled in afterwards; if that update
//! fails it is logged loudly and the row stays at status 0 ("may have applied").
//!
//! Must sit INSIDE JwtAuth (wrapped before it on the scope) — it reads the
//! claims JwtAuth stores in the request extensions.

use std::future::{ready, Ready};
use std::rc::Rc;
use std::task::{Context, Poll};

use actix_service::{Service, Transform};
use actix_web::{
    body::{self, BoxBody, EitherBody, MessageBody},
    dev::{Payload, ServiceRequest, ServiceResponse},
    http::{header, Method, StatusCode},
    web::{self, Bytes, BytesMut},
    Error, HttpMessage, HttpResponse,
};
use futures::future::LocalBoxFuture;
use futures::StreamExt;
use serde_json::{json, Value};
use uuid::Uuid;

use super::claims::AuthClaims;
use crate::db::PgPool;

pub const HEADER_VIA_AI: &str = "X-Via-AI";
pub const HEADER_AI_SUGGESTION_ID: &str = "X-AI-Suggestion-Id";

/// The floor for any write that arrives through the AI.
pub const AI_WRITE_PERMISSION: i32 = 4;

/// The 403 body's `error` for an AI write below AI_WRITE_PERMISSION.
pub const AI_PERMISSION_ERROR: &str =
    "changes made through the AI advisor need permission 4 or higher";

/// Request bodies and responses above this are stored as a size marker only.
const AUDIT_JSON_CAP: usize = 64 * 1024;
/// The largest AI request body read at all — the same limit as JsonConfig in
/// main.rs, so nothing the handler would accept is refused here.
const MAX_BODY: usize = 4 * 1024 * 1024;
/// The via value lands in a text column; a longer one is cut, not refused.
const VIA_CAP: usize = 64;

const REDACTED: &str = "[REDACTED]";

/// GET, HEAD and OPTIONS are reads; everything else can change state.
fn is_write(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// The trimmed `X-Via-AI` value of a WRITE, or None when the request is not an
/// AI write (header absent, empty, or a read).
fn ai_via(req: &ServiceRequest) -> Option<String> {
    if !is_write(req.method()) {
        return None;
    }
    let via = req.headers().get(HEADER_VIA_AI)?.to_str().ok()?.trim();
    if via.is_empty() {
        return None;
    }
    let mut cut = via.len().min(VIA_CAP);
    while !via.is_char_boundary(cut) {
        cut -= 1;
    }
    Some(via[..cut].to_string())
}

/// `X-AI-Suggestion-Id` as a UUID; absent or malformed → None.
fn suggestion_id(req: &ServiceRequest) -> Option<Uuid> {
    let raw = req.headers().get(HEADER_AI_SUGGESTION_ID)?.to_str().ok()?;
    Uuid::parse_str(raw.trim()).ok()
}

/// Field names whose values must never be stored (FalconGo's list).
fn is_sensitive_key(key: &str) -> bool {
    let k = key.to_lowercase();
    [
        "password",
        "passwd",
        "token",
        "secret",
        "authorization",
        "api_key",
        "apikey",
    ]
    .iter()
    .any(|w| k.contains(w))
        || k == "pin"
        || k.starts_with("pin_")
        || k.ends_with("_pin")
}

/// Redact sensitive keys at any depth. NUL characters are replaced too:
/// jsonb rejects `\u0000`, and the audit insert must not fail on a body the
/// handler itself would refuse cleanly.
fn redact(v: Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, val)| {
                    let k = k.replace('\0', "\u{FFFD}");
                    if is_sensitive_key(&k) {
                        (k, Value::String(REDACTED.into()))
                    } else {
                        (k, redact(val))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(redact).collect()),
        Value::String(s) if s.contains('\0') => Value::String(s.replace('\0', "\u{FFFD}")),
        other => other,
    }
}

/// The request body as stored: redacted JSON, or a marker object when it is
/// too large or not JSON (it cannot be redacted reliably). Empty → None.
pub fn audit_body(bytes: &[u8]) -> Option<Value> {
    if bytes.is_empty() {
        return None;
    }
    if bytes.len() > AUDIT_JSON_CAP {
        return Some(json!({ "_audit_omitted": "body larger than 64 KB", "bytes": bytes.len() }));
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(v) => Some(redact(v)),
        Err(_) => Some(json!({ "_audit_omitted": "body is not JSON", "bytes": bytes.len() })),
    }
}

/// The JSON response kept as `results`, when it is JSON and small enough.
fn audit_results(bytes: &[u8]) -> Option<Value> {
    if bytes.is_empty() || bytes.len() > AUDIT_JSON_CAP {
        return None;
    }
    serde_json::from_slice::<Value>(bytes).ok().map(redact)
}

fn error_json<B>(
    req: ServiceRequest,
    status: StatusCode,
    msg: &str,
) -> ServiceResponse<EitherBody<B>> {
    let resp = HttpResponse::build(status).json(json!({ "error": msg }));
    let (http_req, _payload) = req.into_parts();
    ServiceResponse::new(http_req, resp).map_into_right_body()
}

#[derive(Clone, Default)]
pub struct AiAudit;

impl<S, B> Transform<S, ServiceRequest> for AiAudit
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = AiAuditMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(AiAuditMiddleware {
            service: Rc::new(service),
        }))
    }
}

pub struct AiAuditMiddleware<S> {
    service: Rc<S>,
}

impl<S, B> Service<ServiceRequest> for AiAuditMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, mut req: ServiceRequest) -> Self::Future {
        let svc = self.service.clone();

        // Not an AI write: exactly today's behaviour, body untouched.
        let Some(via) = ai_via(&req) else {
            return Box::pin(async move { Ok(svc.call(req).await?.map_into_left_body()) });
        };

        Box::pin(async move {
            let Some(claims) = req.extensions().get::<AuthClaims>().cloned() else {
                return Ok(error_json(
                    req,
                    StatusCode::UNAUTHORIZED,
                    "auth claims not present",
                ));
            };
            if claims.permission < AI_WRITE_PERMISSION {
                return Ok(error_json(req, StatusCode::FORBIDDEN, AI_PERMISSION_ERROR));
            }
            let Some(pool) = req.app_data::<web::Data<PgPool>>().cloned() else {
                tracing::error!("ai audit: no PgPool in app data; refusing AI write");
                return Ok(error_json(
                    req,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "AI audit is unavailable; the change was not applied",
                ));
            };

            // Read the whole body, then hand the handler an identical payload.
            let mut payload = req.take_payload();
            let mut buf = BytesMut::new();
            while let Some(chunk) = payload.next().await {
                let chunk = chunk?;
                if buf.len() + chunk.len() > MAX_BODY {
                    return Ok(error_json(
                        req,
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "request body too large",
                    ));
                }
                buf.extend_from_slice(&chunk);
            }
            let bytes: Bytes = buf.freeze();
            req.set_payload(Payload::from(bytes.clone()));

            let method = req.method().to_string();
            let path = req.path().to_string();
            let inserted: Result<(i64,), sqlx::Error> = sqlx::query_as(
                "INSERT INTO ai_audit_log (user_id, permission, via, suggestion_id, method, path, status, request) \
                 VALUES ($1, $2, $3, $4, $5, $6, 0, $7) RETURNING id",
            )
            .bind(claims.user_id)
            .bind(claims.permission)
            .bind(&via)
            .bind(suggestion_id(&req))
            .bind(&method)
            .bind(&path)
            .bind(audit_body(&bytes))
            .fetch_one(pool.get_ref())
            .await;
            let audit_id = match inserted {
                Ok((id,)) => id,
                Err(e) => {
                    tracing::error!(error = %e, %method, %path, user_id = claims.user_id,
                        "ai audit: could not record the AI write; refusing it");
                    return Ok(error_json(
                        req,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "could not record the AI audit row; the change was not applied",
                    ));
                }
            };

            let (status, results, out) = match svc.call(req).await {
                Ok(res) => {
                    let status = res.status();
                    let is_json = res
                        .headers()
                        .get(header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|ct| ct.starts_with("application/json"));
                    if is_json {
                        // Buffer the (small, JSON) response to keep it as `results`,
                        // then send the same bytes on.
                        let (http_req, http_res) = res.into_parts();
                        let (head, body) = http_res.into_parts();
                        let resp_bytes = body::to_bytes(body).await.map_err(|e| {
                            let e: Box<dyn std::error::Error> = e.into();
                            actix_web::error::ErrorInternalServerError(e.to_string())
                        })?;
                        let results = audit_results(&resp_bytes);
                        let res =
                            ServiceResponse::new(http_req, head.set_body(BoxBody::new(resp_bytes)));
                        (status, results, Ok(res.map_into_right_body()))
                    } else {
                        (status, None, Ok(res.map_into_left_body()))
                    }
                }
                Err(e) => (e.as_response_error().status_code(), None, Err(e)),
            };

            if let Err(e) =
                sqlx::query("UPDATE ai_audit_log SET status = $2, results = $3 WHERE id = $1")
                    .bind(audit_id)
                    .bind(status.as_u16() as i32)
                    .bind(results)
                    .execute(pool.get_ref())
                    .await
            {
                tracing::error!(error = %e, audit_id, status = status.as_u16(),
                    "ai audit: AI write ran but its status was not recorded (row left at status 0)");
            }
            out
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_is_redacted_and_capped() {
        let v =
            audit_body(br#"{"password":"x","nested":[{"api_key":"k","pin":"1","shipping":"ok"}]}"#)
                .unwrap();
        assert_eq!(v["password"], REDACTED);
        assert_eq!(v["nested"][0]["api_key"], REDACTED);
        assert_eq!(v["nested"][0]["pin"], REDACTED);
        assert_eq!(v["nested"][0]["shipping"], "ok");

        assert!(audit_body(b"").is_none());
        assert_eq!(
            audit_body(b"not json").unwrap()["_audit_omitted"],
            "body is not JSON"
        );
        let big = format!("{{\"a\":\"{}\"}}", "x".repeat(AUDIT_JSON_CAP));
        assert_eq!(audit_body(big.as_bytes()).unwrap()["bytes"], big.len());
        // jsonb cannot hold NUL — it must be replaced, not fail the insert.
        let nul = audit_body(br#"{"a":"x\u0000y"}"#).unwrap();
        assert!(!nul["a"].as_str().unwrap().contains('\0'));
    }

    #[test]
    fn only_writes_are_ai_gated() {
        assert!(is_write(&Method::POST));
        assert!(is_write(&Method::DELETE));
        assert!(!is_write(&Method::GET));
        assert!(!is_write(&Method::OPTIONS));
    }
}
