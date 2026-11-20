//! Live-Postgres tests for the office read models and for old service
//! invoices counting as maintenance history (migration 0025).
//!
//! Needs MAINT_TEST_DATABASE_URL; without it every test returns early.

use actix_web::{test, web, App};
use maint_rust::auth::token::mint;
use maint_rust::falcon::{CacheManager, FalconClient};
use maint_rust::handlers::{api_scope, AppState};
use maint_rust::services::legacy_import;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

const SECRET: &str = "office-test-secret";

async fn test_pool() -> Option<(PgPool, String)> {
    let Ok(url) = std::env::var("MAINT_TEST_DATABASE_URL") else {
        assert!(std::env::var("MAINT_REQUIRE_LIVE_TESTS").is_err(), "MAINT_REQUIRE_LIVE_TESTS is set but MAINT_TEST_DATABASE_URL is not");
        return None;
    };
    let pool = PgPoolOptions::new().max_connections(4).connect(&url).await.expect("connect");
    sqlx::migrate!("./migrations").run(&pool).await.expect("migrations");
    Some((pool, url))
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

fn bearer(permission: i32, user_type: &str) -> (&'static str, String) {
    let (t, _) = mint(SECRET, 7, permission, user_type, 1).unwrap();
    ("Authorization", format!("Bearer {t}"))
}

/// A vehicle id no other test or earlier run uses.
fn fresh_vehicle() -> i32 {
    1_000_000 + (Uuid::new_v4().as_u128() % 900_000_000) as i32
}

async fn seed_vehicle(pool: &PgPool, id: i32) -> String {
    let plate = format!("OFF {id}");
    sqlx::query("INSERT INTO vehicles_cache (id, car_no_plate, raw_payload, fetched_at) VALUES ($1, $2, '{}'::jsonb, now())")
        .bind(id)
        .bind(&plate)
        .execute(pool)
        .await
        .unwrap();
    plate
}

/// A class with one km+days plan for `category`, and the vehicle in it.
async fn seed_plan(pool: &PgPool, vehicle: i32, category: &str) {
    let class = format!("cls-{vehicle}");
    sqlx::query("INSERT INTO vehicle_classes (id, name_ar, name_en) VALUES ($1, $1, $1)").bind(&class).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO vehicle_class_assignments (vehicle_id, class_id, created_by_user_id, updated_by_user_id) VALUES ($1, $2, 7, 7)")
        .bind(vehicle)
        .bind(&class)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO service_plans (class_id, category_id, trigger_type, interval_km, interval_days, lead_warn_km, lead_warn_days, created_by_user_id, updated_by_user_id) \
         VALUES ($1, $2, 'mileage', 10000, NULL, 1000, 14, 7, 7)",
    )
    .bind(&class)
    .bind(category)
    .execute(pool)
    .await
    .unwrap();
}

async fn due_row(pool: &PgPool, vehicle: i32, category: &str) -> Value {
    sqlx::query_scalar("SELECT to_jsonb(d) FROM v_maintenance_due d WHERE vehicle_id = $1 AND category_id = $2")
        .bind(vehicle)
        .bind(category)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[actix_web::test]
async fn old_invoice_lines_count_toward_plans_and_can_be_reviewed() {
    let Some((pool, url)) = test_pool().await else { return };
    // A stand-in for Falcon's database: the two tables the importer reads,
    // in a schema of their own.
    let schema = format!("falcon_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(&pool).await.unwrap();
    for ddl in [
        format!("CREATE TABLE {schema}.service_invoices (id bigserial PRIMARY KEY, car_id bigint, date timestamptz, meter_reading bigint, deleted_at timestamptz)"),
        format!("CREATE TABLE {schema}.inspection_items (id bigserial PRIMARY KEY, service_invoice_id bigint, service text, deleted_at timestamptz)"),
    ] {
        sqlx::query(&ddl).execute(&pool).await.unwrap();
    }
    let sp = schema.clone();
    let falcon = PgPoolOptions::new()
        .max_connections(2)
        .after_connect(move |c, _| {
            let sp = sp.clone();
            Box::pin(async move {
                sqlx::query(&format!("SET search_path = {sp}")).execute(c).await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();

    let car = fresh_vehicle();
    seed_vehicle(&pool, car).await;
    seed_plan(&pool, car, "oil_change").await;
    seed_plan_extra(&pool, car, "brakes").await;
    assert_eq!(due_row(&pool, car, "oil_change").await["status"], "never_done");

    // Item ids must not collide with another run's rows in legacy_service_lines.
    let base: i64 = 1_000_000_000 + (Uuid::new_v4().as_u128() % 1_000_000_000_000) as i64;
    let odd = format!("شغلانة غريبة {}", &schema[7..15]);
    sqlx::query(&format!("INSERT INTO {schema}.service_invoices (id, car_id, date, meter_reading) VALUES ($1, $2, now() - interval '10 days', 95000), ($3, $2, now() - interval '400 days', 0)"))
        .bind(base)
        .bind(car as i64)
        .bind(base + 1)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(&format!(
        "INSERT INTO {schema}.inspection_items (id, service_invoice_id, service, deleted_at) VALUES \
         ($1, $5, 'تغيير زيت موتور', NULL), ($2, $5, $6, NULL), ($3, $5, 'تغيير فلتر زيت', now()), ($4, $7, 'غيار زيت', NULL)"
    ))
    .bind(base)
    .bind(base + 1)
    .bind(base + 2)
    .bind(base + 3)
    .bind(base)
    .bind(&odd)
    .bind(base + 1)
    .execute(&pool)
    .await
    .unwrap();

    let s = legacy_import::import(&pool, &falcon).await.unwrap();
    assert!(s.lines >= 3 && s.recognised >= 2, "{s:?}");
    let mine: Vec<(i64, Option<String>, Option<i32>)> =
        sqlx::query_as("SELECT falcon_item_id, category_id, km FROM legacy_service_lines WHERE vehicle_id = $1 ORDER BY 1").bind(car).fetch_all(&pool).await.unwrap();
    assert_eq!(
        mine,
        vec![(base, Some("oil_change".into()), Some(95000)), (base + 1, None, Some(95000)), (base + 3, Some("oil_change".into()), None)],
        "a deleted line is not imported; a zero meter reading is no reading"
    );

    // The newest recognised line is the baseline: done 10 days ago at 95,000 km.
    let due = due_row(&pool, car, "oil_change").await;
    assert_eq!(due["last_km"], 95000, "{due}");
    assert_eq!(due["next_due_km"], 105000);
    assert_ne!(due["status"], "never_done");
    assert_eq!(due_row(&pool, car, "brakes").await["status"], "never_done");

    // Importing again changes nothing.
    legacy_import::import(&pool, &falcon).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM legacy_service_lines WHERE vehicle_id = $1").bind(car).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 3);

    // Review: the odd wording is listed; a person says it is brake work.
    let app = app!(pool);
    let req = test::TestRequest::get().uri("/api/maint/office/legacy/unmatched").insert_header(bearer(1, "admin_user")).to_request();
    let resp = test::call_service(&app, req).await;
    let status = resp.status();
    let raw = test::read_body(resp).await;
    assert!(status.is_success(), "{status} {}", String::from_utf8_lossy(&raw));
    let body: Value = serde_json::from_slice(&raw).unwrap();
    let row = body["rows"].as_array().unwrap().iter().find(|r| r["text"] == odd.as_str()).cloned().expect("listed for review");
    let decide = |perm: i32, cat: Value| {
        test::TestRequest::post()
            .uri("/api/maint/office/legacy/decide")
            .insert_header(bearer(perm, "admin_user"))
            .set_json(json!({ "norm_text": row["norm_text"], "category_id": cat }))
            .to_request()
    };
    assert_eq!(test::call_service(&app, decide(1, json!("brakes"))).await.status(), 403, "review needs permission 3");
    assert_eq!(test::call_service(&app, decide(3, json!("no_such_type"))).await.status(), 400);
    assert!(test::call_service(&app, decide(3, json!("brakes"))).await.status().is_success());
    assert_eq!(due_row(&pool, car, "brakes").await["last_km"], 95000, "the decision applies to history at once");

    // "Not maintenance": off the review list, and no longer counted.
    assert!(test::call_service(&app, decide(3, Value::Null)).await.status().is_success());
    assert_eq!(due_row(&pool, car, "brakes").await["status"], "never_done");
    let req = test::TestRequest::get().uri("/api/maint/office/legacy/unmatched").insert_header(bearer(1, "admin_user")).to_request();
    let body: Value = test::call_and_read_body_json(&app, req).await;
    assert!(body["rows"].as_array().unwrap().iter().all(|r| r["text"] != odd.as_str()));

    // The garage app's feed carries the recognised lines.
    let req = test::TestRequest::get().uri("/api/maint/legacy-events").insert_header(bearer(1, "admin_user")).to_request();
    let feed: Value = test::call_and_read_body_json(&app, req).await;
    let ids: Vec<i64> = feed.as_array().unwrap().iter().filter(|r| r["vehicle_id"] == car).map(|r| r["id"].as_i64().unwrap()).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&base) && ids.contains(&(base + 3)));

    // An invoice deleted in Falcon stops counting.
    sqlx::query(&format!("UPDATE {schema}.service_invoices SET deleted_at = now() WHERE id = $1")).bind(base).execute(&pool).await.unwrap();
    let s = legacy_import::import(&pool, &falcon).await.unwrap();
    assert!(s.removed >= 2, "{s:?}");
    let due = due_row(&pool, car, "oil_change").await;
    assert!(due["last_km"].is_null(), "the remaining line has no reading: {due}");
    assert!(!due["last_at"].is_null());

    falcon.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE")).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM legacy_service_lines WHERE vehicle_id = $1").bind(car).execute(&pool).await.unwrap();
}

async fn seed_plan_extra(pool: &PgPool, vehicle: i32, category: &str) {
    sqlx::query(
        "INSERT INTO service_plans (class_id, category_id, trigger_type, interval_km, lead_warn_km, lead_warn_days, created_by_user_id, updated_by_user_id) \
         VALUES ($1, $2, 'mileage', 10000, 1000, 14, 7, 7)",
    )
    .bind(format!("cls-{vehicle}"))
    .bind(category)
    .execute(pool)
    .await
    .unwrap();
}

#[actix_web::test]
async fn the_office_reads_work_orders_and_a_trucks_maintenance() {
    let Some((pool, _)) = test_pool().await else { return };
    let car = fresh_vehicle();
    let plate = seed_vehicle(&pool, car).await;
    seed_plan(&pool, car, "brakes").await;
    let (open, closed) = (Uuid::new_v4(), Uuid::new_v4());
    sqlx::query(
        "INSERT INTO work_orders (id, vehicle_id, status, title, odometer_at_open, opened_at, closed_at, created_by_user_id, updated_by_user_id) VALUES \
         ($1, $3, 'open', 'شغل مفتوح', 120000, now() - interval '1 hour', NULL, 7, 7), \
         ($2, $3, 'closed', NULL, 110000, now() - interval '30 days', now() - interval '29 days', 7, 7)",
    )
    .bind(open)
    .bind(closed)
    .bind(car)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO work_order_tasks (work_order_id, kind, category_id, description_ar, is_done, done_at, sort_order, created_by_user_id, updated_by_user_id) VALUES \
         ($1, 'free', NULL, 'لحام شكمان', false, NULL, 0, 7, 7), \
         ($1, 'free', 'brakes', NULL, true, now(), 1, 7, 7), \
         ($2, 'free', 'brakes', 'تيل خلفي', true, now() - interval '29 days', 0, 7, 7)",
    )
    .bind(open)
    .bind(closed)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO oil_changes (work_order_id, vehicle_id, oil_type, liters, odometer, created_by_user_id, updated_by_user_id) VALUES ($1, $2, 'Shell 15W40', 35, 120000, 7, 7)")
        .bind(open)
        .bind(car)
        .execute(&pool)
        .await
        .unwrap();

    let app = app!(pool);
    let get = |uri: String, user_type: &'static str| test::TestRequest::get().uri(&uri).insert_header(bearer(1, user_type)).to_request();

    // A driver's token (same secret) reads nothing.
    let r = test::call_service(&app, get(format!("/api/maint/office/work-orders?vehicle_id={car}"), "driver")).await;
    assert!(r.status().is_client_error());

    let list: Value = test::call_and_read_body_json(&app, get(format!("/api/maint/office/work-orders?vehicle_id={car}"), "admin_user")).await;
    assert_eq!(list["total"], 2);
    let rows = list["rows"].as_array().unwrap();
    assert_eq!(rows[0]["id"], open.to_string(), "open work first");
    assert_eq!((rows[0]["task_count"].as_i64(), rows[0]["done_count"].as_i64()), (Some(2), Some(1)));
    assert_eq!(rows[0]["plate"], plate.as_str());
    assert_eq!(rows[0]["oil_liters"], 35.0);
    let summary = rows[0]["summary"].as_str().unwrap();
    assert!(summary.starts_with("لحام شكمان، "), "a task with no text is named by its type: {summary}");
    assert!(summary.len() > "لحام شكمان، ".len());

    // Filters: status, plate in any spelling, Cairo day range.
    let q = |qs: &str| format!("/api/maint/office/work-orders?vehicle_id={car}&{qs}");
    let only: Value = test::call_and_read_body_json(&app, get(q("status=closed"), "admin_user")).await;
    assert_eq!((only["total"].as_i64(), only["rows"][0]["id"].as_str()), (Some(1), Some(closed.to_string().as_str())));
    let by_plate: Value = test::call_and_read_body_json(&app, get(format!("/api/maint/office/work-orders?q=off{car}"), "admin_user")).await;
    assert_eq!(by_plate["total"], 2);
    let today: String = sqlx::query_scalar("SELECT to_char(now() AT TIME ZONE 'Africa/Cairo', 'YYYY-MM-DD')").fetch_one(&pool).await.unwrap();
    let recent: Value = test::call_and_read_body_json(&app, get(q(&format!("from={today}&to={today}")), "admin_user")).await;
    // (An hour ago may be yesterday in Cairo just after midnight.)
    assert!(recent["total"].as_i64().unwrap() <= 1);
    assert_eq!(test::call_service(&app, get(q("from=yesterday"), "admin_user")).await.status(), 400);

    let one: Value = test::call_and_read_body_json(&app, get(format!("/api/maint/office/work-orders/{open}"), "admin_user")).await;
    assert_eq!(one["work_order"]["title"], "شغل مفتوح");
    assert_eq!(one["tasks"].as_array().unwrap().len(), 2);
    assert!(!one["tasks"][1]["category_ar"].is_null());
    assert_eq!(one["oil_changes"][0]["oil_type"], "Shell 15W40");
    assert_eq!(test::call_service(&app, get(format!("/api/maint/office/work-orders/{}", Uuid::new_v4()), "admin_user")).await.status(), 404);

    let m: Value = test::call_and_read_body_json(&app, get(format!("/api/maint/office/vehicles/{car}/maintenance"), "admin_user")).await;
    assert_eq!(m["open_work_orders"].as_array().unwrap().len(), 1);
    assert_eq!(m["recent_work_orders"].as_array().unwrap().len(), 1);
    assert_eq!(m["class"]["id"], format!("cls-{car}"));
    assert_eq!(m["due"][0]["category_id"], "brakes");
    assert_eq!(m["due"][0]["last_km"], 120000);
    assert!(m["tires"].as_array().unwrap().is_empty(), "no wheel layout yet");

    // A vehicle maintenance has never seen: empty, not an error.
    let none: Value = test::call_and_read_body_json(&app, get(format!("/api/maint/office/vehicles/{}/maintenance", fresh_vehicle()), "admin_user")).await;
    assert!(none["odometer"].is_null() || none["odometer"]["km"].is_null());
    assert!(none["due"].as_array().unwrap().is_empty());

    let tires: Value = test::call_and_read_body_json(&app, get("/api/maint/office/tires?limit=5".into(), "admin_user")).await;
    assert!(tires["rows"].is_array() && tires["status_counts"].is_object());
}

#[actix_web::test]
async fn a_stock_count_brings_the_book_to_the_shelf_and_names_who_did_it() {
    let Some((pool, _)) = test_pool().await else { return };
    let app = app!(pool);
    let brand = format!("Count{}", &Uuid::new_v4().simple().to_string()[..8]);
    let post = |uri: &str, perm: i32, body: Value| {
        test::TestRequest::post().uri(&format!("/api/maint{uri}")).insert_header(bearer(perm, "admin_user")).set_json(body).to_request()
    };
    maint_rust::services::legacy_import::remember_user(&pool, 7, "Mona", 4).await.unwrap();

    // Ten arrive; the shelf has eight.
    let r = test::call_service(&app, post("/stock/tires/credit", 4, json!({"brand": brand, "model": "M", "size": "S", "qty": 10}))).await;
    assert!(r.status().is_success());
    let count = |counted: i32, ref_id: Uuid| post("/stock/tires/count", 4, json!({"brand": format!(" {} ", brand.to_lowercase()), "model": "m", "size": "s", "counted": counted, "ref_id": ref_id}));
    assert_eq!(test::call_service(&app, post("/stock/tires/count", 3, json!({"brand": brand, "counted": 1}))).await.status(), 403);
    let ref1 = Uuid::new_v4();
    let v: Value = test::call_and_read_body_json(&app, count(8, ref1)).await;
    assert_eq!((v["before"].as_i64(), v["on_hand_qty"].as_i64(), v["change"].as_i64()), (Some(10), Some(8), Some(-2)), "{v}");
    assert_eq!(v["brand"], brand.as_str(), "found under any spelling, answered in the catalog's");
    // The same request again (a retry) and the same figure again change nothing.
    let v: Value = test::call_and_read_body_json(&app, count(8, ref1)).await;
    assert_eq!(v["change"], 0);
    let v: Value = test::call_and_read_body_json(&app, count(8, Uuid::new_v4())).await;
    assert_eq!(v["change"], 0);
    // More on the shelf than in the book.
    let v: Value = test::call_and_read_body_json(&app, count(11, Uuid::new_v4())).await;
    assert_eq!((v["on_hand_qty"].as_i64(), v["change"].as_i64()), (Some(11), Some(3)));
    assert_eq!(test::call_service(&app, count(-1, Uuid::new_v4())).await.status(), 400);

    let moves: Vec<(String, i32, bool, Option<String>)> = sqlx::query_as(
        "SELECT m.kind::text, m.qty, m.is_count, u.name FROM stock_movements m LEFT JOIN users_cache u ON u.id = m.user_id \
          WHERE m.brand = $1 ORDER BY m.created_at, m.id",
    )
    .bind(&brand)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        moves,
        vec![
            ("tire_credit".into(), 10, false, Some("Mona".into())),
            ("tire_debit".into(), 2, true, Some("Mona".into())),
            ("tire_credit".into(), 3, true, Some("Mona".into())),
        ]
    );

    // Oil, with decimals.
    let oil = format!("Oil{}", &brand[5..]);
    test::call_service(&app, post("/stock/oil/credit", 4, json!({"oil_type": oil, "liters": 200.0}))).await;
    let v: Value = test::call_and_read_body_json(&app, post("/stock/oil/count", 4, json!({"oil_type": oil, "counted_liters": 187.5}))).await;
    assert_eq!((v["before"].as_f64(), v["liters_on_hand"].as_f64(), v["change"].as_f64()), (Some(200.0), Some(187.5), Some(-12.5)), "{v}");
}

#[actix_web::test]
async fn work_orders_say_who_opened_them_and_how_many_of_each_status() {
    let Some((pool, _)) = test_pool().await else { return };
    let car = fresh_vehicle();
    seed_vehicle(&pool, car).await;
    let who = 2_000_000 + i64::from(car);
    let wo = Uuid::new_v4();
    // The id is new to the mirror: the write goes through and gets a placeholder.
    sqlx::query("INSERT INTO work_orders (id, vehicle_id, status, created_by_user_id, updated_by_user_id) VALUES ($1, $2, 'open', $3, $3)")
        .bind(wo)
        .bind(car)
        .bind(who)
        .execute(&pool)
        .await
        .unwrap();
    let job: Uuid = sqlx::query_scalar("INSERT INTO standard_jobs (name_ar, name_en, norm_key) VALUES ($1, 'Weld exhaust', maint_norm($1)) RETURNING id")
        .bind(format!("لحام شكمان {car}"))
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO work_order_tasks (work_order_id, kind, description_ar, standard_job_id, created_by_user_id, updated_by_user_id) VALUES ($1, 'free', $2, $3, $4, $4)")
        .bind(wo)
        .bind(format!("لحام شكمان {car}"))
        .bind(job)
        .bind(who)
        .execute(&pool)
        .await
        .unwrap();
    let app = app!(pool);
    let get = |uri: String| test::TestRequest::get().uri(&uri).insert_header(bearer(1, "admin_user")).to_request();
    let list: Value = test::call_and_read_body_json(&app, get(format!("/api/maint/office/work-orders?vehicle_id={car}&status=closed"))).await;
    assert_eq!(list["total"], 0);
    assert_eq!(list["status_counts"], json!({"open": 1}), "counts ignore the status filter itself");
    let list: Value = test::call_and_read_body_json(&app, get(format!("/api/maint/office/work-orders?vehicle_id={car}"))).await;
    assert!(list["rows"][0]["opened_by"].is_null(), "not known yet");
    assert_eq!(list["rows"][0]["summary_en"], "Weld exhaust");

    maint_rust::services::legacy_import::remember_user(&pool, who, " Hany ", 1).await.unwrap();
    let one: Value = test::call_and_read_body_json(&app, get(format!("/api/maint/office/work-orders/{wo}"))).await;
    assert_eq!(one["work_order"]["opened_by"], "Hany");
    assert_eq!(one["tasks"][0]["description_en"], "Weld exhaust");
    // A later refresh with no name keeps the one we have.
    maint_rust::services::legacy_import::remember_user(&pool, who, "", 1).await.unwrap();
    let one: Value = test::call_and_read_body_json(&app, get(format!("/api/maint/office/work-orders/{wo}"))).await;
    assert_eq!(one["work_order"]["opened_by"], "Hany");
}
