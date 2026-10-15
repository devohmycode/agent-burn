//! How amounts read on screen. Values keep the engine's precision; only
//! their text is rounded, and the exact figure is one hover away.

use std::time::{Duration, SystemTime};

/// `$1,234.56`; amounts between zero and a cent read `<$0.01`.
pub fn money(value: f64) -> String {
    if value > 0.0 && value < 0.005 {
        return "<$0.01".into();
    }
    let cents = (value.abs() * 100.0).round() as u64;
    let sign = if value < 0.0 && cents > 0 { "-" } else { "" };
    format!("{sign}${}.{:02}", group(cents / 100), cents % 100)
}

/// The engine's exact amount, for tooltips.
pub fn exact_money(value: f64) -> String {
    format!("${value}")
}

/// `22.5B`, `1.2M`, `3.4K`, `950`.
pub fn tokens(value: u64) -> String {
    let value_f = value as f64;
    for (size, suffix) in [(1e12, "T"), (1e9, "B"), (1e6, "M"), (1e3, "K")] {
        if value_f >= size {
            let scaled = value_f / size;
            return if scaled >= 100.0 {
                format!("{scaled:.0}{suffix}")
            } else {
                format!("{scaled:.1}{suffix}")
            };
        }
    }
    value.to_string()
}

/// `37%`, `8.7%`.
pub fn percent(value: f64) -> String {
    if value.abs() < 10.0 && value.fract().abs() > f64::EPSILON {
        format!("{value:.1}%")
    } else {
        format!("{value:.0}%")
    }
}

/// `in 2 d 4 h`, `in 3 h 12 min`, `in 12 min`, `now`; past times read `passed`.
pub fn until(time: SystemTime, now: SystemTime) -> String {
    match time.duration_since(now) {
        Err(_) => "passed".into(),
        Ok(left) if left < Duration::from_secs(60) => "now".into(),
        Ok(left) => format!("in {}", duration(left)),
    }
}

/// `5 min`, `3 h 12 min`, `2 d 4 h`.
pub fn duration(span: Duration) -> String {
    let minutes = span.as_secs() / 60;
    let (days, hours, minutes) = (minutes / 1_440, minutes % 1_440 / 60, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{minutes} min"),
        (0, _) => format!("{hours} h {minutes} min"),
        _ => format!("{days} d {hours} h"),
    }
}

/// `5 min ago`, `just now`.
pub fn ago(time: SystemTime, now: SystemTime) -> String {
    match now.duration_since(time) {
        Ok(span) if span >= Duration::from_secs(60) => format!("{} ago", duration(span)),
        _ => "just now".into(),
    }
}

fn group(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_rounds_to_cents_and_groups_thousands() {
        assert_eq!(money(11_071.836), "$11,071.84");
        assert_eq!(money(0.3611375), "$0.36");
        assert_eq!(money(0.0), "$0.00");
        assert_eq!(money(0.004), "<$0.01");
        assert_eq!(money(-99.731175), "-$99.73");
        assert_eq!(exact_money(0.3611375), "$0.3611375");
    }

    #[test]
    fn tokens_use_short_scales() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(140_800), "141K");
        assert_eq!(tokens(57_300), "57.3K");
        assert_eq!(tokens(22_518_075_120), "22.5B");
        assert_eq!(tokens(417_704_206), "418M");
    }

    #[test]
    fn percentages_keep_a_decimal_below_ten() {
        assert_eq!(percent(58.0), "58%");
        assert_eq!(percent(8.7), "8.7%");
        assert_eq!(percent(3.0), "3%");
        assert_eq!(percent(110.249), "110%");
    }

    #[test]
    fn durations_read_in_the_two_largest_units() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let later = |seconds: u64| now + Duration::from_secs(seconds);

        assert_eq!(until(later(30), now), "now");
        assert_eq!(until(later(12 * 60 + 5), now), "in 12 min");
        assert_eq!(until(later(3 * 3_600 + 12 * 60), now), "in 3 h 12 min");
        assert_eq!(until(later(2 * 86_400 + 4 * 3_600), now), "in 2 d 4 h");
        assert_eq!(until(now - Duration::from_secs(5), now), "passed");
        assert_eq!(ago(now - Duration::from_secs(300), now), "5 min ago");
    }
}
