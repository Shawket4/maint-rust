//! Live-Postgres tests for the sync contract added by the 2026-11 audit:
//! exactly-once ops, atomic groups, the commit-ordered change feed, tire
//! status as a pure projection, stock that follows edits and deletes, and the
//! guards on deletes.
//!
//! Needs a throwaway database:  MAINT_TEST_DATABASE_URL=postgres://… cargo test
//! Without it every test returns early — CI sets it, so they run there.

use maint_rust::services::sync_engine::{
    apply_push_batch, pull_rows, EntityType, PushBody, PushOperation, PushResultStatus, SyncOperation,
};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("MAINT_TEST_DATABASE_URL") else {
        // CI sets MAINT_REQUIRE_LIVE_TESTS so a missing database fails loudly
        // instead of every test passing without running.
        assert!(
            std::env::var("MAINT_REQUIRE_LIVE_TESTS").is_err(),
            "MAINT_REQUIRE_LIVE_TESTS is set but MAINT_TEST_DATABASE_URL is not"
        );
        return None;
    };
    let pool = PgPoolOptions::new().max_connections(6).connect(&url).await.expect("connect");
    sqlx::migrate!("./migrations").run(&pool).await.expect("migrations");
    Some(pool)
}

fn vehicle_id() -> i64 {
    2_000_000 + (Uuid::new_v4().as_u128() % 100_000_000) as i64
}

fn op(entity: EntityType, id: impl ToString, operation: SyncOperation, payload: Value, sv: i64) -> PushOperation {
    PushOperation {
        entity_type: entity,
        entity_id: id.to_string(),
        operation,
        payload,
        sync_version: sv,
        op_id: Some(Uuid::new_v4()),
        group_id: None,
    }
}
fn ins(entity: EntityType, id: impl ToString, payload: Value) -> PushOperation {
    op(entity, id, SyncOperation::Insert, payload, 0)
}
fn upd(entity: EntityType, id: impl ToString, payload: Value, sv: i64) -> PushOperation {
    op(entity, id, SyncOperation::Update, payload, sv)
}
fn del(entity: EntityType, id: impl ToString, sv: i64) -> PushOperation {
    op(entity, id, SyncOperation::Delete, json!({}), sv)
}
fn legacy(mut o: PushOperation) -> PushOperation {
    o.op_id = None;
    o
}
fn grouped(mut ops: Vec<PushOperation>) -> Vec<PushOperation> {
    let g = Uuid::new_v4();
    for o in &mut ops {
        o.group_id = Some(g);
    }
    ops
}
fn again(o: &PushOperation) -> PushOperation {
    PushOperation {
        entity_type: o.entity_type,
        entity_id: o.entity_id.clone(),
        operation: o.operation,
        payload: o.payload.clone(),
        sync_version: o.sync_version,
        op_id: o.op_id,
        group_id: o.group_id,
    }
}

async fn push(pool: &PgPool, ops: Vec<PushOperation>) -> Vec<PushResultStatus> {
    apply_push_batch(pool, &PushBody { operations: ops }, 7).await.expect("push").0.results
}
fn sv(r: &PushResultStatus) -> i64 {
    match r {
        PushResultStatus::Applied { new_sync_version, .. } => *new_sync_version,
        other => panic!("expected applied, got {other:?}"),
    }
}
fn code(r: &PushResultStatus) -> &str {
    match r {
        PushResultStatus::Error { code, .. } => code,
        other => panic!("expected error, got {other:?}"),
    }
}

/// A vehicle with a layout and `n` road positions (one single axle each).
async fn chassis(pool: &PgPool, vid: i64, n: usize) -> (Uuid, Vec<Uuid>) {
    sqlx::query("INSERT INTO vehicles_cache (id, car_no_plate, raw_payload) VALUES ($1, 'T', '{}'::jsonb) ON CONFLICT DO NOTHING")
        .bind(vid as i32)
        .execute(pool)
        .await
        .unwrap();
    let layout = Uuid::new_v4();
    let mut ops = vec![ins(EntityType::ChassisLayouts, layout, json!({"vehicle_id": vid}))];
    let mut positions = Vec::new();
    for i in 0..n {
        let axle = Uuid::new_v4();
        let pos = Uuid::new_v4();
        ops.push(ins(
            EntityType::ChassisAxles,
            axle,
            json!({"layout_id": layout, "section": "tractor", "section_index": i + 1, "axle_type": "single", "is_steering": i == 0}),
        ));
        ops.push(ins(
            EntityType::ChassisPositions,
            pos,
            json!({"layout_id": layout, "axle_id": axle, "side": "left", "depth": "single", "is_spare": false,
                   "position_code": format!("H{}L", i + 1)}),
        ));
        positions.push(pos);
    }
    for r in push(pool, grouped(ops)).await {
        assert!(r.is_applied(), "{r:?}");
    }
    (layout, positions)
}

fn tire(id: Uuid, origin: &str, brand: &str) -> PushOperation {
    ins(EntityType::Tires, id, json!({"brand": brand, "model": "M", "size": "S", "origin": origin, "status": "mounted"}))
}
fn mount(id: Uuid, tire: Uuid, pos: Uuid, vid: i64, km: i64) -> PushOperation {
    ins(
        EntityType::TireAssignments,
        id,
        json!({"tire_id": tire, "position_id": pos, "vehicle_id": vid, "mounted_at": chrono::Utc::now().to_rfc3339(),
               "mounted_odometer": km, "mount_reason": "new"}),
    )
}
async fn tire_status(pool: &PgPool, id: Uuid) -> String {
    sqlx::query_scalar("SELECT status::text FROM tires WHERE id = $1").bind(id).fetch_one(pool).await.unwrap()
}
async fn row_sv(pool: &PgPool, table: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(&format!("SELECT sync_version FROM {table} WHERE id = $1")).bind(id).fetch_one(pool).await.unwrap()
}

// ───────────────────────── exactly-once ─────────────────────────────────────

#[tokio::test]
async fn a_replayed_op_returns_its_first_answer_and_changes_nothing() {
    let Some(pool) = test_pool().await else { return };
    let wo = Uuid::new_v4();
    let task = Uuid::new_v4();
    let vid = vehicle_id();
    let create = ins(EntityType::WorkOrders, wo, json!({"vehicle_id": vid, "status": "open"}));
    let add = ins(EntityType::WorkOrderTasks, task, json!({"work_order_id": wo, "kind": "free", "description_ar": "x"}));
    let r = push(&pool, vec![again(&create), again(&add)]).await;
    assert_eq!(sv(&r[1]), 1);

    // Tick the task; pretend the ack was lost and the client sends it again.
    let tick = upd(EntityType::WorkOrderTasks, task, json!({"is_done": true}), 1);
    let first = push(&pool, vec![again(&tick)]).await;
    assert_eq!(sv(&first[0]), 2);
    let replay = push(&pool, vec![again(&tick)]).await;
    assert_eq!(sv(&replay[0]), 2, "a redelivered update is acknowledged, not a conflict");

    // The original insert redelivered AFTER the edit must not undo the edit.
    let replay = push(&pool, vec![again(&add)]).await;
    assert_eq!(sv(&replay[0]), 1, "the stored answer of that op");
    let (done, version): (bool, i64) =
        sqlx::query_as("SELECT is_done, sync_version FROM work_order_tasks WHERE id = $1")
            .bind(task)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(done, "the tick survived the replayed insert");
    assert_eq!(version, 2);

    // A DIFFERENT op inserting an id that exists is a real conflict.
    let other = ins(EntityType::WorkOrderTasks, task, json!({"work_order_id": wo, "kind": "free"}));
    assert!(matches!(push(&pool, vec![other]).await[0], PushResultStatus::Conflict { .. }));
}

#[tokio::test]
async fn clients_without_op_ids_do_not_lose_folded_edits_or_get_false_conflicts() {
    let Some(pool) = test_pool().await else { return };
    let wo = Uuid::new_v4();
    let task = Uuid::new_v4();
    push(&pool, vec![ins(EntityType::WorkOrders, wo, json!({"vehicle_id": vehicle_id(), "status": "open"}))]).await;
    let first = legacy(ins(EntityType::WorkOrderTasks, task, json!({"work_order_id": wo, "kind": "free", "is_done": false})));
    assert_eq!(sv(&push(&pool, vec![first]).await[0]), 1);

    // v0.2.3 folds "tick done" into the still-pending insert and resends it.
    let folded = legacy(ins(EntityType::WorkOrderTasks, task, json!({"work_order_id": wo, "kind": "free", "is_done": true})));
    let r = push(&pool, vec![folded]).await;
    assert_eq!(sv(&r[0]), 2, "the folded edit is applied, not silently dropped");
    let done: bool = sqlx::query_scalar("SELECT is_done FROM work_order_tasks WHERE id = $1").bind(task).fetch_one(&pool).await.unwrap();
    assert!(done);

    // A lost ack on an update: the same update again is not a conflict.
    let close = || legacy(upd(EntityType::WorkOrders, wo, json!({"status": "closed", "closed_at": "2026-11-01T10:00:00Z"}), 1));
    assert_eq!(sv(&push(&pool, vec![close()]).await[0]), 2);
    assert_eq!(sv(&push(&pool, vec![close()]).await[0]), 2);
    // But a genuinely different stale write still conflicts.
    let stale = legacy(upd(EntityType::WorkOrders, wo, json!({"title": "other device"}), 1));
    assert!(matches!(push(&pool, vec![stale]).await[0], PushResultStatus::Conflict { .. }));
}

#[tokio::test]
async fn results_come_back_in_request_order_even_for_one_vehicle() {
    let Some(pool) = test_pool().await else { return };
    let vid = vehicle_id();
    // Class assignment and odometer override share an entity_id (the vehicle).
    let class = ins(EntityType::VehicleClassAssignments, vid, json!({"vehicle_id": vid, "class_id": "no-such-class"}));
    let odo = ins(EntityType::VehicleOdometerOverrides, vid, json!({"vehicle_id": vid, "odometer": 1000, "set_at": "2026-01-01T00:00:00Z"}));
    let (c, o) = (class.op_id, odo.op_id);
    let r = push(&pool, vec![class, odo]).await;
    assert_eq!(code(&r[0]), "missing_reference");
    assert!(r[1].is_applied());
    match (&r[0], &r[1]) {
        (PushResultStatus::Error { op_id: a, .. }, PushResultStatus::Applied { op_id: b, .. }) => {
            assert_eq!((*a, *b), (c, o), "each result names its own op");
        }
        other => panic!("{other:?}"),
    }
}

// ───────────────────────── atomic groups ────────────────────────────────────

#[tokio::test]
async fn a_refused_mount_takes_its_new_tire_and_its_stock_debit_with_it() {
    let Some(pool) = test_pool().await else { return };
    let vid = vehicle_id();
    let (_, pos) = chassis(&pool, vid, 1).await;
    let brand = format!("GRP-{}", &Uuid::new_v4().to_string()[..8]);

    let (t1, a1) = (Uuid::new_v4(), Uuid::new_v4());
    for r in push(&pool, grouped(vec![tire(t1, "shipment", &brand), mount(a1, t1, pos[0], vid, 100)])).await {
        assert!(r.is_applied(), "{r:?}");
    }
    assert_eq!(tire_status(&pool, t1).await, "mounted");

    // Another device mounts a second new tire on the same wheel.
    let (t2, a2) = (Uuid::new_v4(), Uuid::new_v4());
    let r = push(&pool, grouped(vec![tire(t2, "shipment", &brand), mount(a2, t2, pos[0], vid, 100)])).await;
    assert_eq!(code(&r[0]), "group_rolled_back");
    assert_eq!(code(&r[1]), "position_occupied");
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tires WHERE id = $1)").bind(t2).fetch_one(&pool).await.unwrap();
    assert!(!exists, "no orphan tire");
    let on_hand: i32 = sqlx::query_scalar("SELECT on_hand_qty FROM tire_stock_cache WHERE brand = $1").bind(&brand).fetch_one(&pool).await.unwrap();
    assert_eq!(on_hand, -1, "one tire consumed, not two");

    // A later, unrelated op in the same batch still applies.
    let wo = Uuid::new_v4();
    let mut ops = grouped(vec![tire(t2, "shipment", &brand), mount(a2, t2, pos[0], vid, 100)]);
    ops.push(ins(EntityType::WorkOrders, wo, json!({"vehicle_id": vid, "status": "open"})));
    let r = push(&pool, ops).await;
    assert!(r[2].is_applied());
}

// ───────────────────────── change feed ──────────────────────────────────────

#[tokio::test]
async fn a_transaction_that_started_earlier_but_commits_later_is_stamped_later() {
    let Some(pool) = test_pool().await else { return };
    let vid = vehicle_id();
    let slow_id = Uuid::new_v4();
    let fast_id = Uuid::new_v4();

    // The slow writer opens its transaction first (its now() is the oldest)…
    let mut slow = pool.begin().await.unwrap();
    let _: (chrono::DateTime<chrono::Utc>,) = sqlx::query_as("SELECT now()").fetch_one(&mut *slow).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    // …a fast push commits in between, and a device pulls past it…
    push(&pool, vec![ins(EntityType::WorkOrders, fast_id, json!({"vehicle_id": vid, "status": "open"}))]).await;
    let fast_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT updated_at FROM work_orders WHERE id = $1").bind(fast_id).fetch_one(&pool).await.unwrap();
    // …then the slow writer writes and commits.
    sqlx::query("INSERT INTO work_orders (id, vehicle_id, created_by_user_id, updated_by_user_id) VALUES ($1, $2, 1, 1)")
        .bind(slow_id)
        .bind(vid as i32)
        .execute(&mut *slow)
        .await
        .unwrap();
    slow.commit().await.unwrap();

    let page = pull_rows(&pool, EntityType::WorkOrders, fast_at, Some(&fast_id.to_string()), 50).await.unwrap();
    assert!(
        page.rows.iter().any(|r| r["id"] == json!(slow_id.to_string())),
        "the late commit is after the cursor, so the next pull sees it"
    );
}

#[tokio::test]
async fn a_malformed_cursor_id_does_not_break_pull() {
    let Some(pool) = test_pool().await else { return };
    let since = chrono::Utc::now() - chrono::Duration::days(1);
    pull_rows(&pool, EntityType::Tires, since, Some("not-a-uuid"), 5).await.expect("falls back to the timestamp");
    pull_rows(&pool, EntityType::VehicleOdometerOverrides, since, Some("12"), 5).await.expect("integer keys page too");
}

// ───────────────────────── chassis ──────────────────────────────────────────

#[tokio::test]
async fn a_deleted_chassis_can_be_rebuilt_and_a_used_one_cannot_be_deleted() {
    let Some(pool) = test_pool().await else { return };
    let vid = vehicle_id();
    let (layout, pos) = chassis(&pool, vid, 1).await;

    let (t, a) = (Uuid::new_v4(), Uuid::new_v4());
    push(&pool, grouped(vec![tire(t, "legacy", "B"), mount(a, t, pos[0], vid, 10)])).await;
    let r = push(&pool, vec![del(EntityType::ChassisPositions, pos[0], 1)]).await;
    assert_eq!(code(&r[0]), "chassis_in_use");
    let r = push(&pool, vec![del(EntityType::ChassisLayouts, layout, 1)]).await;
    assert_eq!(code(&r[0]), "chassis_in_use");

    // A second layout for the same vehicle is refused while the first lives.
    let r = push(&pool, vec![ins(EntityType::ChassisLayouts, Uuid::new_v4(), json!({"vehicle_id": vid}))]).await;
    assert_eq!(code(&r[0]), "chassis_exists");

    // Dismount, delete, rebuild.
    let asv = row_sv(&pool, "tire_assignments", a).await;
    let r = push(&pool, vec![upd(EntityType::TireAssignments, a, json!({"dismounted_at": chrono::Utc::now().to_rfc3339(), "dismount_reason": "rotation"}), asv)]).await;
    assert!(r[0].is_applied(), "{r:?}");
    let axle: Uuid = sqlx::query_scalar("SELECT axle_id FROM chassis_positions WHERE id = $1").bind(pos[0]).fetch_one(&pool).await.unwrap();
    let r = push(&pool, grouped(vec![
        del(EntityType::ChassisPositions, pos[0], 1),
        del(EntityType::ChassisAxles, axle, 1),
        del(EntityType::ChassisLayouts, layout, 1),
    ])).await;
    assert!(r.iter().all(PushResultStatus::is_applied), "{r:?}");
    let (_, again_pos) = chassis(&pool, vid, 2).await;
    assert_eq!(again_pos.len(), 2, "the rebuild used the same vehicle and the same position codes");
}

#[tokio::test]
async fn a_mount_must_target_a_live_position_on_the_same_vehicle() {
    let Some(pool) = test_pool().await else { return };
    let (v1, v2) = (vehicle_id(), vehicle_id());
    let (_, p1) = chassis(&pool, v1, 1).await;
    let t = Uuid::new_v4();
    assert!(push(&pool, vec![tire(t, "legacy", "B")]).await[0].is_applied());
    let r = push(&pool, vec![mount(Uuid::new_v4(), t, p1[0], v2, 5)]).await;
    assert_eq!(code(&r[0]), "position_wrong_vehicle");
    let r = push(&pool, vec![mount(Uuid::new_v4(), t, Uuid::new_v4(), v1, 5)]).await;
    assert_eq!(code(&r[0]), "missing_reference");
    assert_eq!(tire_status(&pool, t).await, "in_stock_used");
}

// ───────────────────────── tire status ──────────────────────────────────────

#[tokio::test]
async fn tire_status_follows_assignments_and_events_and_nothing_else() {
    let Some(pool) = test_pool().await else { return };
    let vid = vehicle_id();
    let (_, pos) = chassis(&pool, vid, 2).await;
    let (t, a) = (Uuid::new_v4(), Uuid::new_v4());
    push(&pool, grouped(vec![tire(t, "legacy", "B"), mount(a, t, pos[0], vid, 1000)])).await;
    assert_eq!(tire_status(&pool, t).await, "mounted");

    // A client cannot write the status…
    let tsv = row_sv(&pool, "tires", t).await;
    let r = push(&pool, vec![upd(EntityType::Tires, t, json!({"status": "scrapped", "dot_code": "1224"}), tsv)]).await;
    assert!(r[0].is_applied());
    assert_eq!(tire_status(&pool, t).await, "mounted");
    // …nor delete, repair or re-mount a mounted tire.
    let tsv = row_sv(&pool, "tires", t).await;
    assert_eq!(code(&push(&pool, vec![del(EntityType::Tires, t, tsv)]).await[0]), "tire_mounted");
    let event = |to: &str, kind: &str| {
        ins(EntityType::TireStatusEvents, Uuid::new_v4(), json!({"tire_id": t, "event_type": kind, "from_status": "retreading", "to_status": to}))
    };
    assert_eq!(code(&push(&pool, vec![event("in_repair", "repair_out")]).await[0]), "tire_mounted");
    assert_eq!(code(&push(&pool, vec![mount(Uuid::new_v4(), t, pos[1], vid, 1000)]).await[0]), "tire_not_available");

    // Dismount as worn scraps it; correcting the reason un-scraps it.
    let now = chrono::Utc::now().to_rfc3339();
    let asv = row_sv(&pool, "tire_assignments", a).await;
    let r = push(&pool, vec![upd(EntityType::TireAssignments, a, json!({"dismounted_at": now, "dismounted_odometer": 5000, "dismount_reason": "worn"}), asv)]).await;
    assert!(r[0].is_applied(), "{r:?}");
    assert_eq!(tire_status(&pool, t).await, "scrapped");
    let r = push(&pool, vec![upd(EntityType::TireAssignments, a, json!({"dismount_reason": "rotation"}), sv(&r[0]))]).await;
    assert_eq!(tire_status(&pool, t).await, "in_stock_used");

    // Undo the dismount entirely: it is mounted again.
    let r = push(&pool, vec![upd(EntityType::TireAssignments, a, json!({"dismounted_at": null}), sv(&r[0]))]).await;
    assert!(r[0].is_applied(), "{r:?}");
    assert_eq!(tire_status(&pool, t).await, "mounted");
    let (reason, km): (Option<String>, Option<i32>) =
        sqlx::query_as("SELECT dismount_reason::text, dismounted_odometer FROM tire_assignments WHERE id = $1").bind(a).fetch_one(&pool).await.unwrap();
    assert_eq!((reason, km), (None, None), "an open mount carries no dismount data");

    // Undo the mount (delete the assignment): back to the pool.
    let r = push(&pool, vec![del(EntityType::TireAssignments, a, sv(&r[0]))]).await;
    assert!(r[0].is_applied());
    assert_eq!(tire_status(&pool, t).await, "in_stock_used");

    // The retread counter trusts the server's status, not the client's claim.
    assert!(push(&pool, vec![event("in_stock_used", "restock")]).await[0].is_applied());
    let n: i16 = sqlx::query_scalar("SELECT retread_count FROM tires WHERE id = $1").bind(t).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0, "it was never at the retreader");
    push(&pool, vec![event("retreading", "retread")]).await;
    push(&pool, vec![event("in_stock_used", "restock")]).await;
    let n: i16 = sqlx::query_scalar("SELECT retread_count FROM tires WHERE id = $1").bind(t).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn a_mount_cannot_be_moved_by_editing_it() {
    let Some(pool) = test_pool().await else { return };
    let vid = vehicle_id();
    let (_, pos) = chassis(&pool, vid, 2).await;
    let (t, a) = (Uuid::new_v4(), Uuid::new_v4());
    push(&pool, grouped(vec![tire(t, "legacy", "B"), mount(a, t, pos[0], vid, 1)])).await;
    let asv = row_sv(&pool, "tire_assignments", a).await;
    let r = push(&pool, vec![upd(EntityType::TireAssignments, a, json!({"position_id": pos[1]}), asv)]).await;
    assert_eq!(code(&r[0]), "validation");
    // Re-sending the same ids (a full-row snapshot) is fine.
    let r = push(&pool, vec![upd(EntityType::TireAssignments, a, json!({"position_id": pos[0], "tire_id": t, "vehicle_id": vid, "mounted_odometer": 7}), asv)]).await;
    assert!(r[0].is_applied(), "{r:?}");
}

// ───────────────────────── stock ────────────────────────────────────────────

#[tokio::test]
async fn stock_follows_oil_edits_and_deletes_and_records_who() {
    let Some(pool) = test_pool().await else { return };
    let oil = format!("OIL-{}", &Uuid::new_v4().to_string()[..8]);
    let liters = |pool: PgPool, oil: String| async move {
        sqlx::query_scalar::<_, f64>("SELECT liters_on_hand::float8 FROM oil_stock_cache WHERE oil_type = $1").bind(oil).fetch_one(&pool).await.unwrap()
    };
    let id = Uuid::new_v4();
    let create = ins(EntityType::OilChanges, id, json!({"vehicle_id": vehicle_id(), "oil_type": oil, "liters": 35}));
    let r = push(&pool, vec![again(&create)]).await;
    assert_eq!(liters(pool.clone(), oil.clone()).await, -35.0, "an unknown line shows as negative, it does not vanish");
    push(&pool, vec![again(&create)]).await;
    assert_eq!(liters(pool.clone(), oil.clone()).await, -35.0, "replay debits once");

    let r = push(&pool, vec![upd(EntityType::OilChanges, id, json!({"liters": 40}), sv(&r[0]))]).await;
    assert_eq!(liters(pool.clone(), oil.clone()).await, -40.0, "an edit moves the difference");
    let flagged: bool = sqlx::query_scalar("SELECT flagged FROM oil_changes WHERE id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert!(flagged);
    let r = push(&pool, vec![upd(EntityType::OilChanges, id, json!({"notes": "n"}), sv(&r[0]))]).await;
    assert_eq!(liters(pool.clone(), oil.clone()).await, -40.0, "an edit that does not touch liters moves nothing");

    let gone = del(EntityType::OilChanges, id, sv(&r[0]));
    push(&pool, vec![again(&gone)]).await;
    push(&pool, vec![again(&gone)]).await;
    assert_eq!(liters(pool.clone(), oil.clone()).await, 0.0, "a delete gives it all back, once");
    let (n, users): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE user_id = 7) FROM stock_movements WHERE oil_type = $1",
    )
    .bind(&oil)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 4, "debit, edit credit, edit debit, delete credit");
    assert_eq!(users, 4, "every movement names the user");
}

#[tokio::test]
async fn bad_amounts_are_refused_before_they_touch_stock() {
    let Some(pool) = test_pool().await else { return };
    let vid = vehicle_id();
    for payload in [
        json!({"vehicle_id": vid, "oil_type": "X", "liters": -5}),
        json!({"vehicle_id": vid, "oil_type": "X", "liters": 0}),
        json!({"vehicle_id": vid, "oil_type": "X", "liters": "abc"}),
        json!({"vehicle_id": vid, "oil_type": "X", "liters": 35, "odometer": -1}),
        json!({"vehicle_id": vid, "oil_type": "X", "liters": 35, "odometer": 99_999_999}),
        json!({"vehicle_id": vid, "oil_type": "", "liters": 35}),
        json!({"vehicle_id": -3, "oil_type": "X", "liters": 35}),
    ] {
        let r = push(&pool, vec![ins(EntityType::OilChanges, Uuid::new_v4(), payload.clone())]).await;
        assert_eq!(code(&r[0]), "validation", "{payload}");
    }
    // Messages never carry SQL.
    let r = push(&pool, vec![ins(EntityType::WorkOrderTasks, Uuid::new_v4(), json!({"work_order_id": Uuid::new_v4(), "kind": "nonsense"}))]).await;
    match &r[0] {
        PushResultStatus::Error { message, .. } => {
            let m = message.to_lowercase();
            assert!(!m.contains("select") && !m.contains("relation") && !m.contains("enum"), "{message}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_future_device_clock_cannot_win_the_odometer_forever() {
    let Some(pool) = test_pool().await else { return };
    let vid = vehicle_id();
    push(&pool, vec![ins(EntityType::VehicleOdometerOverrides, vid, json!({"vehicle_id": vid, "odometer": 500, "set_at": "2099-01-01T00:00:00Z"}))]).await;
    let set_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT set_at FROM vehicle_odometer_overrides WHERE vehicle_id = $1").bind(vid as i32).fetch_one(&pool).await.unwrap();
    assert!(set_at <= chrono::Utc::now() + chrono::Duration::minutes(1));
}

// ───────────────────────── work orders ──────────────────────────────────────

#[tokio::test]
async fn an_empty_work_order_deletes_with_its_tasks_and_a_used_one_does_not() {
    let Some(pool) = test_pool().await else { return };
    let vid = vehicle_id();
    let (wo, task) = (Uuid::new_v4(), Uuid::new_v4());
    push(&pool, vec![
        ins(EntityType::WorkOrders, wo, json!({"vehicle_id": vid, "status": "open", "odometer_at_open": 4_123_000})),
        ins(EntityType::WorkOrderTasks, task, json!({"work_order_id": wo, "kind": "free", "is_done": true})),
    ]).await;
    let oil = Uuid::new_v4();
    let r = push(&pool, vec![ins(EntityType::OilChanges, oil, json!({"vehicle_id": vid, "work_order_id": wo, "oil_type": "W", "liters": 35}))]).await;
    assert_eq!(code(&push(&pool, vec![del(EntityType::WorkOrders, wo, 1)]).await[0]), "work_order_not_empty");

    push(&pool, vec![del(EntityType::OilChanges, oil, sv(&r[0]))]).await;
    assert!(push(&pool, vec![del(EntityType::WorkOrders, wo, 1)]).await[0].is_applied());
    let (deleted, version): (bool, i64) =
        sqlx::query_as("SELECT deleted_at IS NOT NULL, sync_version FROM work_order_tasks WHERE id = $1").bind(task).fetch_one(&pool).await.unwrap();
    assert!(deleted && version == 2, "the task is tombstoned and re-announced to other devices");

    // Nothing can be added to a deleted work order, or edited on it.
    let r = push(&pool, vec![ins(EntityType::WorkOrderTasks, Uuid::new_v4(), json!({"work_order_id": wo, "kind": "free"}))]).await;
    assert_eq!(code(&r[0]), "deleted");
    let r = push(&pool, vec![upd(EntityType::WorkOrders, wo, json!({"title": "x"}), 2)]).await;
    assert_eq!(code(&r[0]), "deleted");
    // Its typo'd odometer no longer counts.
    let odo: Option<i32> = sqlx::query_scalar("SELECT current_odometer FROM v_current_odometer WHERE vehicle_id = $1").bind(vid as i32).fetch_optional(&pool).await.unwrap();
    assert_eq!(odo, None);
}

#[tokio::test]
async fn closing_and_reopening_keep_status_and_closed_at_in_step() {
    let Some(pool) = test_pool().await else { return };
    let wo = Uuid::new_v4();
    let r = push(&pool, vec![ins(EntityType::WorkOrders, wo, json!({"vehicle_id": vehicle_id(), "status": "open"}))]).await;
    let r = push(&pool, vec![upd(EntityType::WorkOrders, wo, json!({"status": "closed"}), sv(&r[0]))]).await;
    let closed: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("SELECT closed_at FROM work_orders WHERE id = $1").bind(wo).fetch_one(&pool).await.unwrap();
    assert!(closed.is_some(), "the server fills the close time");
    push(&pool, vec![upd(EntityType::WorkOrders, wo, json!({"status": "open"}), sv(&r[0]))]).await;
    let closed: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("SELECT closed_at FROM work_orders WHERE id = $1").bind(wo).fetch_one(&pool).await.unwrap();
    assert!(closed.is_none());
}

// ───────────────────────── due + odometer rules ─────────────────────────────

#[tokio::test]
async fn the_odometer_moves_again_after_a_downward_correction() {
    let Some(pool) = test_pool().await else { return };
    let vid = vehicle_id();
    let set = |fuel: i32, mileage: i32| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                // What the fleet refresh does: each reading's change time
                // moves only when that reading changes.
                "INSERT INTO vehicles_cache (id, car_no_plate, last_fuel_odometer, mileage, raw_payload,
                                             fuel_odometer_changed_at, mileage_changed_at)
                 VALUES ($1, 'ODO', $2, $3, '{}'::jsonb, clock_timestamp(), clock_timestamp())
                 ON CONFLICT (id) DO UPDATE SET
                   fuel_odometer_changed_at = CASE WHEN vehicles_cache.last_fuel_odometer IS DISTINCT FROM $2
                        THEN clock_timestamp() ELSE vehicles_cache.fuel_odometer_changed_at END,
                   mileage_changed_at = CASE WHEN vehicles_cache.mileage IS DISTINCT FROM $3
                        THEN clock_timestamp() ELSE vehicles_cache.mileage_changed_at END,
                   last_fuel_odometer = $2, mileage = $3",
            )
            .bind(vid as i32).bind(fuel).bind(mileage).execute(&pool).await.unwrap();
        }
    };
    let current = || {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (i32, String)>("SELECT current_odometer, odometer_source FROM v_current_odometer WHERE vehicle_id = $1")
                .bind(vid as i32).fetch_one(&pool).await.unwrap()
        }
    };
    // Falcon holds a typo in its mileage (and an older, smaller fuel reading
    // that is also above the truth); the garage corrects downward.
    set(430_000, 4_123_000).await;
    push(&pool, vec![ins(EntityType::VehicleOdometerOverrides, vid, json!({"vehicle_id": vid, "odometer": 412_300, "superseded_km": 4_123_000}))]).await;
    assert_eq!(current().await, (412_300, "manual_override".to_string()));
    // The truck refuels: a real, higher reading. It used to be ignored
    // because it was still below the typo.
    set(418_000, 4_123_000).await;
    assert_eq!(
        current().await,
        (418_000, "falcon_fuel".to_string()),
        "the new fuel reading counts; the stale typo'd mileage still does not"
    );
}

#[tokio::test]
async fn a_plan_with_both_clocks_is_due_on_whichever_comes_first() {
    let Some(pool) = test_pool().await else { return };
    let vid = vehicle_id();
    let class = format!("c-{}", &Uuid::new_v4().to_string()[..8]);
    sqlx::query("INSERT INTO vehicle_classes (id, name_ar, name_en) VALUES ($1, 'x', 'x')").bind(&class).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO vehicles_cache (id, car_no_plate, last_fuel_odometer, raw_payload) VALUES ($1, 'P', 105000, '{}'::jsonb)")
        .bind(vid as i32).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO vehicle_class_assignments (vehicle_id, class_id, created_by_user_id, updated_by_user_id) VALUES ($1, $2, 1, 1)")
        .bind(vid as i32).bind(&class).execute(&pool).await.unwrap();
    let plan: Uuid = sqlx::query_scalar(
        "INSERT INTO service_plans (class_id, category_id, trigger_type, interval_km, interval_days, lead_warn_km, lead_warn_days, created_by_user_id, updated_by_user_id)
         VALUES ($1, 'oil_change', 'mileage', 10000, 180, 500, 14, 1, 1) RETURNING id",
    )
    .bind(&class).fetch_one(&pool).await.unwrap();
    let status = || {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (String, Option<i32>, Option<i32>)>(
                "SELECT status, km_remaining, days_remaining FROM v_maintenance_due WHERE plan_id = $1 AND vehicle_id = $2",
            )
            .bind(plan).bind(vid as i32).fetch_one(&pool).await.unwrap()
        }
    };
    assert_eq!(status().await.0, "never_done");

    // Done at 100,000 km, 200 days ago, on a work order opened long before.
    let (wo, task) = (Uuid::new_v4(), Uuid::new_v4());
    let long_ago = (chrono::Utc::now() - chrono::Duration::days(400)).to_rfc3339();
    let done_at = (chrono::Utc::now() - chrono::Duration::days(200)).to_rfc3339();
    push(&pool, vec![
        ins(EntityType::WorkOrders, wo, json!({"vehicle_id": vid, "status": "open", "odometer_at_open": 100_000, "opened_at": long_ago})),
        ins(EntityType::WorkOrderTasks, task, json!({"work_order_id": wo, "kind": "free", "category_id": "oil_change", "is_done": true, "done_at": done_at})),
    ]).await;
    let (s, km, days) = status().await;
    assert_eq!(s, "overdue", "5,000 km in hand, but the 180 days ran out");
    assert_eq!(km, Some(5000));
    assert!(days.unwrap() < 0);

    // Ticked today instead: the time clock restarts from the tick, not from
    // the day the work order was opened.
    let r = push(&pool, vec![upd(EntityType::WorkOrderTasks, task, json!({"is_done": false}), 1)]).await;
    push(&pool, vec![upd(EntityType::WorkOrderTasks, task, json!({"is_done": true}), sv(&r[0]))]).await;
    let (s, _, days) = status().await;
    assert_eq!(s, "ok");
    assert!((179..=180).contains(&days.unwrap()), "{days:?}");
}
