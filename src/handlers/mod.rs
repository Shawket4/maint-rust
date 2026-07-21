pub mod ai;
pub mod auth_login;
pub mod cache_invoices;
pub mod cache_vehicles;
pub mod due;
pub mod health;
pub mod plans;
pub mod reference;
pub mod service;
pub mod stock;
pub mod sync;
pub mod sync_helpers;
pub mod tires;
pub mod work_orders;

use crate::db::PgPool;
use crate::falcon::{CacheManager, FalconClient};

/// Shared application state injected via app_data.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub falcon: FalconClient,
    pub cache: CacheManager,
    pub falcon_cars_ttl: u64,
    pub falcon_invoices_ttl: u64,
    pub jwt_secret: String,
    /// When true, login mints a dev token if Falcon is unreachable (local dev).
    pub dev_login: bool,
    /// Read-only pool to Falcon's Postgres (service history + AI SQL tool). None if unset.
    pub falcon_pool: Option<PgPool>,
    pub anthropic_api_key: Option<String>,
    pub ai_model: String,
}
