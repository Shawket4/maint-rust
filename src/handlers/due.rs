//! Due list — reads v_maintenance_due (fleet-wide plans × vehicles).

use actix_web::{get, web, HttpResponse};
use serde::Deserialize;
use serde_json::Value;

use crate::db::PgPool;
use crate::error::ApiResult;

#[derive(Debug, Deserialize)]
struct DueQuery {
    vehicle_id: Option<i32>,
}

#[get("/due")]
async fn list_due(pool: web::Data<PgPool>, q: web::Query<DueQuery>) -> ApiResult<HttpResponse> {
    let rows: Vec<(Value,)> = if let Some(vid) = q.vehicle_id {
        sqlx::query_as(
            "SELECT to_jsonb(d) FROM v_maintenance_due d WHERE vehicle_id = $1 \
             ORDER BY CASE status WHEN 'overdue' THEN 0 WHEN 'never_done' THEN 1 \
             WHEN 'due_soon' THEN 2 ELSE 3 END",
        )
        .bind(vid)
        .fetch_all(pool.get_ref())
        .await?
    } else {
        sqlx::query_as(
            "SELECT to_jsonb(d) FROM v_maintenance_due d \
             ORDER BY CASE status WHEN 'overdue' THEN 0 WHEN 'never_done' THEN 1 \
             WHEN 'due_soon' THEN 2 ELSE 3 END, vehicle_id",
        )
        .fetch_all(pool.get_ref())
        .await?
    };
    let out: Vec<Value> = rows.into_iter().map(|(v,)| v).collect();
    Ok(HttpResponse::Ok().json(out))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(list_due);
}
