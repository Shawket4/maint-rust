//! Garage attendance: the month read model for the office (apex-react).
//!
//! A day is one status (present, absent, leave, sick), and a present day
//! carries its check-in and check-out times as recorded: no working hours, no
//! lateness, no hour totals. The garage works every day, so a day nobody
//! marked is unmarked, never a day off. The same reading as the garage app's
//! `services/views/attendance.rs`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgPool;

use crate::error::{ApiError, ApiResult};

#[derive(Debug, Clone, Serialize)]
pub struct MonthDay {
    pub date: NaiveDate,
    pub day: u32,
    pub weekday: u32,
    pub is_today: bool,
    pub is_future: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Cell {
    /// present, absent, leave, sick, unmarked, future, outside
    pub kind: String,
    pub check_in_at: Option<DateTime<Utc>>,
    pub check_out_at: Option<DateTime<Utc>>,
    pub note: Option<String>,
    /// Who marked the day last.
    pub marked_by: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub present: i64,
    pub absent: i64,
    pub leave: i64,
    pub sick: i64,
    pub unmarked: i64,
}

impl Totals {
    fn add(&mut self, o: &Totals) {
        self.present += o.present;
        self.absent += o.absent;
        self.leave += o.leave;
        self.sick += o.sick;
        self.unmarked += o.unmarked;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MonthWorker {
    pub worker_id: String,
    pub name: String,
    pub job: Option<String>,
    pub cells: Vec<Cell>,
    pub totals: Totals,
}

#[derive(Debug, Clone, Serialize)]
pub struct Month {
    pub month: String,
    pub first: NaiveDate,
    pub last: NaiveDate,
    pub today: NaiveDate,
    pub days: Vec<MonthDay>,
    pub workers: Vec<MonthWorker>,
    pub totals: Totals,
}

pub fn month_bounds(month: &str) -> Option<(NaiveDate, NaiveDate)> {
    let first = NaiveDate::parse_from_str(&format!("{}-01", month.trim()), "%Y-%m-%d").ok()?;
    let next = if first.month() == 12 {
        NaiveDate::from_ymd_opt(first.year() + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1)?
    };
    Some((first, next.pred_opt()?))
}

/// (worker id, date, status, check-in, check-out, note, marked by)
type DayRow = (String, NaiveDate, String, Option<DateTime<Utc>>, Option<DateTime<Utc>>, Option<String>, Option<String>);

pub async fn month(pool: &PgPool, month: &str) -> ApiResult<Month> {
    let (first, last) = month_bounds(month).ok_or_else(|| ApiError::BadRequest("month must be YYYY-MM".into()))?;
    let today: NaiveDate = sqlx::query_scalar("SELECT (now() AT TIME ZONE 'Africa/Cairo')::date").fetch_one(pool).await?;

    let workers: Vec<(String, String, Option<String>, Option<NaiveDate>, Option<NaiveDate>)> = sqlx::query_as(
        "SELECT id::text, name, job, joined_on, left_on FROM attendance_workers
          WHERE deleted_at IS NULL ORDER BY sort_order, lower(name), id",
    )
    .fetch_all(pool)
    .await?;

    let days: Vec<DayRow> = sqlx::query_as(
        "SELECT d.worker_id::text, d.work_date, d.status::text, d.check_in_at, d.check_out_at, d.note, u.name
           FROM attendance_days d
           LEFT JOIN users_cache u ON u.id = d.updated_by_user_id
          WHERE d.deleted_at IS NULL AND d.work_date BETWEEN $1 AND $2",
    )
    .bind(first)
    .bind(last)
    .fetch_all(pool)
    .await?;

    Ok(build(month_label(first), first, last, today, &workers, &days))
}

fn month_label(first: NaiveDate) -> String {
    first.format("%Y-%m").to_string()
}

fn build(
    label: String,
    first: NaiveDate,
    last: NaiveDate,
    today: NaiveDate,
    workers: &[(String, String, Option<String>, Option<NaiveDate>, Option<NaiveDate>)],
    days: &[DayRow],
) -> Month {
    let marked: HashSet<&str> = days.iter().map(|d| d.0.as_str()).collect();
    let by_key: HashMap<(&str, NaiveDate), &DayRow> = days.iter().map(|d| ((d.0.as_str(), d.1), d)).collect();
    let dates: Vec<NaiveDate> = first.iter_days().take_while(|d| *d <= last).collect();
    let heads = dates
        .iter()
        .map(|d| MonthDay {
            date: *d,
            day: d.day(),
            weekday: d.weekday().number_from_monday(),
            is_today: *d == today,
            is_future: *d > today,
        })
        .collect();

    let mut all = Totals::default();
    let mut out = Vec::new();
    for (id, name, job, joined, left) in workers {
        let on = |d: NaiveDate| joined.map(|j| j <= d).unwrap_or(true) && left.map(|l| l >= d).unwrap_or(true);
        let overlaps = joined.map(|j| j <= last).unwrap_or(true) && left.map(|l| l >= first).unwrap_or(true);
        if !overlaps && !marked.contains(id.as_str()) {
            continue;
        }
        let mut totals = Totals::default();
        let cells = dates
            .iter()
            .map(|d| match by_key.get(&(id.as_str(), *d)) {
                Some(r) => {
                    match r.2.as_str() {
                        "present" => totals.present += 1,
                        "absent" => totals.absent += 1,
                        "leave" => totals.leave += 1,
                        "sick" => totals.sick += 1,
                        _ => {}
                    }
                    Cell { kind: r.2.clone(), check_in_at: r.3, check_out_at: r.4, note: r.5.clone(), marked_by: r.6.clone() }
                }
                None => {
                    let kind = if *d > today {
                        "future"
                    } else if !on(*d) {
                        "outside"
                    } else {
                        totals.unmarked += 1;
                        "unmarked"
                    };
                    Cell { kind: kind.into(), check_in_at: None, check_out_at: None, note: None, marked_by: None }
                }
            })
            .collect();
        all.add(&totals);
        out.push(MonthWorker { worker_id: id.clone(), name: name.clone(), job: job.clone(), cells, totals });
    }
    Month { month: label, first, last, today, days: heads, workers: out, totals: all }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn months_have_their_bounds() {
        assert_eq!(month_bounds("2026-02"), Some((d("2026-02-01"), d("2026-02-28"))));
        assert_eq!(month_bounds("2026-12"), Some((d("2026-12-01"), d("2026-12-31"))));
        assert_eq!(month_bounds("2026-13"), None);
    }

    #[test]
    fn every_day_is_a_working_day() {
        let workers = vec![("w".to_string(), "W".to_string(), None, None, None)];
        let days = vec![("w".to_string(), d("2026-01-10"), "present".to_string(), None, None, None, None)];
        let m = build("2026-01".into(), d("2026-01-01"), d("2026-01-31"), d("2026-01-31"), &workers, &days);
        let t = &m.workers[0].totals;
        assert_eq!((t.present, t.unmarked), (1, 30), "the five Fridays are unmarked days, not days off");
        assert_eq!(m.workers[0].cells[8].kind, "unmarked", "9 January 2026 is a Friday");
    }
}
