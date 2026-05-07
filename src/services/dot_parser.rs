//! DOT date-code parser per §9.3.
//!
//! 4-character WWYY string. WW ∈ [01,53], YY ∈ [00,99] → year = 2000 + YY.
//! production_date = ISO Monday of week WW year YYYY, ≤ today.

use chrono::{NaiveDate, Utc, Weekday};

#[derive(Debug)]
pub enum DotParseError {
    WrongLength,
    NotDigits,
    WeekOutOfRange,
    InvalidIsoDate,
    InFuture,
}

#[derive(Debug, Clone, Copy)]
pub struct DotDate {
    pub week: i16,
    pub year: i16,
    pub date: NaiveDate,
}

pub fn parse_dot_date_code(s: &str) -> Result<DotDate, DotParseError> {
    let s = s.trim();
    if s.len() != 4 {
        return Err(DotParseError::WrongLength);
    }
    if !s.chars().all(|c| c.is_ascii_digit()) {
        return Err(DotParseError::NotDigits);
    }
    let week: u32 = s[0..2].parse().map_err(|_| DotParseError::NotDigits)?;
    let yy: i32 = s[2..4].parse().map_err(|_| DotParseError::NotDigits)?;
    if !(1..=53).contains(&week) {
        return Err(DotParseError::WeekOutOfRange);
    }
    let year = 2000 + yy;
    let date = NaiveDate::from_isoywd_opt(year, week, Weekday::Mon)
        .ok_or(DotParseError::InvalidIsoDate)?;

    let today = Utc::now().date_naive();
    if date > today {
        return Err(DotParseError::InFuture);
    }

    Ok(DotDate {
        week: week as i16,
        year: year as i16,
        date,
    })
}

/// Compute the ISO-Monday production_date from week+year.
pub fn iso_week_monday(year: i32, week: u32) -> Option<NaiveDate> {
    NaiveDate::from_isoywd_opt(year, week, Weekday::Mon)
}

/// Validate (week, year) pair the same way the DOT parser would, but without a 4-char string.
pub fn validate_week_year(week: Option<i16>, year: Option<i16>) -> Result<(), DotParseError> {
    match (week, year) {
        (None, None) => Ok(()),
        (Some(w), Some(y)) => {
            if !(1..=53).contains(&(w as u32)) {
                return Err(DotParseError::WeekOutOfRange);
            }
            if !(2000..=2099).contains(&(y as i32)) {
                return Err(DotParseError::InvalidIsoDate);
            }
            let date = iso_week_monday(y as i32, w as u32).ok_or(DotParseError::InvalidIsoDate)?;
            if date > Utc::now().date_naive() {
                return Err(DotParseError::InFuture);
            }
            Ok(())
        }
        _ => Err(DotParseError::WrongLength), // §9.3: setting one requires both
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    #[test]
    fn parses_4523() {
        let d = parse_dot_date_code("4523").unwrap();
        assert_eq!(d.week, 45);
        assert_eq!(d.year, 2023);
        assert_eq!(d.date.weekday(), Weekday::Mon);
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(matches!(parse_dot_date_code("452"), Err(DotParseError::WrongLength)));
        assert!(matches!(parse_dot_date_code("45234"), Err(DotParseError::WrongLength)));
    }

    #[test]
    fn rejects_non_digits() {
        assert!(matches!(parse_dot_date_code("45ab"), Err(DotParseError::NotDigits)));
    }

    #[test]
    fn rejects_week_zero_or_too_high() {
        assert!(matches!(parse_dot_date_code("0023"), Err(DotParseError::WeekOutOfRange)));
        assert!(matches!(parse_dot_date_code("5423"), Err(DotParseError::WeekOutOfRange)));
    }
}
