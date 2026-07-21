//! Tire reads: inventory list + per-tire lifecycle history.

use actix_web::{get, web, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};

#[derive(Debug, Deserialize)]
struct TireQuery {
    status: Option<String>,
}

#[get("/tires")]
async fn list_tires(pool: web::Data<PgPool>, q: web::Query<TireQuery>) -> ApiResult<HttpResponse> {
    let rows: Vec<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(t) FROM v_tires_list t \
         WHERE ($1::text IS NULL OR t.status::text = $1) \
         ORDER BY t.status, t.brand, t.created_at DESC",
    )
    .bind(&q.status)
    .fetch_all(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(rows.into_iter().map(|(v,)| v).collect::<Vec<_>>()))
}

#[get("/tires/{id}/history")]
async fn tire_history(pool: web::Data<PgPool>, path: web::Path<String>) -> ApiResult<HttpResponse> {
    let id = Uuid::parse_str(&path.into_inner())
        .map_err(|_| ApiError::BadRequest("invalid tire id".into()))?;

    let mounts: Vec<(Value,)> = sqlx::query_as(
        "SELECT jsonb_build_object('assignment', to_jsonb(a), 'position_code', p.position_code, \
                'plate', vc.car_no_plate) \
         FROM tire_assignments a \
         LEFT JOIN chassis_positions p ON p.id = a.position_id \
         LEFT JOIN vehicles_cache vc ON vc.id = a.vehicle_id \
         WHERE a.tire_id = $1 AND a.deleted_at IS NULL ORDER BY a.mounted_at DESC",
    )
    .bind(id)
    .fetch_all(pool.get_ref())
    .await?;

    let events: Vec<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(e) FROM tire_status_events e \
         WHERE e.tire_id = $1 AND e.deleted_at IS NULL ORDER BY e.occurred_at DESC",
    )
    .bind(id)
    .fetch_all(pool.get_ref())
    .await?;

    Ok(HttpResponse::Ok().json(json!({
        "mounts": mounts.into_iter().map(|(v,)| v).collect::<Vec<_>>(),
        "status_events": events.into_iter().map(|(v,)| v).collect::<Vec<_>>(),
    })))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(list_tires).service(tire_history);
}
