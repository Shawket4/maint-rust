//! Service events — Falcon's service-invoice history per vehicle (read-only).
//! Reads directly from Falcon's Postgres via the read-only pool.

use actix_web::{get, web, HttpResponse};
use serde_json::Value;

use crate::error::{ApiError, ApiResult};
use crate::handlers::AppState;

#[get("/vehicles/{id}/service-events")]
async fn service_events(
    state: web::Data<AppState>,
    path: web::Path<i32>,
) -> ApiResult<HttpResponse> {
    let car_id = path.into_inner();
    let pool = state
        .falcon_pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Falcon DB not configured".into()))?;

    let rows: Vec<(Value,)> = sqlx::query_as(
        r#"
        SELECT jsonb_build_object(
            'id', si.id,
            'date', si.date,
            'meter_reading', si.meter_reading,
            'driver_name', si.driver_name,
            'supervisor', si.supervisor,
            'operating_region', si.operating_region,
            'items', COALESCE(
                (SELECT jsonb_agg(jsonb_build_object('service', ii.service, 'notes', ii.notes)
                        ORDER BY ii.item_order)
                 FROM inspection_items ii WHERE ii.service_invoice_id = si.id), '[]'::jsonb)
        )
        FROM service_invoices si
        WHERE si.car_id = $1 AND si.deleted_at IS NULL
        ORDER BY si.date DESC
        "#,
    )
    .bind(car_id as i64)
    .fetch_all(pool)
    .await?;

    Ok(HttpResponse::Ok().json(rows.into_iter().map(|(v,)| v).collect::<Vec<_>>()))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(service_events);
}
