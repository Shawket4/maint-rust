//! Live-Postgres tests for batteries (migrations 0030/0031): counted per
//! product on the shelf in maint-rust alone, one record per battery once it is
//! fitted or registered, numbered slots per truck, removal outcomes.
//!
//! Needs MAINT_TEST_DATABASE_URL; without it every test returns early.

use actix_web::{test, web, App};
use maint_rust::auth::token::mint;
use maint_rust::falcon::{CacheManager, FalconClient};
use maint_rust::handlers::{api_scope, AppState};
use maint_rust::services::sync_engine::{
    apply_push_batch, EntityType, PushBody, PushOperation, PushResultStatus, SyncOperation,
};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

const SECRET: &str = "battery-test-secret";

async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("MAINT_TEST_DATABASE_URL") else {
        assert!(std::env::var("MAINT_REQUIRE_LIVE_TESTS").is_err(), "MAINT_REQUIRE_LIVE_TESTS is set but MAINT_TEST_DATABASE_URL is not");
        return None;
    };
    let pool = PgPoolOptions::new().max_connections(4).connect(&url).await.expect("connect");
    sqlx::migrate!("./migrations").run(&pool).await.expect("migrations");
    Some(pool)
}

async fn state(pool: &PgPool) -> AppState {
    AppState {
        pool: pool.clone(),
        falcon: FalconClient::new("http://127.0.0.1:1".into()),
        cache: CacheManager::new(None).await,
        falcon_cars_ttl: 60,
        falcon_invoices_ttl: 60,
        jwt_secret: SECRET.into(),
        dev_login: false,
        falcon_pool: None,
        anthropic_api_key: None,
        ai_model: "test".into(),
        permissions: Default::default(),
        mirror_busy: Default::default(),
        http: maint_rust::handlers::ai::http_client(),
        ai_limiter: Default::default(),
    }
}

macro_rules! app {
    ($pool:expr) => {
        test::init_service(
            App::new()
                .app_data(web::Data::new(state(&$pool).await))
                .app_data(web::Data::new($pool.clone()))
                .service(api_scope(SECRET.into())),
        )
        .await
    };
}

fn bearer(permission: i32) -> (&'static str, String) {
    let (t, _) = mint(SECRET, 7, permission, "admin_user", 1).unwrap();
    ("Authorization", format!("Bearer {t}"))
}

fn fresh_vehicle() -> i64 {
    2_000_000 + (Uuid::new_v4().as_u128() % 900_000_000) as i64
}

fn op(entity_type: EntityType, operation: SyncOperation, id: &str, payload: Value, sync_version: i64) -> PushOperation {
    PushOperation { entity_type, entity_id: id.to_string(), operation, payload, sync_version, op_id: None, group_id: None }
}

async fn push(pool: &PgPool, ops: Vec<PushOperation>) -> Vec<PushResultStatus> {
    apply_push_batch(pool, &PushBody { operations: ops }, 1).await.unwrap().0.results
}

fn code(r: &PushResultStatus) -> &str {
    match r {
        PushResultStatus::Error { code, .. } => code,
        PushResultStatus::Applied { .. } => "applied",
        PushResultStatus::Conflict { .. } => "conflict",
    }
}

async fn sv(pool: &PgPool, table: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(&format!("SELECT sync_version FROM {table} WHERE id = $1")).bind(id).fetch_one(pool).await.unwrap()
}

async fn status(pool: &PgPool, battery: Uuid) -> String {
    sqlx::query_scalar("SELECT status::text FROM batteries WHERE id = $1").bind(battery).fetch_one(pool).await.unwrap()
}

#[actix_web::test]
async fn batteries_are_counted_on_the_shelf_fitted_into_slots_and_taken_off_with_an_outcome() {
    let Some(pool) = test_pool().await else { return };
    let app = app!(pool);
    let brand = format!("Varta{}", &Uuid::new_v4().simple().to_string()[..6]);
    let post = |uri: &str, perm: i32, body: Value| {
        test::TestRequest::post().uri(uri).insert_header(bearer(perm)).set_json(body).to_request()
    };

    // Receiving needs permission 4; the same product typed another way is one line.
    let spec = json!({"brand": brand, "model": "Blue Dynamic", "capacity_ah": 100, "voltage": 12, "cca": 830});
    let mut body = spec.clone();
    body["qty"] = json!(5);
    assert_eq!(test::call_service(&app, post("/api/maint/stock/batteries/credit", 1, body.clone())).await.status(), 403);
    let line: Value = test::call_and_read_body_json(&app, post("/api/maint/stock/batteries/credit", 4, body)).await;
    assert_eq!((line["on_hand_qty"].as_i64(), line["cca"].as_i64()), (Some(5), Some(830)));
    let product = line["id"].as_str().unwrap().to_string();
    let again: Value = test::call_and_read_body_json(
        &app,
        post("/api/maint/stock/batteries/credit", 4, json!({"brand": format!(" {} ", brand.to_uppercase()), "model": "blue  dynamic", "capacity_ah": 100, "voltage": 12, "qty": 1})),
    )
    .await;
    assert_eq!((again["id"].as_str(), again["on_hand_qty"].as_i64()), (Some(product.as_str()), Some(6)));

    // Fitting one from stock: a battery record + an open install, one off the count.
    let vid = fresh_vehicle();
    let (battery, install) = (Uuid::new_v4(), Uuid::new_v4());
    let r = push(
        &pool,
        vec![
            op(EntityType::Batteries, SyncOperation::Insert, &battery.to_string(), json!({
                "id": battery, "brand": brand, "model": "Blue Dynamic", "capacity_ah": 100, "voltage": 12,
                "serial": "SN-1", "date_code": "2611", "warranty_months": 12, "origin": "stock", "status": "installed"}), 0),
            op(EntityType::BatteryInstalls, SyncOperation::Insert, &install.to_string(), json!({
                "id": install, "battery_id": battery, "vehicle_id": vid, "slot": 1,
                "installed_at": chrono::Utc::now().to_rfc3339(), "installed_odometer": 100000}), 0),
        ],
    )
    .await;
    assert_eq!(r.iter().map(code).collect::<Vec<_>>(), ["applied", "applied"]);
    assert_eq!(status(&pool, battery).await, "installed");
    let stock: Vec<Value> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get().uri("/api/maint/stock/batteries").insert_header(bearer(1)).to_request(),
    )
    .await;
    let mine = stock.iter().find(|l| l["id"] == product.as_str()).unwrap();
    assert_eq!((mine["on_hand_qty"].as_i64(), mine["batteries"].as_i64()), (Some(5), Some(1)));
    let linked: Option<Uuid> = sqlx::query_scalar("SELECT battery_product_id FROM batteries WHERE id = $1").bind(battery).fetch_one(&pool).await.unwrap();
    assert_eq!(linked.map(|u| u.to_string()), Some(product.clone()));
    // Battery stock is never sent to Falcon.
    let mirrorable: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM stock_movements WHERE battery_product_id = $1::uuid AND kind::text NOT LIKE 'battery%'",
    )
    .bind(&product)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(mirrorable, 0);

    // The slot is taken; a used battery cannot go into it, nor can a fitted one move.
    let (legacy, legacy_install) = (Uuid::new_v4(), Uuid::new_v4());
    let r = push(
        &pool,
        vec![
            op(EntityType::Batteries, SyncOperation::Insert, &legacy.to_string(), json!({
                "id": legacy, "brand": "Chloride", "capacity_ah": 150, "voltage": 12, "origin": "legacy"}), 0),
            op(EntityType::BatteryInstalls, SyncOperation::Insert, &legacy_install.to_string(), json!({
                "id": legacy_install, "battery_id": legacy, "vehicle_id": vid, "slot": 1}), 0),
        ],
    )
    .await;
    assert_eq!(code(&r[1]), "slot_occupied");
    let r = push(&pool, vec![op(EntityType::BatteryInstalls, SyncOperation::Insert, &legacy_install.to_string(), json!({
        "id": legacy_install, "battery_id": legacy, "vehicle_id": vid, "slot": 2}), 0)]).await;
    assert_eq!(code(&r[0]), "applied", "a registered battery goes into the free slot");
    let r = push(&pool, vec![op(EntityType::BatteryInstalls, SyncOperation::Update, &install.to_string(),
        json!({"slot": 2}), sv(&pool, "battery_installs", install).await)]).await;
    assert_eq!(code(&r[0]), "validation", "moving is a removal + a fitting");
    // A fitted battery's status is not set by hand, and it cannot be deleted.
    let r = push(&pool, vec![op(EntityType::Batteries, SyncOperation::Update, &battery.to_string(),
        json!({"status": "scrapped"}), sv(&pool, "batteries", battery).await)]).await;
    assert_eq!(code(&r[0]), "battery_installed");
    let r = push(&pool, vec![op(EntityType::Batteries, SyncOperation::Delete, &battery.to_string(), json!({}),
        sv(&pool, "batteries", battery).await)]).await;
    assert_eq!(code(&r[0]), "battery_installed");

    // Taken off for a warranty claim: the outcome is its status.
    let r = push(&pool, vec![op(EntityType::BatteryInstalls, SyncOperation::Update, &install.to_string(), json!({
        "removed_at": chrono::Utc::now().to_rfc3339(), "removed_odometer": 112000,
        "removal_reason": "dead", "removal_outcome": "warranty"}), sv(&pool, "battery_installs", install).await)]).await;
    assert_eq!(code(&r[0]), "applied");
    assert_eq!(status(&pool, battery).await, "warranty");
    // Not available to fit until it is back in stock; then back in stock by hand.
    let refit = Uuid::new_v4();
    let r = push(&pool, vec![op(EntityType::BatteryInstalls, SyncOperation::Insert, &refit.to_string(), json!({
        "id": refit, "battery_id": battery, "vehicle_id": vid, "slot": 1}), 0)]).await;
    assert_eq!(code(&r[0]), "battery_not_available");
    let r = push(&pool, vec![op(EntityType::Batteries, SyncOperation::Update, &battery.to_string(),
        json!({"status": "in_stock_used"}), sv(&pool, "batteries", battery).await)]).await;
    assert_eq!(code(&r[0]), "applied");
    let r = push(&pool, vec![op(EntityType::BatteryInstalls, SyncOperation::Insert, &refit.to_string(), json!({
        "id": refit, "battery_id": battery, "vehicle_id": vid, "slot": 1}), 0)]).await;
    assert_eq!(code(&r[0]), "applied");
    assert_eq!(status(&pool, battery).await, "installed");
    // Undoing that fitting puts it back where its last removal left it.
    let r = push(&pool, vec![op(EntityType::BatteryInstalls, SyncOperation::Delete, &refit.to_string(), json!({}),
        sv(&pool, "battery_installs", refit).await)]).await;
    assert_eq!(code(&r[0]), "applied");
    assert_eq!(status(&pool, battery).await, "warranty");
    // A removal with no outcome or reason: back in stock as used, reason other.
    let r = push(&pool, vec![op(EntityType::BatteryInstalls, SyncOperation::Update, &legacy_install.to_string(), json!({
        "removed_at": chrono::Utc::now().to_rfc3339()}), sv(&pool, "battery_installs", legacy_install).await)]).await;
    assert_eq!(code(&r[0]), "applied");
    assert_eq!(status(&pool, legacy).await, "in_stock_used");
    let (reason, outcome): (String, String) = sqlx::query_as(
        "SELECT removal_reason::text, removal_outcome::text FROM battery_installs WHERE id = $1",
    )
    .bind(legacy_install)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((reason.as_str(), outcome.as_str()), ("other", "in_stock_used"));

    // Slots per truck: an insert upserts (one row per vehicle).
    for slots in [3, 4] {
        let r = push(&pool, vec![op(EntityType::BatteryLayouts, SyncOperation::Insert, &vid.to_string(),
            json!({"vehicle_id": vid, "slots": slots}), 0)]).await;
        assert_eq!(code(&r[0]), "applied");
    }
    let r = push(&pool, vec![op(EntityType::BatteryLayouts, SyncOperation::Insert, &vid.to_string(),
        json!({"vehicle_id": vid, "slots": 40}), 0)]).await;
    assert_eq!(code(&r[0]), "validation");

    // The office car page: slots, nothing fitted now, both removals.
    let m: Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get().uri(&format!("/api/maint/office/vehicles/{vid}/maintenance")).insert_header(bearer(1)).to_request(),
    )
    .await;
    let b = &m["batteries"];
    assert_eq!(b["slots"], 4);
    assert!(b["fitted"].as_array().unwrap().is_empty());
    let removed = b["removed"].as_array().unwrap();
    assert_eq!(removed.len(), 2);
    let first = removed.iter().find(|r| r["install_id"] == install.to_string()).unwrap();
    assert_eq!((first["removal_outcome"].as_str(), first["removal_reason"].as_str(), first["km_run"].as_i64()), (Some("warranty"), Some("dead"), Some(12000)));
    assert_eq!((first["battery"]["serial"].as_str(), first["battery"]["warranty_months"].as_i64()), (Some("SN-1"), Some(12)));
    assert!(first["battery"]["warranty_until"].is_string());

    // The office battery list and the movements feed know batteries.
    let list: Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get().uri(&format!("/api/maint/office/batteries?q={brand}")).insert_header(bearer(1)).to_request(),
    )
    .await;
    assert_eq!(list["total"], 1);
    assert_eq!(list["rows"][0]["status"], "warranty");
    let moves: Vec<Value> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get().uri("/api/maint/stock/movements?kind=battery&limit=200").insert_header(bearer(1)).to_request(),
    )
    .await;
    let mine: Vec<&Value> = moves.iter().filter(|m| m["brand"] == brand.as_str()).collect();
    assert_eq!(mine.len(), 3, "two receipts and one fitting");
    assert!(mine.iter().all(|m| m["size"] == "100Ah 12V"));

    // A count brings the shelf to what is there, marked as a count.
    let mut body = spec.clone();
    body["counted"] = json!(3);
    let counted: Value = test::call_and_read_body_json(&app, post("/api/maint/stock/batteries/count", 4, body)).await;
    assert_eq!((counted["before"].as_i64(), counted["on_hand_qty"].as_i64(), counted["change"].as_i64()), (Some(5), Some(3), Some(-2)));
    let is_count: bool = sqlx::query_scalar(
        "SELECT is_count FROM stock_movements WHERE battery_product_id = $1::uuid ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&product)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(is_count);

    // The catalog offers the product with its count.
    let catalog: Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get().uri("/api/maint/catalog").insert_header(bearer(1)).to_request(),
    )
    .await;
    let p = catalog["battery_products"].as_array().unwrap().iter().find(|p| p["id"] == product.as_str()).unwrap();
    assert_eq!((p["on_hand"].as_i64(), p["capacity_ah"].as_i64(), p["voltage"].as_i64()), (Some(3), Some(100), Some(12)));
}

#[actix_web::test]
async fn deleting_a_battery_from_stock_gives_it_back_and_a_busy_work_order_stays() {
    let Some(pool) = test_pool().await else { return };
    let vid = fresh_vehicle();
    let brand = format!("Bosch{}", &Uuid::new_v4().simple().to_string()[..6]);
    let (wo, battery, install) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let r = push(
        &pool,
        vec![
            op(EntityType::WorkOrders, SyncOperation::Insert, &wo.to_string(), json!({
                "id": wo, "vehicle_id": vid, "status": "open", "opened_at": chrono::Utc::now().to_rfc3339()}), 0),
            op(EntityType::Batteries, SyncOperation::Insert, &battery.to_string(), json!({
                "id": battery, "brand": brand, "capacity_ah": 200, "voltage": 12, "origin": "stock"}), 0),
            op(EntityType::BatteryInstalls, SyncOperation::Insert, &install.to_string(), json!({
                "id": install, "battery_id": battery, "vehicle_id": vid, "slot": 1, "work_order_id": wo}), 0),
            // The hidden task that restarts the battery due date.
            op(EntityType::WorkOrderTasks, SyncOperation::Insert, &Uuid::new_v4().to_string(), json!({
                "id": Uuid::new_v4(), "work_order_id": wo, "kind": "battery", "category_id": "battery",
                "description_ar": "تغيير بطارية", "is_done": true, "done_at": chrono::Utc::now().to_rfc3339()}), 0),
        ],
    )
    .await;
    assert_eq!(r.iter().map(code).collect::<Vec<_>>(), ["applied"; 4]);
    let on_hand = || async {
        sqlx::query_scalar::<_, i32>("SELECT s.on_hand_qty FROM v_battery_stock s JOIN batteries b ON b.battery_product_id = s.id WHERE b.id = $1")
            .bind(battery)
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    assert_eq!(on_hand().await, -1, "an offline fitting may oversell: a count corrects it");

    // The work order has battery work: it cannot be deleted.
    let r = push(&pool, vec![op(EntityType::WorkOrders, SyncOperation::Delete, &wo.to_string(), json!({}), sv(&pool, "work_orders", wo).await)]).await;
    assert_eq!(code(&r[0]), "work_order_not_empty");

    // Undo: the fitting, then the battery record — the unit goes back on the shelf.
    let r = push(&pool, vec![op(EntityType::BatteryInstalls, SyncOperation::Delete, &install.to_string(), json!({}),
        sv(&pool, "battery_installs", install).await)]).await;
    assert_eq!(code(&r[0]), "applied");
    assert_eq!(status(&pool, battery).await, "in_stock_used");
    let r = push(&pool, vec![op(EntityType::Batteries, SyncOperation::Delete, &battery.to_string(), json!({}),
        sv(&pool, "batteries", battery).await)]).await;
    assert_eq!(code(&r[0]), "applied");
    assert_eq!(on_hand().await, 0);
}
