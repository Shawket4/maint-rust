//! Live-Postgres tests for the sync engine's two field-critical properties:
//!
//!  1. Push batches survive per-op constraint violations (SAVEPOINT isolation):
//!     one bad op yields one per-op error, and every other op still applies.
//!  2. Pull paging never skips rows that share an updated_at (composite cursor):
//!     rows written in one transaction all carry the same timestamp, and a
//!     timestamp-only cursor drops the tail of the group at page boundaries.
//!
//! Needs a throwaway database:  MAINT_TEST_DATABASE_URL=postgres://… cargo test
//! Skips (passes vacuously) when the variable is unset, so plain `cargo test`
//! stays green without infrastructure.

use maint_rust::services::sync_engine::{
    apply_push_batch, pull_rows, EntityType, PushBody, PushOperation, PushResultStatus,
    SyncOperation,
};
use serde_json::json;
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

fn template_op(id: Uuid, name: &str, interval_km: Option<i64>) -> PushOperation {
    // interval_km = None with trigger_type 'mileage' trips the table CHECK.
    PushOperation {
        entity_type: EntityType::MaintenanceTemplates,
        entity_id: id.to_string(),
        operation: SyncOperation::Insert,
        payload: json!({
            "id": id.to_string(),
            "vehicle_id": 901,
            "category_id": "oil_change",
            "name_ar": name,
            "name_en": name,
            "trigger_type": "mileage",
            "interval_km": interval_km,
            "lead_warn_km": 500,
            "lead_warn_days": 14,
            "is_active": true,
        }),
        sync_version: 0,
    }
}

#[tokio::test]
async fn push_batch_survives_check_violation_mid_batch() {
    let Some(pool) = test_pool().await else { return };

    let a = Uuid::new_v4();
    let bad = Uuid::new_v4();
    let c = Uuid::new_v4();
    let body = PushBody {
        operations: vec![
            template_op(a, "sp-ok-a", Some(10_000)),
            template_op(bad, "sp-bad", None), // CHECK violation
            template_op(c, "sp-ok-c", Some(20_000)),
        ],
    };

    let resp = apply_push_batch(&pool, &body, 1).await.expect("batch must not 500");
    assert_eq!(resp.results.len(), 3);
    assert!(matches!(resp.results[0], PushResultStatus::Applied { .. }), "op A applies");
    assert!(matches!(resp.results[1], PushResultStatus::Error { .. }), "bad op errors");
    assert!(
        matches!(resp.results[2], PushResultStatus::Applied { .. }),
        "op C must still apply — before the savepoint fix the aborted tx killed it"
    );

    // And the good rows genuinely landed.
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM maintenance_templates WHERE id = ANY($1)",
    )
    .bind(vec![a, c])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 2);
}

#[tokio::test]
async fn push_batch_survives_unique_violation() {
    let Some(pool) = test_pool().await else { return };

    let dot = format!("DOT-{}", Uuid::new_v4());
    let mk = |id: Uuid| PushOperation {
        entity_type: EntityType::Tires,
        entity_id: id.to_string(),
        operation: SyncOperation::Insert,
        payload: json!({
            "id": id.to_string(),
            "dot_code": dot,        // same DOT twice → unique violation on op 2
            "brand": "TestBrand",
            "status": "in_stock",
            "is_retread": false,
            "retread_count": 0,
        }),
        sync_version: 0,
    };
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let body = PushBody { operations: vec![mk(t1), mk(t2)] };

    let resp = apply_push_batch(&pool, &body, 1)
        .await
        .expect("unique violation must not roll back the batch");
    assert!(matches!(resp.results[0], PushResultStatus::Applied { .. }));
    assert!(
        matches!(resp.results[1], PushResultStatus::Error { .. }),
        "duplicate DOT is a per-op error, not a batch 409"
    );

    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM tires WHERE dot_code = $1")
        .bind(&dot)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1, "first tire survives the sibling's failure");
}

#[tokio::test]
async fn pull_pages_through_identical_timestamps_without_skipping() {
    let Some(pool) = test_pool().await else { return };

    // Five records in ONE transaction ⇒ byte-identical updated_at on all five.
    let tpl = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO maintenance_templates
           (id, vehicle_id, category_id, name_ar, name_en, trigger_type, interval_km,
            created_by_user_id, updated_by_user_id)
         VALUES ($1, 901, 'oil_change', 'x', 'x', 'mileage', 10000, 1, 1)",
    )
    .bind(tpl)
    .execute(&pool)
    .await
    .unwrap();

    let ids: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
    let mut tx = pool.begin().await.unwrap();
    for id in &ids {
        sqlx::query(
            "INSERT INTO maintenance_records
               (id, template_id, vehicle_id, category_id, performed_at,
                odometer_at_service, created_by_user_id, updated_by_user_id)
             VALUES ($1, $2, 901, 'oil_change', now(), 100000, 1, 1)",
        )
        .bind(id)
        .bind(tpl)
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
    for _ in 0..20 {
        let page = pull_rows(&pool, EntityType::MaintenanceRecords, since, after_id.as_deref(), 2)
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
