//! Work order reads. Writes flow through /sync/push (offline-first).

use actix_web::{get, web, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};

#[derive(Debug, Deserialize)]
struct WoQuery {
    status: Option<String>,
    vehicle_id: Option<i32>,
}

#[get("/work-orders")]
async fn list_work_orders(pool: web::Data<PgPool>, q: web::Query<WoQuery>) -> ApiResult<HttpResponse> {
    let rows: Vec<(Value,)> = sqlx::query_as(
        r#"
        SELECT jsonb_build_object(
            'work_order', to_jsonb(w),
            'vehicle_plate', vc.car_no_plate,
            'task_count', (SELECT count(*) FROM work_order_tasks t
                           WHERE t.work_order_id = w.id AND t.deleted_at IS NULL)
        )
        FROM work_orders w
        LEFT JOIN vehicles_cache vc ON vc.id = w.vehicle_id
        WHERE w.deleted_at IS NULL
          AND ($1::text IS NULL OR w.status::text = $1)
          AND ($2::int  IS NULL OR w.vehicle_id = $2)
        ORDER BY w.opened_at DESC
        "#,
    )
    .bind(&q.status)
    .bind(q.vehicle_id)
    .fetch_all(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(rows.into_iter().map(|(v,)| v).collect::<Vec<_>>()))
}

#[get("/work-orders/{id}")]
async fn get_work_order(pool: web::Data<PgPool>, path: web::Path<String>) -> ApiResult<HttpResponse> {
    let id = Uuid::parse_str(&path.into_inner())
        .map_err(|_| ApiError::BadRequest("invalid work order id".into()))?;

    let wo: Option<(Value,)> =
        sqlx::query_as("SELECT to_jsonb(w) FROM work_orders w WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(pool.get_ref())
            .await?;
    let wo = wo.ok_or_else(|| ApiError::NotFound("work order not found".into()))?.0;

    let tasks: Vec<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(t) FROM work_order_tasks t \
         WHERE t.work_order_id = $1 AND t.deleted_at IS NULL ORDER BY t.sort_order, t.created_at",
    )
    .bind(id)
    .fetch_all(pool.get_ref())
    .await?;

    let oil: Vec<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(o) FROM oil_changes o WHERE o.work_order_id = $1 AND o.deleted_at IS NULL",
    )
    .bind(id)
    .fetch_all(pool.get_ref())
    .await?;

    // Both attributions (0012): a dismount performed under THIS WO on a tire
    // mounted under an earlier WO belongs to this WO's tire work too.
    let tires: Vec<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(a) FROM tire_assignments a \
         WHERE (a.work_order_id = $1 OR a.dismount_work_order_id = $1) AND a.deleted_at IS NULL",
    )
    .bind(id)
    .fetch_all(pool.get_ref())
    .await?;

    Ok(HttpResponse::Ok().json(json!({
        "work_order": wo,
        "tasks": tasks.into_iter().map(|(v,)| v).collect::<Vec<_>>(),
        "oil_changes": oil.into_iter().map(|(v,)| v).collect::<Vec<_>>(),
        "tire_assignments": tires.into_iter().map(|(v,)| v).collect::<Vec<_>>(),
    })))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    // Specific/static path before parameterized — actix matches in registration order.
    cfg.service(list_work_orders).service(get_work_order);
}
