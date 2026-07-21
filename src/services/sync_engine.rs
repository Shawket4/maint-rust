//! Sync engine — implements §7 push/pull.
//!
//! Push: client sends operations with their current sync_version; server applies
//! them within a transaction. On a stale sync_version, the operation reports
//! "conflict" and the server's current row is returned.
//!
//! Pull: server returns rows where updated_at > since, ordered by updated_at,
//! up to `limit`. Client advances cursor to MAX(updated_at) of returned rows.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Acquire, PgPool, Postgres, Transaction};

use crate::error::{ApiError, ApiResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    WorkOrders,
    WorkOrderTasks,
    Tires,
    TireAssignments,
    TireStatusEvents,
    OilChanges,
    ChassisLayouts,
    ChassisAxles,
    ChassisPositions,
    VehicleClassAssignments,
    VehicleOdometerOverrides,
}

impl EntityType {
    pub fn table_name(self) -> &'static str {
        match self {
            Self::WorkOrders => "work_orders",
            Self::WorkOrderTasks => "work_order_tasks",
            Self::Tires => "tires",
            Self::TireAssignments => "tire_assignments",
            Self::TireStatusEvents => "tire_status_events",
            Self::OilChanges => "oil_changes",
            Self::ChassisLayouts => "chassis_layouts",
            Self::ChassisAxles => "chassis_axles",
            Self::ChassisPositions => "chassis_positions",
            Self::VehicleClassAssignments => "vehicle_class_assignments",
            Self::VehicleOdometerOverrides => "vehicle_odometer_overrides",
        }
    }

    pub fn pk_column(self) -> &'static str {
        match self {
            Self::VehicleClassAssignments | Self::VehicleOdometerOverrides => "vehicle_id",
            _ => "id",
        }
    }

    /// Tables that upsert on PK conflict rather than insert (PK-keyed singletons).
    pub fn upserts_on_conflict(self) -> bool {
        matches!(self, Self::VehicleClassAssignments | Self::VehicleOdometerOverrides)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncOperation {
    Insert,
    Update,
    Delete,
}

#[derive(Debug, Deserialize)]
pub struct PushOperation {
    pub entity_type: EntityType,
    /// PK as string (UUID for most tables, integer for vehicle_odometer_overrides).
    pub entity_id: String,
    pub operation: SyncOperation,
    pub payload: Value,
    /// Client's current sync_version. For Insert, set to 0 (server will accept any value
    /// since the row doesn't exist yet).
    pub sync_version: i64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PushResultStatus {
    Applied {
        entity_id: String,
        new_sync_version: i64,
    },
    Conflict {
        entity_id: String,
        server_row: Value,
    },
    Error {
        entity_id: String,
        message: String,
    },
}

#[derive(Debug, Deserialize)]
pub struct PushBody {
    pub operations: Vec<PushOperation>,
}

#[derive(Debug, Serialize)]
pub struct PushResponse {
    pub results: Vec<PushResultStatus>,
}

#[derive(Debug, Deserialize)]
pub struct PullQuery {
    pub entity_type: EntityType,
    /// ISO-8601 timestamp; default = epoch.
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    /// PK (as text) of the last row of the previous page. Together with `since`
    /// this forms a composite keyset cursor — REQUIRED for correct paging,
    /// because rows written in one transaction share a byte-identical
    /// updated_at (Postgres now() is transaction_timestamp()), and a
    /// timestamp-only `>` cursor silently skips the remainder of such a group
    /// at every page boundary.
    #[serde(default)]
    pub after_id: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    200
}

#[derive(Debug, Serialize)]
pub struct PullResponse {
    pub entity_type: EntityType,
    pub rows: Vec<Value>,
    /// updated_at of the last returned row; pass back as `since`.
    /// Null when rows is empty.
    pub next_cursor: Option<DateTime<Utc>>,
    /// PK (text) of the last returned row; pass back as `after_id`.
    pub next_cursor_id: Option<String>,
    pub has_more: bool,
}

/// Apply a whole push batch: one outer transaction, one SAVEPOINT per operation.
///
/// The savepoints are the load-bearing part. Without them, the first statement
/// that trips a Postgres constraint aborts the surrounding transaction (25P02):
/// the per-op `Error` result was produced, but every LATER statement in the
/// batch then failed with "current transaction is aborted" and the client got
/// one opaque 500 instead of per-op results. Unique violations were worse —
/// they map to ApiError::Conflict, which propagated and rolled back the entire
/// batch. With a savepoint around each op, a failed op rolls back only itself
/// and the batch keeps its promise: independent per-op results.
pub async fn apply_push_batch(
    pool: &PgPool,
    body: &PushBody,
    user_id: i64,
) -> ApiResult<PushResponse> {
    let mut tx = pool.begin().await?;
    let mut results = Vec::with_capacity(body.operations.len());
    for op in &body.operations {
        // sqlx: Transaction::begin on a &mut Transaction issues a SAVEPOINT.
        let mut sp = tx.begin().await?;
        match apply_push_operation(&mut sp, op, user_id).await {
            Ok(r) => {
                // Commit ONLY when the op applied. A per-op Error is often a
                // constraint violation that apply_push_operation swallowed into
                // a result — the subtransaction is already aborted underneath,
                // and RELEASE SAVEPOINT on an aborted subtransaction is itself
                // a 25P02. Rolling back Error/Conflict discards nothing (those
                // paths wrote nothing durable) and always leaves the outer
                // transaction healthy.
                match &r {
                    PushResultStatus::Applied { .. } => sp.commit().await?,
                    _ => sp.rollback().await?,
                }
                results.push(r);
            }
            Err(e) => {
                sp.rollback().await?;
                results.push(PushResultStatus::Error {
                    entity_id: op.entity_id.clone(),
                    message: e.to_string(),
                });
            }
        }
    }
    tx.commit().await?;
    Ok(PushResponse { results })
}

/// Apply a single push operation inside an existing transaction.
///
/// Returns the per-op result (applied / conflict / error). Errors are returned
/// as Ok(PushResultStatus::Error) — the *transaction* itself is rolled back only
/// for unrecoverable DB errors, which propagate as Err.
pub async fn apply_push_operation(
    tx: &mut Transaction<'_, Postgres>,
    op: &PushOperation,
    user_id: i64,
) -> ApiResult<PushResultStatus> {
    let table = op.entity_type.table_name();
    let pk = op.entity_type.pk_column();

    // Read the current row (if any) inside the same transaction so concurrent
    // writers don't race us.
    let row_now = fetch_row_json(tx, table, pk, &op.entity_id).await?;

    match op.operation {
        SyncOperation::Insert => {
            if row_now.is_some() && !op.entity_type.upserts_on_conflict() {
                // Idempotent insert: if sync_version matches, treat as no-op applied.
                let server_sv = row_now
                    .as_ref()
                    .and_then(|v| v.get("sync_version"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                if op.sync_version == server_sv {
                    return Ok(PushResultStatus::Applied {
                        entity_id: op.entity_id.clone(),
                        new_sync_version: server_sv,
                    });
                }
                return Ok(PushResultStatus::Conflict {
                    entity_id: op.entity_id.clone(),
                    server_row: row_now.unwrap_or(Value::Null),
                });
            }
            // Build column list from payload, force audit columns server-side.
            match generic_insert(tx, op.entity_type, &op.payload, user_id).await {
                Ok(new_sv) => Ok(PushResultStatus::Applied {
                    entity_id: op.entity_id.clone(),
                    new_sync_version: new_sv,
                }),
                Err(ApiError::BadRequest(m)) => Ok(PushResultStatus::Error {
                    entity_id: op.entity_id.clone(),
                    message: m,
                }),
                Err(e) => Err(e),
            }
        }
        SyncOperation::Update => {
            let row_now = match row_now {
                Some(r) => r,
                None => {
                    return Ok(PushResultStatus::Error {
                        entity_id: op.entity_id.clone(),
                        message: "row not found".into(),
                    });
                }
            };
            let server_sv = row_now
                .get("sync_version")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            if server_sv != op.sync_version {
                return Ok(PushResultStatus::Conflict {
                    entity_id: op.entity_id.clone(),
                    server_row: row_now,
                });
            }
            match generic_update(tx, op.entity_type, &op.entity_id, &op.payload, user_id).await {
                Ok(new_sv) => Ok(PushResultStatus::Applied {
                    entity_id: op.entity_id.clone(),
                    new_sync_version: new_sv,
                }),
                Err(ApiError::BadRequest(m)) => Ok(PushResultStatus::Error {
                    entity_id: op.entity_id.clone(),
                    message: m,
                }),
                Err(e) => Err(e),
            }
        }
        SyncOperation::Delete => {
            let row_now = match row_now {
                Some(r) => r,
                None => {
                    // Already gone — treat as applied for idempotency.
                    return Ok(PushResultStatus::Applied {
                        entity_id: op.entity_id.clone(),
                        new_sync_version: op.sync_version,
                    });
                }
            };
            let server_sv = row_now
                .get("sync_version")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            if server_sv != op.sync_version {
                return Ok(PushResultStatus::Conflict {
                    entity_id: op.entity_id.clone(),
                    server_row: row_now,
                });
            }
            // Soft delete only; vehicle_odometer_overrides has no deleted_at.
            if op.entity_type == EntityType::VehicleOdometerOverrides {
                return Ok(PushResultStatus::Error {
                    entity_id: op.entity_id.clone(),
                    message: "vehicle_odometer_overrides does not support delete".into(),
                });
            }
            let new_sv = soft_delete(tx, op.entity_type, &op.entity_id, user_id).await?;
            Ok(PushResultStatus::Applied {
                entity_id: op.entity_id.clone(),
                new_sync_version: new_sv,
            })
        }
    }
}

async fn fetch_row_json(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    pk: &str,
    pk_value: &str,
) -> ApiResult<Option<Value>> {
    // Use to_jsonb for a generic single-row JSON. PK type is auto-cast by Postgres
    // because UUID/INT both accept text inputs through ::uuid / ::int.
    let q = format!(
        "SELECT to_jsonb(t) AS row FROM {table} t WHERE {pk}::text = $1 LIMIT 1"
    );
    let opt: Option<(Value,)> = sqlx::query_as(&q).bind(pk_value).fetch_optional(&mut **tx).await?;
    Ok(opt.map(|t| t.0))
}

/// Generic INSERT for an entity from a JSON payload.
///
/// We trust the client's UUID PK (offline-first §7.1). All audit columns and
/// sync_version are server-assigned.
async fn generic_insert(
    tx: &mut Transaction<'_, Postgres>,
    entity: EntityType,
    payload: &Value,
    user_id: i64,
) -> ApiResult<i64> {
    use crate::handlers::sync_helpers as h;
    h::insert_from_payload(tx, entity, payload, user_id).await
}

async fn generic_update(
    tx: &mut Transaction<'_, Postgres>,
    entity: EntityType,
    entity_id: &str,
    payload: &Value,
    user_id: i64,
) -> ApiResult<i64> {
    use crate::handlers::sync_helpers as h;
    h::update_from_payload(tx, entity, entity_id, payload, user_id).await
}

async fn soft_delete(
    tx: &mut Transaction<'_, Postgres>,
    entity: EntityType,
    entity_id: &str,
    user_id: i64,
) -> ApiResult<i64> {
    let table = entity.table_name();
    let pk = entity.pk_column();
    let q = format!(
        r#"
        UPDATE {table}
           SET deleted_at = now(),
               updated_at = now(),
               updated_by_user_id = $1,
               sync_version = sync_version + 1
         WHERE {pk}::text = $2
         RETURNING sync_version
        "#
    );
    let (sv,): (i64,) = sqlx::query_as(&q)
        .bind(user_id)
        .bind(entity_id)
        .fetch_one(&mut **tx)
        .await?;
    Ok(sv)
}

/// Cursor-based incremental pull with a composite (updated_at, pk) keyset.
///
/// All rows are returned, including soft-deleted ones (clients need tombstones).
pub async fn pull_rows(
    pool: &PgPool,
    entity: EntityType,
    since: DateTime<Utc>,
    after_id: Option<&str>,
    limit: i64,
) -> ApiResult<PullResponse> {
    let table = entity.table_name();
    let pk = entity.pk_column();

    // Row-value comparison makes the keyset exact: strictly after the last row
    // we handed out, even when many rows share one updated_at. Without
    // after_id (first page) a plain > on the timestamp is correct.
    let rows: Vec<(Value, DateTime<Utc>, String)> = if let Some(aid) = after_id {
        let q = format!(
            r#"
            SELECT to_jsonb(t) AS row, updated_at, {pk}::text AS pk_text
              FROM {table} t
             WHERE (updated_at, {pk}::text) > ($1, $2)
             ORDER BY updated_at ASC, {pk}::text ASC
             LIMIT $3
            "#
        );
        sqlx::query_as(&q).bind(since).bind(aid).bind(limit).fetch_all(pool).await?
    } else {
        let q = format!(
            r#"
            SELECT to_jsonb(t) AS row, updated_at, {pk}::text AS pk_text
              FROM {table} t
             WHERE updated_at > $1
             ORDER BY updated_at ASC, {pk}::text ASC
             LIMIT $2
            "#
        );
        sqlx::query_as(&q).bind(since).bind(limit).fetch_all(pool).await?
    };

    let next_cursor = rows.last().map(|(_, t, _)| *t);
    let next_cursor_id = rows.last().map(|(_, _, id)| id.clone());
    let has_more = rows.len() as i64 == limit;

    Ok(PullResponse {
        entity_type: entity,
        rows: rows.into_iter().map(|(v, _, _)| v).collect(),
        next_cursor,
        next_cursor_id,
        has_more,
    })
}
