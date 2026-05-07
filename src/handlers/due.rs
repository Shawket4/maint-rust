use actix_web::{get, web, HttpResponse, Scope};
use serde::Deserialize;

use crate::db::PgPool;
use crate::error::ApiResult;
use crate::models::record::DueRow;

#[derive(Deserialize)]
struct DueQuery {
    vehicle_id: Option<i32>,
    /// Filter by status (ok | due_soon | overdue | never_done)
    status: Option<String>,
}

#[get("/due")]
async fn list_due(
    pool: web::Data<PgPool>,
    q: web::Query<DueQuery>,
) -> ApiResult<HttpResponse> {
    let rows: Vec<DueRow> = match (q.vehicle_id, q.status.as_deref()) {
        (Some(vid), Some(st)) => {
            sqlx::query_as(
                r#"SELECT * FROM v_maintenance_due
                   WHERE vehicle_id = $1 AND status = $2
                   ORDER BY status, vehicle_id"#,
            )
            .bind(vid)
            .bind(st)
            .fetch_all(pool.get_ref())
            .await?
        }
        (Some(vid), None) => {
            sqlx::query_as(
                r#"SELECT * FROM v_maintenance_due
                   WHERE vehicle_id = $1
                   ORDER BY status, category_id"#,
            )
            .bind(vid)
            .fetch_all(pool.get_ref())
            .await?
        }
        (None, Some(st)) => {
            sqlx::query_as(
                r#"SELECT * FROM v_maintenance_due
                   WHERE status = $1
                   ORDER BY vehicle_id, category_id"#,
            )
            .bind(st)
            .fetch_all(pool.get_ref())
            .await?
        }
        (None, None) => {
            sqlx::query_as(
                r#"SELECT * FROM v_maintenance_due
                   ORDER BY
                     CASE status
                       WHEN 'overdue'   THEN 0
                       WHEN 'due_soon'  THEN 1
                       WHEN 'never_done' THEN 2
                       ELSE 3
                     END,
                     vehicle_id, category_id
                   LIMIT 5000"#,
            )
            .fetch_all(pool.get_ref())
            .await?
        }
    };
    Ok(HttpResponse::Ok().json(rows))
}

pub fn routes() -> Scope {
    web::scope("").service(list_due)
}
