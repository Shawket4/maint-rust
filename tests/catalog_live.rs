//! Live-Postgres tests for the catalogs (migration 0024): typed text links to
//! a catalog row, new products are learned, duplicates merge, and vehicles are
//! real rows.
//!
//! Needs MAINT_TEST_DATABASE_URL; without it every test returns early.

use maint_rust::services::sync_engine::{apply_push_batch, EntityType, PushBody, PushOperation, PushResultStatus, SyncOperation};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("MAINT_TEST_DATABASE_URL") else {
        assert!(std::env::var("MAINT_REQUIRE_LIVE_TESTS").is_err(), "MAINT_REQUIRE_LIVE_TESTS is set but MAINT_TEST_DATABASE_URL is not");
        return None;
    };
    let pool = PgPoolOptions::new().max_connections(4).connect(&url).await.expect("connect");
    sqlx::migrate!("./migrations").run(&pool).await.expect("migrations");
    Some(pool)
}

fn ins(entity: EntityType, id: Uuid, payload: Value) -> PushOperation {
    PushOperation { entity_type: entity, entity_id: id.to_string(), operation: SyncOperation::Insert, payload, sync_version: 0, op_id: Some(Uuid::new_v4()), group_id: None }
}
async fn push(pool: &PgPool, ops: Vec<PushOperation>) -> Vec<PushResultStatus> {
    apply_push_batch(pool, &PushBody { operations: ops }, 7).await.expect("push").0.results
}
fn tag() -> String {
    Uuid::new_v4().simple().to_string()[..8].to_uppercase()
}
async fn tire_row(pool: &PgPool, id: Uuid) -> (String, Option<String>, Option<String>, Option<Uuid>, i64) {
    sqlx::query_as("SELECT brand, model, size, tire_product_id, sync_version FROM tires WHERE id = $1").bind(id).fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn two_spellings_of_one_tire_are_one_product() {
    let Some(pool) = test_pool().await else { return };
    let b = format!("Brand{}", tag());
    let (t1, t2, t3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let r = push(&pool, vec![
        ins(EntityType::Tires, t1, json!({"brand": b, "model": "FR01", "size": "315/80R22.5", "origin": "legacy"})),
        ins(EntityType::Tires, t2, json!({"brand": format!("  {}  ", b.to_lowercase()), "model": "fr-01", "size": "315/80 R 22.5", "origin": "legacy"})),
        ins(EntityType::Tires, t3, json!({"brand": b, "model": "FR01", "size": "385/65R22.5", "origin": "legacy"})),
    ]).await;
    assert!(r.iter().all(PushResultStatus::is_applied), "{r:?}");
    let (a, c, other) = (tire_row(&pool, t1).await, tire_row(&pool, t2).await, tire_row(&pool, t3).await);
    assert!(a.3.is_some());
    assert_eq!(a.3, c.3, "case, spaces and punctuation do not make a new product");
    assert_eq!((c.0.as_str(), c.1.as_deref(), c.2.as_deref()), (b.as_str(), Some("FR01"), Some("315/80R22.5")), "the second tire takes the product's spelling");
    assert_ne!(a.3, other.3, "a different size is a different product");
}

#[tokio::test]
async fn merging_a_misspelling_moves_tires_stock_and_future_arrivals() {
    let Some(pool) = test_pool().await else { return };
    let good = format!("Pirelli{}", tag());
    let bad = format!("Pireli{}", &good[7..]);
    let (t_good, t_bad) = (Uuid::new_v4(), Uuid::new_v4());
    push(&pool, vec![
        ins(EntityType::Tires, t_good, json!({"brand": good, "model": "M", "size": "S", "origin": "shipment"})),
        ins(EntityType::Tires, t_bad, json!({"brand": bad, "model": "M", "size": "S", "origin": "shipment"})),
    ]).await;
    let (good_id, bad_id) = (tire_row(&pool, t_good).await.3.unwrap(), tire_row(&pool, t_bad).await.3.unwrap());
    assert_ne!(good_id, bad_id);
    let lines: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tire_stock_cache WHERE tire_product_id IN ($1, $2)").bind(good_id).bind(bad_id).fetch_one(&pool).await.unwrap();
    assert_eq!(lines, 2, "the typo made a second stock line");

    // What the merge endpoint runs.
    let before = tire_row(&pool, t_bad).await.4;
    sqlx::query("UPDATE tire_products SET merged_into = $2 WHERE id = $1").bind(bad_id).bind(good_id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE tires SET brand = $2, sync_version = sync_version + 1 WHERE tire_product_id = $1").bind(bad_id).bind(&good).execute(&pool).await.unwrap();
    let moved = tire_row(&pool, t_bad).await;
    assert_eq!((moved.0.as_str(), moved.3), (good.as_str(), Some(good_id)));
    assert_eq!(moved.4, before + 1, "re-announced to other devices");

    // The typo arriving again (an older app, Falcon's ledger) lands on the survivor.
    let t_again = Uuid::new_v4();
    push(&pool, vec![ins(EntityType::Tires, t_again, json!({"brand": bad, "model": "M", "size": "S", "origin": "legacy"}))]).await;
    let again = tire_row(&pool, t_again).await;
    assert_eq!((again.0.as_str(), again.3), (good.as_str(), Some(good_id)), "old spelling → surviving product, rewritten");
    let (resolved,): (Uuid,) = sqlx::query_as("SELECT maint_tire_product($1, 'M', 'S', NULL)").bind(&bad).fetch_one(&pool).await.unwrap();
    assert_eq!(resolved, good_id);
}

#[tokio::test]
async fn oil_types_and_stock_movements_link_to_products() {
    let Some(pool) = test_pool().await else { return };
    let oil = format!("15W-40 {}", tag());
    let vid = 4_000_000 + (Uuid::new_v4().as_u128() % 1_000_000) as i64;
    let (o1, o2) = (Uuid::new_v4(), Uuid::new_v4());
    let r = push(&pool, vec![
        ins(EntityType::OilChanges, o1, json!({"vehicle_id": vid, "oil_type": oil, "liters": 35})),
        ins(EntityType::OilChanges, o2, json!({"vehicle_id": vid, "oil_type": oil.to_lowercase().replace('-', " "), "liters": 35})),
    ]).await;
    assert!(r.iter().all(PushResultStatus::is_applied), "{r:?}");
    let rows: Vec<(String, Option<Uuid>)> = sqlx::query_as("SELECT oil_type, oil_product_id FROM oil_changes WHERE id IN ($1, $2)").bind(o1).bind(o2).fetch_all(&pool).await.unwrap();
    assert!(rows[0].1.is_some() && rows[0].1 == rows[1].1);
    assert_eq!(rows[0].0, rows[1].0, "one spelling");
    let (movements, lines, liters): (i64, i64, f64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM stock_movements WHERE oil_product_id = $1), \
                (SELECT COUNT(*) FROM oil_stock_cache WHERE oil_product_id = $1), \
                (SELECT liters_on_hand::float8 FROM oil_stock_cache WHERE oil_product_id = $1)",
    )
    .bind(rows[0].1)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((movements, lines, liters), (2, 1, -70.0), "both debits on one stock line");

    // The vehicle nobody had mirrored yet became a real (placeholder) row.
    let plate: String = sqlx::query_scalar("SELECT car_no_plate FROM vehicles_cache WHERE id = $1").bind(vid as i32).fetch_one(&pool).await.unwrap();
    assert_eq!(plate, format!("#{vid}"));
}

#[tokio::test]
async fn a_task_that_is_a_standard_job_is_linked_and_a_sentence_is_not() {
    let Some(pool) = test_pool().await else { return };
    let wo = Uuid::new_v4();
    let (t1, t2) = (Uuid::new_v4(), Uuid::new_v4());
    push(&pool, vec![
        ins(EntityType::WorkOrders, wo, json!({"vehicle_id": 4_100_000, "status": "open"})),
        // A seeded job of the oil change, typed with extra spaces.
        ins(EntityType::WorkOrderTasks, t1, json!({"work_order_id": wo, "kind": "free", "description_ar": " تغيير  زيت  موتور "})),
        ins(EntityType::WorkOrderTasks, t2, json!({"work_order_id": wo, "kind": "free", "description_ar": "السواق بيقول فيه صوت في الفرامل"})),
    ]).await;
    let job: Option<Uuid> = sqlx::query_scalar("SELECT standard_job_id FROM work_order_tasks WHERE id = $1").bind(t1).fetch_one(&pool).await.unwrap();
    let cat: Option<String> = sqlx::query_scalar("SELECT category_id FROM standard_jobs WHERE id = $1").bind(job).fetch_one(&pool).await.unwrap();
    assert_eq!(cat.as_deref(), Some("oil_change"));
    let none: Option<Uuid> = sqlx::query_scalar("SELECT standard_job_id FROM work_order_tasks WHERE id = $1").bind(t2).fetch_one(&pool).await.unwrap();
    assert!(none.is_none(), "free text stays free text");
}

#[tokio::test]
async fn a_type_holds_its_jobs_and_typed_types_and_jobs_are_kept() {
    let Some(pool) = test_pool().await else { return };
    // The old one-per-type job left the list; the type's own jobs are there.
    let jobs: Vec<String> = sqlx::query_scalar(
        "SELECT name_ar FROM standard_jobs WHERE category_id = 'greasing' AND is_active ORDER BY name_ar",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(jobs.contains(&"تشحيم صنيه".to_string()) && jobs.contains(&"تشحيم بلي (الاجناب)".to_string()), "{jobs:?}");
    assert!(!jobs.contains(&"تشحيم".to_string()));

    let wo = Uuid::new_v4();
    let t = tag();
    let (new_type, typed_job, same_name, only_type) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let cid = format!("c-{t}");
    let r = push(&pool, vec![
        ins(EntityType::WorkOrders, wo, json!({"vehicle_id": 4_100_001, "status": "open"})),
        // A type nobody has: created from the task that names it.
        ins(EntityType::WorkOrderTasks, new_type, json!({"work_order_id": wo, "kind": "free", "category_id": cid, "category_name_ar": format!("كهربا {t}"), "description_ar": format!("تغيير لمبة {t}")})),
        // A job typed under an existing type joins its jobs.
        ins(EntityType::WorkOrderTasks, typed_job, json!({"work_order_id": wo, "kind": "free", "category_id": "greasing", "description_ar": format!("تشحيم كرنك {t}")})),
        // A typed type an existing type already has is that type.
        ins(EntityType::WorkOrderTasks, same_name, json!({"work_order_id": wo, "kind": "free", "category_id": format!("c-x{t}"), "category_name_ar": " تشحيم ", "description_ar": "تشحيم صنيه"})),
        // Only a type: its name is not a job of its own.
        ins(EntityType::WorkOrderTasks, only_type, json!({"work_order_id": wo, "kind": "free", "category_id": "battery", "description_ar": "بطارية"})),
    ]).await;
    for x in &r {
        assert!(matches!(x, PushResultStatus::Applied { .. }), "{x:?}");
    }
    let name: String = sqlx::query_scalar("SELECT name_ar FROM maintenance_categories WHERE id = $1").bind(&cid).fetch_one(&pool).await.unwrap();
    assert_eq!(name, format!("كهربا {t}"));
    let job_cat: Option<String> = sqlx::query_scalar("SELECT category_id FROM standard_jobs WHERE norm_key = maint_norm($1)")
        .bind(format!("تغيير لمبة {t}")).fetch_one(&pool).await.unwrap();
    assert_eq!(job_cat.as_deref(), Some(cid.as_str()));
    let job_cat: Option<String> = sqlx::query_scalar("SELECT category_id FROM standard_jobs WHERE norm_key = maint_norm($1)")
        .bind(format!("تشحيم كرنك {t}")).fetch_one(&pool).await.unwrap();
    assert_eq!(job_cat.as_deref(), Some("greasing"));
    let cat: Option<String> = sqlx::query_scalar("SELECT category_id FROM work_order_tasks WHERE id = $1").bind(same_name).fetch_one(&pool).await.unwrap();
    assert_eq!(cat.as_deref(), Some("greasing"));
    let active: bool = sqlx::query_scalar("SELECT is_active FROM standard_jobs WHERE norm_key = maint_norm('بطارية')").fetch_one(&pool).await.unwrap();
    assert!(!active, "the type's own name stays off the list");
}

#[tokio::test]
async fn norm_ignores_what_people_vary() {
    let Some(pool) = test_pool().await else { return };
    let same = |a: &'static str, b: &'static str| {
        let pool = pool.clone();
        async move {
            let (x, y): (String, String) = sqlx::query_as("SELECT maint_norm($1), maint_norm($2)").bind(a).bind(b).fetch_one(&pool).await.unwrap();
            assert_eq!(x, y, "{a:?} vs {b:?}");
            assert!(!x.is_empty());
        }
    };
    same("315/80R22.5", " 315 / 80 r 22.5 ").await;
    same("أحمد", "احمد").await;
    same("٣١٥", "315").await;
    same("Bridge-Stone", "bridgestone").await;
}
