//! Stock caches — reads for the garage, credits for the dashboard (level 4).
//!
//! Per §4 the authoritative credit ledger is Falcon's; these caches mirror it.
//! For local/standalone running, credits write directly here; a real deploy
//! reconciles with Falcon server-side.

use actix_web::{get, post, web, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::middleware::BearerToken;
use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};
use crate::handlers::AppState;
use crate::services::stock_ledger;

/// Mirror Falcon's authoritative tire ledger into tire_stock_cache, then return it (§4).
#[get("/stock/tires")]
async fn tire_stock(state: web::Data<AppState>, token: BearerToken) -> ApiResult<HttpResponse> {
    if let Ok(v) = state.falcon.get_json("/api/maint-stock/tires", &token.0).await {
        if let Some(arr) = v.as_array() {
            for row in arr {
                let brand = row.get("brand").and_then(|x| x.as_str()).unwrap_or("");
                let model = row.get("model").and_then(|x| x.as_str()).unwrap_or("");
                let size = row.get("size").and_then(|x| x.as_str()).unwrap_or("");
                let qty = row.get("on_hand_qty").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                let _ = sqlx::query(
                    "INSERT INTO tire_stock_cache (brand, model, size, on_hand_qty) VALUES ($1,$2,$3,$4) \
                     ON CONFLICT (brand, model, size) DO UPDATE SET on_hand_qty = EXCLUDED.on_hand_qty, fetched_at = now()",
                ).bind(brand).bind(model).bind(size).bind(qty).execute(&state.pool).await;
            }
        }
    }
    let rows: Vec<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(s) FROM tire_stock_cache s ORDER BY brand, model, size",
    ).fetch_all(&state.pool).await?;
    Ok(HttpResponse::Ok().json(rows.into_iter().map(|(v,)| v).collect::<Vec<_>>()))
}

#[get("/stock/oil")]
async fn oil_stock(state: web::Data<AppState>, token: BearerToken) -> ApiResult<HttpResponse> {
    if let Ok(v) = state.falcon.get_json("/api/maint-stock/oil", &token.0).await {
        if let Some(arr) = v.as_array() {
            for row in arr {
                let oil = row.get("oil_type").and_then(|x| x.as_str()).unwrap_or("");
                let liters = row.get("liters_on_hand").and_then(|x| x.as_f64()).unwrap_or(0.0);
                let _ = sqlx::query(
                    "INSERT INTO oil_stock_cache (oil_type, liters_on_hand) VALUES ($1,$2) \
                     ON CONFLICT (oil_type) DO UPDATE SET liters_on_hand = EXCLUDED.liters_on_hand, fetched_at = now()",
                ).bind(oil).bind(liters).execute(&state.pool).await;
            }
        }
    }
    let rows: Vec<(Value,)> =
        sqlx::query_as("SELECT to_jsonb(s) FROM oil_stock_cache s ORDER BY oil_type")
            .fetch_all(&state.pool).await?;
    Ok(HttpResponse::Ok().json(rows.into_iter().map(|(v,)| v).collect::<Vec<_>>()))
}

/// Debit stock: record the movement locally, decrement the cache, and push the
/// debit up to Falcon (§4). Idempotent upstream on ref_id.
#[derive(Debug, Deserialize)]
struct DebitInput {
    kind: String,       // tire_debit | oil_debit
    brand: Option<String>,
    model: Option<String>,
    size: Option<String>,
    oil_type: Option<String>,
    qty: Option<i32>,
    liters: Option<f64>,
    ref_id: String,     // the mount/oil-change UUID (idempotency key)
    work_order_id: Option<String>,
}

#[post("/stock/debit")]
async fn debit_stock(
    state: web::Data<AppState>,
    token: BearerToken,
    body: web::Json<DebitInput>,
) -> ApiResult<HttpResponse> {
    let d = body.into_inner();
    let ref_uuid = uuid::Uuid::parse_str(&d.ref_id)
        .map_err(|_| ApiError::BadRequest("ref_id must be a uuid".into()))?;

    let kind: &'static str = match d.kind.as_str() {
        "tire_debit" => "tire_debit",
        "oil_debit" => "oil_debit",
        other => {
            return Err(ApiError::BadRequest(format!("unknown debit kind: {other}")));
        }
    };
    let spec = stock_ledger::DebitSpec {
        kind,
        brand: d.brand,
        model: d.model,
        size: d.size,
        oil_type: d.oil_type,
        qty: d.qty,
        liters: d.liters,
        ref_id: ref_uuid,
        work_order_id: d.work_order_id.as_ref().and_then(|s| uuid::Uuid::parse_str(s).ok()),
    };

    // Local movement + cache decrement (idempotent on ref_id).
    let mut tx = state.pool.begin().await?;
    stock_ledger::record_debit(&mut tx, &spec).await?;
    tx.commit().await?;

    // Push up to Falcon (best-effort; idempotent on ref_id upstream).
    stock_ledger::mirror_debits(&state, &token.0, &[ref_uuid]).await;
    Ok(HttpResponse::Ok().json(json!({"ok": true})))
}

#[derive(Debug, Deserialize)]
struct TireCredit {
    brand: String,
    model: Option<String>,
    size: Option<String>,
    qty: i32,
}

#[post("/stock/tires/credit")]
async fn credit_tires(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    body: web::Json<TireCredit>,
) -> ApiResult<HttpResponse> {
    if claims.permission < 4 {
        return Err(ApiError::Forbidden("stock credit requires permission >= 4".into()));
    }
    let b = body.into_inner();
    let row: (Value,) = sqlx::query_as(
        r#"
        INSERT INTO tire_stock_cache (brand, model, size, on_hand_qty)
        VALUES ($1, COALESCE($2,''), COALESCE($3,''), $4)
        ON CONFLICT (brand, model, size) DO UPDATE SET
            on_hand_qty = tire_stock_cache.on_hand_qty + EXCLUDED.on_hand_qty,
            fetched_at = now()
        RETURNING to_jsonb(tire_stock_cache)
        "#,
    )
    .bind(&b.brand)
    .bind(&b.model)
    .bind(&b.size)
    .bind(b.qty)
    .fetch_one(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(row.0))
}

#[derive(Debug, Deserialize)]
struct OilCredit {
    oil_type: String,
    liters: f64,
}

#[post("/stock/oil/credit")]
async fn credit_oil(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    body: web::Json<OilCredit>,
) -> ApiResult<HttpResponse> {
    if claims.permission < 4 {
        return Err(ApiError::Forbidden("stock credit requires permission >= 4".into()));
    }
    let b = body.into_inner();
    let row: (Value,) = sqlx::query_as(
        r#"
        INSERT INTO oil_stock_cache (oil_type, liters_on_hand)
        VALUES ($1, $2)
        ON CONFLICT (oil_type) DO UPDATE SET
            liters_on_hand = oil_stock_cache.liters_on_hand + EXCLUDED.liters_on_hand,
            fetched_at = now()
        RETURNING to_jsonb(oil_stock_cache)
        "#,
    )
    .bind(&b.oil_type)
    .bind(b.liters)
    .fetch_one(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(row.0))
}

/// Dev-only: seed a batch of tire + oil stock so the garage has something to consume.
#[post("/stock/dev-seed")]
async fn dev_seed(state: web::Data<AppState>, pool: web::Data<PgPool>) -> ApiResult<HttpResponse> {
    if !state.dev_login {
        return Err(ApiError::Forbidden("dev-seed disabled".into()));
    }
    sqlx::query(
        "INSERT INTO tire_stock_cache (brand, model, size, on_hand_qty) VALUES \
         ('Pirelli', 'FR01', '315/80R22.5', 20), ('Tegrys', 'T20', '315/80R22.5', 10) \
         ON CONFLICT (brand, model, size) DO UPDATE SET on_hand_qty = EXCLUDED.on_hand_qty",
    )
    .execute(pool.get_ref())
    .await?;
    sqlx::query(
        "INSERT INTO oil_stock_cache (oil_type, liters_on_hand) VALUES \
         ('15W-40', 400), ('80W-90', 120) \
         ON CONFLICT (oil_type) DO UPDATE SET liters_on_hand = EXCLUDED.liters_on_hand",
    )
    .execute(pool.get_ref())
    .await?;
    // A few dev vehicles so the app has trucks to service without Falcon.
    sqlx::query(
        "INSERT INTO vehicles_cache (id, car_no_plate, car_type, transporter, last_fuel_odometer, raw_payload) VALUES \
         (101, 'ن ص ر 1234', 'تريلا', 'Apex', 250000, '{}'::jsonb), \
         (102, 'ب ح ر 5678', 'تريلا', 'Apex', 180500, '{}'::jsonb), \
         (103, 'ق ا د 9012', 'No Trailer', 'Apex', 320000, '{}'::jsonb) \
         ON CONFLICT (id) DO UPDATE SET car_no_plate = EXCLUDED.car_no_plate",
    )
    .execute(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(serde_json::json!({"seeded": true})))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(tire_stock)
        .service(oil_stock)
        .service(debit_stock)
        .service(credit_tires)
        .service(credit_oil)
        .service(dev_seed);
}
