pub mod ai;
pub mod ai_audit;
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

use actix_web::{dev::HttpServiceFactory, web};

use crate::auth::{AiAudit, JwtAuth};
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

/// Everything under /api/maint. Shared by main.rs and the HTTP tests so both
/// run the exact same routes and middleware order.
///
/// Middleware order on the authenticated scope: `.wrap()` calls run last-
/// registered first, so JwtAuth verifies the token and stores the claims,
/// then AiAudit gates and records AI writes, then the handler runs.
pub fn api_scope(jwt_secret: String) -> impl HttpServiceFactory {
    web::scope("/api/maint")
        .configure(health::configure)
        .configure(auth_login::configure)
        .service(
            web::scope("")
                .wrap(AiAudit)
                .wrap(JwtAuth::new(jwt_secret))
                .configure(cache_vehicles::configure)
                .configure(cache_invoices::configure)
                .configure(reference::configure)
                .configure(due::configure)
                .configure(plans::configure)
                .configure(stock::configure)
                .configure(work_orders::configure)
                .configure(tires::configure)
                .configure(service::configure)
                .configure(ai::configure)
                .configure(ai_audit::configure)
                .configure(sync::configure),
        )
}
