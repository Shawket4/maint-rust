//! Service plans (fleet-wide preventive rules). Config CRUD, not offline-synced.

use actix_web::{get, post, web, HttpResponse};
use serde::Deserialize;
use serde_json::Value;

use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};

#[get("/service-plans")]
async fn list_plans(pool: web::Data<PgPool>) -> ApiResult<HttpResponse> {
    let rows: Vec<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(p) FROM service_plans p WHERE deleted_at IS NULL ORDER BY class_id, category_id",
    )
    .fetch_all(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(rows.into_iter().map(|(v,)| v).collect::<Vec<_>>()))
}

#[derive(Debug, Deserialize)]
struct PlanInput {
    class_id: String,
    category_id: String,
    trigger_type: String,
    interval_km: Option<i32>,
    interval_days: Option<i32>,
    lead_warn_km: Option<i32>,
    lead_warn_days: Option<i32>,
    is_active: Option<bool>,
}

#[post("/service-plans")]
async fn upsert_plan(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    body: web::Json<PlanInput>,
) -> ApiResult<HttpResponse> {
    if claims.permission < 3 {
        return Err(ApiError::Forbidden("service plans require permission >= 3".into()));
    }
    let p = body.into_inner();
    // Upsert on (class_id, category_id) — one plan per class+category.
    let row: (Value,) = sqlx::query_as(
        r#"
        INSERT INTO service_plans
            (class_id, category_id, trigger_type, interval_km, interval_days,
             lead_warn_km, lead_warn_days, is_active, created_by_user_id, updated_by_user_id)
        VALUES ($1, $2, $3::plan_trigger, $4, $5,
                COALESCE($6, 500), COALESCE($7, 14), COALESCE($8, TRUE), $9, $9)
        ON CONFLICT (class_id, category_id) DO UPDATE SET
            trigger_type = EXCLUDED.trigger_type,
            interval_km = EXCLUDED.interval_km,
            interval_days = EXCLUDED.interval_days,
            lead_warn_km = EXCLUDED.lead_warn_km,
            lead_warn_days = EXCLUDED.lead_warn_days,
            is_active = EXCLUDED.is_active,
            updated_at = now(),
            updated_by_user_id = EXCLUDED.updated_by_user_id,
            sync_version = service_plans.sync_version + 1
        RETURNING to_jsonb(service_plans)
        "#,
    )
    .bind(&p.class_id)
    .bind(&p.category_id)
    .bind(&p.trigger_type)
    .bind(p.interval_km)
    .bind(p.interval_days)
    .bind(p.lead_warn_km)
    .bind(p.lead_warn_days)
    .bind(p.is_active)
    .bind(claims.user_id)
    .fetch_one(pool.get_ref())
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_check_violation() => {
            ApiError::BadRequest(format!("check failed: {}", db.message()))
        }
        _ => ApiError::Db(e),
    })?;
    Ok(HttpResponse::Ok().json(row.0))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(list_plans).service(upsert_plan);
}
