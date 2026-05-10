use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "maintenance_trigger", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum MaintenanceTrigger {
    Mileage,
    Time,
}

#[derive(Debug, Serialize, FromRow)]
pub struct MaintenanceTemplateRow {
    pub id: Uuid,
    pub vehicle_id: i32,
    #[sqlx(default)]
    pub plate_number: Option<String>,
    pub category_id: String,
    pub name_ar: String,
    pub name_en: String,
    pub notes_ar: Option<String>,
    pub notes_en: Option<String>,
    pub trigger_type: MaintenanceTrigger,
    pub interval_km: Option<i32>,
    pub interval_days: Option<i32>,
    pub lead_warn_km: i32,
    pub lead_warn_days: i32,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_by_user_id: i64,
    pub updated_by_user_id: i64,
    pub sync_version: i64,
    // UI fields from join
    #[sqlx(default)]
    pub category_name_ar: Option<String>,
    #[sqlx(default)]
    pub category_name_en: Option<String>,
    #[sqlx(default)]
    pub category_icon: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTemplateInput {
    /// Optional: client-supplied UUID (offline-first). Server generates one if missing.
    pub id: Option<Uuid>,
    pub vehicle_id: i32,
    pub category_id: String,
    pub name_ar: String,
    pub name_en: String,
    pub notes_ar: Option<String>,
    pub notes_en: Option<String>,
    pub trigger_type: MaintenanceTrigger,
    pub interval_km: Option<i32>,
    pub interval_days: Option<i32>,
    #[serde(default = "default_lead_warn_km")]
    pub lead_warn_km: i32,
    #[serde(default = "default_lead_warn_days")]
    pub lead_warn_days: i32,
    #[serde(default = "default_is_active")]
    pub is_active: bool,
}

fn default_lead_warn_km() -> i32 {
    500
}
fn default_lead_warn_days() -> i32 {
    14
}
fn default_is_active() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct UpdateTemplateInput {
    pub name_ar: Option<String>,
    pub name_en: Option<String>,
    pub notes_ar: Option<Option<String>>,
    pub notes_en: Option<Option<String>>,
    pub trigger_type: Option<MaintenanceTrigger>,
    pub interval_km: Option<Option<i32>>,
    pub interval_days: Option<Option<i32>>,
    pub lead_warn_km: Option<i32>,
    pub lead_warn_days: Option<i32>,
    pub is_active: Option<bool>,
    pub sync_version: i64,
}
