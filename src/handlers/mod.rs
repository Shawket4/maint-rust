pub mod assignments;
pub mod cache_invoices;
pub mod cache_vehicles;
pub mod chassis;
pub mod due;
pub mod health;
pub mod overrides;
pub mod records;
pub mod sync;
pub mod sync_helpers;
pub mod templates;
pub mod tires;

use crate::falcon::{CacheManager, FalconClient};
use crate::db::PgPool;

/// Shared application state injected via app_data.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub falcon: FalconClient,
    pub cache: CacheManager,
    pub falcon_cars_ttl: u64,
    pub falcon_invoices_ttl: u64,
}
