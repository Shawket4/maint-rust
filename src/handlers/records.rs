use actix_web::{delete, get, post, put, web, HttpResponse, Scope};
use chrono::Utc;
use uuid::Uuid;

use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};
use crate::models::record::{
    CreateRecordInput, MaintenanceRecordRow, UpdateRecordInput,
};
use crate::models::template::MaintenanceTemplateRow;
use crate::services::due_engine::compute_next_due;

#[post("/records")]
async fn create_record(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    body: web::Json<CreateRecordInput>,
) -> ApiResult<HttpResponse> {
    let inp = body.into_inner();
    if inp.performed_at > Utc::now() {
        return Err(ApiError::BadRequest(
            "performed_at must be <= now()".into(),
        ));
    }

    let mut tx = pool.begin().await?;

    // Compute next_due_* if linked to a template.
    let (next_due_km, next_due_at) = match inp.template_id {
        Some(tid) => {
            let tpl: Option<MaintenanceTemplateRow> = sqlx::query_as(
                "SELECT * FROM maintenance_templates WHERE id = $1 AND deleted_at IS NULL",
            )
            .bind(tid)
            .fetch_optional(&mut *tx)
            .await?;
            let tpl = tpl
                .ok_or_else(|| ApiError::BadRequest(format!("template {tid} not found")))?;
            // Sanity: the record's category should match the template
            if tpl.category_id != inp.category_id {
                return Err(ApiError::BadRequest(
                    "record.category_id must match template.category_id".into(),
                ));
            }
            compute_next_due(&tpl, inp.performed_at, inp.odometer_at_service)
        }
        None => (None, None),
    };

    let id = inp.id.unwrap_or_else(Uuid::new_v4);
    let row: MaintenanceRecordRow = sqlx::query_as(
        r#"
        INSERT INTO maintenance_records (
            id, template_id, vehicle_id, category_id,
            performed_at, odometer_at_service,
            next_due_at, next_due_km,
            cost, vendor, performed_by, notes, attachments,
            created_by_user_id, updated_by_user_id
        ) VALUES (
            $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$14
        )
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(inp.template_id)
    .bind(inp.vehicle_id)
    .bind(&inp.category_id)
    .bind(inp.performed_at)
    .bind(inp.odometer_at_service)
    .bind(next_due_at)
    .bind(next_due_km)
    .bind(inp.cost)
    .bind(&inp.vendor)
    .bind(&inp.performed_by)
    .bind(&inp.notes)
    .bind(&inp.attachments)
    .bind(claims.user_id)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(HttpResponse::Created().json(row))
}

#[derive(serde::Deserialize)]
struct ListRecordsQuery {
    vehicle_id: Option<i32>,
    template_id: Option<Uuid>,
}

#[get("/records")]
async fn list_records(
    pool: web::Data<PgPool>,
    q: web::Query<ListRecordsQuery>,
) -> ApiResult<HttpResponse> {
    let rows: Vec<MaintenanceRecordRow> = match (q.vehicle_id, q.template_id) {
        (Some(vid), Some(tid)) => {
            sqlx::query_as(
                r#"SELECT * FROM maintenance_records
                   WHERE deleted_at IS NULL AND vehicle_id = $1 AND template_id = $2
                   ORDER BY performed_at DESC LIMIT 1000"#,
            )
            .bind(vid)
            .bind(tid)
            .fetch_all(pool.get_ref())
            .await?
        }
        (Some(vid), None) => {
            sqlx::query_as(
                r#"SELECT * FROM maintenance_records
                   WHERE deleted_at IS NULL AND vehicle_id = $1
                   ORDER BY performed_at DESC LIMIT 1000"#,
            )
            .bind(vid)
            .fetch_all(pool.get_ref())
            .await?
        }
        (None, Some(tid)) => {
            sqlx::query_as(
                r#"SELECT * FROM maintenance_records
                   WHERE deleted_at IS NULL AND template_id = $1
                   ORDER BY performed_at DESC LIMIT 1000"#,
            )
            .bind(tid)
            .fetch_all(pool.get_ref())
            .await?
        }
        (None, None) => {
            sqlx::query_as(
                r#"SELECT * FROM maintenance_records
                   WHERE deleted_at IS NULL
                   ORDER BY performed_at DESC LIMIT 1000"#,
            )
            .fetch_all(pool.get_ref())
            .await?
        }
    };
    Ok(HttpResponse::Ok().json(rows))
}

#[get("/records/{id}")]
async fn get_record(
    pool: web::Data<PgPool>,
    path: web::Path<Uuid>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let row: Option<MaintenanceRecordRow> = sqlx::query_as(
        "SELECT * FROM maintenance_records WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool.get_ref())
    .await?;
    match row {
        Some(r) => Ok(HttpResponse::Ok().json(r)),
        None => Err(ApiError::NotFound(format!("record {id}"))),
    }
}

#[put("/records/{id}")]
async fn update_record(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
    body: web::Json<UpdateRecordInput>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let upd = body.into_inner();

    let mut tx = pool.begin().await?;
    let current: Option<MaintenanceRecordRow> = sqlx::query_as(
        "SELECT * FROM maintenance_records WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let current = current.ok_or_else(|| ApiError::NotFound(format!("record {id}")))?;

    if current.sync_version != upd.sync_version {
        let server_row = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT to_jsonb(t) FROM maintenance_records t WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        return Err(ApiError::SyncConflict {
            entity_id: id.to_string(),
            server_row,
        });
    }

    let new_performed_at = upd.performed_at.unwrap_or(current.performed_at);
    if new_performed_at > Utc::now() {
        return Err(ApiError::BadRequest("performed_at must be <= now()".into()));
    }
    let new_odo = match upd.odometer_at_service {
        Some(opt) => opt,
        None => current.odometer_at_service,
    };
    let new_cost = match upd.cost {
        Some(opt) => opt,
        None => current.cost.clone(),
    };
    let new_vendor = match upd.vendor {
        Some(opt) => opt,
        None => current.vendor.clone(),
    };
    let new_performed_by = match upd.performed_by {
        Some(opt) => opt,
        None => current.performed_by.clone(),
    };
    let new_notes = match upd.notes {
        Some(opt) => opt,
        None => current.notes.clone(),
    };
    let new_attachments = upd.attachments.unwrap_or(current.attachments.clone());

    // Recompute next_due if this record is linked to a template
    let (next_due_km, next_due_at) = if let Some(tid) = current.template_id {
        let tpl: MaintenanceTemplateRow = sqlx::query_as(
            "SELECT * FROM maintenance_templates WHERE id = $1",
        )
        .bind(tid)
        .fetch_one(&mut *tx)
        .await?;
        compute_next_due(&tpl, new_performed_at, new_odo)
    } else {
        (current.next_due_km, current.next_due_at)
    };

    let updated: MaintenanceRecordRow = sqlx::query_as(
        r#"
        UPDATE maintenance_records SET
            performed_at = $1,
            odometer_at_service = $2,
            next_due_at = $3,
            next_due_km = $4,
            cost = $5,
            vendor = $6,
            performed_by = $7,
            notes = $8,
            attachments = $9,
            updated_at = now(),
            updated_by_user_id = $10,
            sync_version = sync_version + 1
        WHERE id = $11
        RETURNING *
        "#,
    )
    .bind(new_performed_at)
    .bind(new_odo)
    .bind(next_due_at)
    .bind(next_due_km)
    .bind(new_cost)
    .bind(new_vendor)
    .bind(new_performed_by)
    .bind(new_notes)
    .bind(new_attachments)
    .bind(claims.user_id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;

    // Side effect: if this update made another record the latest, recompute on that one.
    if let Some(tid) = updated.template_id {
        let latest: Option<MaintenanceRecordRow> = sqlx::query_as(
            r#"SELECT * FROM maintenance_records
               WHERE template_id = $1 AND deleted_at IS NULL
               ORDER BY performed_at DESC LIMIT 1"#,
        )
        .bind(tid)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(rec) = latest {
            if rec.id != updated.id {
                let tpl: MaintenanceTemplateRow = sqlx::query_as(
                    "SELECT * FROM maintenance_templates WHERE id = $1",
                )
                .bind(tid)
                .fetch_one(&mut *tx)
                .await?;
                let (nk, na) = compute_next_due(&tpl, rec.performed_at, rec.odometer_at_service);
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
        }
    }

    tx.commit().await?;
    Ok(HttpResponse::Ok().json(updated))
}

#[delete("/records/{id}")]
async fn delete_record(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();

    let mut tx = pool.begin().await?;
    let current: Option<MaintenanceRecordRow> = sqlx::query_as(
        "SELECT * FROM maintenance_records WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let current = current.ok_or_else(|| ApiError::NotFound(format!("record {id}")))?;

    sqlx::query(
        r#"UPDATE maintenance_records
           SET deleted_at = now(),
               updated_at = now(),
               updated_by_user_id = $1,
               sync_version = sync_version + 1
           WHERE id = $2"#,
    )
    .bind(claims.user_id)
    .bind(id)
    .execute(&mut *tx)
    .await?;

    // Recompute next_due_* on the new latest record for this template
    if let Some(tid) = current.template_id {
        let latest: Option<MaintenanceRecordRow> = sqlx::query_as(
            r#"SELECT * FROM maintenance_records
               WHERE template_id = $1 AND deleted_at IS NULL
               ORDER BY performed_at DESC LIMIT 1"#,
        )
        .bind(tid)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(rec) = latest {
            let tpl: MaintenanceTemplateRow = sqlx::query_as(
                "SELECT * FROM maintenance_templates WHERE id = $1",
            )
            .bind(tid)
            .fetch_one(&mut *tx)
            .await?;
            let (nk, na) = compute_next_due(&tpl, rec.performed_at, rec.odometer_at_service);
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
    }

    tx.commit().await?;
    Ok(HttpResponse::NoContent().finish())
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(create_record)
        .service(list_records)
        .service(get_record)
        .service(update_record)
        .service(delete_record);
}
