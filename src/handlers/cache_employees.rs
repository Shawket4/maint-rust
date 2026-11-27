//! Falcon's employees, for the garage's attendance list. Only the id, name,
//! type and whether they are active leave this service: Falcon's employee
//! rows also carry phone numbers, loans and credit balances, none of which
//! the garage needs.
//!
//! Falcon lists employees at permission 3. A lower token gets an empty list
//! with `available: false`; the garage can still type workers in by name.

use actix_web::{get, web, HttpResponse};
use serde_json::{json, Value};

use crate::auth::middleware::BearerToken;
use crate::error::{ApiError, ApiResult};
use crate::handlers::AppState;

pub const FALCON_EMPLOYEES_PATH: &str = "/api/employees";

/// Falcon's employees, scrubbed and sorted by name.
pub fn project_employees(raw: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = raw
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|e| e.get("DeletedAt").map(Value::is_null).unwrap_or(true))
        .filter_map(|e| {
            let id = e.get("ID").or_else(|| e.get("id")).and_then(Value::as_i64).filter(|n| *n > 0 && *n <= i32::MAX as i64)?;
            let name = e.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty())?;
            Some(json!({
                "id": id,
                "name": name,
                "type": e.get("type").and_then(Value::as_str).map(str::trim).unwrap_or(""),
                "is_active": e.get("is_active").and_then(Value::as_bool).unwrap_or(true),
            }))
        })
        .collect();
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out
}

#[get("/cache/employees")]
async fn get_cache_employees(state: web::Data<AppState>, token: BearerToken) -> ApiResult<HttpResponse> {
    match state.falcon.get_json(FALCON_EMPLOYEES_PATH, &token.0).await {
        Ok(raw) => Ok(HttpResponse::Ok().json(json!({ "employees": project_employees(&raw), "available": true }))),
        // Falcon refuses a token below permission 3: not an error for the garage.
        Err(ApiError::UpstreamFalcon { status: 403, .. }) => {
            Ok(HttpResponse::Ok().json(json!({ "employees": [], "available": false })))
        }
        Err(e) => Err(e),
    }
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(get_cache_employees);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_four_fields_leave_and_deleted_or_nameless_rows_are_dropped() {
        let raw = json!([
            {"ID": 3, "name": " Zed ", "mobile_number": "0100", "type": "garage", "is_active": true, "loans": [{"amount": 5}]},
            {"ID": 4, "name": "Amr", "type": "office", "is_active": false, "DeletedAt": null},
            {"ID": 5, "name": "Gone", "DeletedAt": "2026-01-01T00:00:00Z"},
            {"ID": 6, "name": "  "},
            {"name": "No id"}
        ]);
        let e = project_employees(&raw);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0], json!({"id": 4, "name": "Amr", "type": "office", "is_active": false}));
        assert_eq!(e[1], json!({"id": 3, "name": "Zed", "type": "garage", "is_active": true}));
    }
}
