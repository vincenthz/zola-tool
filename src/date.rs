//! A minimal, dependency-free calendar date (`YYYY-MM-DD`) with the arithmetic
//! the date picker needs: add days/weeks/months/years with correct rollover and
//! end-of-month clamping, plus weekday computation for laying out the grid.

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Date {
    pub year: i32,
    pub month: u32, // 1..=12
    pub day: u32,   // 1..=31
}

impl Date {
    /// Construct a date, validating it against the calendar.
    pub fn new(year: i32, month: u32, day: u32) -> Option<Date> {
        if (1..=12).contains(&month) && (1..=days_in_month(year, month)).contains(&day) {
            Some(Date { year, month, day })
        } else {
            None
        }
    }

    /// Parse a strict `YYYY-MM-DD` string.
    pub fn parse(s: &str) -> Option<Date> {
        let s = s.trim();
        if s.len() != 10 {
            return None;
        }
        let bytes = s.as_bytes();
        if bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        if bytes
            .iter()
            .enumerate()
            .any(|(i, c)| i != 4 && i != 7 && !c.is_ascii_digit())
        {
            return None;
        }
        // All ten bytes are ASCII now, so slicing is safe.
        Date::new(
            s[0..4].parse().ok()?,
            s[5..7].parse().ok()?,
            s[8..10].parse().ok()?,
        )
    }

    /// Today's date in UTC.
    pub fn today() -> Date {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0) as i64;
        let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
        Date {
            year: y as i32,
            month: m,
            day: d,
        }
    }

    pub fn to_iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    pub fn days_in_month(self) -> u32 {
        days_in_month(self.year, self.month)
    }

    /// Weekday, Monday = 0 .. Sunday = 6.
    pub fn weekday(self) -> u32 {
        let z = days_from_civil(self.year as i64, self.month, self.day);
        (z + 3).rem_euclid(7) as u32
    }

    pub fn add_days(self, n: i64) -> Date {
        let z = days_from_civil(self.year as i64, self.month, self.day) + n;
        let (y, m, d) = civil_from_days(z);
        Date {
            year: y as i32,
            month: m,
            day: d,
        }
    }

    /// Shift by whole months, clamping the day to the target month's length
    /// (e.g. Jan 31 + 1 month = Feb 28/29).
    pub fn add_months(self, n: i32) -> Date {
        let index = (self.month as i32 - 1) + n;
        let year = self.year + index.div_euclid(12);
        let month = index.rem_euclid(12) as u32 + 1;
        let day = self.day.min(days_in_month(year, month));
        Date { year, month, day }
    }

    pub fn add_years(self, n: i32) -> Date {
        let year = self.year + n;
        let day = self.day.min(days_in_month(year, self.month));
        Date {
            year,
            month: self.month,
            day,
        }
    }
}

pub fn month_name(month: u32) -> &'static str {
    const NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    NAMES
        .get((month.max(1) - 1) as usize)
        .copied()
        .unwrap_or("")
}

/// Abbreviated weekday name, Monday = 0.
pub fn weekday_short(weekday: u32) -> &'static str {
    const NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    NAMES.get(weekday as usize).copied().unwrap_or("")
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Inverse of `days_from_civil`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_validate() {
        assert_eq!(Date::parse("2020-01-26"), Date::new(2020, 1, 26));
        assert_eq!(Date::parse("2020-02-29"), Date::new(2020, 2, 29)); // leap
        assert!(Date::parse("2021-02-29").is_none()); // not leap
        assert!(Date::parse("2020-13-01").is_none());
        assert!(Date::parse("2020-04-31").is_none());
        assert!(Date::parse("2020-1-1").is_none()); // wrong shape
        assert!(Date::parse("nonsense!!").is_none());
    }

    #[test]
    fn civil_round_trip() {
        // 2020-01-26 is 18287 days after the epoch.
        assert_eq!(civil_from_days(18_287), (2020, 1, 26));
        assert_eq!(days_from_civil(2020, 1, 26), 18_287);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn weekday_is_monday_zero() {
        // 1970-01-01 was a Thursday (index 3 with Monday = 0).
        assert_eq!(Date::new(1970, 1, 1).unwrap().weekday(), 3);
        // 2026-07-24 is a Friday (index 4).
        assert_eq!(Date::new(2026, 7, 24).unwrap().weekday(), 4);
    }

    #[test]
    fn add_days_rolls_over_months_and_years() {
        assert_eq!(
            Date::new(2020, 1, 31).unwrap().add_days(1),
            Date::new(2020, 2, 1).unwrap()
        );
        assert_eq!(
            Date::new(2020, 12, 31).unwrap().add_days(1),
            Date::new(2021, 1, 1).unwrap()
        );
        assert_eq!(
            Date::new(2020, 3, 1).unwrap().add_days(-1),
            Date::new(2020, 2, 29).unwrap()
        );
        assert_eq!(
            Date::new(2026, 7, 24).unwrap().add_days(7),
            Date::new(2026, 7, 31).unwrap()
        );
    }

    #[test]
    fn add_months_clamps_day() {
        assert_eq!(
            Date::new(2020, 1, 31).unwrap().add_months(1),
            Date::new(2020, 2, 29).unwrap()
        );
        assert_eq!(
            Date::new(2021, 1, 31).unwrap().add_months(1),
            Date::new(2021, 2, 28).unwrap()
        );
        assert_eq!(
            Date::new(2020, 12, 15).unwrap().add_months(1),
            Date::new(2021, 1, 15).unwrap()
        );
        assert_eq!(
            Date::new(2020, 1, 15).unwrap().add_months(-1),
            Date::new(2019, 12, 15).unwrap()
        );
    }

    #[test]
    fn add_years_clamps_leap_day() {
        assert_eq!(
            Date::new(2020, 2, 29).unwrap().add_years(1),
            Date::new(2021, 2, 28).unwrap()
        );
        assert_eq!(
            Date::new(2020, 2, 29).unwrap().add_years(4),
            Date::new(2024, 2, 29).unwrap()
        );
    }
}
