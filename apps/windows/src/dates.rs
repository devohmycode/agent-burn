//! Calendar days, as the engine groups usage (`YYYY-MM-DD`).

use std::fmt;

use windows::Win32::System::SystemInformation::GetLocalTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl Date {
    pub fn new(year: i32, month: u8, day: u8) -> Option<Self> {
        ((1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month))
            .then_some(Self { year, month, day })
    }

    /// `YYYY-MM-DD` or `YYYYMMDD`, the forms the engine accepts.
    pub fn parse(text: &str) -> Option<Self> {
        let digits: String = text.trim().chars().filter(|c| *c != '-').collect();
        let dashed = text.trim().len() == 10;
        if digits.len() != 8 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        if dashed && (text.trim().as_bytes()[4] != b'-' || text.trim().as_bytes()[7] != b'-') {
            return None;
        }
        Self::new(
            digits[..4].parse().ok()?,
            digits[4..6].parse().ok()?,
            digits[6..].parse().ok()?,
        )
    }

    /// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
    pub fn days(self) -> i64 {
        let year = i64::from(self.year) - i64::from(self.month <= 2);
        let era = year.div_euclid(400);
        let yoe = year - era * 400;
        let month = i64::from(self.month);
        let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5
            + i64::from(self.day)
            - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// The day `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
    pub fn from_days(days: i64) -> Self {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Self {
            year: year as i32,
            month: month as u8,
            day: day as u8,
        }
    }

    #[must_use]
    pub fn add_days(self, days: i64) -> Self {
        Self::from_days(self.days() + days)
    }

    /// 0 for Monday … 6 for Sunday.
    pub fn weekday_from_monday(self) -> i64 {
        // 1970-01-01 was a Thursday.
        (self.days() + 3).rem_euclid(7)
    }

    /// Today in the Windows time zone, the engine's default grouping zone.
    pub fn today() -> Self {
        let now = unsafe { GetLocalTime() };
        Self::new(now.wYear as i32, now.wMonth as u8, now.wDay as u8).unwrap_or(Self::from_days(0))
    }
}

impl fmt::Display for Date {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02}",
            self.year, self.month, self.day
        )
    }
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Every day from `first` to `last`, both included.
pub fn days_between(first: Date, last: Date) -> impl Iterator<Item = Date> {
    (first.days()..=last.days()).map(Date::from_days)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_engine_forms() {
        let date = Date::new(2026, 10, 13).unwrap();

        assert_eq!(Date::parse("2026-10-13"), Some(date));
        assert_eq!(Date::parse("20261013"), Some(date));
        assert_eq!(date.to_string(), "2026-10-13");
    }

    #[test]
    fn rejects_impossible_dates() {
        assert_eq!(Date::parse("2026-02-29"), None);
        assert_eq!(Date::parse("2026-13-01"), None);
        assert_eq!(Date::parse("2026/10/13"), None);
        assert_eq!(Date::parse("26-10-13"), None);
        assert!(Date::parse("2028-02-29").is_some());
    }

    #[test]
    fn day_numbers_round_trip() {
        for days in [-1, 0, 59, 365, 20_739, 50_000] {
            assert_eq!(Date::from_days(days).days(), days);
        }
        assert_eq!(Date::new(1970, 1, 1).unwrap().days(), 0);
        assert_eq!(Date::new(2026, 10, 13).unwrap().days(), 20_739);
    }

    #[test]
    fn weekdays_start_on_monday() {
        assert_eq!(Date::new(2026, 10, 12).unwrap().weekday_from_monday(), 0);
        assert_eq!(Date::new(2026, 10, 18).unwrap().weekday_from_monday(), 6);
    }

    #[test]
    fn ranges_include_both_ends() {
        let days: Vec<Date> = days_between(
            Date::new(2026, 2, 27).unwrap(),
            Date::new(2026, 3, 2).unwrap(),
        )
        .collect();

        assert_eq!(days.len(), 4);
        assert_eq!(days[2], Date::new(2026, 3, 1).unwrap());
    }
}
