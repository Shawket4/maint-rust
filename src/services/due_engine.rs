use chrono::{DateTime, Duration, Utc};

use crate::models::template::{MaintenanceTemplateRow, MaintenanceTrigger};

/// Given a template and the moment-of-service (performed_at, odometer_at_service),
/// compute (next_due_km, next_due_at).
///
/// Per §8.1: a template is pure mileage OR pure time. The non-active dimension is null.
pub fn compute_next_due(
    template: &MaintenanceTemplateRow,
    performed_at: DateTime<Utc>,
    odometer_at_service: Option<i32>,
) -> (Option<i32>, Option<DateTime<Utc>>) {
    match template.trigger_type {
        MaintenanceTrigger::Mileage => {
            let next_km = match (odometer_at_service, template.interval_km) {
                (Some(o), Some(i)) => Some(o.saturating_add(i)),
                _ => None,
            };
            (next_km, None)
        }
        MaintenanceTrigger::Time => {
            let next_at = template
                .interval_days
                .map(|days| performed_at + Duration::days(days as i64));
            (None, next_at)
        }
    }
}
