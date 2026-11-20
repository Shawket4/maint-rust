use actix_web::{get, web, HttpResponse};
use serde_json::json;
use sqlx::PgPool;

#[get("/health")]
async fn health(pool: web::Data<PgPool>, state: web::Data<crate::handlers::AppState>) -> HttpResponse {
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(pool.get_ref())
        .await
        .is_ok();
    let body = json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "service": "maint-rust",
        "version": env!("CARGO_PKG_VERSION"),
        // Oldest desktop build this server still supports. Set
        // MIN_CLIENT_VERSION after a breaking sync-contract change; clients
        // compare at startup and prompt for an update.
        "min_client_version": std::env::var("MIN_CLIENT_VERSION").unwrap_or_else(|_| "0.0.0".into()),
        "db": if db_ok { "ok" } else { "down" },
        // Optional parts: what is switched on, not whether upstream is up.
        "service_history": state.falcon_pool.is_some(),
        "ai_search": state.anthropic_api_key.is_some() && state.falcon_pool.is_some(),
    });
    if db_ok {
        HttpResponse::Ok().json(body)
    } else {
        HttpResponse::ServiceUnavailable().json(body)
    }
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(health);
}
