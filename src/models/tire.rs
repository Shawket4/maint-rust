use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "tire_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TireStatus {
    InStock,
    Mounted,
    InRepair,
    Retreading,
    Scrapped,
}

#[derive(Debug, Serialize, FromRow)]
pub struct TireRow {
    pub id: Uuid,
    pub dot_code: String,
    pub internal_serial: Option<String>,
    pub brand: String,
    pub model: Option<String>,
    pub purchase_date: Option<NaiveDate>,
    pub purchase_cost: Option<Decimal>,
    pub supplier: Option<String>,
    pub production_week: Option<i16>,
    pub production_year: Option<i16>,
    pub production_date: Option<NaiveDate>,
    pub is_retread: bool,
    pub retread_count: i16,
    pub parent_tire_id: Option<Uuid>,
    pub status: TireStatus,
    pub stock_location: Option<String>,
    pub scrap_reason: Option<String>,
    pub scrap_date: Option<NaiveDate>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_by_user_id: i64,
    pub updated_by_user_id: i64,
    pub sync_version: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateTireInput {
    pub id: Option<Uuid>,
    pub dot_code: String,
    pub internal_serial: Option<String>,
    pub brand: String,
    pub model: Option<String>,
    pub purchase_date: Option<NaiveDate>,
    pub purchase_cost: Option<Decimal>,
    pub supplier: Option<String>,
    /// 4-character WWYY string. Optional. If present and well-formed, server will
    /// derive (production_week, production_year, production_date).
    pub dot_date_code: Option<String>,
    /// Caller may also pass these explicitly; if both present they override the parsed values.
    pub production_week: Option<i16>,
    pub production_year: Option<i16>,
    #[serde(default)]
    pub is_retread: bool,
    #[serde(default)]
    pub retread_count: i16,
    pub parent_tire_id: Option<Uuid>,
    pub stock_location: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTireInput {
    pub dot_code: Option<String>,
    pub internal_serial: Option<Option<String>>,
    pub brand: Option<String>,
    pub model: Option<Option<String>>,
    pub purchase_date: Option<Option<NaiveDate>>,
    pub purchase_cost: Option<Option<Decimal>>,
    pub supplier: Option<Option<String>>,
    pub dot_date_code: Option<Option<String>>,
    pub production_week: Option<Option<i16>>,
    pub production_year: Option<Option<i16>>,
    pub is_retread: Option<bool>,
    pub retread_count: Option<i16>,
    pub stock_location: Option<Option<String>>,
    pub notes: Option<Option<String>>,
    pub sync_version: i64,
}
