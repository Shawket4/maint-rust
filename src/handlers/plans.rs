//! Service plans (fleet-wide preventive rules). Config CRUD, not offline-synced.

use actix_web::{delete, get, post, web, HttpResponse};
use serde::Deserialize;
use serde_json::Value;

use crate::auth::claims::level;
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
    /// Ignored: derived from which intervals are set.
    #[serde(default)]
    trigger_type: Option<String>,
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
    claims.require(level::CONFIGURE, "service plans")?;
    let mut p = body.into_inner();
    // A plan is a km interval, a day interval, or both (whichever comes
    // first). trigger_type is derived, kept only for older clients.
    let ok = |v: Option<i32>, max: i32| v.map_or(true, |v| v > 0 && v <= max);
    if p.interval_km.is_none() && p.interval_days.is_none() {
        return Err(ApiError::BadRequest("a plan needs interval_km, interval_days, or both".into()));
    }
    if !ok(p.interval_km, 2_000_000) || !ok(p.interval_days, 3650) {
        return Err(ApiError::BadRequest("interval_km must be 1..2000000 and interval_days 1..3650".into()));
    }
    if p.lead_warn_km.map_or(false, |v| v < 0) || p.lead_warn_days.map_or(false, |v| v < 0) {
        return Err(ApiError::BadRequest("warning lead cannot be negative".into()));
    }
    p.trigger_type = Some(if p.interval_km.is_some() { "mileage".into() } else { "time".into() });
    // A warning lead longer than the interval would keep the plan "due soon"
    // forever; default it to a tenth of the interval.
    let lead_km = p.lead_warn_km.or(p.interval_km.map(|k| (k / 10).clamp(100, 2000)));
    let lead_days = p.lead_warn_days.or(p.interval_days.map(|d| (d / 10).clamp(1, 14)));
    if p.class_id.trim().is_empty() || p.category_id.trim().is_empty() {
        return Err(ApiError::BadRequest("class_id and category_id are required".into()));
    }
    // Postgres text rejects NUL bytes — reject at the boundary as a 400, not a 500.
    if p.class_id.contains('\0') || p.category_id.contains('\0') {
        return Err(ApiError::BadRequest("ids must not contain NUL bytes".into()));
    }
    // Upsert on (class_id, category_id) — one plan per class+category.
    let row: (Value,) = sqlx::query_as(
        r#"
        INSERT INTO service_plans
            (class_id, category_id, trigger_type, interval_km, interval_days,
             lead_warn_km, lead_warn_days, is_active, created_by_user_id, updated_by_user_id)
        VALUES ($1, $2, $3::plan_trigger, $4, $5,
                COALESCE($6, $10, 500), COALESCE($7, $11, 14), COALESCE($8, TRUE), $9, $9)
        ON CONFLICT (class_id, category_id) DO UPDATE SET
            trigger_type = EXCLUDED.trigger_type,
            interval_km = EXCLUDED.interval_km,
            interval_days = EXCLUDED.interval_days,
            -- Omitted on an update = keep the stored value. EXCLUDED already
            -- carries the insert defaults (500/14/TRUE), so read the raw params.
            lead_warn_km = COALESCE($6, service_plans.lead_warn_km),
            lead_warn_days = COALESCE($7, service_plans.lead_warn_days),
            is_active = COALESCE($8, service_plans.is_active),
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
    .bind(lead_km)
    .bind(lead_days)
    .fetch_one(pool.get_ref())
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_foreign_key_violation() => {
            ApiError::BadRequest("unknown vehicle class or category".into())
        }
        _ => ApiError::Db(e),
    })?;
    Ok(HttpResponse::Ok().json(row.0))
}

/// Retire a plan. Soft: the row stays for history and can be re-created.
#[delete("/service-plans/{id}")]
async fn delete_plan(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<uuid::Uuid>,
) -> ApiResult<HttpResponse> {
    claims.require(level::CONFIGURE, "service plans")?;
    let n = sqlx::query(
        "UPDATE service_plans SET is_active = FALSE, updated_by_user_id = $2, sync_version = sync_version + 1 \
          WHERE id = $1 AND is_active",
    )
    .bind(path.into_inner())
    .bind(claims.user_id)
    .execute(pool.get_ref())
    .await?
    .rows_affected();
    Ok(HttpResponse::Ok().json(serde_json::json!({ "removed": n > 0 })))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(list_plans).service(upsert_plan).service(delete_plan);
}
