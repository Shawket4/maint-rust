use serde::{Deserialize, Serialize};

/// Mirrors the Falcon-emitted JWT payload.
///
/// Falcon's `iss` is the user_id rendered as a string, hence the quirk note in §3.4.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthClaims {
    pub user_type: String,
    pub user_id: i64,
    #[serde(default)]
    pub driver_id: i64,
    pub permission: i32,
    pub iss: String,
    pub exp: i64,
}
