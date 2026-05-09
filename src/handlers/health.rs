use actix_web::{get, web, HttpResponse};
use serde_json::json;
use sqlx::PgPool;

#[get("/health")]
async fn health(pool: web::Data<PgPool>) -> HttpResponse {
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(pool.get_ref())
        .await
        .is_ok();
    let body = json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "service": "maint-rust",
        "version": env!("CARGO_PKG_VERSION"),
        "db": if db_ok { "ok" } else { "down" },
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
