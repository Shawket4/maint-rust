//! Catalogs — the lists behind every dropdown: tire products, oil products,
//! standard jobs.
//!
//! Products are learned: the database links any text that arrives on a tire,
//! an oil change, a stock line or a stock movement to a catalog row, creating
//! it the first time (migration 0024). These endpoints are for reading the
//! lists, adding to them on purpose, and repairing them: two spellings of one
//! product are MERGED, and a rename is a merge into the corrected spelling —
//! the old spelling stays as an alias so text that still arrives that way
//! (an older app, Falcon's ledger) lands on the right product.

use actix_web::{delete, get, post, put, web, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::auth::claims::level;
use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};

fn clean(s: Option<&str>, what: &str, required: bool) -> ApiResult<String> {
    let t = s.map(str::trim).unwrap_or("");
    if t.contains('\0') || t.chars().count() > 120 {
        return Err(ApiError::BadRequest(format!("{what} is not valid")));
    }
    if required && t.is_empty() {
        return Err(ApiError::BadRequest(format!("{what} is required")));
    }
    Ok(t.to_string())
}

/// Everything a picker needs, in one call. Merged-away spellings are left out.
#[get("/catalog")]
async fn catalog(pool: web::Data<PgPool>) -> ApiResult<HttpResponse> {
    let (tires,): (Value,) = sqlx::query_as(
        "SELECT COALESCE(jsonb_agg(jsonb_build_object(
                    'id', p.id, 'brand', p.brand, 'model', p.model, 'size', p.size,
                    'tires', (SELECT COUNT(*) FROM tires t WHERE t.tire_product_id = p.id AND t.deleted_at IS NULL),
                    'on_hand', (SELECT s.on_hand_qty FROM tire_stock_cache s WHERE s.tire_product_id = p.id))
                  ORDER BY p.brand, p.model, p.size), '[]'::jsonb)
           FROM tire_products p WHERE p.merged_into IS NULL",
    )
    .fetch_one(pool.get_ref())
    .await?;
    let (oil,): (Value,) = sqlx::query_as(
        "SELECT COALESCE(jsonb_agg(jsonb_build_object(
                    'id', p.id, 'name', p.name,
                    'uses', (SELECT COUNT(*) FROM oil_changes o WHERE o.oil_product_id = p.id AND o.deleted_at IS NULL),
                    'on_hand', (SELECT s.liters_on_hand FROM oil_stock_cache s WHERE s.oil_product_id = p.id))
                  ORDER BY p.name), '[]'::jsonb)
           FROM oil_products p WHERE p.merged_into IS NULL",
    )
    .fetch_one(pool.get_ref())
    .await?;
    let (jobs,): (Value,) = sqlx::query_as(
        "SELECT COALESCE(jsonb_agg(jsonb_build_object(
                    'id', j.id, 'name_ar', j.name_ar, 'name_en', j.name_en, 'category_id', j.category_id,
                    'uses', (SELECT COUNT(*) FROM work_order_tasks t WHERE t.standard_job_id = j.id AND t.deleted_at IS NULL))
                  ORDER BY j.name_ar), '[]'::jsonb)
           FROM standard_jobs j WHERE j.is_active",
    )
    .fetch_one(pool.get_ref())
    .await?;
    let (batteries,): (Value,) = sqlx::query_as(
        "SELECT COALESCE(jsonb_agg(jsonb_build_object(
                    'id', s.id, 'brand', s.brand, 'model', s.model, 'capacity_ah', s.capacity_ah,
                    'voltage', s.voltage, 'cca', s.cca, 'batteries', s.batteries, 'on_hand', s.on_hand_qty)
                  ORDER BY s.brand, s.model, s.capacity_ah NULLS LAST, s.voltage NULLS LAST), '[]'::jsonb)
           FROM v_battery_stock s",
    )
    .fetch_one(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(json!({
        "tire_products": tires, "oil_products": oil, "standard_jobs": jobs, "battery_products": batteries,
    })))
}

#[derive(Debug, Deserialize)]
struct TireProductInput {
    brand: String,
    model: Option<String>,
    size: Option<String>,
}

/// Add a tire product (or get the existing one for that spelling).
#[post("/catalog/tire-products")]
async fn add_tire_product(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    body: web::Json<TireProductInput>,
) -> ApiResult<HttpResponse> {
    claims.require(level::USE, "the catalog")?;
    let brand = clean(Some(&body.brand), "brand", true)?;
    let model = clean(body.model.as_deref(), "model", false)?;
    let size = clean(body.size.as_deref(), "size", false)?;
    let (row,): (Value,) = sqlx::query_as(
        "SELECT to_jsonb(p) FROM tire_products p WHERE p.id = maint_tire_product($1, $2, $3, $4)",
    )
    .bind(&brand)
    .bind(&model)
    .bind(&size)
    .bind(claims.user_id)
    .fetch_one(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(row))
}

#[derive(Debug, Deserialize)]
struct OilProductInput {
    name: String,
}

#[post("/catalog/oil-products")]
async fn add_oil_product(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    body: web::Json<OilProductInput>,
) -> ApiResult<HttpResponse> {
    claims.require(level::USE, "the catalog")?;
    let name = clean(Some(&body.name), "name", true)?;
    let (row,): (Value,) =
        sqlx::query_as("SELECT to_jsonb(p) FROM oil_products p WHERE p.id = maint_oil_product($1, $2)")
            .bind(&name)
            .bind(claims.user_id)
            .fetch_one(pool.get_ref())
            .await?;
    Ok(HttpResponse::Ok().json(row))
}

/// Fold tire product `from` into `into`: every tire, movement and stock line
/// moves, and `from` stays behind as an alias.
async fn merge_tire(tx: &mut Transaction<'_, Postgres>, from: Uuid, into: Uuid, user_id: i64) -> ApiResult<()> {
    if from == into {
        return Err(ApiError::BadRequest("a product cannot be merged into itself".into()));
    }
    let target: Option<(String, String, String, Option<Uuid>)> =
        sqlx::query_as("SELECT brand, model, size, merged_into FROM tire_products WHERE id = $1 FOR UPDATE")
            .bind(into)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((brand, model, size, target_merged)) = target else {
        return Err(ApiError::NotFound("target product".into()));
    };
    if target_merged.is_some() {
        return Err(ApiError::BadRequest("the target was itself merged away".into()));
    }
    let n = sqlx::query("UPDATE tire_products SET merged_into = $2, updated_at = now() WHERE id = $1 AND merged_into IS NULL")
        .bind(from)
        .bind(into)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound("product".into()));
    }
    // Anything already merged into `from` follows it.
    sqlx::query("UPDATE tire_products SET merged_into = $2 WHERE merged_into = $1").bind(from).bind(into).execute(&mut **tx).await?;
    // Tires take the surviving spelling (the link trigger re-points them) and
    // are re-announced to every device.
    sqlx::query(
        "UPDATE tires SET brand = $2, model = NULLIF($3,''), size = NULLIF($4,''), \
                updated_by_user_id = $5, sync_version = sync_version + 1 \
          WHERE tire_product_id = $1",
    )
    .bind(from)
    .bind(&brand)
    .bind(&model)
    .bind(&size)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE stock_movements SET tire_product_id = $2, brand = $3, model = $4, size = $5 WHERE tire_product_id = $1")
        .bind(from)
        .bind(into)
        .bind(&brand)
        .bind(&model)
        .bind(&size)
        .execute(&mut **tx)
        .await?;
    let moved: Option<(i32,)> = sqlx::query_as("DELETE FROM tire_stock_cache WHERE tire_product_id = $1 RETURNING on_hand_qty")
        .bind(from)
        .fetch_optional(&mut **tx)
        .await?;
    if let Some((qty,)) = moved {
        sqlx::query(
            "INSERT INTO tire_stock_cache (brand, model, size, on_hand_qty) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (brand, model, size) DO UPDATE SET on_hand_qty = tire_stock_cache.on_hand_qty + EXCLUDED.on_hand_qty",
        )
        .bind(&brand)
        .bind(&model)
        .bind(&size)
        .bind(qty)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn merge_oil(tx: &mut Transaction<'_, Postgres>, from: Uuid, into: Uuid, user_id: i64) -> ApiResult<()> {
    if from == into {
        return Err(ApiError::BadRequest("a product cannot be merged into itself".into()));
    }
    let target: Option<(String, Option<Uuid>)> =
        sqlx::query_as("SELECT name, merged_into FROM oil_products WHERE id = $1 FOR UPDATE")
            .bind(into)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((name, target_merged)) = target else {
        return Err(ApiError::NotFound("target product".into()));
    };
    if target_merged.is_some() {
        return Err(ApiError::BadRequest("the target was itself merged away".into()));
    }
    let n = sqlx::query("UPDATE oil_products SET merged_into = $2, updated_at = now() WHERE id = $1 AND merged_into IS NULL")
        .bind(from)
        .bind(into)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound("product".into()));
    }
    sqlx::query("UPDATE oil_products SET merged_into = $2 WHERE merged_into = $1").bind(from).bind(into).execute(&mut **tx).await?;
    sqlx::query(
        "UPDATE oil_changes SET oil_type = $2, updated_by_user_id = $3, sync_version = sync_version + 1 WHERE oil_product_id = $1",
    )
    .bind(from)
    .bind(&name)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE stock_movements SET oil_product_id = $2, oil_type = $3 WHERE oil_product_id = $1")
        .bind(from)
        .bind(into)
        .bind(&name)
        .execute(&mut **tx)
        .await?;
    let moved: Option<(f64,)> =
        sqlx::query_as("DELETE FROM oil_stock_cache WHERE oil_product_id = $1 RETURNING liters_on_hand::float8")
            .bind(from)
            .fetch_optional(&mut **tx)
            .await?;
    if let Some((liters,)) = moved {
        sqlx::query(
            "INSERT INTO oil_stock_cache (oil_type, liters_on_hand) VALUES ($1, $2) \
             ON CONFLICT (oil_type) DO UPDATE SET liters_on_hand = oil_stock_cache.liters_on_hand + EXCLUDED.liters_on_hand",
        )
        .bind(&name)
        .bind(liters)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct MergeInput {
    into: Uuid,
}

/// "These two are the same tire": fold `{id}` into `into`.
#[post("/catalog/tire-products/{id}/merge")]
async fn merge_tire_product(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
    body: web::Json<MergeInput>,
) -> ApiResult<HttpResponse> {
    claims.require(level::CONFIGURE, "merging catalog entries")?;
    let mut tx = pool.begin().await?;
    merge_tire(&mut tx, path.into_inner(), body.into, claims.user_id).await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(json!({ "merged": true })))
}

#[post("/catalog/oil-products/{id}/merge")]
async fn merge_oil_product(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
    body: web::Json<MergeInput>,
) -> ApiResult<HttpResponse> {
    claims.require(level::CONFIGURE, "merging catalog entries")?;
    let mut tx = pool.begin().await?;
    merge_oil(&mut tx, path.into_inner(), body.into, claims.user_id).await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(json!({ "merged": true })))
}

/// Correct a product's spelling. The corrected spelling becomes (or already
/// is) a product and the old one is merged into it, so the old spelling keeps
/// resolving to the same product wherever it still turns up.
#[put("/catalog/tire-products/{id}")]
async fn rename_tire_product(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
    body: web::Json<TireProductInput>,
) -> ApiResult<HttpResponse> {
    claims.require(level::CONFIGURE, "editing the catalog")?;
    let id = path.into_inner();
    let brand = clean(Some(&body.brand), "brand", true)?;
    let model = clean(body.model.as_deref(), "model", false)?;
    let size = clean(body.size.as_deref(), "size", false)?;
    let mut tx = pool.begin().await?;
    let (target,): (Uuid,) = sqlx::query_as("SELECT maint_tire_product($1, $2, $3, $4)")
        .bind(&brand)
        .bind(&model)
        .bind(&size)
        .bind(claims.user_id)
        .fetch_one(&mut *tx)
        .await?;
    if target == id {
        // Same product (only case/spacing differs): just store the new spelling.
        sqlx::query("UPDATE tire_products SET brand = $2, model = $3, size = $4, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(&brand)
            .bind(&model)
            .bind(&size)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE tires SET brand = brand, updated_by_user_id = $2, sync_version = sync_version + 1 WHERE tire_product_id = $1")
            .bind(id)
            .bind(claims.user_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE tire_stock_cache SET brand = brand WHERE tire_product_id = $1").bind(id).execute(&mut *tx).await?;
    } else {
        merge_tire(&mut tx, id, target, claims.user_id).await?;
    }
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(json!({ "id": target })))
}

#[put("/catalog/oil-products/{id}")]
async fn rename_oil_product(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
    body: web::Json<OilProductInput>,
) -> ApiResult<HttpResponse> {
    claims.require(level::CONFIGURE, "editing the catalog")?;
    let id = path.into_inner();
    let name = clean(Some(&body.name), "name", true)?;
    let mut tx = pool.begin().await?;
    let (target,): (Uuid,) = sqlx::query_as("SELECT maint_oil_product($1, $2)")
        .bind(&name)
        .bind(claims.user_id)
        .fetch_one(&mut *tx)
        .await?;
    if target == id {
        sqlx::query("UPDATE oil_products SET name = $2, updated_at = now() WHERE id = $1").bind(id).bind(&name).execute(&mut *tx).await?;
        sqlx::query("UPDATE oil_changes SET oil_type = oil_type, updated_by_user_id = $2, sync_version = sync_version + 1 WHERE oil_product_id = $1")
            .bind(id)
            .bind(claims.user_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE oil_stock_cache SET oil_type = oil_type WHERE oil_product_id = $1").bind(id).execute(&mut *tx).await?;
    } else {
        merge_oil(&mut tx, id, target, claims.user_id).await?;
    }
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(json!({ "id": target })))
}

#[derive(Debug, Deserialize)]
struct JobInput {
    name_ar: String,
    name_en: Option<String>,
    category_id: Option<String>,
}

/// Add a standard job, or bring a removed one back.
#[post("/catalog/standard-jobs")]
async fn add_job(pool: web::Data<PgPool>, claims: AuthClaims, body: web::Json<JobInput>) -> ApiResult<HttpResponse> {
    claims.require(level::USE, "the catalog")?;
    let name_ar = clean(Some(&body.name_ar), "name", true)?;
    let name_en = clean(body.name_en.as_deref(), "name", false)?;
    let category = body.category_id.as_deref().map(str::trim).filter(|c| !c.is_empty());
    let (row,): (Value,) = sqlx::query_as(
        "INSERT INTO standard_jobs (name_ar, name_en, category_id, norm_key, created_by_user_id) \
         VALUES ($1, NULLIF($2,''), $3, maint_norm($1), $4) \
         ON CONFLICT (norm_key) DO UPDATE SET is_active = TRUE, updated_at = now(), \
               category_id = COALESCE(EXCLUDED.category_id, standard_jobs.category_id) \
         RETURNING to_jsonb(standard_jobs)",
    )
    .bind(&name_ar)
    .bind(&name_en)
    .bind(category)
    .bind(claims.user_id)
    .fetch_one(pool.get_ref())
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_foreign_key_violation() => ApiError::BadRequest("unknown maintenance type".into()),
        _ => ApiError::Db(e),
    })?;
    // Tasks already written with exactly this text become this job.
    sqlx::query("UPDATE work_order_tasks SET description_ar = description_ar WHERE maint_norm(description_ar) = maint_norm($1) AND standard_job_id IS NULL")
        .bind(&name_ar)
        .execute(pool.get_ref())
        .await?;
    Ok(HttpResponse::Ok().json(row))
}

#[put("/catalog/standard-jobs/{id}")]
async fn edit_job(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
    body: web::Json<JobInput>,
) -> ApiResult<HttpResponse> {
    claims.require(level::CONFIGURE, "editing the catalog")?;
    let name_ar = clean(Some(&body.name_ar), "name", true)?;
    let name_en = clean(body.name_en.as_deref(), "name", false)?;
    let category = body.category_id.as_deref().map(str::trim).filter(|c| !c.is_empty());
    let row: Option<(Value,)> = sqlx::query_as(
        "UPDATE standard_jobs SET name_ar = $2, name_en = NULLIF($3,''), category_id = $4, norm_key = maint_norm($2), updated_at = now() \
          WHERE id = $1 RETURNING to_jsonb(standard_jobs)",
    )
    .bind(path.into_inner())
    .bind(&name_ar)
    .bind(&name_en)
    .bind(category)
    .fetch_optional(pool.get_ref())
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => ApiError::Conflict("a job with that name already exists".into()),
        sqlx::Error::Database(db) if db.is_foreign_key_violation() => ApiError::BadRequest("unknown maintenance type".into()),
        _ => ApiError::Db(e),
    })?;
    row.map(|r| HttpResponse::Ok().json(r.0)).ok_or_else(|| ApiError::NotFound("job".into()))
}

/// Take a job off the list. Tasks that used it keep their text and their link.
#[delete("/catalog/standard-jobs/{id}")]
async fn remove_job(pool: web::Data<PgPool>, claims: AuthClaims, path: web::Path<Uuid>) -> ApiResult<HttpResponse> {
    claims.require(level::CONFIGURE, "editing the catalog")?;
    let n = sqlx::query("UPDATE standard_jobs SET is_active = FALSE, updated_at = now() WHERE id = $1 AND is_active")
        .bind(path.into_inner())
        .execute(pool.get_ref())
        .await?
        .rows_affected();
    Ok(HttpResponse::Ok().json(json!({ "removed": n > 0 })))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(catalog)
        .service(add_tire_product)
        .service(add_oil_product)
        .service(merge_tire_product)
        .service(merge_oil_product)
        .service(rename_tire_product)
        .service(rename_oil_product)
        .service(add_job)
        .service(edit_job)
        .service(remove_job);
}
