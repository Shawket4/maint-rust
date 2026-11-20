//! Stock caches — reads for the garage, credits for the dashboard (level 4).
//!
//! Per §4 the authoritative credit ledger is Falcon's; these caches mirror it.
//! For local/standalone running, credits write directly here; a real deploy
//! reconciles with Falcon server-side.

use actix_web::{get, post, web, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::middleware::BearerToken;
use crate::auth::claims::level;
use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};
use crate::handlers::AppState;
use crate::services::stock_ledger;

/// Falcon's counts are authoritative, but they do not yet include movements
/// we recorded and have not managed to push up. on-hand = Falcon's number plus
/// our pending movements — never just Falcon's number, which silently gave
/// consumed stock back.
async fn refresh_tire_cache(state: &AppState, token: &str) -> bool {
    let Ok(v) = state.falcon.get_json("/api/maint-stock/tires", token).await else { return false };
    let Some(arr) = v.as_array().or_else(|| v.get("data").and_then(Value::as_array)) else { return false };
    let Ok(mut tx) = state.pool.begin().await else { return false };
    let ok = async {
        sqlx::query("UPDATE tire_stock_cache SET on_hand_qty = 0").execute(&mut *tx).await?;
        for row in arr {
            let brand = row.get("brand").and_then(|x| x.as_str()).unwrap_or("").trim();
            if brand.is_empty() {
                continue;
            }
            let model = row.get("model").and_then(|x| x.as_str()).unwrap_or("").trim();
            let size = row.get("size").and_then(|x| x.as_str()).unwrap_or("").trim();
            let qty = row.get("on_hand_qty").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
            sqlx::query(
                "INSERT INTO tire_stock_cache (brand, model, size, on_hand_qty) VALUES ($1,$2,$3,$4) \
                 ON CONFLICT (brand, model, size) DO UPDATE SET on_hand_qty = EXCLUDED.on_hand_qty, fetched_at = now()",
            )
            .bind(brand).bind(model).bind(size).bind(qty)
            .execute(&mut *tx).await?;
        }
        // Re-apply what Falcon has not seen yet.
        sqlx::query(
            "INSERT INTO tire_stock_cache (brand, model, size, on_hand_qty) \
             SELECT btrim(brand), COALESCE(btrim(model),''), COALESCE(btrim(size),''), \
                    SUM(CASE WHEN kind = 'tire_credit' THEN qty ELSE -qty END)::int \
               FROM stock_movements \
              WHERE kind IN ('tire_debit','tire_credit') AND mirrored_to_falcon_at IS NULL \
                AND brand IS NOT NULL AND qty IS NOT NULL \
              GROUP BY 1,2,3 \
             ON CONFLICT (brand, model, size) DO UPDATE \
                SET on_hand_qty = tire_stock_cache.on_hand_qty + EXCLUDED.on_hand_qty",
        )
        .execute(&mut *tx).await?;
        // A line Falcon no longer lists and nothing is pending against is gone.
        sqlx::query("DELETE FROM tire_stock_cache WHERE on_hand_qty = 0 AND fetched_at < now()")
            .execute(&mut *tx).await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;
    match ok {
        Ok(()) => tx.commit().await.is_ok(),
        Err(e) => {
            tracing::warn!(error = %e, "tire stock refresh failed");
            false
        }
    }
}

async fn refresh_oil_cache(state: &AppState, token: &str) -> bool {
    let Ok(v) = state.falcon.get_json("/api/maint-stock/oil", token).await else { return false };
    let Some(arr) = v.as_array().or_else(|| v.get("data").and_then(Value::as_array)) else { return false };
    let Ok(mut tx) = state.pool.begin().await else { return false };
    let ok = async {
        sqlx::query("UPDATE oil_stock_cache SET liters_on_hand = 0").execute(&mut *tx).await?;
        for row in arr {
            let oil = row.get("oil_type").and_then(|x| x.as_str()).unwrap_or("").trim();
            if oil.is_empty() {
                continue;
            }
            let liters = row.get("liters_on_hand").and_then(|x| x.as_f64()).unwrap_or(0.0);
            sqlx::query(
                "INSERT INTO oil_stock_cache (oil_type, liters_on_hand) VALUES ($1,$2) \
                 ON CONFLICT (oil_type) DO UPDATE SET liters_on_hand = EXCLUDED.liters_on_hand, fetched_at = now()",
            )
            .bind(oil).bind(liters)
            .execute(&mut *tx).await?;
        }
        sqlx::query(
            "INSERT INTO oil_stock_cache (oil_type, liters_on_hand) \
             SELECT btrim(oil_type), SUM(CASE WHEN kind = 'oil_credit' THEN liters ELSE -liters END) \
               FROM stock_movements \
              WHERE kind IN ('oil_debit','oil_credit') AND mirrored_to_falcon_at IS NULL \
                AND oil_type IS NOT NULL AND liters IS NOT NULL \
              GROUP BY 1 \
             ON CONFLICT (oil_type) DO UPDATE \
                SET liters_on_hand = oil_stock_cache.liters_on_hand + EXCLUDED.liters_on_hand",
        )
        .execute(&mut *tx).await?;
        sqlx::query("DELETE FROM oil_stock_cache WHERE liters_on_hand = 0 AND fetched_at < now()")
            .execute(&mut *tx).await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;
    match ok {
        Ok(()) => tx.commit().await.is_ok(),
        Err(e) => {
            tracing::warn!(error = %e, "oil stock refresh failed");
            false
        }
    }
}

/// Tire stock. `X-Stock-Fresh: false` says Falcon could not be reached and
/// the numbers are the last mirror plus our own movements.
#[get("/stock/tires")]
async fn tire_stock(state: web::Data<AppState>, token: BearerToken) -> ApiResult<HttpResponse> {
    stock_ledger::spawn_mirror(&state, &token.0);
    let fresh = refresh_tire_cache(&state, &token.0).await;
    let rows: Vec<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(s) FROM tire_stock_cache s ORDER BY brand, model, size",
    ).fetch_all(&state.pool).await?;
    Ok(HttpResponse::Ok()
        .insert_header(("X-Stock-Fresh", if fresh { "true" } else { "false" }))
        .json(rows.into_iter().map(|(v,)| v).collect::<Vec<_>>()))
}

#[get("/stock/oil")]
async fn oil_stock(state: web::Data<AppState>, token: BearerToken) -> ApiResult<HttpResponse> {
    stock_ledger::spawn_mirror(&state, &token.0);
    let fresh = refresh_oil_cache(&state, &token.0).await;
    let rows: Vec<(Value,)> =
        sqlx::query_as("SELECT to_jsonb(s) FROM oil_stock_cache s ORDER BY oil_type")
            .fetch_all(&state.pool).await?;
    Ok(HttpResponse::Ok()
        .insert_header(("X-Stock-Fresh", if fresh { "true" } else { "false" }))
        .json(rows.into_iter().map(|(v,)| v).collect::<Vec<_>>()))
}

#[derive(Debug, Deserialize)]
struct TireCredit {
    brand: String,
    model: Option<String>,
    size: Option<String>,
    qty: i32,
    /// Idempotency key: a retried or double-clicked credit with the same
    /// ref_id is a no-op. Send one — without it a retry credits twice.
    ref_id: Option<String>,
    note: Option<String>,
}

fn parse_opt_uuid(s: Option<&str>) -> ApiResult<Option<uuid::Uuid>> {
    match s {
        None => Ok(None),
        Some(s) => uuid::Uuid::parse_str(s)
            .map(Some)
            .map_err(|_| ApiError::BadRequest("ref_id must be a uuid".into())),
    }
}

fn clean(s: Option<&str>, what: &str) -> ApiResult<Option<String>> {
    match s.map(str::trim).filter(|t| !t.is_empty()) {
        None => Ok(None),
        Some(t) if t.contains('\0') || t.chars().count() > 120 => {
            Err(ApiError::BadRequest(format!("{what} is not valid")))
        }
        Some(t) => Ok(Some(t.to_string())),
    }
}

#[post("/stock/tires/credit")]
async fn credit_tires(
    state: web::Data<AppState>,
    claims: AuthClaims,
    token: BearerToken,
    body: web::Json<TireCredit>,
) -> ApiResult<HttpResponse> {
    claims.require(level::STOCK_CREDIT, "adding stock")?;
    let b = body.into_inner();
    if b.qty <= 0 || b.qty > 10_000 {
        return Err(ApiError::BadRequest("qty must be between 1 and 10000".into()));
    }
    let brand = clean(Some(&b.brand), "brand")?.ok_or_else(|| ApiError::BadRequest("brand is required".into()))?;
    let model = clean(b.model.as_deref(), "model")?;
    let size = clean(b.size.as_deref(), "size")?;
    let ref_id = parse_opt_uuid(b.ref_id.as_deref())?.unwrap_or_else(uuid::Uuid::new_v4);

    let mut tx = state.pool.begin().await?;
    stock_ledger::record(
        &mut tx,
        &stock_ledger::Movement {
            kind: "tire_credit",
            brand: Some(brand.clone()),
            model: model.clone(),
            size: size.clone(),
            oil_type: None,
            qty: Some(b.qty),
            liters: None,
            ref_id,
            work_order_id: None,
            user_id: Some(claims.user_id),
            note: clean(b.note.as_deref(), "note")?,
        },
    )
    .await?;
    let row: Option<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(t) FROM tire_stock_cache t \
          WHERE brand = $1 AND model = COALESCE($2,'') AND size = COALESCE($3,'')",
    )
    .bind(&brand)
    .bind(&model)
    .bind(&size)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    stock_ledger::spawn_mirror(&state, &token.0);
    Ok(HttpResponse::Ok().json(row.map(|r| r.0).unwrap_or_else(|| {
        json!({ "brand": brand, "model": model.unwrap_or_default(), "size": size.unwrap_or_default(), "on_hand_qty": 0 })
    })))
}

#[derive(Debug, Deserialize)]
struct OilCredit {
    oil_type: String,
    liters: f64,
    /// Idempotency key — see TireCredit::ref_id.
    ref_id: Option<String>,
    note: Option<String>,
}

#[post("/stock/oil/credit")]
async fn credit_oil(
    state: web::Data<AppState>,
    claims: AuthClaims,
    token: BearerToken,
    body: web::Json<OilCredit>,
) -> ApiResult<HttpResponse> {
    claims.require(level::STOCK_CREDIT, "adding stock")?;
    let b = body.into_inner();
    if !b.liters.is_finite() || b.liters <= 0.0 || b.liters > 100_000.0 {
        return Err(ApiError::BadRequest("liters must be between 0 and 100000".into()));
    }
    let oil_type =
        clean(Some(&b.oil_type), "oil_type")?.ok_or_else(|| ApiError::BadRequest("oil_type is required".into()))?;
    let ref_id = parse_opt_uuid(b.ref_id.as_deref())?.unwrap_or_else(uuid::Uuid::new_v4);

    let mut tx = state.pool.begin().await?;
    stock_ledger::record(
        &mut tx,
        &stock_ledger::Movement {
            kind: "oil_credit",
            brand: None,
            model: None,
            size: None,
            oil_type: Some(oil_type.clone()),
            qty: None,
            liters: Some((b.liters * 100.0).round() / 100.0),
            ref_id,
            work_order_id: None,
            user_id: Some(claims.user_id),
            note: clean(b.note.as_deref(), "note")?,
        },
    )
    .await?;
    let row: Option<(Value,)> = sqlx::query_as("SELECT to_jsonb(t) FROM oil_stock_cache t WHERE oil_type = $1")
        .bind(&oil_type)
        .fetch_optional(&mut *tx)
        .await?;
    tx.commit().await?;
    stock_ledger::spawn_mirror(&state, &token.0);
    Ok(HttpResponse::Ok()
        .json(row.map(|r| r.0).unwrap_or_else(|| json!({ "oil_type": oil_type, "liters_on_hand": 0.0 }))))
}

/// The in/out ledger: every recorded stock movement (debits from oil changes
/// and shipment-tire mounts, credits from the dashboard), newest first, with
/// the consuming work order's plate resolved. `kind` filters to tire | oil.
#[derive(Debug, Deserialize)]
struct MovementsQuery {
    kind: Option<String>,
    /// Inclusive lower bound on created_at (date or timestamp) — the
    /// time-range filter. Absent = no lower bound.
    from: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[get("/stock/movements")]
async fn stock_movements(
    pool: web::Data<PgPool>,
    _claims: AuthClaims,
    q: web::Query<MovementsQuery>,
) -> ApiResult<HttpResponse> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);
    let kind_filter = match q.kind.as_deref() {
        Some("tire") => " AND m.kind IN ('tire_debit','tire_credit')",
        Some("oil") => " AND m.kind IN ('oil_debit','oil_credit')",
        _ => "",
    };
    // `from` binds as $3 (Option → NULL when absent). A bad timestamp casts to
    // a SQLSTATE-22 error, which error.rs maps to a clean 400, not a 500.
    let sql = format!(
        "SELECT jsonb_build_object(
             'id', m.id, 'kind', m.kind::text, 'brand', m.brand, 'model', m.model, 'size', m.size,
             'oil_type', m.oil_type, 'qty', m.qty, 'liters', m.liters,
             'work_order_id', m.work_order_id, 'plate', v.car_no_plate,
             -- Render in explicit UTC so the string is constant-offset and the
             -- client's offline lexical ORDER BY created_at is always correct
             -- (a Cairo-DST session tz would otherwise vary the offset).
             'created_at', to_char(m.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'),
             'mirrored', (m.mirrored_to_falcon_at IS NOT NULL),
             'mirror_error', m.mirror_error, 'user_id', m.user_id, 'note', m.note)
           FROM stock_movements m
           LEFT JOIN work_orders wo ON wo.id = m.work_order_id
           LEFT JOIN vehicles_cache v ON v.id = wo.vehicle_id
          WHERE ($3::text IS NULL OR m.created_at >= CASE WHEN $3 ~ '^\\d{{4}}-\\d{{2}}-\\d{{2}}$' THEN ($3::date)::timestamp AT TIME ZONE 'Africa/Cairo' ELSE $3::timestamptz END){kind_filter}
          ORDER BY m.created_at DESC, m.id DESC
          LIMIT $1 OFFSET $2"
    );
    let rows: Vec<(Value,)> = sqlx::query_as(&sql)
        .bind(limit)
        .bind(offset)
        .bind(&q.from)
        .fetch_all(pool.get_ref())
        .await?;
    Ok(HttpResponse::Ok().json(rows.into_iter().map(|(v,)| v).collect::<Vec<_>>()))
}

/// Dev-only: seed a batch of tire + oil stock so the garage has something to consume.
#[post("/stock/dev-seed")]
async fn dev_seed(state: web::Data<AppState>, pool: web::Data<PgPool>) -> ApiResult<HttpResponse> {
    if !cfg!(debug_assertions) || !state.dev_login {
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
        .service(stock_movements)
        .service(credit_tires)
        .service(credit_oil)
        .service(dev_seed);
}
