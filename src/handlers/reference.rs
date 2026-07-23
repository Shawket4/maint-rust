//! Reference reads: categories, vehicle classes, class assignments — plus
//! class creation (perm ≥3; a third class used to require psql on the VPS).

use actix_web::{get, post, web, HttpResponse};
use serde::Deserialize;
use serde_json::Value;

use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};

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

#[derive(Debug, Deserialize)]
struct CreateClassInput {
    id: String,
    name_ar: String,
    name_en: Option<String>,
    sort_order: Option<i32>,
}

#[post("/vehicle-classes")]
async fn create_vehicle_class(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    body: web::Json<CreateClassInput>,
) -> ApiResult<HttpResponse> {
    if claims.permission < 3 {
        return Err(ApiError::Forbidden("vehicle classes require permission >= 3".into()));
    }
    let inp = body.into_inner();
    let id = inp.id.trim().to_lowercase().replace(' ', "_");
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(ApiError::BadRequest("class id must be ascii slug".into()));
    }
    if inp.name_ar.contains('\0') || inp.name_en.as_deref().unwrap_or("").contains('\0') {
        return Err(ApiError::BadRequest("names must not contain NUL bytes".into()));
    }
    let row: (Value,) = sqlx::query_as(
        "INSERT INTO vehicle_classes (id, name_ar, name_en, sort_order)
         VALUES ($1, $2, COALESCE($3, $2), COALESCE($4, (SELECT COALESCE(MAX(sort_order),0)+1 FROM vehicle_classes)))
         ON CONFLICT (id) DO UPDATE SET name_ar = EXCLUDED.name_ar, name_en = EXCLUDED.name_en
         RETURNING to_jsonb(vehicle_classes)",
    )
    .bind(&id)
    .bind(inp.name_ar.trim())
    .bind(inp.name_en.as_deref().map(str::trim))
    .bind(inp.sort_order)
    .fetch_one(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(row.0))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(categories)
        .service(vehicle_classes)
        .service(create_vehicle_class)
        .service(vehicles)
        .service(class_assignments);
}
