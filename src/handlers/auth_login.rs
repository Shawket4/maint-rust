//! Login proxy (§5.1). The desktop client posts credentials here; maint-rust
//! verifies against Falcon and mints its own token. Falls back to a dev token
//! when Falcon is unreachable AND dev_login is enabled — so the app runs locally.

use actix_web::{post, web, HttpResponse};
use serde::Deserialize;
use serde_json::json;

use crate::auth::token;
use crate::error::{ApiError, ApiResult};
use crate::handlers::AppState;

#[derive(Debug, Deserialize)]
struct LoginBody {
    email: String,
    password: String,
}

#[post("/auth/login")]
async fn login(state: web::Data<AppState>, body: web::Json<LoginBody>) -> ApiResult<HttpResponse> {
    let creds = json!({ "email": body.email, "password": body.password });
    let result = state.falcon.post_json_unauth("/api/Login", &creds).await;

    // Happy path: Falcon accepted the credentials.
    if let Ok((200, v)) = &result {
        let falcon_jwt = v.get("jwt").and_then(|j| j.as_str()).unwrap_or_default();
        let permission = v.get("permission").and_then(|p| p.as_i64()).unwrap_or(0) as i32;
        let user_id = token::user_id_from_falcon_token(&state.jwt_secret, falcon_jwt)
            .ok_or_else(|| ApiError::Unauthorized("could not read user id from Falcon token".into()))?;
        let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        let (jwt, exp) = token::mint(&state.jwt_secret, user_id, permission, "employee", 31)?;
        return Ok(HttpResponse::Ok().json(json!({
            "jwt": jwt, "exp": exp, "user_id": user_id, "permission": permission, "name": name,
        })));
    }

    // Dev fallback: mint a local admin token regardless of why Falcon said no.
    // Enabled only via MAINT_DEV_LOGIN=1 — never in a real deploy.
    // user_id must be a REAL Falcon user so the minted token also passes
    // FalconGo's Verify (it looks the user up by id) when maint-rust proxies
    // /api/cars. MAINT_DEV_USER_ID overrides; default 3 (perm-4 admin in prod).
    if state.dev_login {
        let user_id = std::env::var("MAINT_DEV_USER_ID").ok()
            .and_then(|v| v.parse().ok()).unwrap_or(3);
        let (jwt, exp) = token::mint(&state.jwt_secret, user_id, 4, "admin_user", 31)?;
        tracing::warn!("issued DEV login token (permission 4) — Falcon not authoritative locally");
        return Ok(HttpResponse::Ok().json(json!({
            "jwt": jwt, "exp": exp, "user_id": user_id,
            "permission": 4, "name": format!("dev:{}", body.email), "dev": true,
        })));
    }

    // Production: surface Falcon's verdict.
    match result {
        Ok((404, v)) => Err(ApiError::NotFound(
            v.get("error").and_then(|e| e.as_str()).unwrap_or("user not found").into(),
        )),
        Ok((_, v)) => Err(ApiError::Unauthorized(
            v.get("error").and_then(|e| e.as_str()).unwrap_or("login failed").into(),
        )),
        Err(e) => Err(e),
    }
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(login);
}
