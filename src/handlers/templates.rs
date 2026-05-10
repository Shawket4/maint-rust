use actix_web::{delete, get, post, put, web, HttpResponse};
use uuid::Uuid;

use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};
use crate::models::record::MaintenanceRecordRow;
use crate::models::template::{
    CreateTemplateInput, MaintenanceTemplateRow, MaintenanceTrigger, UpdateTemplateInput,
};
use crate::services::due_engine::compute_next_due;

fn validate_trigger(
    trigger: MaintenanceTrigger,
    interval_km: Option<i32>,
    interval_days: Option<i32>,
) -> ApiResult<()> {
    match trigger {
        MaintenanceTrigger::Mileage => {
            let Some(km) = interval_km else {
                return Err(ApiError::BadRequest(
                    "trigger_type=mileage requires interval_km".into(),
                ));
            };
            if km <= 0 {
                return Err(ApiError::BadRequest(
                    "interval_km must be > 0 for mileage triggers".into(),
                ));
            }
            if interval_days.is_some() {
                return Err(ApiError::BadRequest(
                    "trigger_type=mileage must not set interval_days".into(),
                ));
            }
        }
        MaintenanceTrigger::Time => {
            let Some(d) = interval_days else {
                return Err(ApiError::BadRequest(
                    "trigger_type=time requires interval_days".into(),
                ));
            };
            if d <= 0 {
                return Err(ApiError::BadRequest(
                    "interval_days must be > 0 for time triggers".into(),
                ));
            }
            if interval_km.is_some() {
                return Err(ApiError::BadRequest(
                    "trigger_type=time must not set interval_km".into(),
                ));
            }
        }
    }
    Ok(())
}

#[post("/templates")]
async fn create_template(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    body: web::Json<CreateTemplateInput>,
) -> ApiResult<HttpResponse> {
    let inp = body.into_inner();
    validate_trigger(inp.trigger_type, inp.interval_km, inp.interval_days)?;

    let id = inp.id.unwrap_or_else(Uuid::new_v4);
    sqlx::query(
        r#"
        INSERT INTO maintenance_templates (
            id, vehicle_id, category_id, name_ar, name_en, notes_ar, notes_en,
            trigger_type, interval_km, interval_days,
            lead_warn_km, lead_warn_days, is_active,
            created_by_user_id, updated_by_user_id
        ) VALUES (
            $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$14
        )
        "#,
    )
    .bind(id)
    .bind(inp.vehicle_id)
    .bind(&inp.category_id)
    .bind(&inp.name_ar)
    .bind(&inp.name_en)
    .bind(&inp.notes_ar)
    .bind(&inp.notes_en)
    .bind(inp.trigger_type)
    .bind(inp.interval_km)
    .bind(inp.interval_days)
    .bind(inp.lead_warn_km)
    .bind(inp.lead_warn_days)
    .bind(inp.is_active)
    .bind(claims.user_id)
    .execute(pool.get_ref())
    .await?;

    let row: MaintenanceTemplateRow = sqlx::query_as("SELECT * FROM v_maintenance_templates WHERE id = $1")
        .bind(id)
        .fetch_one(pool.get_ref())
        .await?;

    Ok(HttpResponse::Created().json(row))
}

#[get("/templates")]
async fn list_templates(
    pool: web::Data<PgPool>,
    q: web::Query<ListTemplatesQuery>,
) -> ApiResult<HttpResponse> {
    let rows: Vec<MaintenanceTemplateRow> = match q.vehicle_id {
        Some(vid) => {
            sqlx::query_as(
                r#"SELECT * FROM v_maintenance_templates
                   WHERE deleted_at IS NULL AND vehicle_id = $1
                   ORDER BY created_at DESC"#,
            )
            .bind(vid)
            .fetch_all(pool.get_ref())
            .await?
        }
        None => {
            sqlx::query_as(
                r#"SELECT * FROM v_maintenance_templates
                   WHERE deleted_at IS NULL
                   ORDER BY created_at DESC LIMIT 1000"#,
            )
            .fetch_all(pool.get_ref())
            .await?
        }
    };
    Ok(HttpResponse::Ok().json(rows))
}

#[derive(serde::Deserialize)]
struct ListTemplatesQuery {
    vehicle_id: Option<i32>,
}

#[get("/templates/{id}")]
async fn get_template(
    pool: web::Data<PgPool>,
    path: web::Path<Uuid>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let row: Option<MaintenanceTemplateRow> = sqlx::query_as(
        "SELECT * FROM v_maintenance_templates WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool.get_ref())
    .await?;
    match row {
        Some(r) => Ok(HttpResponse::Ok().json(r)),
        None => Err(ApiError::NotFound(format!("template {id}"))),
    }
}

#[put("/templates/{id}")]
async fn update_template(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
    body: web::Json<UpdateTemplateInput>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let upd = body.into_inner();

    let mut tx = pool.begin().await?;

    let current: Option<MaintenanceTemplateRow> = sqlx::query_as(
        "SELECT * FROM maintenance_templates WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let current = current.ok_or_else(|| ApiError::NotFound(format!("template {id}")))?;

    if current.sync_version != upd.sync_version {
        let server_row = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT to_jsonb(t) FROM maintenance_templates t WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        return Err(ApiError::SyncConflict {
            entity_id: id.to_string(),
            server_row,
        });
    }

    // Resolve effective values
    let new_trigger = upd.trigger_type.unwrap_or(current.trigger_type);
    let new_interval_km = match upd.interval_km {
        Some(opt) => opt,
        None => current.interval_km,
    };
    let new_interval_days = match upd.interval_days {
        Some(opt) => opt,
        None => current.interval_days,
    };
    validate_trigger(new_trigger, new_interval_km, new_interval_days)?;

    let new_name_ar = upd.name_ar.unwrap_or(current.name_ar.clone());
    let new_name_en = upd.name_en.unwrap_or(current.name_en.clone());
    let new_notes_ar = match upd.notes_ar {
        Some(opt) => opt,
        None => current.notes_ar.clone(),
    };
    let new_notes_en = match upd.notes_en {
        Some(opt) => opt,
        None => current.notes_en.clone(),
    };
    let new_lead_warn_km = upd.lead_warn_km.unwrap_or(current.lead_warn_km);
    let new_lead_warn_days = upd.lead_warn_days.unwrap_or(current.lead_warn_days);
    let new_is_active = upd.is_active.unwrap_or(current.is_active);

    sqlx::query(
        r#"
        UPDATE maintenance_templates SET
            name_ar = $1,
            name_en = $2,
            notes_ar = $3,
            notes_en = $4,
            trigger_type = $5,
            interval_km = $6,
            interval_days = $7,
            lead_warn_km = $8,
            lead_warn_days = $9,
            is_active = $10,
            updated_at = now(),
            updated_by_user_id = $11,
            sync_version = sync_version + 1
        WHERE id = $12
        "#,
    )
    .bind(new_name_ar)
    .bind(new_name_en)
    .bind(new_notes_ar)
    .bind(new_notes_en)
    .bind(new_trigger)
    .bind(new_interval_km)
    .bind(new_interval_days)
    .bind(new_lead_warn_km)
    .bind(new_lead_warn_days)
    .bind(new_is_active)
    .bind(claims.user_id)
    .bind(id)
    .execute(&mut *tx)
    .await?;

    let updated: MaintenanceTemplateRow = sqlx::query_as("SELECT * FROM v_maintenance_templates WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;

    // Side effect: recompute next_due_* on the latest record for this template.
    let latest: Option<MaintenanceRecordRow> = sqlx::query_as(
        r#"SELECT * FROM maintenance_records
           WHERE template_id = $1 AND deleted_at IS NULL
           ORDER BY performed_at DESC LIMIT 1
           FOR UPDATE"#,
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(rec) = latest {
        let (nk, na) = compute_next_due(&updated, rec.performed_at, rec.odometer_at_service);
        sqlx::query(
            r#"UPDATE maintenance_records
               SET next_due_km = $1,
                   next_due_at = $2,
                   updated_at = now(),
                   updated_by_user_id = $3,
                   sync_version = sync_version + 1
               WHERE id = $4"#,
        )
        .bind(nk)
        .bind(na)
        .bind(claims.user_id)
        .bind(rec.id)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(HttpResponse::Ok().json(updated))
}

#[delete("/templates/{id}")]
async fn delete_template(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let res = sqlx::query(
        r#"UPDATE maintenance_templates
           SET deleted_at = now(),
               updated_at = now(),
               updated_by_user_id = $1,
               sync_version = sync_version + 1
           WHERE id = $2 AND deleted_at IS NULL"#,
    )
    .bind(claims.user_id)
    .bind(id)
    .execute(pool.get_ref())
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("template {id}")));
    }
    Ok(HttpResponse::NoContent().finish())
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(create_template)
        .service(list_templates)
        .service(get_template)
        .service(update_template)
        .service(delete_template);
}
