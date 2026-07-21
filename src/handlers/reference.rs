//! Reference reads: categories, vehicle classes, class assignments.

use actix_web::{get, web, HttpResponse};
use serde_json::Value;

use crate::db::PgPool;
use crate::error::ApiResult;

#[get("/categories")]
async fn categories(pool: web::Data<PgPool>) -> ApiResult<HttpResponse> {
    let rows: Vec<(Value,)> =
        sqlx::query_as("SELECT to_jsonb(c) FROM maintenance_categories c ORDER BY sort_order")
            .fetch_all(pool.get_ref())
            .await?;
    Ok(HttpResponse::Ok().json(rows.into_iter().map(|(v,)| v).collect::<Vec<_>>()))
}

#[get("/vehicle-classes")]
async fn vehicle_classes(pool: web::Data<PgPool>) -> ApiResult<HttpResponse> {
    let rows: Vec<(Value,)> =
        sqlx::query_as("SELECT to_jsonb(c) FROM vehicle_classes c ORDER BY sort_order")
            .fetch_all(pool.get_ref())
            .await?;
    Ok(HttpResponse::Ok().json(rows.into_iter().map(|(v,)| v).collect::<Vec<_>>()))
}

#[get("/vehicles")]
async fn vehicles(pool: web::Data<PgPool>) -> ApiResult<HttpResponse> {
    let rows: Vec<(Value,)> = sqlx::query_as(
        "SELECT jsonb_build_object('id', v.id, 'car_no_plate', v.car_no_plate, \
                'car_type', v.car_type, 'transporter', v.transporter, \
                'last_fuel_odometer', v.last_fuel_odometer, 'class_id', a.class_id) \
         FROM vehicles_cache v \
         LEFT JOIN vehicle_class_assignments a ON a.vehicle_id = v.id AND a.deleted_at IS NULL \
         WHERE v.source_deleted_at IS NULL ORDER BY v.car_no_plate",
    )
    .fetch_all(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(rows.into_iter().map(|(v,)| v).collect::<Vec<_>>()))
}

#[get("/vehicle-class-assignments")]
async fn class_assignments(pool: web::Data<PgPool>) -> ApiResult<HttpResponse> {
    let rows: Vec<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(a) FROM vehicle_class_assignments a WHERE deleted_at IS NULL",
    )
    .fetch_all(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(rows.into_iter().map(|(v,)| v).collect::<Vec<_>>()))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(categories)
        .service(vehicle_classes)
        .service(vehicles)
        .service(class_assignments);
}
