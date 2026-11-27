//! Garage attendance: the month read model for the office (apex-react).
//!
//! The same rule as the garage app's `services/views/attendance.rs`: late
//! counts from the start of the working day once past the grace (never on a
//! weekly day off), worked = out − in, extra = worked beyond the working day.
//! Cairo wall-clock times come from Postgres (`AT TIME ZONE 'Africa/Cairo'`),
//! so summer time is handled there, never with a fixed offset.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde::Serialize;
use sqlx::PgPool;

use crate::error::{ApiError, ApiResult};

pub const DEFAULT_WORK_START: &str = "08:00";
pub const DEFAULT_WORK_END: &str = "17:00";
pub const DEFAULT_GRACE_MINUTES: i64 = 15;
pub const DEFAULT_WEEKLY_OFF: &[i64] = &[5];

#[derive(Debug, Clone, Serialize)]
pub struct Settings {
    pub work_start: String,
    pub work_end: String,
    pub late_grace_minutes: i64,
    pub weekly_off_days: Vec<i64>,
    /// False until someone saves the hours in the garage app.
    pub saved: bool,
}

impl Settings {
    fn defaults() -> Self {
        Self {
            work_start: DEFAULT_WORK_START.into(),
            work_end: DEFAULT_WORK_END.into(),
            late_grace_minutes: DEFAULT_GRACE_MINUTES,
            weekly_off_days: DEFAULT_WEEKLY_OFF.to_vec(),
            saved: false,
        }
    }
    fn start(&self) -> NaiveTime {
        NaiveTime::parse_from_str(&self.work_start, "%H:%M").unwrap_or(NaiveTime::from_hms_opt(8, 0, 0).unwrap())
    }
    fn end(&self) -> NaiveTime {
        NaiveTime::parse_from_str(&self.work_end, "%H:%M").unwrap_or(NaiveTime::from_hms_opt(17, 0, 0).unwrap())
    }
    fn day_minutes(&self) -> i64 {
        let m = (self.end() - self.start()).num_minutes();
        if m <= 0 { m + 24 * 60 } else { m }
    }
    pub fn is_off(&self, d: NaiveDate) -> bool {
        self.weekly_off_days.contains(&(d.weekday().number_from_monday() as i64))
    }
}

/// (late minutes, worked minutes, extra minutes) of one marked day, from Cairo
/// wall-clock times.
pub fn figures(
    s: &Settings,
    work_date: NaiveDate,
    in_local: Option<NaiveDateTime>,
    out_local: Option<NaiveDateTime>,
) -> (i64, Option<i64>, i64) {
    let late = in_local
        .filter(|_| !s.is_off(work_date))
        .map(|i| (i - work_date.and_time(s.start())).num_minutes())
        .filter(|m| *m > s.late_grace_minutes)
        .unwrap_or(0);
    let worked = match (in_local, out_local) {
        (Some(i), Some(o)) if o > i => Some((o - i).num_minutes()),
        _ => None,
    };
    let extra = worked.map(|w| (w - s.day_minutes()).max(0)).unwrap_or(0);
    (late, worked, extra)
}

#[derive(Debug, Clone, Serialize)]
pub struct MonthDay {
    pub date: NaiveDate,
    pub day: u32,
    pub weekday: u32,
    pub is_weekly_off: bool,
    pub is_today: bool,
    pub is_future: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Cell {
    /// present, late, absent, leave, sick, off, weekly_off, unmarked, future, outside
    pub kind: String,
    pub check_in_at: Option<DateTime<Utc>>,
    pub check_out_at: Option<DateTime<Utc>>,
    pub note: Option<String>,
    pub late_minutes: i64,
    pub worked_minutes: Option<i64>,
    /// Who marked the day last.
    pub marked_by: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub present: i64,
    pub late: i64,
    pub late_minutes: i64,
    pub absent: i64,
    pub leave: i64,
    pub sick: i64,
    pub off: i64,
    pub unmarked: i64,
    pub worked_minutes: i64,
    pub extra_minutes: i64,
}

impl Totals {
    fn add(&mut self, o: &Totals) {
        self.present += o.present;
        self.late += o.late;
        self.late_minutes += o.late_minutes;
        self.absent += o.absent;
        self.leave += o.leave;
        self.sick += o.sick;
        self.off += o.off;
        self.unmarked += o.unmarked;
        self.worked_minutes += o.worked_minutes;
        self.extra_minutes += o.extra_minutes;
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
    pub settings: Settings,
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

type DayRow = (
    String,
    NaiveDate,
    String,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<NaiveDateTime>,
    Option<NaiveDateTime>,
    Option<String>,
    Option<String>,
);

pub async fn month(pool: &PgPool, month: &str) -> ApiResult<Month> {
    let (first, last) = month_bounds(month).ok_or_else(|| ApiError::BadRequest("month must be YYYY-MM".into()))?;
    let today: NaiveDate = sqlx::query_scalar("SELECT (now() AT TIME ZONE 'Africa/Cairo')::date").fetch_one(pool).await?;

    let settings = sqlx::query_as::<_, (String, String, i32, String)>(
        "SELECT work_start, work_end, late_grace_minutes, weekly_off_days FROM attendance_settings
          WHERE id = 1 AND deleted_at IS NULL",
    )
    .fetch_optional(pool)
    .await?
    .map(|(s, e, g, off)| Settings {
        work_start: s,
        work_end: e,
        late_grace_minutes: g as i64,
        weekly_off_days: off.split(',').filter_map(|d| d.trim().parse().ok()).collect(),
        saved: true,
    })
    .unwrap_or_else(Settings::defaults);

    let workers: Vec<(String, String, Option<String>, Option<NaiveDate>, Option<NaiveDate>)> = sqlx::query_as(
        "SELECT id::text, name, job, joined_on, left_on FROM attendance_workers
          WHERE deleted_at IS NULL ORDER BY sort_order, lower(name), id",
    )
    .fetch_all(pool)
    .await?;

    let days: Vec<DayRow> = sqlx::query_as(
        "SELECT d.worker_id::text, d.work_date, d.status::text, d.check_in_at, d.check_out_at,
                d.check_in_at AT TIME ZONE 'Africa/Cairo', d.check_out_at AT TIME ZONE 'Africa/Cairo',
                d.note, u.name
           FROM attendance_days d
           LEFT JOIN users_cache u ON u.id = d.updated_by_user_id
          WHERE d.deleted_at IS NULL AND d.work_date BETWEEN $1 AND $2",
    )
    .bind(first)
    .bind(last)
    .fetch_all(pool)
    .await?;

    Ok(build(month_label(first), first, last, today, settings, &workers, &days))
}

fn month_label(first: NaiveDate) -> String {
    first.format("%Y-%m").to_string()
}

fn build(
    label: String,
    first: NaiveDate,
    last: NaiveDate,
    today: NaiveDate,
    settings: Settings,
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
            is_weekly_off: settings.is_off(*d),
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
                    let (late, worked, extra) = figures(&settings, *d, r.5, r.6);
                    match r.2.as_str() {
                        "present" => {
                            totals.present += 1;
                            if late > 0 {
                                totals.late += 1;
                                totals.late_minutes += late;
                            }
                            totals.worked_minutes += worked.unwrap_or(0);
                            totals.extra_minutes += extra;
                        }
                        "absent" => totals.absent += 1,
                        "leave" => totals.leave += 1,
                        "sick" => totals.sick += 1,
                        "off" => totals.off += 1,
                        _ => {}
                    }
                    Cell {
                        kind: if r.2 == "present" && late > 0 { "late".into() } else { r.2.clone() },
                        check_in_at: r.3,
                        check_out_at: r.4,
                        note: r.7.clone(),
                        late_minutes: late,
                        worked_minutes: worked,
                        marked_by: r.8.clone(),
                    }
                }
                None => {
                    let kind = if *d > today {
                        "future"
                    } else if !on(*d) {
                        "outside"
                    } else if settings.is_off(*d) {
                        "weekly_off"
                    } else {
                        totals.unmarked += 1;
                        "unmarked"
                    };
                    Cell {
                        kind: kind.into(),
                        check_in_at: None,
                        check_out_at: None,
                        note: None,
                        late_minutes: 0,
                        worked_minutes: None,
                        marked_by: None,
                    }
                }
            })
            .collect();
        all.add(&totals);
        out.push(MonthWorker { worker_id: id.clone(), name: name.clone(), job: job.clone(), cells, totals });
    }
    Month { month: label, first, last, today, settings, days: heads, workers: out, totals: all }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }
    fn t(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }
    fn s() -> Settings {
        Settings { saved: true, ..Settings::defaults() }
    }

    #[test]
    fn late_counts_from_the_start_past_the_grace_and_never_on_the_day_off() {
        assert_eq!(figures(&s(), d("2026-01-10"), Some(t("2026-01-10 08:15")), None).0, 0);
        assert_eq!(figures(&s(), d("2026-01-10"), Some(t("2026-01-10 08:16")), None).0, 16);
        assert_eq!(figures(&s(), d("2026-01-09"), Some(t("2026-01-09 12:00")), None).0, 0, "Friday");
    }

    #[test]
    fn worked_and_extra_cross_midnight() {
        let (_, w, e) = figures(&s(), d("2026-01-10"), Some(t("2026-01-10 08:00")), Some(t("2026-01-11 00:30")));
        assert_eq!(w, Some(16 * 60 + 30));
        assert_eq!(e, 16 * 60 + 30 - 9 * 60);
    }

    #[test]
    fn months_have_their_bounds() {
        assert_eq!(month_bounds("2026-02"), Some((d("2026-02-01"), d("2026-02-28"))));
        assert_eq!(month_bounds("2026-12"), Some((d("2026-12-01"), d("2026-12-31"))));
        assert_eq!(month_bounds("2026-13"), None);
    }
}
