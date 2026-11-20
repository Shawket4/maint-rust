//! Live permission lookup.
//!
//! A maint-rust token lives for a month and carries a snapshot of the user's
//! Falcon permission. Falcon itself re-reads the user on every request, so a
//! fired or demoted user loses access there immediately — and used to keep it
//! here until the token expired. When the read-only Falcon database is
//! configured, the middleware asks it (cached for a minute per user) and the
//! live answer replaces the token's claim.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlx::PgPool;

const TTL: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
pub struct PermissionCache(Arc<Mutex<HashMap<i64, (Instant, Option<i32>)>>>);

pub enum Live {
    /// Falcon's current answer: Some(permission), or None when the user no
    /// longer exists.
    Known(Option<i32>),
    /// No Falcon database configured, or it could not be asked: the token's
    /// own claim stands.
    Unverified,
}

impl PermissionCache {
    pub async fn lookup(&self, falcon_pool: Option<&PgPool>, user_id: i64) -> Live {
        let Some(pool) = falcon_pool else { return Live::Unverified };
        if let Some((at, p)) = self.0.lock().ok().and_then(|m| m.get(&user_id).copied()) {
            if at.elapsed() < TTL {
                return Live::Known(p);
            }
        }
        let row: Result<Option<(i64,)>, _> =
            sqlx::query_as("SELECT permission::bigint FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(pool)
                .await;
        match row {
            Ok(r) => {
                let p = r.map(|t| t.0 as i32);
                if let Ok(mut m) = self.0.lock() {
                    if m.len() > 10_000 {
                        m.clear();
                    }
                    m.insert(user_id, (Instant::now(), p));
                }
                Live::Known(p)
            }
            Err(e) => {
                tracing::warn!(error = %e, "live permission lookup failed; trusting the token");
                Live::Unverified
            }
        }
    }
}
