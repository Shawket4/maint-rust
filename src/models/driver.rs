use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::FromRow;

#[derive(Debug, Serialize, FromRow)]
#[allow(dead_code)]
pub struct DriverCacheRow {
    pub id: i32,
    pub name: String,
    pub mobile_number: Option<String>,
    pub transporter: Option<String>,
    pub id_license_expiration_date: Option<NaiveDate>,
    pub driver_license_expiration_date: Option<NaiveDate>,
    pub safety_license_expiration_date: Option<NaiveDate>,
    pub drug_test_expiration_date: Option<NaiveDate>,
    pub is_approved: bool,
    pub raw_payload: serde_json::Value,
    pub source_created_at: Option<DateTime<Utc>>,
    pub source_updated_at: Option<DateTime<Utc>>,
    pub fetched_at: DateTime<Utc>,
}
