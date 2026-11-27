//! Garage attendance for the office: one month, display-ready.

use actix_web::{get, web, HttpResponse};
use serde::Deserialize;

use crate::auth::claims::level;
use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::ApiResult;
use crate::services::attendance;

#[derive(Debug, Deserialize)]
struct MonthQuery {
    /// YYYY-MM; this month in Cairo when left out.
    month: Option<String>,
}

#[get("/office/attendance")]
async fn attendance_month(pool: web::Data<PgPool>, claims: AuthClaims, q: web::Query<MonthQuery>) -> ApiResult<HttpResponse> {
    claims.require(level::USE, "attendance")?;
    let month = match q.month.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => m.to_string(),
        None => sqlx::query_scalar::<_, String>("SELECT to_char(now() AT TIME ZONE 'Africa/Cairo', 'YYYY-MM')")
            .fetch_one(pool.get_ref())
            .await?,
    };
    Ok(HttpResponse::Ok().json(attendance::month(pool.get_ref(), &month).await?))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(attendance_month);
}
