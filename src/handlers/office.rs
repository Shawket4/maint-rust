//! Read models for the office dashboard (apex-react) and for the garage
//! app's history: flat, display-ready JSON, so a screen never has to join
//! tables or know table shapes.
//!
//! apex-react signs in to Falcon; its Falcon token is a staff token signed
//! with the same secret, so these routes need nothing extra.

use actix_web::{get, post, web, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::claims::level;
use crate::auth::middleware::BearerToken;
use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};
use crate::handlers::AppState;
use crate::services::legacy_import;

const WO_ROW: &str = "jsonb_build_object(
    'id', w.id, 'vehicle_id', w.vehicle_id, 'plate', v.car_no_plate,
    'status', w.status::text, 'title', w.title, 'notes', w.notes_ar,
    'opened_at', w.opened_at, 'closed_at', w.closed_at, 'odometer', w.odometer_at_open,
    'opened_by', (SELECT u.name FROM users_cache u WHERE u.id = w.created_by_user_id),
    'summary_en', (SELECT string_agg(COALESCE(NULLIF(btrim(t.description_en), ''), j.name_en, c.name_en, NULLIF(btrim(t.description_ar), '')), ', ' ORDER BY t.sort_order)
                  FROM work_order_tasks t LEFT JOIN maintenance_categories c ON c.id = t.category_id
                  LEFT JOIN standard_jobs j ON j.id = t.standard_job_id
                 WHERE t.work_order_id = w.id AND t.deleted_at IS NULL AND t.kind <> 'oil'),
    'task_count', (SELECT COUNT(*) FROM work_order_tasks t WHERE t.work_order_id = w.id AND t.deleted_at IS NULL AND t.kind <> 'oil'),
    'done_count', (SELECT COUNT(*) FROM work_order_tasks t WHERE t.work_order_id = w.id AND t.deleted_at IS NULL AND t.kind <> 'oil' AND t.is_done),
    'oil_liters', (SELECT SUM(o.liters)::float8 FROM oil_changes o WHERE o.work_order_id = w.id AND o.deleted_at IS NULL),
    'tire_ops', (SELECT COUNT(*) FROM tire_assignments a
                  WHERE (a.work_order_id = w.id OR a.dismount_work_order_id = w.id) AND a.deleted_at IS NULL),
    'summary', (SELECT string_agg(COALESCE(NULLIF(btrim(t.description_ar), ''), c.name_ar), '، ' ORDER BY t.sort_order)
                  FROM work_order_tasks t LEFT JOIN maintenance_categories c ON c.id = t.category_id
                 WHERE t.work_order_id = w.id AND t.deleted_at IS NULL AND t.kind <> 'oil'))";

#[derive(Debug, Deserialize)]
struct WoListQuery {
    status: Option<String>,
    vehicle_id: Option<i32>,
    /// Plate search (any spelling).
    q: Option<String>,
    /// Opened on or after / before this Cairo date (YYYY-MM-DD).
    from: Option<String>,
    to: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

fn cairo_day(s: &Option<String>, what: &str) -> ApiResult<Option<chrono::NaiveDate>> {
    match s.as_deref().map(str::trim).filter(|x| !x.is_empty()) {
        None => Ok(None),
        Some(x) => chrono::NaiveDate::parse_from_str(x, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| ApiError::BadRequest(format!("{what} must be YYYY-MM-DD"))),
    }
}

/// Work orders, newest first, with the counts a list row shows.
#[get("/office/work-orders")]
async fn list_work_orders(pool: web::Data<PgPool>, q: web::Query<WoListQuery>) -> ApiResult<HttpResponse> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);
    let status = q.status.as_deref().filter(|s| matches!(*s, "open" | "closed"));
    let from = cairo_day(&q.from, "from")?;
    let to = cairo_day(&q.to, "to")?;
    let filter = "w.deleted_at IS NULL
        AND ($1::text IS NULL OR w.status::text = $1)
        AND ($2::int  IS NULL OR w.vehicle_id = $2)
        AND ($3::text IS NULL OR maint_norm(v.car_no_plate) LIKE '%' || maint_norm($3) || '%')
        AND ($4::date IS NULL OR w.opened_at >= ($4::date)::timestamp AT TIME ZONE 'Africa/Cairo')
        AND ($5::date IS NULL OR w.opened_at <  (($5::date) + 1)::timestamp AT TIME ZONE 'Africa/Cairo')";
    let sql = format!(
        "SELECT COALESCE(jsonb_agg(r.row), '[]'::jsonb), (SELECT COUNT(*) FROM work_orders w LEFT JOIN vehicles_cache v ON v.id = w.vehicle_id WHERE {filter})
           FROM (SELECT {WO_ROW} AS row
                   FROM work_orders w LEFT JOIN vehicles_cache v ON v.id = w.vehicle_id
                  WHERE {filter}
                  ORDER BY (w.status = 'open') DESC, w.opened_at DESC, w.id
                  LIMIT $6 OFFSET $7) r"
    );
    let counts_sql = format!(
        "SELECT COALESCE(jsonb_object_agg(s.status, s.n), '{{}}'::jsonb) FROM (
            SELECT w.status::text AS status, COUNT(*) AS n
              FROM work_orders w LEFT JOIN vehicles_cache v ON v.id = w.vehicle_id
             WHERE {} GROUP BY 1) s",
        filter.replace("$1::text IS NULL OR w.status::text = $1", "$1::text IS NULL OR TRUE")
    );
    let term = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let (status_counts,): (Value,) = sqlx::query_as(&counts_sql)
        .bind(status)
        .bind(q.vehicle_id)
        .bind(term)
        .bind(from)
        .bind(to)
        .fetch_one(pool.get_ref())
        .await?;
    let (rows, total): (Value, i64) = sqlx::query_as(&sql)
        .bind(status)
        .bind(q.vehicle_id)
        .bind(q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()))
        .bind(from)
        .bind(to)
        .bind(limit)
        .bind(offset)
        .fetch_one(pool.get_ref())
        .await?;
    Ok(HttpResponse::Ok().json(json!({ "rows": rows, "total": total, "limit": limit, "offset": offset, "status_counts": status_counts })))
}

/// One work order with everything done under it, in display form.
#[get("/office/work-orders/{id}")]
async fn get_work_order(pool: web::Data<PgPool>, path: web::Path<Uuid>) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let sql = format!(
        "SELECT {WO_ROW} FROM work_orders w LEFT JOIN vehicles_cache v ON v.id = w.vehicle_id
          WHERE w.id = $1 AND w.deleted_at IS NULL"
    );
    let head: Option<(Value,)> = sqlx::query_as(&sql).bind(id).fetch_optional(pool.get_ref()).await?;
    let Some((head,)) = head else {
        return Err(ApiError::NotFound("work order".into()));
    };
    let (tasks,): (Value,) = sqlx::query_as(
        "SELECT COALESCE(jsonb_agg(jsonb_build_object(
                    'id', t.id, 'description', t.description_ar,
                    'description_en', COALESCE(NULLIF(btrim(t.description_en), ''), j.name_en),
                    'done_by', (SELECT u.name FROM users_cache u WHERE u.id = t.updated_by_user_id AND t.is_done),
                    'category_id', t.category_id,
                    'category_ar', c.name_ar, 'category_en', c.name_en,
                    'is_done', t.is_done, 'done_at', t.done_at) ORDER BY t.sort_order, t.created_at), '[]'::jsonb)
           FROM work_order_tasks t LEFT JOIN maintenance_categories c ON c.id = t.category_id
           LEFT JOIN standard_jobs j ON j.id = t.standard_job_id
          WHERE t.work_order_id = $1 AND t.deleted_at IS NULL AND t.kind <> 'oil'",
    )
    .bind(id)
    .fetch_one(pool.get_ref())
    .await?;
    let (oil,): (Value,) = sqlx::query_as(
        "SELECT COALESCE(jsonb_agg(jsonb_build_object(
                    'id', o.id, 'oil_type', o.oil_type, 'liters', o.liters::float8, 'odometer', o.odometer,
                    'flagged', o.flagged, 'performed_at', o.performed_at) ORDER BY o.performed_at), '[]'::jsonb)
           FROM oil_changes o WHERE o.work_order_id = $1 AND o.deleted_at IS NULL",
    )
    .bind(id)
    .fetch_one(pool.get_ref())
    .await?;
    let (tires,): (Value,) = sqlx::query_as(
        "SELECT COALESCE(jsonb_agg(jsonb_build_object(
                    'assignment_id', a.id, 'tire_id', a.tire_id, 'position_code', p.position_code,
                    'brand', t.brand, 'model', t.model, 'size', t.size, 'dot_code', t.dot_code,
                    'fitted_here', (a.work_order_id = $1), 'removed_here', (a.dismount_work_order_id = $1),
                    'mounted_at', a.mounted_at, 'mounted_odometer', a.mounted_odometer, 'mount_reason', a.mount_reason::text,
                    'dismounted_at', a.dismounted_at, 'dismounted_odometer', a.dismounted_odometer,
                    'dismount_reason', a.dismount_reason::text) ORDER BY a.mounted_at), '[]'::jsonb)
           FROM tire_assignments a
           JOIN tires t ON t.id = a.tire_id
           LEFT JOIN chassis_positions p ON p.id = a.position_id
          WHERE (a.work_order_id = $1 OR a.dismount_work_order_id = $1) AND a.deleted_at IS NULL",
    )
    .bind(id)
    .fetch_one(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(json!({ "work_order": head, "tasks": tasks, "oil_changes": oil, "tire_work": tires })))
}

/// Everything maintenance knows about one vehicle, for its page in the office
/// dashboard: odometer, class, open work, what is due, the tires on it.
#[get("/office/vehicles/{id}/maintenance")]
async fn vehicle_maintenance(pool: web::Data<PgPool>, path: web::Path<i32>) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let (odometer,): (Value,) = sqlx::query_as(
        "SELECT COALESCE((SELECT jsonb_build_object('km', o.current_odometer, 'source', o.odometer_source, 'observed_at', o.odometer_observed_at)
                            FROM v_current_odometer o WHERE o.vehicle_id = $1), 'null'::jsonb)",
    )
    .bind(id)
    .fetch_one(pool.get_ref())
    .await?;
    let (class,): (Value,) = sqlx::query_as(
        "SELECT COALESCE((SELECT jsonb_build_object('id', c.id, 'name_ar', c.name_ar, 'name_en', c.name_en)
                            FROM vehicle_class_assignments a JOIN vehicle_classes c ON c.id = a.class_id
                           WHERE a.vehicle_id = $1 AND a.deleted_at IS NULL), 'null'::jsonb)",
    )
    .bind(id)
    .fetch_one(pool.get_ref())
    .await?;
    let wo_sql = |cond: &str, limit: i64| {
        format!(
            "SELECT COALESCE(jsonb_agg(r.row), '[]'::jsonb) FROM (
                SELECT {WO_ROW} AS row FROM work_orders w LEFT JOIN vehicles_cache v ON v.id = w.vehicle_id
                 WHERE w.vehicle_id = $1 AND w.deleted_at IS NULL AND {cond}
                 ORDER BY w.opened_at DESC LIMIT {limit}) r"
        )
    };
    let (open,): (Value,) = sqlx::query_as(&wo_sql("w.status = 'open'", 20)).bind(id).fetch_one(pool.get_ref()).await?;
    let (recent,): (Value,) = sqlx::query_as(&wo_sql("w.status = 'closed'", 10)).bind(id).fetch_one(pool.get_ref()).await?;
    let (due,): (Value,) = sqlx::query_as(
        "SELECT COALESCE(jsonb_agg(to_jsonb(d) ORDER BY
                    CASE d.status WHEN 'overdue' THEN 0 WHEN 'never_done' THEN 1 WHEN 'due_soon' THEN 2 WHEN 'unknown' THEN 3 ELSE 4 END,
                    d.category_id), '[]'::jsonb)
           FROM v_maintenance_due d WHERE d.vehicle_id = $1",
    )
    .bind(id)
    .fetch_one(pool.get_ref())
    .await?;
    let (tires,): (Value,) = sqlx::query_as(
        "SELECT COALESCE(jsonb_agg(jsonb_build_object(
                    'position_id', p.id, 'position_code', p.position_code, 'is_spare', p.is_spare, 'spare_index', p.spare_index,
                    'side', p.side::text, 'depth', p.depth::text,
                    'section', x.section::text, 'axle_index', x.section_index, 'is_steering', x.is_steering,
                    'tire_id', t.id, 'brand', t.brand, 'model', t.model, 'size', t.size, 'dot_code', t.dot_code,
                    'total_km', t.total_km, 'mounted_at', t.mounted_at)
                  ORDER BY p.is_spare, (x.section = 'trailer'), x.section_index, p.side, p.depth, p.spare_index), '[]'::jsonb)
           FROM chassis_layouts l
           JOIN chassis_positions p ON p.layout_id = l.id AND p.deleted_at IS NULL
           LEFT JOIN chassis_axles x ON x.id = p.axle_id
           LEFT JOIN v_tires_list t ON t.mounted_position_id = p.id
          WHERE l.vehicle_id = $1 AND l.deleted_at IS NULL",
    )
    .bind(id)
    .fetch_one(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(json!({
        "vehicle_id": id, "odometer": odometer, "class": class,
        "open_work_orders": open, "recent_work_orders": recent, "due": due, "tires": tires,
    })))
}

#[derive(Debug, Deserialize)]
struct TireListQuery {
    status: Option<String>,
    q: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

/// Every tire with where it is now and its km.
#[get("/office/tires")]
async fn list_tires(pool: web::Data<PgPool>, q: web::Query<TireListQuery>) -> ApiResult<HttpResponse> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let filter = "($1::text IS NULL OR t.status::text = $1)
        AND ($2::text IS NULL OR maint_norm(concat_ws(' ', t.brand, t.model, t.size, t.dot_code, v.car_no_plate)) LIKE '%' || maint_norm($2) || '%')";
    let sql = format!(
        "SELECT COALESCE(jsonb_agg(r.row), '[]'::jsonb),
                (SELECT COUNT(*) FROM v_tires_list t LEFT JOIN vehicles_cache v ON v.id = t.mounted_vehicle_id WHERE {filter}),
                (SELECT COALESCE(jsonb_object_agg(s.status, s.n), '{{}}'::jsonb) FROM (SELECT status::text, COUNT(*) AS n FROM tires WHERE deleted_at IS NULL GROUP BY 1) s)
           FROM (SELECT jsonb_build_object(
                    'id', t.id, 'brand', t.brand, 'model', t.model, 'size', t.size, 'dot_code', t.dot_code,
                    'status', t.status::text, 'origin', t.origin::text, 'total_km', t.total_km, 'retread_count', t.retread_count,
                    'vehicle_id', t.mounted_vehicle_id, 'plate', v.car_no_plate, 'position_code', t.mounted_position_code,
                    'mounted_at', t.mounted_at) AS row
                   FROM v_tires_list t LEFT JOIN vehicles_cache v ON v.id = t.mounted_vehicle_id
                  WHERE {filter}
                  ORDER BY (t.status = 'mounted') DESC, v.car_no_plate, t.mounted_position_code, t.brand, t.id
                  LIMIT $3 OFFSET $4) r"
    );
    let (rows, total, counts): (Value, i64, Value) = sqlx::query_as(&sql)
        .bind(q.status.as_deref().map(str::trim).filter(|s| !s.is_empty()))
        .bind(q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()))
        .bind(limit)
        .bind(offset)
        .fetch_one(pool.get_ref())
        .await?;
    Ok(HttpResponse::Ok().json(json!({ "rows": rows, "total": total, "status_counts": counts })))
}

// ── old service invoices as maintenance history ─────────────────────────────

/// Recognised old invoice lines, for the garage app's due clock (it mirrors
/// them and counts them next to ticked tasks).
#[get("/legacy-events")]
async fn legacy_events(pool: web::Data<PgPool>, state: web::Data<AppState>, token: BearerToken) -> ApiResult<HttpResponse> {
    legacy_import::refresh_via_api_if_stale(&state, &token.0);
    let (rows,): (Value,) = sqlx::query_as(
        "SELECT COALESCE(jsonb_agg(jsonb_build_object(
                    'id', l.falcon_item_id, 'vehicle_id', l.vehicle_id, 'category_id', l.category_id,
                    'done_at', l.done_at, 'km', l.km)), '[]'::jsonb)
           FROM legacy_service_lines l WHERE l.category_id IS NOT NULL",
    )
    .fetch_one(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(rows))
}

/// Old invoice wording nobody has recognised yet, most common first.
#[get("/office/legacy/unmatched")]
async fn legacy_unmatched(pool: web::Data<PgPool>, state: web::Data<AppState>, token: BearerToken) -> ApiResult<HttpResponse> {
    legacy_import::refresh_via_api_if_stale(&state, &token.0);
    let (rows,): (Value,) = sqlx::query_as(
        "SELECT COALESCE(jsonb_agg(r.row), '[]'::jsonb) FROM (
            SELECT jsonb_build_object('norm_text', l.norm_text, 'text', MIN(l.service),
                                      'lines', COUNT(*), 'vehicles', COUNT(DISTINCT l.vehicle_id),
                                      'last_at', MAX(l.done_at)) AS row
              FROM legacy_service_lines l
             WHERE l.category_id IS NULL
               AND NOT EXISTS (SELECT 1 FROM legacy_service_map m WHERE m.norm_text = l.norm_text)
             GROUP BY l.norm_text
             ORDER BY COUNT(*) DESC, l.norm_text
             LIMIT 300) r",
    )
    .fetch_one(pool.get_ref())
    .await?;
    let (stats,): (Value,) = sqlx::query_as(
        "SELECT jsonb_build_object('lines', COUNT(*), 'recognised', COUNT(category_id),
                                   'ignored', (SELECT COUNT(*) FROM legacy_service_lines l JOIN legacy_service_map m ON m.norm_text = l.norm_text WHERE m.category_id IS NULL))
           FROM legacy_service_lines",
    )
    .fetch_one(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(json!({ "rows": rows, "stats": stats })))
}

#[derive(Debug, Deserialize)]
struct Decision {
    norm_text: String,
    /// A maintenance type, or null for "not maintenance — ignore".
    category_id: Option<String>,
}

/// Say what an old wording is. Applies to every line with that wording.
#[post("/office/legacy/decide")]
async fn legacy_decide(pool: web::Data<PgPool>, claims: AuthClaims, body: web::Json<Decision>) -> ApiResult<HttpResponse> {
    claims.require(level::CONFIGURE, "reviewing old service lines")?;
    let d = body.into_inner();
    let category = d.category_id.as_deref().map(str::trim).filter(|c| !c.is_empty());
    let mut tx = pool.begin().await?;
    let sample: Option<(String,)> = sqlx::query_as("SELECT MIN(service) FROM legacy_service_lines WHERE norm_text = $1 HAVING COUNT(*) > 0")
        .bind(&d.norm_text)
        .fetch_optional(&mut *tx)
        .await?;
    let Some((sample,)) = sample else {
        return Err(ApiError::NotFound("no old line has that wording".into()));
    };
    sqlx::query(
        "INSERT INTO legacy_service_map (norm_text, sample_text, category_id, decided_by_user_id) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (norm_text) DO UPDATE SET category_id = EXCLUDED.category_id, \
               decided_by_user_id = EXCLUDED.decided_by_user_id, decided_at = now()",
    )
    .bind(&d.norm_text)
    .bind(&sample)
    .bind(category)
    .bind(claims.user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_foreign_key_violation() => ApiError::BadRequest("unknown maintenance type".into()),
        _ => ApiError::Db(e),
    })?;
    legacy_import::reclassify(&mut tx).await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(json!({ "ok": true })))
}

/// Read Falcon's service invoices again now (it also runs hourly).
#[post("/office/legacy/import")]
async fn legacy_import_now(state: web::Data<AppState>, claims: AuthClaims, token: BearerToken) -> ApiResult<HttpResponse> {
    claims.require(level::CONFIGURE, "importing old service lines")?;
    let summary = match state.falcon_pool.as_ref() {
        Some(falcon) => legacy_import::import(&state.pool, falcon).await?,
        // No database link: ask Falcon's API in the caller's name.
        None => legacy_import::import_via_api(&state.pool, &state.falcon, &token.0).await?,
    };
    Ok(HttpResponse::Ok().json(summary))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(list_work_orders)
        .service(get_work_order)
        .service(vehicle_maintenance)
        .service(list_tires)
        .service(legacy_events)
        .service(legacy_unmatched)
        .service(legacy_decide)
        .service(legacy_import_now);
}
