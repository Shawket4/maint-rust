use actix_web::{delete, get, post, put, web, HttpResponse, Scope};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};
use crate::models::assignment::{
    AssignmentRow, DismountDestination, DismountInput, MountInput, MountReason,
};
use crate::models::tire::{CreateTireInput, TireRow, TireStatus, UpdateTireInput};
use crate::services::dot_parser::{
    iso_week_monday, parse_dot_date_code, validate_week_year, DotParseError,
};

fn dot_err_msg(e: &DotParseError) -> String {
    match e {
        DotParseError::WrongLength => "DOT date code must be exactly 4 characters".into(),
        DotParseError::NotDigits => "DOT date code must be 4 digits".into(),
        DotParseError::WeekOutOfRange => "DOT week must be in 01..=53".into(),
        DotParseError::InvalidIsoDate => "DOT week/year do not yield a valid ISO date".into(),
        DotParseError::InFuture => "DOT production date cannot be in the future".into(),
    }
}

#[post("/tires")]
async fn create_tire(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    body: web::Json<CreateTireInput>,
) -> ApiResult<HttpResponse> {
    let inp = body.into_inner();

    // Resolve production_week / production_year / production_date
    let (week, year, date) = if let Some(code) = inp.dot_date_code.as_ref() {
        let d = parse_dot_date_code(code).map_err(|e| ApiError::BadRequest(dot_err_msg(&e)))?;
        (Some(d.week), Some(d.year), Some(d.date))
    } else {
        validate_week_year(inp.production_week, inp.production_year)
            .map_err(|e| ApiError::BadRequest(dot_err_msg(&e)))?;
        let date = match (inp.production_week, inp.production_year) {
            (Some(w), Some(y)) => iso_week_monday(y as i32, w as u32),
            _ => None,
        };
        (inp.production_week, inp.production_year, date)
    };

    let id = inp.id.unwrap_or_else(Uuid::new_v4);
    let row: TireRow = sqlx::query_as(
        r#"
        INSERT INTO tires (
            id, dot_code, internal_serial, brand, model,
            purchase_date, purchase_cost, supplier,
            production_week, production_year, production_date,
            is_retread, retread_count, parent_tire_id,
            stock_location, notes,
            created_by_user_id, updated_by_user_id
        ) VALUES (
            $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$17
        )
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(&inp.dot_code)
    .bind(&inp.internal_serial)
    .bind(&inp.brand)
    .bind(&inp.model)
    .bind(inp.purchase_date)
    .bind(inp.purchase_cost)
    .bind(&inp.supplier)
    .bind(week)
    .bind(year)
    .bind(date)
    .bind(inp.is_retread)
    .bind(inp.retread_count)
    .bind(inp.parent_tire_id)
    .bind(&inp.stock_location)
    .bind(&inp.notes)
    .bind(claims.user_id)
    .fetch_one(pool.get_ref())
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.constraint() == Some("tires_dot_code_key") => {
            ApiError::Conflict(format!("DOT code '{}' already exists", inp.dot_code))
        }
        _ => ApiError::Db(e),
    })?;
    Ok(HttpResponse::Created().json(row))
}

#[derive(Deserialize)]
struct ListTiresQuery {
    status: Option<String>,
    brand: Option<String>,
}

#[get("/tires")]
async fn list_tires(
    pool: web::Data<PgPool>,
    q: web::Query<ListTiresQuery>,
) -> ApiResult<HttpResponse> {
    // Use the v_tires_list view which already joins lifetime metrics + current location.
    let mut sql = String::from("SELECT * FROM v_tires_list WHERE 1=1");
    if q.status.is_some() {
        sql.push_str(" AND status = $1");
    }
    if q.brand.is_some() {
        let n = if q.status.is_some() { 2 } else { 1 };
        sql.push_str(&format!(" AND brand ILIKE ${n}"));
    }
    sql.push_str(" ORDER BY updated_at DESC LIMIT 5000");

    let rows: Vec<serde_json::Value> = match (q.status.as_deref(), q.brand.as_deref()) {
        (Some(s), Some(b)) => {
            sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT to_jsonb(t) FROM v_tires_list t WHERE status = $1::tire_status AND brand ILIKE $2 ORDER BY updated_at DESC LIMIT 5000",
            )
            .bind(s)
            .bind(format!("%{b}%"))
            .fetch_all(pool.get_ref())
            .await?
        }
        (Some(s), None) => {
            sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT to_jsonb(t) FROM v_tires_list t WHERE status = $1::tire_status ORDER BY updated_at DESC LIMIT 5000",
            )
            .bind(s)
            .fetch_all(pool.get_ref())
            .await?
        }
        (None, Some(b)) => {
            sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT to_jsonb(t) FROM v_tires_list t WHERE brand ILIKE $1 ORDER BY updated_at DESC LIMIT 5000",
            )
            .bind(format!("%{b}%"))
            .fetch_all(pool.get_ref())
            .await?
        }
        (None, None) => {
            sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT to_jsonb(t) FROM v_tires_list t ORDER BY updated_at DESC LIMIT 5000",
            )
            .fetch_all(pool.get_ref())
            .await?
        }
    };
    Ok(HttpResponse::Ok().json(rows))
}

#[get("/tires/{id}")]
async fn get_tire(pool: web::Data<PgPool>, path: web::Path<Uuid>) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let row: Option<TireRow> = sqlx::query_as(
        "SELECT * FROM tires WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool.get_ref())
    .await?;
    match row {
        Some(r) => Ok(HttpResponse::Ok().json(r)),
        None => Err(ApiError::NotFound(format!("tire {id}"))),
    }
}

#[get("/tires/{id}/history")]
async fn tire_history(
    pool: web::Data<PgPool>,
    path: web::Path<Uuid>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let assignments: Vec<AssignmentRow> = sqlx::query_as(
        r#"SELECT * FROM tire_assignments
           WHERE tire_id = $1 AND deleted_at IS NULL
           ORDER BY mounted_at DESC"#,
    )
    .bind(id)
    .fetch_all(pool.get_ref())
    .await?;
    let lifetime: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM v_tire_lifetime t WHERE tire_id = $1",
    )
    .bind(id)
    .fetch_optional(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(json!({
        "tire_id": id,
        "lifetime": lifetime,
        "assignments": assignments,
    })))
}

#[put("/tires/{id}")]
async fn update_tire(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
    body: web::Json<UpdateTireInput>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let upd = body.into_inner();

    let mut tx = pool.begin().await?;
    let curr: Option<TireRow> = sqlx::query_as(
        "SELECT * FROM tires WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let curr = curr.ok_or_else(|| ApiError::NotFound(format!("tire {id}")))?;

    if curr.sync_version != upd.sync_version {
        let server_row =
            sqlx::query_scalar::<_, serde_json::Value>("SELECT to_jsonb(t) FROM tires t WHERE id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        return Err(ApiError::SyncConflict {
            entity_id: id.to_string(),
            server_row,
        });
    }

    // Resolve DOT-related fields
    let (new_week, new_year, new_date) = if let Some(opt) = &upd.dot_date_code {
        match opt {
            Some(code) => {
                let d = parse_dot_date_code(code).map_err(|e| ApiError::BadRequest(dot_err_msg(&e)))?;
                (Some(d.week), Some(d.year), Some(d.date))
            }
            None => (None, None, None),
        }
    } else {
        let new_w = match upd.production_week {
            Some(o) => o,
            None => curr.production_week,
        };
        let new_y = match upd.production_year {
            Some(o) => o,
            None => curr.production_year,
        };
        validate_week_year(new_w, new_y).map_err(|e| ApiError::BadRequest(dot_err_msg(&e)))?;
        let date = match (new_w, new_y) {
            (Some(w), Some(y)) => iso_week_monday(y as i32, w as u32),
            _ => None,
        };
        (new_w, new_y, date)
    };

    let new_dot = upd.dot_code.unwrap_or(curr.dot_code.clone());
    let new_internal = match upd.internal_serial {
        Some(o) => o,
        None => curr.internal_serial.clone(),
    };
    let new_brand = upd.brand.unwrap_or(curr.brand.clone());
    let new_model = match upd.model {
        Some(o) => o,
        None => curr.model.clone(),
    };
    let new_pdate = match upd.purchase_date {
        Some(o) => o,
        None => curr.purchase_date,
    };
    let new_pcost = match upd.purchase_cost {
        Some(o) => o,
        None => curr.purchase_cost.clone(),
    };
    let new_supplier = match upd.supplier {
        Some(o) => o,
        None => curr.supplier.clone(),
    };
    let new_is_retread = upd.is_retread.unwrap_or(curr.is_retread);
    let new_retread_count = upd.retread_count.unwrap_or(curr.retread_count);
    let new_stock_loc = match upd.stock_location {
        Some(o) => o,
        None => curr.stock_location.clone(),
    };
    let new_notes = match upd.notes {
        Some(o) => o,
        None => curr.notes.clone(),
    };

    let row: TireRow = sqlx::query_as(
        r#"UPDATE tires SET
              dot_code = $1, internal_serial = $2, brand = $3, model = $4,
              purchase_date = $5, purchase_cost = $6, supplier = $7,
              production_week = $8, production_year = $9, production_date = $10,
              is_retread = $11, retread_count = $12,
              stock_location = $13, notes = $14,
              updated_at = now(), updated_by_user_id = $15,
              sync_version = sync_version + 1
           WHERE id = $16
           RETURNING *"#,
    )
    .bind(new_dot)
    .bind(new_internal)
    .bind(new_brand)
    .bind(new_model)
    .bind(new_pdate)
    .bind(new_pcost)
    .bind(new_supplier)
    .bind(new_week)
    .bind(new_year)
    .bind(new_date)
    .bind(new_is_retread)
    .bind(new_retread_count)
    .bind(new_stock_loc)
    .bind(new_notes)
    .bind(claims.user_id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(row))
}

#[delete("/tires/{id}")]
async fn delete_tire(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let curr: Option<TireRow> = sqlx::query_as(
        "SELECT * FROM tires WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool.get_ref())
    .await?;
    let curr = curr.ok_or_else(|| ApiError::NotFound(format!("tire {id}")))?;
    if curr.status == TireStatus::Mounted {
        return Err(ApiError::Conflict(
            "cannot delete a mounted tire; dismount first".into(),
        ));
    }
    sqlx::query(
        r#"UPDATE tires SET deleted_at = now(), updated_at = now(),
                  updated_by_user_id = $1, sync_version = sync_version + 1
           WHERE id = $2"#,
    )
    .bind(claims.user_id)
    .bind(id)
    .execute(pool.get_ref())
    .await?;
    Ok(HttpResponse::NoContent().finish())
}

// ---------- Bulk operations (§9.5) ----------

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BulkOp {
    Mount {
        tire_id: Uuid,
        position_id: Uuid,
        mounted_at: chrono::DateTime<Utc>,
        mounted_odometer: Option<i32>,
        mount_reason: MountReason,
        notes: Option<String>,
    },
    Dismount {
        assignment_id: Uuid,
        dismounted_at: chrono::DateTime<Utc>,
        dismounted_odometer: Option<i32>,
        dismount_reason: MountReason,
        destination: DismountDestination,
        scrap_reason: Option<String>,
        stock_location: Option<String>,
        notes: Option<String>,
    },
    Scrap {
        tire_id: Uuid,
        scrap_reason: String,
        scrap_date: chrono::NaiveDate,
    },
    UpdateStockLocation {
        tire_id: Uuid,
        stock_location: String,
    },
}

#[derive(Deserialize)]
struct BulkBody {
    operations: Vec<BulkOp>,
}

#[derive(Serialize)]
struct BulkResultItem {
    kind: &'static str,
    entity_id: String,
    success: bool,
    data: serde_json::Value,
}

#[post("/tires/bulk")]
async fn tires_bulk(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    body: web::Json<BulkBody>,
) -> ApiResult<HttpResponse> {
    let ops = body.into_inner().operations;
    if ops.is_empty() {
        return Err(ApiError::BadRequest("no operations provided".into()));
    }

    // Single transaction. Operations execute in submitted order.
    // On any failure, we return the failing op's index + reason, and the
    // transaction rolls back via the early return (tx is dropped without commit).
    let mut tx = pool.begin().await?;
    let mut results: Vec<BulkResultItem> = Vec::with_capacity(ops.len());

    for (idx, op) in ops.iter().enumerate() {
        match op {
            BulkOp::Mount {
                tire_id,
                position_id,
                mounted_at,
                mounted_odometer,
                mount_reason,
                notes,
            } => {
                let row = mount_inner(
                    &mut tx,
                    *tire_id,
                    *position_id,
                    *mounted_at,
                    *mounted_odometer,
                    *mount_reason,
                    notes.clone(),
                    claims.user_id,
                )
                .await
                .map_err(|e| annotate_bulk(e, idx, "mount"))?;
                results.push(BulkResultItem {
                    kind: "mount",
                    entity_id: row.id.to_string(),
                    success: true,
                    data: serde_json::to_value(&row).unwrap_or(serde_json::Value::Null),
                });
            }
            BulkOp::Dismount {
                assignment_id,
                dismounted_at,
                dismounted_odometer,
                dismount_reason,
                destination,
                scrap_reason,
                stock_location,
                notes,
            } => {
                let row = dismount_inner(
                    &mut tx,
                    *assignment_id,
                    *dismounted_at,
                    *dismounted_odometer,
                    *dismount_reason,
                    *destination,
                    scrap_reason.clone(),
                    stock_location.clone(),
                    notes.clone(),
                    claims.user_id,
                )
                .await
                .map_err(|e| annotate_bulk(e, idx, "dismount"))?;
                results.push(BulkResultItem {
                    kind: "dismount",
                    entity_id: row.id.to_string(),
                    success: true,
                    data: serde_json::to_value(&row).unwrap_or(serde_json::Value::Null),
                });
            }
            BulkOp::Scrap {
                tire_id,
                scrap_reason,
                scrap_date,
            } => {
                let curr: Option<TireRow> = sqlx::query_as(
                    "SELECT * FROM tires WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
                )
                .bind(tire_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| annotate_bulk(ApiError::Db(e), idx, "scrap"))?;
                let curr = curr.ok_or_else(|| {
                    annotate_bulk(
                        ApiError::NotFound(format!("tire {tire_id}")),
                        idx,
                        "scrap",
                    )
                })?;
                if curr.status == TireStatus::Mounted {
                    return Err(annotate_bulk(
                        ApiError::Conflict("cannot scrap a mounted tire; dismount first".into()),
                        idx,
                        "scrap",
                    ));
                }
                let row: TireRow = sqlx::query_as(
                    r#"UPDATE tires SET status = 'scrapped'::tire_status,
                              scrap_reason = $1, scrap_date = $2,
                              updated_at = now(), updated_by_user_id = $3,
                              sync_version = sync_version + 1
                       WHERE id = $4
                       RETURNING *"#,
                )
                .bind(scrap_reason)
                .bind(scrap_date)
                .bind(claims.user_id)
                .bind(tire_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| annotate_bulk(ApiError::Db(e), idx, "scrap"))?;
                results.push(BulkResultItem {
                    kind: "scrap",
                    entity_id: row.id.to_string(),
                    success: true,
                    data: serde_json::to_value(&row).unwrap_or(serde_json::Value::Null),
                });
            }
            BulkOp::UpdateStockLocation {
                tire_id,
                stock_location,
            } => {
                let row: TireRow = sqlx::query_as(
                    r#"UPDATE tires SET stock_location = $1,
                              updated_at = now(), updated_by_user_id = $2,
                              sync_version = sync_version + 1
                       WHERE id = $3 AND deleted_at IS NULL
                       RETURNING *"#,
                )
                .bind(stock_location)
                .bind(claims.user_id)
                .bind(tire_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| annotate_bulk(ApiError::Db(e), idx, "update_stock_location"))?
                .ok_or_else(|| {
                    annotate_bulk(
                        ApiError::NotFound(format!("tire {tire_id}")),
                        idx,
                        "update_stock_location",
                    )
                })?;
                results.push(BulkResultItem {
                    kind: "update_stock_location",
                    entity_id: row.id.to_string(),
                    success: true,
                    data: serde_json::to_value(&row).unwrap_or(serde_json::Value::Null),
                });
            }
        }
    }

    tx.commit().await?;
    Ok(HttpResponse::Ok().json(json!({ "results": results })))
}

fn annotate_bulk(err: ApiError, idx: usize, kind: &str) -> ApiError {
    match err {
        ApiError::BadRequest(m) => ApiError::BadRequest(format!("op[{idx}] ({kind}): {m}")),
        ApiError::NotFound(m) => ApiError::NotFound(format!("op[{idx}] ({kind}): {m}")),
        ApiError::Conflict(m) => ApiError::Conflict(format!("op[{idx}] ({kind}): {m}")),
        other => other,
    }
}

// ---------- Mount / dismount inner helpers (also used by the assignments handler) ----------

#[allow(clippy::too_many_arguments)]
pub async fn mount_inner(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tire_id: Uuid,
    position_id: Uuid,
    mounted_at: chrono::DateTime<Utc>,
    mounted_odometer: Option<i32>,
    mount_reason: MountReason,
    notes: Option<String>,
    user_id: i64,
) -> ApiResult<AssignmentRow> {
    if mounted_at > Utc::now() {
        return Err(ApiError::BadRequest("mounted_at must be <= now()".into()));
    }

    // Lock the tire row + position row to avoid races.
    let tire: Option<TireRow> = sqlx::query_as(
        "SELECT * FROM tires WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(tire_id)
    .fetch_optional(&mut **tx)
    .await?;
    let tire = tire.ok_or_else(|| ApiError::NotFound(format!("tire {tire_id}")))?;
    if tire.status != TireStatus::InStock {
        return Err(ApiError::Conflict(format!(
            "tire is in status '{:?}'; must be in_stock to mount",
            tire.status
        )));
    }

    // Position exists, not deleted, in a non-deleted layout
    let pos_check: Option<(Uuid, i32)> = sqlx::query_as(
        r#"SELECT cp.id, cl.vehicle_id
             FROM chassis_positions cp
             JOIN chassis_layouts cl ON cl.id = cp.layout_id
            WHERE cp.id = $1 AND cp.deleted_at IS NULL AND cl.deleted_at IS NULL"#,
    )
    .bind(position_id)
    .fetch_optional(&mut **tx)
    .await?;
    let (_pid, vehicle_id) =
        pos_check.ok_or_else(|| ApiError::NotFound(format!("position {position_id}")))?;

    // Vehicle not source-deleted
    let vehicle_ok: Option<i32> = sqlx::query_scalar(
        "SELECT id FROM vehicles_cache WHERE id = $1 AND source_deleted_at IS NULL",
    )
    .bind(vehicle_id)
    .fetch_optional(&mut **tx)
    .await?;
    if vehicle_ok.is_none() {
        return Err(ApiError::Conflict(format!(
            "vehicle {vehicle_id} is not active in cache"
        )));
    }

    // Position has no active assignment
    let active_pos: Option<Uuid> = sqlx::query_scalar(
        r#"SELECT id FROM tire_assignments
           WHERE position_id = $1 AND dismounted_at IS NULL AND deleted_at IS NULL"#,
    )
    .bind(position_id)
    .fetch_optional(&mut **tx)
    .await?;
    if active_pos.is_some() {
        return Err(ApiError::Conflict(
            "position already has an active assignment; dismount first".into(),
        ));
    }

    let assignment_id = Uuid::new_v4();
    let row: AssignmentRow = sqlx::query_as(
        r#"INSERT INTO tire_assignments (
              id, tire_id, position_id, vehicle_id,
              mounted_at, mounted_odometer, mount_reason, notes,
              created_by_user_id, updated_by_user_id
           ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$9)
           RETURNING *"#,
    )
    .bind(assignment_id)
    .bind(tire_id)
    .bind(position_id)
    .bind(vehicle_id)
    .bind(mounted_at)
    .bind(mounted_odometer)
    .bind(mount_reason)
    .bind(&notes)
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await?;

    sqlx::query(
        r#"UPDATE tires SET status = 'mounted'::tire_status,
                  updated_at = now(), updated_by_user_id = $1,
                  sync_version = sync_version + 1
           WHERE id = $2"#,
    )
    .bind(user_id)
    .bind(tire_id)
    .execute(&mut **tx)
    .await?;
    Ok(row)
}

#[allow(clippy::too_many_arguments)]
pub async fn dismount_inner(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    assignment_id: Uuid,
    dismounted_at: chrono::DateTime<Utc>,
    dismounted_odometer: Option<i32>,
    dismount_reason: MountReason,
    destination: DismountDestination,
    scrap_reason: Option<String>,
    stock_location: Option<String>,
    notes: Option<String>,
    user_id: i64,
) -> ApiResult<AssignmentRow> {
    if matches!(destination, DismountDestination::Scrapped) && scrap_reason.is_none() {
        return Err(ApiError::BadRequest(
            "scrap_reason is required when destination=scrapped".into(),
        ));
    }

    let curr: Option<AssignmentRow> = sqlx::query_as(
        "SELECT * FROM tire_assignments WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(assignment_id)
    .fetch_optional(&mut **tx)
    .await?;
    let curr = curr.ok_or_else(|| ApiError::NotFound(format!("assignment {assignment_id}")))?;
    if curr.dismounted_at.is_some() {
        return Err(ApiError::Conflict(
            "assignment is already dismounted".into(),
        ));
    }
    if dismounted_at < curr.mounted_at {
        return Err(ApiError::BadRequest(
            "dismounted_at must be >= mounted_at".into(),
        ));
    }
    if let (Some(d), Some(m)) = (dismounted_odometer, curr.mounted_odometer) {
        if d < m {
            return Err(ApiError::BadRequest(
                "dismounted_odometer must be >= mounted_odometer".into(),
            ));
        }
    }

    let updated: AssignmentRow = sqlx::query_as(
        r#"UPDATE tire_assignments SET
              dismounted_at = $1, dismounted_odometer = $2,
              dismount_reason = $3, notes = COALESCE($4, notes),
              updated_at = now(), updated_by_user_id = $5,
              sync_version = sync_version + 1
           WHERE id = $6
           RETURNING *"#,
    )
    .bind(dismounted_at)
    .bind(dismounted_odometer)
    .bind(dismount_reason)
    .bind(&notes)
    .bind(user_id)
    .bind(assignment_id)
    .fetch_one(&mut **tx)
    .await?;

    let new_status: TireStatus = match destination {
        DismountDestination::InStock => TireStatus::InStock,
        DismountDestination::InRepair => TireStatus::InRepair,
        DismountDestination::Retreading => TireStatus::Retreading,
        DismountDestination::Scrapped => TireStatus::Scrapped,
    };

    sqlx::query(
        r#"UPDATE tires SET
              status = $1,
              scrap_reason = CASE WHEN $1 = 'scrapped'::tire_status THEN $2 ELSE scrap_reason END,
              scrap_date   = CASE WHEN $1 = 'scrapped'::tire_status THEN CURRENT_DATE ELSE scrap_date END,
              stock_location = CASE WHEN $1 = 'in_stock'::tire_status THEN COALESCE($3, stock_location) ELSE stock_location END,
              updated_at = now(), updated_by_user_id = $4,
              sync_version = sync_version + 1
           WHERE id = $5"#,
    )
    .bind(new_status)
    .bind(&scrap_reason)
    .bind(&stock_location)
    .bind(user_id)
    .bind(curr.tire_id)
    .execute(&mut **tx)
    .await?;
    Ok(updated)
}

// ---------- Single-shot mount/dismount handlers ----------

#[post("/tires/{tire_id}/mount")]
async fn mount_handler(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
    body: web::Json<MountInput>,
) -> ApiResult<HttpResponse> {
    let tire_id = path.into_inner();
    let inp = body.into_inner();
    let mut tx = pool.begin().await?;
    let row = mount_inner(
        &mut tx,
        tire_id,
        inp.position_id,
        inp.mounted_at,
        inp.mounted_odometer,
        inp.mount_reason,
        inp.notes,
        claims.user_id,
    )
    .await?;
    let tire: TireRow = sqlx::query_as("SELECT * FROM tires WHERE id = $1")
        .bind(tire_id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(json!({
        "tire": tire,
        "assignment": row,
    })))
}

#[post("/tires/assignments/{assignment_id}/dismount")]
async fn dismount_handler(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
    body: web::Json<DismountInput>,
) -> ApiResult<HttpResponse> {
    let assignment_id = path.into_inner();
    let inp = body.into_inner();
    let mut tx = pool.begin().await?;
    let row = dismount_inner(
        &mut tx,
        assignment_id,
        inp.dismounted_at,
        inp.dismounted_odometer,
        inp.dismount_reason,
        inp.destination,
        inp.scrap_reason,
        inp.stock_location,
        inp.notes,
        claims.user_id,
    )
    .await?;
    let tire: TireRow = sqlx::query_as("SELECT * FROM tires WHERE id = $1")
        .bind(row.tire_id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(json!({
        "tire": tire,
        "assignment": row,
    })))
}

pub fn routes() -> Scope {
    // Bulk path must register before the parameterized /tires/{id} so it isn't shadowed.
    web::scope("")
        .service(tires_bulk)
        .service(mount_handler)
        .service(dismount_handler)
        .service(create_tire)
        .service(list_tires)
        .service(tire_history)
        .service(get_tire)
        .service(update_tire)
        .service(delete_tire)
}
