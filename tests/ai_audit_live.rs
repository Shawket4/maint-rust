//! Live-Postgres HTTP tests for the AI audit trail and the routes the advisor
//! leans on, run through the real /api/maint scope (handlers::api_scope —
//! the same routes and middleware order main.rs serves):
//!
//!  1. A write carrying X-Via-AI below permission 4 is refused (403) before the
//!     handler runs and leaves no audit row; at permission 4 it applies and
//!     leaves exactly one row with the user, suggestion id, body and results.
//!  2. Requests without the header behave as before and are never recorded —
//!     the desktop app depends on that, at every permission level.
//!  3. GET /records returns the row + sync_version through the sync map.
//!  4. /stock/debit refuses non-positive amounts; /service-plans keeps the
//!     lead/active values an update omits.
//!  5. An AI write whose audit row cannot be written is refused (500) and
//!     never applied.
//!
//! Needs a throwaway database:  MAINT_TEST_DATABASE_URL=postgres://… cargo test
//! Skips (passes vacuously) when the variable is unset, like sync_engine_live.

use actix_web::{test, web, App};
use maint_rust::auth::ai_audit::AI_PERMISSION_ERROR;
use maint_rust::auth::token::mint;
use maint_rust::falcon::{CacheManager, FalconClient};
use maint_rust::handlers::{api_scope, AppState};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

const SECRET: &str = "ai-audit-test-secret";

async fn test_pool() -> Option<PgPool> {
    let url = std::env::var("MAINT_TEST_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect to MAINT_TEST_DATABASE_URL");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migrations");
    sqlx::query(
        "INSERT INTO vehicles_cache (id, car_no_plate, raw_payload, fetched_at)
         VALUES (901, 'TEST 901', '{}'::jsonb, now())
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(&pool)
    .await
    .expect("seed vehicle");
    Some(pool)
}

async fn state(pool: &PgPool) -> AppState {
    AppState {
        pool: pool.clone(),
        // Nothing listens here: Falcon mirroring fails fast and is best-effort.
        falcon: FalconClient::new("http://127.0.0.1:1".into()),
        cache: CacheManager::new(None).await,
        falcon_cars_ttl: 60,
        falcon_invoices_ttl: 60,
        jwt_secret: SECRET.into(),
        dev_login: false,
        falcon_pool: None,
        anthropic_api_key: None,
        ai_model: "test".into(),
    }
}

macro_rules! app {
    ($pool:expr) => {
        test::init_service(
            App::new()
                .app_data(web::Data::new(state(&$pool).await))
                .app_data(web::Data::new($pool.clone()))
                .app_data(web::JsonConfig::default().limit(4 * 1024 * 1024))
                .service(api_scope(SECRET.into())),
        )
        .await
    };
}

/// A user id no other test (or earlier run) uses, so row counts are exact.
fn fresh_user() -> i64 {
    1_000_000 + (Uuid::new_v4().as_u128() % 1_000_000_000) as i64
}

fn bearer(user_id: i64, permission: i32) -> (&'static str, String) {
    let (t, _) = mint(SECRET, user_id, permission, "admin_user", 1).unwrap();
    ("Authorization", format!("Bearer {t}"))
}

fn wo_push(id: Uuid) -> Value {
    json!({ "operations": [{
        "entity_type": "work_orders",
        "entity_id": id.to_string(),
        "operation": "insert",
        "sync_version": 0,
        "payload": {
            "id": id.to_string(), "vehicle_id": 901, "status": "open",
            "title": "ai audit test", "opened_at": chrono::Utc::now().to_rfc3339(),
        },
    }]})
}

async fn wo_exists(pool: &PgPool, id: Uuid) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM work_orders WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
        == 1
}

async fn audit_rows_for_user(pool: &PgPool, user_id: i64) -> Vec<Value> {
    sqlx::query_scalar("SELECT to_jsonb(a) FROM ai_audit_log a WHERE user_id = $1 ORDER BY id")
        .bind(user_id)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[actix_web::test]
async fn ai_write_below_permission_4_is_refused_and_unrecorded() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let app = app!(pool);
    let user = fresh_user();
    let wo = Uuid::new_v4();

    let req = test::TestRequest::post()
        .uri("/api/maint/sync/push")
        .insert_header(bearer(user, 3))
        .insert_header(("X-Via-AI", "advisor"))
        .insert_header(("X-AI-Suggestion-Id", Uuid::new_v4().to_string()))
        .set_json(wo_push(wo))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body, json!({ "error": AI_PERMISSION_ERROR }));

    assert!(!wo_exists(&pool, wo).await, "the handler must not have run");
    assert!(
        audit_rows_for_user(&pool, user).await.is_empty(),
        "a refused AI write leaves no row"
    );
}

#[actix_web::test]
async fn ai_write_at_permission_4_applies_and_records_one_row() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let app = app!(pool);
    let user = fresh_user();
    let wo = Uuid::new_v4();
    let sid = Uuid::new_v4();

    let req = test::TestRequest::post()
        .uri("/api/maint/sync/push")
        .insert_header(bearer(user, 4))
        .insert_header(("X-Via-AI", "advisor"))
        .insert_header(("X-AI-Suggestion-Id", sid.to_string()))
        .set_json(wo_push(wo))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    // The buffered response still reaches the client intact.
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["results"][0]["status"], "applied");
    assert!(wo_exists(&pool, wo).await);

    let rows = audit_rows_for_user(&pool, user).await;
    assert_eq!(rows.len(), 1, "exactly one audit row: {rows:?}");
    let r = &rows[0];
    assert_eq!(r["permission"], 4);
    assert_eq!(r["via"], "advisor");
    assert_eq!(r["suggestion_id"], sid.to_string());
    assert_eq!(r["method"], "POST");
    assert_eq!(r["path"], "/api/maint/sync/push");
    assert_eq!(r["status"], 200);
    assert_eq!(r["request"]["operations"][0]["entity_id"], wo.to_string());
    assert_eq!(r["results"]["results"][0]["status"], "applied");

    // The read side: perm 4 sees it filtered by suggestion, perm 3 is refused.
    let req = test::TestRequest::get()
        .uri(&format!("/api/maint/ai-audit?limit=5&suggestion_id={sid}"))
        .insert_header(bearer(user, 4))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let list: Value = test::read_body_json(resp).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], r["id"]);

    let req = test::TestRequest::get()
        .uri("/api/maint/ai-audit?limit=1")
        .insert_header(bearer(user, 3))
        .to_request();
    assert_eq!(test::call_service(&app, req).await.status(), 403);
    for bad in ["limit=0", "suggestion_id=nope"] {
        let req = test::TestRequest::get()
            .uri(&format!("/api/maint/ai-audit?{bad}"))
            .insert_header(bearer(user, 4))
            .to_request();
        assert_eq!(test::call_service(&app, req).await.status(), 400, "{bad}");
    }
}

#[actix_web::test]
async fn ai_write_refused_by_the_handler_is_recorded_with_its_status() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let app = app!(pool);
    let user = fresh_user();

    let req = test::TestRequest::post()
        .uri("/api/maint/stock/debit")
        .insert_header(bearer(user, 5))
        .insert_header(("X-Via-AI", "advisor"))
        .insert_header(("X-AI-Suggestion-Id", "not-a-uuid"))
        .set_json(json!({ "kind": "tire_debit", "brand": "X", "qty": 0, "ref_id": Uuid::new_v4() }))
        .to_request();
    assert_eq!(test::call_service(&app, req).await.status(), 400);

    let rows = audit_rows_for_user(&pool, user).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], 400);
    assert_eq!(
        rows[0]["suggestion_id"],
        Value::Null,
        "a malformed suggestion id is stored as null"
    );
    assert_eq!(rows[0]["results"]["error"], "bad_request");
}

#[actix_web::test]
async fn requests_without_the_header_are_unchanged_and_unrecorded() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let app = app!(pool);
    let user = fresh_user();
    let wo = Uuid::new_v4();

    // Permission 1 (a garage user) pushes as the desktop app does today.
    let req = test::TestRequest::post()
        .uri("/api/maint/sync/push")
        .insert_header(bearer(user, 1))
        .set_json(wo_push(wo))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["results"][0]["status"], "applied");
    assert!(wo_exists(&pool, wo).await);

    // An empty X-Via-AI is not an AI request either.
    let wo2 = Uuid::new_v4();
    let req = test::TestRequest::post()
        .uri("/api/maint/sync/push")
        .insert_header(bearer(user, 1))
        .insert_header(("X-Via-AI", "  "))
        .set_json(wo_push(wo2))
        .to_request();
    assert_eq!(test::call_service(&app, req).await.status(), 200);
    assert!(wo_exists(&pool, wo2).await);

    // Reads with the header are not gated (FalconGo parity): the advisor's
    // record lookups pass at any permission.
    let req = test::TestRequest::get()
        .uri(&format!("/api/maint/records/work_orders/{wo}"))
        .insert_header(bearer(user, 1))
        .insert_header(("X-Via-AI", "advisor"))
        .to_request();
    assert_eq!(test::call_service(&app, req).await.status(), 200);

    assert!(audit_rows_for_user(&pool, user).await.is_empty());
}

#[actix_web::test]
async fn records_returns_the_row_and_its_sync_version() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let app = app!(pool);
    let user = fresh_user();
    let wo = Uuid::new_v4();
    let req = test::TestRequest::post()
        .uri("/api/maint/sync/push")
        .insert_header(bearer(user, 1))
        .set_json(wo_push(wo))
        .to_request();
    assert_eq!(test::call_service(&app, req).await.status(), 200);
    sqlx::query("UPDATE work_orders SET sync_version = 7 WHERE id = $1")
        .bind(wo)
        .execute(&pool)
        .await
        .unwrap();

    let req = test::TestRequest::get()
        .uri(&format!("/api/maint/records/work_orders/{wo}"))
        .insert_header(bearer(user, 1))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["entity_type"], "work_orders");
    assert_eq!(body["entity_id"], wo.to_string());
    assert_eq!(body["sync_version"], 7);
    assert_eq!(body["row"]["id"], wo.to_string());
    assert_eq!(body["row"]["title"], "ai audit test");

    // Integer-PK singleton through the same map.
    let vid = 20_000 + (Uuid::new_v4().as_u128() % 1_000_000) as i64;
    sqlx::query(
        "INSERT INTO vehicle_odometer_overrides (vehicle_id, odometer, created_by_user_id, updated_by_user_id)
         VALUES ($1, 1234, 1, 1)",
    )
    .bind(vid as i32)
    .execute(&pool)
    .await
    .unwrap();
    let req = test::TestRequest::get()
        .uri(&format!(
            "/api/maint/records/vehicle_odometer_overrides/{vid}"
        ))
        .insert_header(bearer(user, 1))
        .to_request();
    let body: Value = test::read_body_json(test::call_service(&app, req).await).await;
    assert_eq!(body["row"]["odometer"], 1234);
    assert_eq!(body["sync_version"], 1);

    let cases = [
        (
            format!("/api/maint/records/work_orders/{}", Uuid::new_v4()),
            404,
        ),
        (format!("/api/maint/records/service_plans/{wo}"), 400),
        ("/api/maint/records/nope/1".to_string(), 400),
    ];
    for (uri, want) in cases {
        let req = test::TestRequest::get()
            .uri(&uri)
            .insert_header(bearer(user, 1))
            .to_request();
        assert_eq!(test::call_service(&app, req).await.status(), want, "{uri}");
    }
    // Unauthenticated: JwtAuth still guards it.
    let req = test::TestRequest::get()
        .uri(&format!("/api/maint/records/work_orders/{wo}"))
        .to_request();
    assert_eq!(test::call_service(&app, req).await.status(), 401);
}

#[actix_web::test]
async fn debit_refuses_non_positive_amounts() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let app = app!(pool);
    let user = fresh_user();
    let brand = format!("B-{}", &Uuid::new_v4().to_string()[..8]);
    sqlx::query(
        "INSERT INTO tire_stock_cache (brand, model, size, on_hand_qty) VALUES ($1, '', '', 10)",
    )
    .bind(&brand)
    .execute(&pool)
    .await
    .unwrap();

    let bad = [
        json!({ "kind": "tire_debit", "brand": brand, "qty": 0 }),
        json!({ "kind": "tire_debit", "brand": brand, "qty": -3 }),
        json!({ "kind": "tire_debit", "brand": brand }),
        json!({ "kind": "oil_debit", "oil_type": "15W-40", "liters": 0.0 }),
        json!({ "kind": "oil_debit", "oil_type": "15W-40", "liters": -2.5 }),
        json!({ "kind": "oil_debit", "oil_type": "15W-40" }),
    ];
    for mut b in bad {
        b["ref_id"] = json!(Uuid::new_v4());
        let req = test::TestRequest::post()
            .uri("/api/maint/stock/debit")
            .insert_header(bearer(user, 1))
            .set_json(&b)
            .to_request();
        assert_eq!(test::call_service(&app, req).await.status(), 400, "{b}");
    }
    let on_hand: i32 =
        sqlx::query_scalar("SELECT on_hand_qty FROM tire_stock_cache WHERE brand = $1")
            .bind(&brand)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(on_hand, 10, "a refused debit must not move stock");

    let req = test::TestRequest::post()
        .uri("/api/maint/stock/debit")
        .insert_header(bearer(user, 1))
        .set_json(
            json!({ "kind": "tire_debit", "brand": brand, "qty": 2, "ref_id": Uuid::new_v4() }),
        )
        .to_request();
    assert_eq!(test::call_service(&app, req).await.status(), 200);
    let on_hand: i32 =
        sqlx::query_scalar("SELECT on_hand_qty FROM tire_stock_cache WHERE brand = $1")
            .bind(&brand)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(on_hand, 8);
}

#[actix_web::test]
async fn service_plan_update_keeps_omitted_lead_and_active_values() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let app = app!(pool);
    let user = fresh_user();
    // A fresh class per run — plans are unique on (class_id, category_id).
    let class = format!("test_{}", &Uuid::new_v4().simple().to_string()[..12]);
    sqlx::query("INSERT INTO vehicle_classes (id, name_ar, name_en) VALUES ($1, $1, $1)")
        .bind(&class)
        .execute(&pool)
        .await
        .unwrap();

    let post = |body: Value| {
        test::TestRequest::post()
            .uri("/api/maint/service-plans")
            .insert_header(bearer(user, 3))
            .set_json(body)
            .to_request()
    };
    let resp = test::call_service(
        &app,
        post(json!({
            "class_id": class, "category_id": "oil_change", "trigger_type": "mileage",
            "interval_km": 10000, "lead_warn_km": 900, "lead_warn_days": 30, "is_active": false,
        })),
    )
    .await;
    assert_eq!(resp.status(), 200);

    // Update only the interval: the lead values and is_active must survive.
    let resp = test::call_service(
        &app,
        post(json!({
            "class_id": class, "category_id": "oil_change", "trigger_type": "mileage",
            "interval_km": 12000,
        })),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let row: Value = test::read_body_json(resp).await;
    assert_eq!(row["interval_km"], 12000);
    assert_eq!(row["lead_warn_km"], 900);
    assert_eq!(row["lead_warn_days"], 30);
    assert_eq!(row["is_active"], false);
    assert_eq!(row["sync_version"], 2);

    // A value that IS sent still overwrites.
    let row: Value = test::read_body_json(
        test::call_service(
            &app,
            post(json!({
                "class_id": class, "category_id": "oil_change", "trigger_type": "mileage",
                "interval_km": 12000, "is_active": true,
            })),
        )
        .await,
    )
    .await;
    assert_eq!(row["is_active"], true);
    assert_eq!(row["lead_warn_km"], 900);

    // Insert defaults are unchanged.
    let row: Value = test::read_body_json(
        test::call_service(
            &app,
            post(json!({
                "class_id": class, "category_id": "air_filter", "trigger_type": "time",
                "interval_days": 90,
            })),
        )
        .await,
    )
    .await;
    assert_eq!(row["lead_warn_km"], 500);
    assert_eq!(row["lead_warn_days"], 14);
    assert_eq!(row["is_active"], true);
}

#[actix_web::test]
async fn ai_write_is_refused_when_the_audit_row_cannot_be_written() {
    let Some(pool) = test_pool().await else {
        return;
    };
    // A pool that sees no tables at all: the pending-row insert fails, so the
    // AI write must be refused BEFORE its handler runs.
    let url = std::env::var("MAINT_TEST_DATABASE_URL").unwrap();
    let blind = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO no_such_schema")
                    .execute(conn)
                    .await
                    .map(|_| ())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let app = app!(blind);
    let wo = Uuid::new_v4();
    let req = test::TestRequest::post()
        .uri("/api/maint/sync/push")
        .insert_header(bearer(fresh_user(), 4))
        .insert_header(("X-Via-AI", "advisor"))
        .set_json(wo_push(wo))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 500);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(
        body["error"],
        "could not record the AI audit row; the change was not applied"
    );
    assert!(
        !wo_exists(&pool, wo).await,
        "the change must not have applied"
    );
}
