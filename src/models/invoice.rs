use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;

#[derive(Debug, Serialize, FromRow)]
#[allow(dead_code)]
pub struct ServiceInvoiceCacheRow {
    pub id: i32,
    pub car_id: i32,
    pub driver_name: Option<String>,
    pub date: Option<DateTime<Utc>>,
    pub meter_reading: Option<i32>,
    pub plate_number: Option<String>,
    pub supervisor: Option<String>,
    pub operating_region: Option<String>,
    pub raw_payload: serde_json::Value,
    pub source_created_at: Option<DateTime<Utc>>,
    pub source_updated_at: Option<DateTime<Utc>>,
    pub source_deleted_at: Option<DateTime<Utc>>,
    pub fetched_at: DateTime<Utc>,
}
