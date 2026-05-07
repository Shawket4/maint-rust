use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "mount_reason", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MountReason {
    NewInstall,
    Rotation,
    Replacement,
    Puncture,
    Wear,
    Damage,
    RetreadReturn,
}

#[derive(Debug, Serialize, FromRow)]
pub struct AssignmentRow {
    pub id: Uuid,
    pub tire_id: Uuid,
    pub position_id: Uuid,
    pub vehicle_id: i32,
    pub mounted_at: DateTime<Utc>,
    pub mounted_odometer: Option<i32>,
    pub dismounted_at: Option<DateTime<Utc>>,
    pub dismounted_odometer: Option<i32>,
    pub mount_reason: MountReason,
    pub dismount_reason: Option<MountReason>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_by_user_id: i64,
    pub updated_by_user_id: i64,
    pub sync_version: i64,
}

#[derive(Debug, Deserialize)]
pub struct MountInput {
    pub id: Option<Uuid>,
    pub position_id: Uuid,
    pub mounted_at: DateTime<Utc>,
    pub mounted_odometer: Option<i32>,
    pub mount_reason: MountReason,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DismountDestination {
    InStock,
    InRepair,
    Retreading,
    Scrapped,
}

#[derive(Debug, Deserialize)]
pub struct DismountInput {
    pub dismounted_at: DateTime<Utc>,
    pub dismounted_odometer: Option<i32>,
    pub dismount_reason: MountReason,
    pub destination: DismountDestination,
    pub scrap_reason: Option<String>,
    pub stock_location: Option<String>,
    pub notes: Option<String>,
}
