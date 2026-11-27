//! Live-Postgres tests for garage attendance: workers typed in by name, one
//! row per worker per day under its worker-and-date id, no days off.
//!
//! Needs a throwaway database:  MAINT_TEST_DATABASE_URL=postgres://… cargo test

use maint_rust::services::rules::attendance_day_id;
use maint_rust::services::sync_engine::{
    apply_push_batch, EntityType, PushBody, PushOperation, PushResultStatus, SyncOperation,
};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("MAINT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("MAINT_REQUIRE_LIVE_TESTS").is_err(),
            "MAINT_REQUIRE_LIVE_TESTS is set but MAINT_TEST_DATABASE_URL is not"
        );
        return None;
    };
    let pool = PgPoolOptions::new().max_connections(4).connect(&url).await.expect("connect");
    sqlx::migrate!("./migrations").run(&pool).await.expect("migrations");
    Some(pool)
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
async fn push_as(pool: &PgPool, user: i64, ops: Vec<PushOperation>) -> Vec<PushResultStatus> {
    apply_push_batch(pool, &PushBody { operations: ops }, user).await.expect("push").0.results
}
async fn push(pool: &PgPool, ops: Vec<PushOperation>) -> Vec<PushResultStatus> {
    push_as(pool, 7, ops).await
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

async fn worker(pool: &PgPool, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    let r = push(pool, vec![ins(EntityType::AttendanceWorkers, id, json!({"name": name}))]).await;
    sv(&r[0]);
    id
}

fn day(worker: Uuid, date: &str, payload: Value) -> PushOperation {
    let mut p = payload;
    p["worker_id"] = json!(worker);
    p["work_date"] = json!(date);
    ins(EntityType::AttendanceDays, attendance_day_id(&worker, date), p)
}

const PAST: &str = "2026-01-10";

#[tokio::test]
async fn a_worker_typed_in_by_name_needs_only_the_name() {
    let Some(pool) = test_pool().await else { return };
    let id = Uuid::new_v4();
    let r = push(&pool, vec![ins(EntityType::AttendanceWorkers, id, json!({"name": "  Test Worker  ", "job": " "}))]).await;
    sv(&r[0]);
    let (name, job): (String, Option<String>) =
        sqlx::query_as("SELECT name, job FROM attendance_workers WHERE id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!((name.as_str(), job), ("Test Worker", None));

    let r = push(&pool, vec![ins(EntityType::AttendanceWorkers, Uuid::new_v4(), json!({"name": "   "}))]).await;
    assert_eq!(code(&r[0]), "validation");
    let r = push(
        &pool,
        vec![ins(EntityType::AttendanceWorkers, Uuid::new_v4(), json!({"name": "W", "joined_on": "2026-05-01", "left_on": "2026-04-01"}))],
    )
    .await;
    assert_eq!(code(&r[0]), "validation", "left before joined");
}

#[tokio::test]
async fn a_second_device_marking_the_same_day_is_told_and_the_first_mark_stays() {
    let Some(pool) = test_pool().await else { return };
    let w = worker(&pool, "Two Devices").await;
    let first = push_as(
        &pool,
        7,
        vec![day(w, PAST, json!({"status": "present", "check_in_at": "2026-01-10T06:05:00Z"}))],
    )
    .await;
    sv(&first[0]);
    let second = push_as(&pool, 8, vec![day(w, PAST, json!({"status": "absent"}))]).await;
    match &second[0] {
        PushResultStatus::Conflict { server_row, .. } => assert_eq!(server_row["status"], "present"),
        other => panic!("expected conflict, got {other:?}"),
    }
    let status: String = sqlx::query_scalar("SELECT status::text FROM attendance_days WHERE worker_id = $1")
        .bind(w)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "present");
}

#[tokio::test]
async fn a_day_row_with_any_other_id_is_refused() {
    let Some(pool) = test_pool().await else { return };
    let w = worker(&pool, "Random Id").await;
    let r = push(
        &pool,
        vec![ins(EntityType::AttendanceDays, Uuid::new_v4(), json!({"worker_id": w, "work_date": PAST, "status": "absent"}))],
    )
    .await;
    assert_eq!(code(&r[0]), "validation");
}

#[tokio::test]
async fn only_a_present_day_keeps_times_and_out_comes_after_in() {
    let Some(pool) = test_pool().await else { return };
    let w = worker(&pool, "Times").await;
    let id = attendance_day_id(&w, PAST);
    let r = push(
        &pool,
        vec![day(w, PAST, json!({"status": "present", "check_in_at": "2026-01-10T06:00:00Z", "check_out_at": "2026-01-10T05:00:00Z"}))],
    )
    .await;
    assert_eq!(code(&r[0]), "validation", "out before in");

    let r = push(&pool, vec![day(w, PAST, json!({"status": "present", "check_in_at": "2026-01-10T06:00:00Z"}))]).await;
    let v = sv(&r[0]);
    // Late night: out after midnight still belongs to the day it started.
    let r = push(&pool, vec![upd(EntityType::AttendanceDays, id, json!({"check_out_at": "2026-01-10T23:30:00Z"}), v)]).await;
    let v = sv(&r[0]);
    // Turning it into sick leave drops the times instead of refusing.
    let r = push(&pool, vec![upd(EntityType::AttendanceDays, id, json!({"status": "sick"}), v)]).await;
    let v = sv(&r[0]);
    let (cin, cout): (Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT check_in_at, check_out_at FROM attendance_days WHERE id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert!(cin.is_none() && cout.is_none());
    // The worker and the date never change.
    let r = push(&pool, vec![upd(EntityType::AttendanceDays, id, json!({"work_date": "2026-01-11"}), v)]).await;
    assert_eq!(code(&r[0]), "validation");
}

#[tokio::test]
async fn a_worker_with_marked_days_is_not_deleted() {
    let Some(pool) = test_pool().await else { return };
    let w = worker(&pool, "Has Days").await;
    sv(&push(&pool, vec![day(w, PAST, json!({"status": "absent"}))]).await[0]);
    let r = push(&pool, vec![op(EntityType::AttendanceWorkers, w, SyncOperation::Delete, json!({}), 1)]).await;
    assert_eq!(code(&r[0]), "worker_has_days");
    let typo = worker(&pool, "Typo").await;
    let r = push(&pool, vec![op(EntityType::AttendanceWorkers, typo, SyncOperation::Delete, json!({}), 1)]).await;
    sv(&r[0]);
}

#[tokio::test]
async fn a_day_in_the_future_is_refused() {
    let Some(pool) = test_pool().await else { return };
    let w = worker(&pool, "Future").await;
    let r = push(&pool, vec![day(w, "2099-01-01", json!({"status": "absent"}))]).await;
    assert_eq!(code(&r[0]), "validation");
}

#[tokio::test]
async fn the_office_month_counts_statuses_and_has_no_days_off() {
    let Some(pool) = test_pool().await else { return };
    let w = worker(&pool, "Office Month").await;
    let r = push(
        &pool,
        vec![
            day(w, "2026-07-11", json!({"status": "present", "check_in_at": "2026-07-11T05:30:00Z", "check_out_at": "2026-07-11T14:30:00Z"})),
            // A Friday is a working day like any other.
            day(w, "2026-07-10", json!({"status": "present"})),
            day(w, "2026-07-12", json!({"status": "absent"})),
        ],
    )
    .await;
    for x in &r {
        sv(x);
    }
    let m = maint_rust::services::attendance::month(&pool, "2026-07").await.unwrap();
    assert_eq!(m.days.len(), 31);
    let row = m.workers.iter().find(|x| x.worker_id == w.to_string()).expect("worker in the month");
    assert_eq!(row.totals.present, 2);
    assert_eq!(row.totals.absent, 1);
    assert_eq!(row.cells[10].check_out_at.map(|t| t.to_rfc3339()), Some("2026-07-11T14:30:00+00:00".into()));
    assert_eq!(row.cells[9].kind, "present", "the Friday");
    assert_eq!(row.cells[2].kind, "unmarked", "3 July 2026 is a Friday, not a day off");
}
