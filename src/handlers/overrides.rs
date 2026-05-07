use actix_web::{delete, get, put, web, HttpResponse, Scope};

use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};
use crate::models::record::{OverrideRow, UpsertOverrideInput};

#[put("/overrides/{vehicle_id}")]
async fn upsert_override(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<i32>,
    body: web::Json<UpsertOverrideInput>,
) -> ApiResult<HttpResponse> {
    let vehicle_id = path.into_inner();
    let inp = body.into_inner();

    if inp.vehicle_id != vehicle_id {
        return Err(ApiError::BadRequest(
            "path vehicle_id does not match body vehicle_id".into(),
        ));
    }
    if inp.odometer < 0 {
        return Err(ApiError::BadRequest("odometer must be >= 0".into()));
    }

    let mut tx = pool.begin().await?;

    let existing: Option<OverrideRow> = sqlx::query_as(
        "SELECT * FROM vehicle_odometer_overrides WHERE vehicle_id = $1 FOR UPDATE",
    )
    .bind(vehicle_id)
    .fetch_optional(&mut *tx)
    .await?;

    // Sync version contract: if the row exists, the client must send a matching sync_version.
    if let Some(curr) = &existing {
        if let Some(client_sv) = inp.sync_version {
            if client_sv != curr.sync_version {
                let server_row = sqlx::query_scalar::<_, serde_json::Value>(
                    "SELECT to_jsonb(t) FROM vehicle_odometer_overrides t WHERE vehicle_id = $1",
                )
                .bind(vehicle_id)
                .fetch_one(&mut *tx)
                .await?;
                return Err(ApiError::SyncConflict {
                    entity_id: vehicle_id.to_string(),
                    server_row,
                });
            }
        }
    }

    let row: OverrideRow = sqlx::query_as(
        r#"
        INSERT INTO vehicle_odometer_overrides (
            vehicle_id, odometer, set_at, notes,
            created_by_user_id, updated_by_user_id, sync_version
        ) VALUES (
            $1, $2, COALESCE($3, now()), $4, $5, $5, 1
        )
        ON CONFLICT (vehicle_id) DO UPDATE SET
            odometer = EXCLUDED.odometer,
            set_at = EXCLUDED.set_at,
            notes = EXCLUDED.notes,
            updated_at = now(),
            updated_by_user_id = $5,
            sync_version = vehicle_odometer_overrides.sync_version + 1
        RETURNING *
        "#,
    )
    .bind(vehicle_id)
    .bind(inp.odometer)
    .bind(inp.set_at)
    .bind(&inp.notes)
    .bind(claims.user_id)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(HttpResponse::Ok().json(row))
}

#[get("/overrides/{vehicle_id}")]
async fn get_override(
    pool: web::Data<PgPool>,
    path: web::Path<i32>,
) -> ApiResult<HttpResponse> {
    let vehicle_id = path.into_inner();
    let row: Option<OverrideRow> = sqlx::query_as(
        "SELECT * FROM vehicle_odometer_overrides WHERE vehicle_id = $1",
    )
    .bind(vehicle_id)
    .fetch_optional(pool.get_ref())
    .await?;
    match row {
        Some(r) => Ok(HttpResponse::Ok().json(r)),
        None => Err(ApiError::NotFound(format!("override for vehicle {vehicle_id}"))),
    }
}

#[delete("/overrides/{vehicle_id}")]
async fn delete_override(
    pool: web::Data<PgPool>,
    _claims: AuthClaims,
    path: web::Path<i32>,
) -> ApiResult<HttpResponse> {
    // No deleted_at column on this table — and §3.6 audit rules apply, so we hard-delete with claims logged.
    let vehicle_id = path.into_inner();
    let res = sqlx::query("DELETE FROM vehicle_odometer_overrides WHERE vehicle_id = $1")
        .bind(vehicle_id)
        .execute(pool.get_ref())
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("override for vehicle {vehicle_id}")));
    }
    tracing::info!(user_id = _claims.user_id, vehicle_id, "deleted odometer override");
    Ok(HttpResponse::NoContent().finish())
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(upsert_override)
        .service(get_override)
        .service(delete_override);
}
