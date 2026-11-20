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

/// Permission levels (Falcon's `users.permission`).
pub mod level {
    /// Anything at all: Falcon itself asks 1 for the fleet list.
    pub const USE: i32 = 1;
    /// Service plans and vehicle classes.
    pub const CONFIGURE: i32 = 3;
    /// AI search (model-written SQL over Falcon's service history).
    pub const AI_SEARCH: i32 = 3;
    /// Stock credits.
    pub const STOCK_CREDIT: i32 = 4;
}

impl AuthClaims {
    pub fn require(&self, min: i32, what: &str) -> Result<(), crate::error::ApiError> {
        if self.permission < min {
            return Err(crate::error::ApiError::Forbidden(format!("{what} requires permission {min} or higher")));
        }
        Ok(())
    }
}
