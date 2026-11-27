//! Sync engine: push (client → server) and pull (server → client).
//!
//! PUSH
//! * Every op may carry an `op_id` (the client's outbox UUID). An applied op is
//!   remembered in `sync_applied_ops`; a redelivery returns the stored result.
//!   This is the only sound idempotency: a response the client never heard is
//!   neither re-applied nor turned into a conflict.
//! * Consecutive ops sharing a `group_id` are one business action (mint a tire
//!   AND mount it). They apply inside one savepoint: all or none.
//! * Everything else is per-op: applied / conflict / error, each in its own
//!   savepoint, so one bad op never takes its batch-mates down.
//! * A transient database failure (deadlock, lock timeout, lost connection)
//!   fails the WHOLE request with 503. It is not the op's fault and must not
//!   be dead-lettered by the client.
//! * Clients that predate `op_id` (≤ v0.2.3) keep working through the
//!   version-arithmetic replay rules in `legacy_replay`.
//!
//! PULL
//! Keyset over (updated_at, pk). updated_at is stamped by a trigger with
//! clock_timestamp() under one advisory lock (migration 0021), so commit order
//! equals cursor order and a cursor can never advance past an uncommitted row.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Acquire, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::handlers::sync_helpers as sql;
use crate::services::{rules, side_effects};

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
    AttendanceWorkers,
    AttendanceDays,
    AttendanceSettings,
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
            Self::AttendanceWorkers => "attendance_workers",
            Self::AttendanceDays => "attendance_days",
            Self::AttendanceSettings => "attendance_settings",
        }
    }

    pub fn pk_column(self) -> &'static str {
        match self {
            Self::VehicleClassAssignments | Self::VehicleOdometerOverrides => "vehicle_id",
            _ => "id",
        }
    }

    /// SQL cast for a text-bound PK parameter. Comparing `pk = $1::uuid` keeps
    /// the primary-key index usable; the old `pk::text = $1` forced a
    /// sequential scan several times per op.
    pub fn pk_cast(self) -> &'static str {
        if self.is_singleton() {
            "::integer"
        } else {
            "::uuid"
        }
    }

    /// Integer-keyed singletons (one row per vehicle, or the one settings
    /// row): an insert upserts.
    pub fn is_singleton(self) -> bool {
        matches!(self, Self::VehicleClassAssignments | Self::VehicleOdometerOverrides | Self::AttendanceSettings)
    }

    /// Every owned table soft-deletes except the odometer override.
    pub fn has_deleted_at(self) -> bool {
        !matches!(self, Self::VehicleOdometerOverrides)
    }

    /// Canonical form of a PK, or None when it is not a valid key for this
    /// entity. UUIDs are lower-cased so `ABC…` and `abc…` are the same row.
    pub fn canonical_id(self, raw: &str) -> Option<String> {
        if self.is_singleton() {
            raw.trim().parse::<i32>().ok().filter(|n| *n > 0).map(|n| n.to_string())
        } else {
            Uuid::parse_str(raw.trim()).ok().map(|u| u.to_string())
        }
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
    /// PK as string (UUID for most tables, integer for the per-vehicle singletons).
    pub entity_id: String,
    pub operation: SyncOperation,
    #[serde(default)]
    pub payload: Value,
    /// The sync_version the client based this op on (0 for inserts).
    #[serde(default)]
    pub sync_version: i64,
    /// Client-minted idempotency key. Absent on clients ≤ v0.2.3.
    #[serde(default)]
    pub op_id: Option<Uuid>,
    /// Ops sharing a group_id (and adjacent in the batch) apply atomically.
    #[serde(default)]
    pub group_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PushResultStatus {
    Applied {
        entity_id: String,
        new_sync_version: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        op_id: Option<Uuid>,
    },
    Conflict {
        entity_id: String,
        server_row: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        op_id: Option<Uuid>,
    },
    Error {
        entity_id: String,
        /// Stable machine code (see `rules::codes`); clients translate it.
        #[serde(default)]
        code: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        op_id: Option<Uuid>,
    },
}

impl PushResultStatus {
    pub fn is_applied(&self) -> bool {
        matches!(self, Self::Applied { .. })
    }
}

#[derive(Debug, Deserialize)]
pub struct PushBody {
    pub operations: Vec<PushOperation>,
}

#[derive(Debug, Serialize)]
pub struct PushResponse {
    /// One result per op, in request order.
    pub results: Vec<PushResultStatus>,
}

#[derive(Debug, Deserialize)]
pub struct PullQuery {
    pub entity_type: EntityType,
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    /// PK (text) of the last row of the previous page; with `since` it forms
    /// the keyset cursor.
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
    pub next_cursor: Option<DateTime<Utc>>,
    pub next_cursor_id: Option<String>,
    pub has_more: bool,
}

/// Hard cap on ops per request. The client sends 50; anything far above that
/// is a bug or abuse, and one transaction holding the feed lock that long
/// would stall every other device.
pub const MAX_OPS_PER_PUSH: usize = 500;

/// What an op did, before it is turned into a wire result.
enum Outcome {
    Applied { new_sync_version: i64, stock_moved: bool },
    Conflict { server_row: Value },
    Rejected { code: &'static str, message: String },
}

fn rejected(code: &'static str, message: impl Into<String>) -> Outcome {
    Outcome::Rejected { code, message: message.into() }
}

fn wire(op: &PushOperation, entity_id: &str, o: &Outcome) -> PushResultStatus {
    match o {
        Outcome::Applied { new_sync_version, .. } => PushResultStatus::Applied {
            entity_id: entity_id.to_string(),
            new_sync_version: *new_sync_version,
            op_id: op.op_id,
        },
        Outcome::Conflict { server_row } => PushResultStatus::Conflict {
            entity_id: entity_id.to_string(),
            server_row: server_row.clone(),
            op_id: op.op_id,
        },
        Outcome::Rejected { code, message } => PushResultStatus::Error {
            entity_id: entity_id.to_string(),
            code: (*code).to_string(),
            message: message.clone(),
            op_id: op.op_id,
        },
    }
}

/// Apply a whole push batch. Returns the per-op results and whether any stock
/// movement was recorded (the caller mirrors those to Falcon after commit).
pub async fn apply_push_batch(
    pool: &PgPool,
    body: &PushBody,
    user_id: i64,
) -> ApiResult<(PushResponse, bool)> {
    if body.operations.len() > MAX_OPS_PER_PUSH {
        return Err(ApiError::BadRequest(format!(
            "too many operations in one push (max {MAX_OPS_PER_PUSH})"
        )));
    }
    let mut tx = pool.begin().await.map_err(transient_or_db)?;
    let mut results: Vec<PushResultStatus> = Vec::with_capacity(body.operations.len());
    let mut stock_moved = false;

    let ops = &body.operations;
    let mut i = 0;
    while i < ops.len() {
        // A unit = one op, or a run of adjacent ops sharing a group_id.
        let mut j = i + 1;
        if let Some(g) = ops[i].group_id {
            while j < ops.len() && ops[j].group_id == Some(g) {
                j += 1;
            }
        }
        let unit = &ops[i..j];

        // sqlx: Transaction::begin on a &mut Transaction issues a SAVEPOINT.
        let mut sp = tx.begin().await.map_err(transient_or_db)?;
        let mut unit_results: Vec<PushResultStatus> = Vec::with_capacity(unit.len());
        let mut unit_ok = true;
        let mut unit_moved = false;
        for op in unit {
            let entity_id = op
                .entity_type
                .canonical_id(&op.entity_id)
                .unwrap_or_else(|| op.entity_id.clone());
            let outcome = match apply_one(&mut sp, op, user_id).await {
                Ok(o) => o,
                Err(e) => match classify(e) {
                    Classified::Transient(e) => return Err(e),
                    Classified::Rejected(code, message) => rejected(code, message),
                },
            };
            if let Outcome::Applied { stock_moved: m, .. } = &outcome {
                unit_moved |= *m;
            } else {
                unit_ok = false;
            }
            unit_results.push(wire(op, &entity_id, &outcome));
            if !unit_ok {
                break;
            }
        }

        if unit_ok {
            sp.commit().await.map_err(transient_or_db)?;
            stock_moved |= unit_moved;
        } else {
            // Rolls back everything the unit wrote, and clears an aborted
            // subtransaction so the outer transaction stays healthy.
            sp.rollback().await.map_err(transient_or_db)?;
            if unit.len() > 1 {
                let failed_at = unit_results.len() - 1;
                let failed = unit_results.pop().expect("at least the failing result");
                unit_results.clear();
                for (k, op) in unit.iter().enumerate() {
                    if k == failed_at {
                        unit_results.push(failed.clone());
                    } else {
                        unit_results.push(PushResultStatus::Error {
                            entity_id: op.entity_id.clone(),
                            code: rules::codes::GROUP_ROLLED_BACK.to_string(),
                            message: "another change in the same action was refused".into(),
                            op_id: op.op_id,
                        });
                    }
                }
            }
        }
        results.extend(unit_results);
        i = j;
    }
    tx.commit().await.map_err(transient_or_db)?;
    Ok((PushResponse { results }, stock_moved))
}

enum Classified {
    Transient(ApiError),
    Rejected(&'static str, String),
}

/// SQLSTATEs that say "try again", not "this op is wrong".
fn is_transient_db(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Database(db) => matches!(
            db.code().as_deref(),
            Some("40001" | "40P01" | "55P03" | "57014" | "57P01" | "57P02" | "57P03" | "53300" | "08006" | "08003")
        ),
        sqlx::Error::Io(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => true,
        _ => false,
    }
}

fn transient_or_db(e: sqlx::Error) -> ApiError {
    if is_transient_db(&e) {
        tracing::warn!(error = %e, "transient database failure during push");
        ApiError::Unavailable("database busy, retry".into())
    } else {
        ApiError::Db(e)
    }
}

/// Turn a failure inside one op into either a whole-request retry or a per-op
/// rejection with a stable code and a message that leaks no SQL.
fn classify(e: ApiError) -> Classified {
    match e {
        ApiError::Db(db) if is_transient_db(&db) => {
            tracing::warn!(error = %db, "transient database failure during push");
            Classified::Transient(ApiError::Unavailable("database busy, retry".into()))
        }
        ApiError::Unavailable(m) => Classified::Transient(ApiError::Unavailable(m)),
        ApiError::Db(db) => {
            let (code, msg) = rules::explain_db_error(&db);
            if code == rules::codes::INTERNAL {
                tracing::error!(error = %db, "unexpected database error applying a sync op");
            }
            Classified::Rejected(code, msg)
        }
        ApiError::BadRequest(m) => Classified::Rejected(rules::codes::VALIDATION, m),
        ApiError::Conflict(m) => Classified::Rejected(rules::codes::CONFLICT, m),
        ApiError::NotFound(m) => Classified::Rejected(rules::codes::NOT_FOUND, m),
        ApiError::Rule { code, message } => Classified::Rejected(code, message),
        other => {
            tracing::error!(error = %other, "unexpected error applying a sync op");
            Classified::Rejected(rules::codes::INTERNAL, "internal error".into())
        }
    }
}

/// Apply one op inside the caller's savepoint.
async fn apply_one(
    tx: &mut Transaction<'_, Postgres>,
    op: &PushOperation,
    user_id: i64,
) -> ApiResult<Outcome> {
    let entity = op.entity_type;
    let Some(entity_id) = entity.canonical_id(&op.entity_id) else {
        return Ok(rejected(rules::codes::VALIDATION, "entity_id is not a valid key"));
    };

    // Exactly-once: an op we already applied returns what it returned then.
    if let Some(op_id) = op.op_id {
        let stored: Option<(Value,)> =
            sqlx::query_as("SELECT result FROM sync_applied_ops WHERE op_id = $1")
                .bind(op_id)
                .fetch_optional(&mut **tx)
                .await?;
        if let Some((v,)) = stored {
            let sv = v.get("new_sync_version").and_then(Value::as_i64).unwrap_or(0);
            return Ok(Outcome::Applied { new_sync_version: sv, stock_moved: false });
        }
    }

    if rules::json_has_nul(&op.payload) {
        return Ok(rejected(rules::codes::VALIDATION, "text contains an unsupported character"));
    }

    // Lock the row for the rest of the savepoint: the version check and the
    // write below see the same row no concurrent push can change in between.
    let row_now = fetch_row_json(&mut **tx, entity, &entity_id, true).await?;

    let outcome = match op.operation {
        SyncOperation::Insert => apply_insert(tx, op, &entity_id, row_now, user_id).await?,
        SyncOperation::Update => apply_update(tx, op, &entity_id, row_now, user_id).await?,
        SyncOperation::Delete => apply_delete(tx, op, &entity_id, row_now, user_id).await?,
    };

    if let (Some(op_id), Outcome::Applied { new_sync_version, .. }) = (op.op_id, &outcome) {
        sqlx::query(
            "INSERT INTO sync_applied_ops (op_id, user_id, entity_type, entity_id, result) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (op_id) DO NOTHING",
        )
        .bind(op_id)
        .bind(user_id)
        .bind(entity.table_name())
        .bind(&entity_id)
        .bind(json!({ "new_sync_version": new_sync_version }))
        .execute(&mut **tx)
        .await?;
    }
    Ok(outcome)
}

fn sv_of(row: &Value) -> i64 {
    row.get("sync_version").and_then(Value::as_i64).unwrap_or(0)
}

fn is_tombstone(row: &Value) -> bool {
    row.get("deleted_at").map(|v| !v.is_null()).unwrap_or(false)
}

async fn apply_insert(
    tx: &mut Transaction<'_, Postgres>,
    op: &PushOperation,
    entity_id: &str,
    row_now: Option<Value>,
    user_id: i64,
) -> ApiResult<Outcome> {
    let entity = op.entity_type;
    if let Some(row) = &row_now {
        if !entity.is_singleton() {
            // Pre-op_id clients: a redelivered insert carries sync_version 0
            // against a row at its DEFAULT 1 (or the version it already knew).
            let server_sv = sv_of(row);
            if op.op_id.is_none() && (op.sync_version == server_sv || (op.sync_version == 0 && server_sv == 1)) {
                // Those clients also fold later edits into a still-pending
                // insert. If the redelivery carries values the stored row
                // lacks, they are edits made after the first delivery: apply
                // them rather than acknowledge and lose them.
                if op.payload.is_object() && !is_tombstone(row) {
                    let mut edits = op.payload.clone();
                    if rules::normalize(entity, SyncOperation::Update, &mut edits, Some(row)).is_ok()
                        && !sql::payload_matches_row(entity, &edits, row)
                    {
                        return write_update(tx, entity, entity_id, &edits, row, user_id).await;
                    }
                }
                return Ok(Outcome::Applied { new_sync_version: server_sv, stock_moved: false });
            }
            return Ok(Outcome::Conflict { server_row: row.clone() });
        }
    }
    let Some(obj) = op.payload.as_object() else {
        return Ok(rejected(rules::codes::VALIDATION, "payload must be a JSON object"));
    };
    let mut payload = Value::Object(obj.clone());
    // The ack is keyed on entity_id, so the row MUST land under it.
    payload[entity.pk_column()] = Value::String(entity_id.to_string());
    if let Err(m) = rules::normalize(entity, SyncOperation::Insert, &mut payload, None) {
        return Ok(rejected(rules::codes::VALIDATION, m));
    }
    if let Some(reject) = side_effects::before_insert(tx, entity, &mut payload).await? {
        return Ok(rejected(reject.0, reject.1));
    }
    let new_sv = sql::insert_from_payload(tx, entity, &payload, user_id).await?;
    let moved =
        side_effects::after_apply(tx, entity, SyncOperation::Insert, entity_id, &payload, row_now.as_ref(), user_id)
            .await?;
    Ok(Outcome::Applied { new_sync_version: new_sv, stock_moved: moved })
}

async fn apply_update(
    tx: &mut Transaction<'_, Postgres>,
    op: &PushOperation,
    entity_id: &str,
    row_now: Option<Value>,
    user_id: i64,
) -> ApiResult<Outcome> {
    let entity = op.entity_type;
    let Some(row_now) = row_now else {
        return Ok(rejected(rules::codes::NOT_FOUND, "the record does not exist on the server"));
    };
    if !op.payload.is_object() {
        return Ok(rejected(rules::codes::VALIDATION, "payload must be a JSON object"));
    }
    let mut payload = op.payload.clone();
    if let Err(m) = rules::normalize(entity, SyncOperation::Update, &mut payload, Some(&row_now)) {
        return Ok(rejected(rules::codes::VALIDATION, m));
    }
    let server_sv = sv_of(&row_now);
    if server_sv != op.sync_version {
        // Pre-op_id clients: the update already applied (version moved by
        // exactly one and the stored row equals what was sent) — a lost ack,
        // not a conflict.
        if op.op_id.is_none()
            && server_sv == op.sync_version + 1
            && sql::payload_matches_row(entity, &payload, &row_now)
        {
            return Ok(Outcome::Applied { new_sync_version: server_sv, stock_moved: false });
        }
        return Ok(Outcome::Conflict { server_row: row_now });
    }
    if entity.has_deleted_at() && is_tombstone(&row_now) {
        return Ok(rejected(rules::codes::DELETED, "the record was deleted"));
    }
    write_update(tx, entity, entity_id, &payload, &row_now, user_id).await
}

/// The write half of an update: guards, the version-guarded UPDATE, side
/// effects. `payload` is already normalized; `row_now` is locked.
async fn write_update(
    tx: &mut Transaction<'_, Postgres>,
    entity: EntityType,
    entity_id: &str,
    payload: &Value,
    row_now: &Value,
    user_id: i64,
) -> ApiResult<Outcome> {
    let server_sv = sv_of(row_now);
    if let Some(reject) = side_effects::before_update(tx, entity, entity_id, payload, row_now).await? {
        return Ok(rejected(reject.0, reject.1));
    }
    match sql::update_from_payload(tx, entity, entity_id, payload, user_id, server_sv).await? {
        Some(new_sv) => {
            let moved = side_effects::after_apply(
                tx, entity, SyncOperation::Update, entity_id, payload, Some(row_now), user_id,
            )
            .await?;
            Ok(Outcome::Applied { new_sync_version: new_sv, stock_moved: moved })
        }
        // Unreachable while the row lock is held; kept as a correct fallback.
        None => match fetch_row_json(&mut **tx, entity, entity_id, false).await? {
            Some(server_row) => Ok(Outcome::Conflict { server_row }),
            None => Ok(rejected(rules::codes::NOT_FOUND, "the record does not exist on the server")),
        },
    }
}

async fn apply_delete(
    tx: &mut Transaction<'_, Postgres>,
    op: &PushOperation,
    entity_id: &str,
    row_now: Option<Value>,
    user_id: i64,
) -> ApiResult<Outcome> {
    let entity = op.entity_type;
    // Already gone: deleting nothing is a success.
    let Some(row_now) = row_now else {
        return Ok(Outcome::Applied { new_sync_version: op.sync_version, stock_moved: false });
    };
    if !entity.has_deleted_at() {
        return Ok(rejected(rules::codes::VALIDATION, "an odometer correction cannot be deleted; set a new one"));
    }
    let server_sv = sv_of(&row_now);
    if is_tombstone(&row_now) {
        // Deleted already (by this op's lost first delivery, or by another
        // device). Either way the client's intent holds.
        return Ok(Outcome::Applied { new_sync_version: server_sv, stock_moved: false });
    }
    if server_sv != op.sync_version {
        return Ok(Outcome::Conflict { server_row: row_now });
    }
    if let Some(reject) = side_effects::before_delete(tx, entity, entity_id, &row_now).await? {
        return Ok(rejected(reject.0, reject.1));
    }
    match sql::soft_delete(tx, entity, entity_id, user_id, server_sv).await? {
        Some(new_sv) => {
            let moved = side_effects::after_apply(
                tx, entity, SyncOperation::Delete, entity_id, &op.payload, Some(&row_now), user_id,
            )
            .await?;
            Ok(Outcome::Applied { new_sync_version: new_sv, stock_moved: moved })
        }
        None => match fetch_row_json(&mut **tx, entity, entity_id, false).await? {
            Some(server_row) => Ok(Outcome::Conflict { server_row }),
            None => Ok(Outcome::Applied { new_sync_version: op.sync_version, stock_moved: false }),
        },
    }
}

async fn fetch_row_json<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    entity: EntityType,
    pk_value: &str,
    for_update: bool,
) -> ApiResult<Option<Value>> {
    let table = entity.table_name();
    let pk = entity.pk_column();
    let cast = entity.pk_cast();
    let lock = if for_update { " FOR UPDATE" } else { "" };
    let q = format!("SELECT to_jsonb(t) AS row FROM {table} t WHERE {pk} = $1{cast}{lock}");
    let opt: Option<(Value,)> = sqlx::query_as(&q).bind(pk_value).fetch_optional(exec).await?;
    Ok(opt.map(|t| t.0))
}

/// The server's current row for one synced entity (soft-deleted rows
/// included). Used by GET /records/{entity_type}/{entity_id}.
pub async fn fetch_record(
    pool: &PgPool,
    entity: EntityType,
    entity_id: &str,
) -> ApiResult<Option<Value>> {
    let Some(id) = entity.canonical_id(entity_id) else {
        return Err(ApiError::BadRequest("entity_id is not a valid key".into()));
    };
    fetch_row_json(pool, entity, &id, false).await
}

/// Incremental pull with a composite (updated_at, pk) keyset. Soft-deleted
/// rows are included: clients need the tombstones.
pub async fn pull_rows(
    pool: &PgPool,
    entity: EntityType,
    since: DateTime<Utc>,
    after_id: Option<&str>,
    limit: i64,
) -> ApiResult<PullResponse> {
    let table = entity.table_name();
    let pk = entity.pk_column();
    let cast = entity.pk_cast();

    // A malformed after_id is ignored (falls back to the timestamp cursor)
    // rather than failing the pull forever.
    let after = after_id.and_then(|a| entity.canonical_id(a));
    let rows: Vec<(Value, DateTime<Utc>, String)> = if let Some(aid) = after {
        let q = format!(
            "SELECT to_jsonb(t) AS row, updated_at, {pk}::text AS pk_text
               FROM {table} t
              WHERE (updated_at, {pk}) > ($1, $2{cast})
              ORDER BY updated_at ASC, {pk} ASC
              LIMIT $3"
        );
        sqlx::query_as(&q).bind(since).bind(aid).bind(limit).fetch_all(pool).await?
    } else {
        let q = format!(
            "SELECT to_jsonb(t) AS row, updated_at, {pk}::text AS pk_text
               FROM {table} t
              WHERE updated_at > $1
              ORDER BY updated_at ASC, {pk} ASC
              LIMIT $2"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_ids() {
        let e = EntityType::Tires;
        assert_eq!(
            e.canonical_id("  3F2504E0-4F89-41D3-9A0C-0305E82C3301 ").as_deref(),
            Some("3f2504e0-4f89-41d3-9a0c-0305e82c3301")
        );
        assert!(e.canonical_id("12").is_none());
        let s = EntityType::VehicleOdometerOverrides;
        assert_eq!(s.canonical_id("12").as_deref(), Some("12"));
        assert!(s.canonical_id("0").is_none());
        assert!(s.canonical_id("-4").is_none());
        assert!(s.canonical_id("abc").is_none());
    }

    #[test]
    fn op_without_new_fields_still_parses() {
        // The wire shape of clients <= v0.2.3.
        let op: PushOperation = serde_json::from_value(json!({
            "entity_type": "tires", "entity_id": "x", "operation": "insert",
            "payload": {}, "sync_version": 0
        }))
        .unwrap();
        assert!(op.op_id.is_none() && op.group_id.is_none());
    }
}
