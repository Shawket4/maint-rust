use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "chassis_section", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ChassisSection {
    Tractor,
    Trailer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "axle_type", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum AxleType {
    Single,
    Dual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "position_side", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PositionSide {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "position_depth", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PositionDepth {
    Single,
    Inner,
    Outer,
}

#[derive(Debug, Serialize, FromRow)]
pub struct LayoutRow {
    pub id: Uuid,
    pub vehicle_id: i32,
    pub name_ar: Option<String>,
    pub name_en: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_by_user_id: i64,
    pub updated_by_user_id: i64,
    pub sync_version: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct AxleRow {
    pub id: Uuid,
    pub layout_id: Uuid,
    pub section: ChassisSection,
    pub section_index: i16,
    pub axle_type: AxleType,
    pub label_ar: Option<String>,
    pub label_en: Option<String>,
    pub is_steering: bool,
    pub is_lifted: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_by_user_id: i64,
    pub updated_by_user_id: i64,
    pub sync_version: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct PositionRow {
    pub id: Uuid,
    pub layout_id: Uuid,
    pub axle_id: Option<Uuid>,
    pub side: Option<PositionSide>,
    pub depth: Option<PositionDepth>,
    pub is_spare: bool,
    pub spare_index: Option<i16>,
    pub position_code: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_by_user_id: i64,
    pub updated_by_user_id: i64,
    pub sync_version: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateLayoutInput {
    pub id: Option<Uuid>,
    pub vehicle_id: i32,
    pub name_ar: Option<String>,
    pub name_en: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateLayoutInput {
    pub name_ar: Option<Option<String>>,
    pub name_en: Option<Option<String>>,
    pub sync_version: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateAxleInput {
    pub id: Option<Uuid>,
    pub layout_id: Uuid,
    pub section: ChassisSection,
    pub section_index: i16,
    pub axle_type: AxleType,
    pub label_ar: Option<String>,
    pub label_en: Option<String>,
    #[serde(default)]
    pub is_steering: bool,
    #[serde(default)]
    pub is_lifted: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAxleInput {
    pub section_index: Option<i16>,
    pub axle_type: Option<AxleType>,
    pub label_ar: Option<Option<String>>,
    pub label_en: Option<Option<String>>,
    pub is_steering: Option<bool>,
    pub is_lifted: Option<bool>,
    pub sync_version: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateSpareInput {
    pub id: Option<Uuid>,
    pub layout_id: Uuid,
    pub spare_index: i16,
}
