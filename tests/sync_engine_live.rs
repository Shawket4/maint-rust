//! Live-Postgres tests for the sync engine's field-critical properties:
//!
//!  1. Push batches survive per-op failures (SAVEPOINT isolation): one bad op
//!     yields one per-op error, and every other op still applies.
//!  2. The mount invariant (partial unique on active position) errors per-op,
//!     never rolls back the batch.
//!  3. Pull paging never skips rows that share an updated_at (composite cursor).
//!  4. Derived side effects: an applied oil change debits the mirrored oil
//!     ledger exactly once (idempotent on ref_id) and the server owns the
//!     34–36 L flag; a mount from shipment stock debits the tire ledger and
//!     marks the tire mounted; a dismount derives status from the reason and
//!     accrues km.
//!
//! Needs a throwaway database:  MAINT_TEST_DATABASE_URL=postgres://… cargo test
//! Skips (passes vacuously) when the variable is unset, so plain `cargo test`
//! stays green without infrastructure.

use maint_rust::services::sync_engine::{
    apply_push_batch, pull_rows, EntityType, PushBody, PushOperation, PushResultStatus,
    SyncOperation,
};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

async fn test_pool() -> Option<PgPool> {
    let url = std::env::var("MAINT_TEST_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect to MAINT_TEST_DATABASE_URL");
    sqlx::migrate!("./migrations").run(&pool).await.expect("migrations");
    // A vehicle for FKs/joins. raw_payload is NOT NULL in the cache mirror.
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

fn insert_op(entity_type: EntityType, id: &str, payload: Value) -> PushOperation {
    PushOperation {
        entity_type,
        entity_id: id.to_string(),
        operation: SyncOperation::Insert,
        payload,
        sync_version: 0,
    }
}

fn wo_op(id: Uuid, status: &str) -> PushOperation {
    insert_op(
        EntityType::WorkOrders,
        &id.to_string(),
        json!({
            "id": id.to_string(),
            "vehicle_id": 901,
            "status": status,
            "title": "test",
            "opened_at": chrono::Utc::now().to_rfc3339(),
        }),
    )
}

/// A unique vehicle id per run — chassis_layouts.vehicle_id is UNIQUE, so a
/// fixed id would break the second `cargo test` against the same database.
fn fresh_vehicle_id() -> i64 {
    10_000 + (Uuid::new_v4().as_u128() % 1_000_000) as i64
}

/// Direct-SQL chassis fixture: layout + one axle + one road position (+ optionally a spare).
async fn seed_chassis(pool: &PgPool, vehicle_id: i64) -> (Uuid, Uuid) {
    let layout = Uuid::new_v4();
    let axle = Uuid::new_v4();
    let pos = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO chassis_layouts (id, vehicle_id, created_by_user_id, updated_by_user_id)
         VALUES ($1, $2, 1, 1)",
    )
    .bind(layout)
    .bind(vehicle_id)
    .execute(pool)
    .await
    .expect("layout");
    sqlx::query(
        "INSERT INTO chassis_axles (id, layout_id, section, section_index, axle_type, is_steering, created_by_user_id, updated_by_user_id)
         VALUES ($1, $2, 'tractor', 1, 'single', true, 1, 1)",
    )
    .bind(axle)
    .bind(layout)
    .execute(pool)
    .await
    .expect("axle");
    sqlx::query(
        "INSERT INTO chassis_positions (id, layout_id, axle_id, side, depth, is_spare, position_code, created_by_user_id, updated_by_user_id)
         VALUES ($1, $2, $3, 'left', 'single', false, $4, 1, 1)",
    )
    .bind(pos)
    .bind(layout)
    .bind(axle)
    .bind(format!("T-{}", &pos.to_string()[..8]))
    .execute(pool)
    .await
    .expect("position");
    (layout, pos)
}

#[tokio::test]
async fn push_batch_survives_per_op_failure_mid_batch() {
    let Some(pool) = test_pool().await else { return };

    let a = Uuid::new_v4();
    let bad = Uuid::new_v4();
    let c = Uuid::new_v4();
    let body = PushBody {
        operations: vec![
            wo_op(a, "open"),
            wo_op(bad, "bogus_status"), // enum cast failure
            wo_op(c, "open"),
        ],
    };

    let (resp, _) = apply_push_batch(&pool, &body, 1).await.expect("batch must not 500");
    assert_eq!(resp.results.len(), 3);
    assert!(matches!(resp.results[0], PushResultStatus::Applied { .. }), "op A applies");
    assert!(matches!(resp.results[1], PushResultStatus::Error { .. }), "bad op errors");
    assert!(
        matches!(resp.results[2], PushResultStatus::Applied { .. }),
        "op C must still apply — before the savepoint fix the aborted tx killed it"
    );

    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM work_orders WHERE id = ANY($1)")
        .bind(vec![a, c])
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 2);
}

#[tokio::test]
async fn double_mount_on_position_errors_per_op_and_first_survives() {
    let Some(pool) = test_pool().await else { return };
    let vid = fresh_vehicle_id();
    let (_layout, pos) = seed_chassis(&pool, vid).await;

    let mk_tire = |id: Uuid| {
        insert_op(
            EntityType::Tires,
            &id.to_string(),
            json!({
                "id": id.to_string(),
                "brand": "TestBrand",
                "status": "in_stock_used",
                "origin": "legacy",
            }),
        )
    };
    let mk_mount = |id: Uuid, tire: Uuid| {
        insert_op(
            EntityType::TireAssignments,
            &id.to_string(),
            json!({
                "id": id.to_string(),
                "tire_id": tire.to_string(),
                "position_id": pos.to_string(),
                "vehicle_id": vid,
                "mounted_at": chrono::Utc::now().to_rfc3339(),
                "mounted_odometer": 100_000,
                "mount_reason": "new",
            }),
        )
    };

    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let a1 = Uuid::new_v4();
    let a2 = Uuid::new_v4();
    let body = PushBody {
        operations: vec![mk_tire(t1), mk_tire(t2), mk_mount(a1, t1), mk_mount(a2, t2)],
    };

    let (resp, _) = apply_push_batch(&pool, &body, 1)
        .await
        .expect("unique violation must not roll back the batch");
    assert!(matches!(resp.results[2], PushResultStatus::Applied { .. }), "first mount lands");
    assert!(
        matches!(resp.results[3], PushResultStatus::Error { .. }),
        "second mount on an occupied position is a per-op error"
    );

    // Side effect: the mounted tire's status was derived server-side.
    let status: String = sqlx::query_scalar("SELECT status::text FROM tires WHERE id = $1")
        .bind(t1)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "mounted");
    let status2: String = sqlx::query_scalar("SELECT status::text FROM tires WHERE id = $1")
        .bind(t2)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status2, "in_stock_used", "failed mount must not mark its tire mounted");
}

#[tokio::test]
async fn pull_pages_through_identical_timestamps_without_skipping() {
    let Some(pool) = test_pool().await else { return };

    let wo = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO work_orders (id, vehicle_id, status, created_by_user_id, updated_by_user_id)
         VALUES ($1, 901, 'open', 1, 1)",
    )
    .bind(wo)
    .execute(&pool)
    .await
    .unwrap();

    // Five tasks in ONE transaction ⇒ byte-identical updated_at on all five.
    let ids: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
    let mut tx = pool.begin().await.unwrap();
    for id in &ids {
        sqlx::query(
            "INSERT INTO work_order_tasks
               (id, work_order_id, kind, description_ar, created_by_user_id, updated_by_user_id)
             VALUES ($1, $2, 'free', 'x', 1, 1)",
        )
        .bind(id)
        .bind(wo)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    // Page through with limit 2 using the composite cursor.
    let epoch = chrono::TimeZone::timestamp_opt(&chrono::Utc, 0, 0).unwrap();
    let mut since = epoch;
    let mut after_id: Option<String> = None;
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..40 {
        let page = pull_rows(&pool, EntityType::WorkOrderTasks, since, after_id.as_deref(), 2)
            .await
            .unwrap();
        for r in &page.rows {
            seen.push(r.get("id").and_then(|v| v.as_str()).unwrap().to_string());
        }
        if !page.has_more {
            break;
        }
        since = page.next_cursor.unwrap();
        after_id = page.next_cursor_id.clone();
    }

    for id in &ids {
        assert!(
            seen.contains(&id.to_string()),
            "row {id} was skipped by the pager — the timestamp-only cursor bug"
        );
    }
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "pager must not duplicate rows either");
}

#[tokio::test]
async fn oil_change_debits_ledger_once_and_server_owns_the_flag() {
    let Some(pool) = test_pool().await else { return };

    let oil_type = format!("TEST-{}", &Uuid::new_v4().to_string()[..8]);
    sqlx::query(
        "INSERT INTO oil_stock_cache (oil_type, liters_on_hand) VALUES ($1, 100)
         ON CONFLICT (oil_type) DO UPDATE SET liters_on_hand = 100",
    )
    .bind(&oil_type)
    .execute(&pool)
    .await
    .unwrap();

    let oc = Uuid::new_v4();
    let op = insert_op(
        EntityType::OilChanges,
        &oc.to_string(),
        json!({
            "id": oc.to_string(),
            "vehicle_id": 901,
            "oil_type": oil_type,
            "liters": 40.0,
            "odometer": 500_000,
            "flagged": false, // lies — server must overwrite
        }),
    );
    let body = PushBody { operations: vec![op] };
    let (resp, refs) = apply_push_batch(&pool, &body, 1).await.unwrap();
    assert!(matches!(resp.results[0], PushResultStatus::Applied { .. }));
    assert_eq!(refs, vec![oc], "debit ref surfaces for Falcon mirroring");

    let flagged: bool = sqlx::query_scalar("SELECT flagged FROM oil_changes WHERE id = $1")
        .bind(oc)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(flagged, "40 L is outside 34–36 — server recomputes the flag");

    let left: f64 = sqlx::query_scalar(
        "SELECT liters_on_hand::float8 FROM oil_stock_cache WHERE oil_type = $1",
    )
    .bind(
        sqlx::query_scalar::<_, String>("SELECT oil_type FROM oil_changes WHERE id = $1")
            .bind(oc)
            .fetch_one(&pool)
            .await
            .unwrap(),
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, 60.0, "100 − 40");

    // Lost-ack replay: same insert again (sync_version 0 vs server 1) must
    // no-op — no conflict, no second movement, no double debit.
    let op2 = insert_op(
        EntityType::OilChanges,
        &oc.to_string(),
        json!({ "id": oc.to_string(), "vehicle_id": 901, "liters": 40.0 }),
    );
    let (resp2, refs2) =
        apply_push_batch(&pool, &PushBody { operations: vec![op2] }, 1).await.unwrap();
    assert!(
        matches!(resp2.results[0], PushResultStatus::Applied { .. }),
        "replayed insert acks as no-op, not conflict"
    );
    assert!(refs2.is_empty());
    let movements: i64 =
        sqlx::query_scalar("SELECT count(*) FROM stock_movements WHERE ref_id = $1")
            .bind(oc)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(movements, 1, "exactly one movement despite the replay");
}

#[tokio::test]
async fn mount_from_shipment_debits_stock_and_dismount_derives_status_and_km() {
    let Some(pool) = test_pool().await else { return };
    let vid = fresh_vehicle_id();
    let (_layout, pos) = seed_chassis(&pool, vid).await;

    let brand = format!("B-{}", &Uuid::new_v4().to_string()[..8]);
    sqlx::query(
        "INSERT INTO tire_stock_cache (brand, model, size, on_hand_qty) VALUES ($1, 'M', 'S', 5)",
    )
    .bind(&brand)
    .execute(&pool)
    .await
    .unwrap();

    let tire = Uuid::new_v4();
    let asg = Uuid::new_v4();
    let body = PushBody {
        operations: vec![
            insert_op(
                EntityType::Tires,
                &tire.to_string(),
                json!({
                    "id": tire.to_string(),
                    "brand": brand,
                    "model": "M",
                    "size": "S",
                    "status": "mounted",
                    "origin": "shipment",
                    "dot_code": format!("DOT-{}", &tire.to_string()[..8]),
                }),
            ),
            insert_op(
                EntityType::TireAssignments,
                &asg.to_string(),
                json!({
                    "id": asg.to_string(),
                    "tire_id": tire.to_string(),
                    "position_id": pos.to_string(),
                    "vehicle_id": vid,
                    "mounted_at": chrono::Utc::now().to_rfc3339(),
                    "mounted_odometer": 200_000,
                    "mount_reason": "new",
                }),
            ),
        ],
    };
    let (resp, refs) = apply_push_batch(&pool, &body, 1).await.unwrap();
    assert!(matches!(resp.results[0], PushResultStatus::Applied { .. }));
    assert!(matches!(resp.results[1], PushResultStatus::Applied { .. }));
    assert_eq!(refs, vec![tire], "new-from-stock mount debits the tire ledger");

    let qty: i32 = sqlx::query_scalar(
        "SELECT on_hand_qty FROM tire_stock_cache WHERE brand = $1 AND model = 'M' AND size = 'S'",
    )
    .bind(
        sqlx::query_scalar::<_, String>("SELECT brand FROM tires WHERE id = $1")
            .bind(tire)
            .fetch_one(&pool)
            .await
            .unwrap(),
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(qty, 4, "5 − 1");

    // Dismount: worn → scrapped, km accrues (240k − 200k on a road position).
    let sv: i64 =
        sqlx::query_scalar("SELECT sync_version FROM tire_assignments WHERE id = $1")
            .bind(asg)
            .fetch_one(&pool)
            .await
            .unwrap();
    let dismount = PushOperation {
        entity_type: EntityType::TireAssignments,
        entity_id: asg.to_string(),
        operation: SyncOperation::Update,
        payload: json!({
            "dismounted_at": chrono::Utc::now().to_rfc3339(),
            "dismounted_odometer": 240_000,
            "dismount_reason": "worn",
        }),
        sync_version: sv,
    };
    let (resp2, _) =
        apply_push_batch(&pool, &PushBody { operations: vec![dismount] }, 1).await.unwrap();
    assert!(matches!(resp2.results[0], PushResultStatus::Applied { .. }));

    let (status, km): (String, i32) = sqlx::query_as(
        "SELECT status::text, total_km FROM tires WHERE id = $1",
    )
    .bind(tire)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(status, "scrapped", "worn → scrapped, derived server-side");
    assert_eq!(km, 40_000, "km accrued from the odometer delta");
}
