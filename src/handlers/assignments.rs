//! Assignments routes are registered as part of `tires.rs` (mount/dismount endpoints
//! live at /tires/{id}/mount and /tires/assignments/{id}/dismount per §9.4).
//! This module exists for §10 layout symmetry and to expose a list endpoint.

use actix_web::{get, web, HttpResponse, Scope};
use uuid::Uuid;

use crate::db::PgPool;
use crate::error::ApiResult;
use crate::models::assignment::AssignmentRow;

#[derive(serde::Deserialize)]
struct AssignListQuery {
    tire_id: Option<Uuid>,
    vehicle_id: Option<i32>,
    position_id: Option<Uuid>,
    /// "true" to include only currently-active (not dismounted)
    #[serde(default)]
    active_only: bool,
}

#[get("/assignments")]
async fn list_assignments(
    pool: web::Data<PgPool>,
    q: web::Query<AssignListQuery>,
) -> ApiResult<HttpResponse> {
    let mut sql =
        String::from("SELECT * FROM tire_assignments WHERE deleted_at IS NULL");
    if q.active_only {
        sql.push_str(" AND dismounted_at IS NULL");
    }
    let mut idx = 1;
    if q.tire_id.is_some() {
        sql.push_str(&format!(" AND tire_id = ${idx}"));
        idx += 1;
    }
    if q.vehicle_id.is_some() {
        sql.push_str(&format!(" AND vehicle_id = ${idx}"));
        idx += 1;
    }
    if q.position_id.is_some() {
        sql.push_str(&format!(" AND position_id = ${idx}"));
    }
    sql.push_str(" ORDER BY mounted_at DESC LIMIT 5000");

    let mut q_obj = sqlx::query_as::<_, AssignmentRow>(&sql);
    if let Some(t) = q.tire_id {
        q_obj = q_obj.bind(t);
    }
    if let Some(v) = q.vehicle_id {
        q_obj = q_obj.bind(v);
    }
    if let Some(p) = q.position_id {
        q_obj = q_obj.bind(p);
    }
    let rows = q_obj.fetch_all(pool.get_ref()).await?;
    Ok(HttpResponse::Ok().json(rows))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(list_assignments);
}
