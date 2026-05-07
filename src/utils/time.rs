#![allow(dead_code)]
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Africa::Cairo;

/// Convert a UTC timestamp to Cairo wall-clock and back to a string.
/// All timestamps are stored UTC and rendered in Cairo locally.
pub fn to_cairo_str(t: DateTime<Utc>) -> String {
    t.with_timezone(&Cairo).format("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

pub fn cairo_now() -> DateTime<chrono_tz::Tz> {
    Cairo.from_utc_datetime(&Utc::now().naive_utc())
}
