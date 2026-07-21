//! Minting maint-rust JWTs.
//!
//! The client authenticates through maint-rust (§5.1 proxy): maint-rust verifies
//! credentials against Falcon when reachable, then mints its OWN token carrying
//! the full AuthClaims the middleware expects. This keeps the token self-consistent
//! with our own verifier and lets the desktop app run against maint-rust alone.

use chrono::{Duration, Utc};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};

use super::claims::AuthClaims;
use crate::error::{ApiError, ApiResult};

pub fn mint(
    secret: &str,
    user_id: i64,
    permission: i32,
    user_type: &str,
    ttl_days: i64,
) -> ApiResult<(String, i64)> {
    let exp = (Utc::now() + Duration::days(ttl_days)).timestamp();
    let claims = AuthClaims {
        user_type: user_type.to_string(),
        user_id,
        driver_id: 0,
        permission,
        iss: user_id.to_string(),
        exp,
    };
    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| ApiError::Internal(format!("token mint failed: {e}")))?;
    Ok((token, exp))
}

/// Best-effort extraction of the user_id from a Falcon token's `iss` claim,
/// which Falcon stamps as the user_id string. Verified against the shared secret.
pub fn user_id_from_falcon_token(secret: &str, token: &str) -> Option<i64> {
    use jsonwebtoken::{decode, DecodingKey, Validation};
    #[derive(serde::Deserialize)]
    struct Iss {
        iss: String,
    }
    let mut v = Validation::new(Algorithm::HS256);
    v.validate_aud = false;
    v.validate_exp = true;
    v.required_spec_claims.clear();
    decode::<Iss>(token, &DecodingKey::from_secret(secret.as_bytes()), &v)
        .ok()
        .and_then(|d| d.claims.iss.parse::<i64>().ok())
}
