use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, FromRow)]
pub struct MaintenanceRecordRow {
    pub id: Uuid,
    pub template_id: Option<Uuid>,
    pub vehicle_id: i32,
    pub plate_number: String,
    pub category_id: String,
    pub performed_at: DateTime<Utc>,
    pub odometer_at_service: Option<i32>,
    pub next_due_at: Option<DateTime<Utc>>,
    pub next_due_km: Option<i32>,
    pub cost: Option<Decimal>,
    pub vendor: Option<String>,
    pub performed_by: Option<String>,
    pub notes: Option<String>,
    pub attachments: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_by_user_id: i64,
    pub updated_by_user_id: i64,
    pub sync_version: i64,
    // UI fields from join
    pub name_ar: Option<String>,
    pub name_en: Option<String>,
    pub category_icon: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRecordInput {
    pub id: Option<Uuid>,
    pub template_id: Option<Uuid>,
    pub vehicle_id: i32,
    pub category_id: String,
    pub performed_at: DateTime<Utc>,
    pub odometer_at_service: Option<i32>,
    pub cost: Option<Decimal>,
    pub vendor: Option<String>,
    pub performed_by: Option<String>,
    pub notes: Option<String>,
    #[serde(default = "empty_attachments")]
    pub attachments: serde_json::Value,
}

fn empty_attachments() -> serde_json::Value {
    serde_json::json!([])
}

#[derive(Debug, Deserialize)]
pub struct UpdateRecordInput {
    pub performed_at: Option<DateTime<Utc>>,
    pub odometer_at_service: Option<Option<i32>>,
    pub cost: Option<Option<Decimal>>,
    pub vendor: Option<Option<String>>,
    pub performed_by: Option<Option<String>>,
    pub notes: Option<Option<String>>,
    pub attachments: Option<serde_json::Value>,
    pub sync_version: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct DueRow {
    pub template_id: Uuid,
    pub vehicle_id: i32,
    pub plate_number: String,
    pub category_id: String,
    pub name_ar: String,
    pub name_en: String,
    pub trigger_type: super::template::MaintenanceTrigger,
    pub interval_km: Option<i32>,
    pub interval_days: Option<i32>,
    pub lead_warn_km: i32,
    pub lead_warn_days: i32,
    pub last_done_at: Option<DateTime<Utc>>,
    pub last_done_km: Option<i32>,
    pub next_due_at: Option<DateTime<Utc>>,
    pub next_due_km: Option<i32>,
    pub current_odometer: i32,
    pub odometer_source: String,
    pub status: String,
    pub category_icon: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpsertOverrideInput {
    pub vehicle_id: i32,
    pub odometer: i32,
    pub set_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    /// Optional sync_version; if missing on first upsert, server sets to 1
    pub sync_version: Option<i64>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct OverrideRow {
    pub vehicle_id: i32,
    pub odometer: i32,
    pub set_at: DateTime<Utc>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by_user_id: i64,
    pub updated_by_user_id: i64,
    pub sync_version: i64,
}
