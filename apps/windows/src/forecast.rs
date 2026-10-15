//! Reading the quota history, after the macOS `QuotaHistory.swift` and
//! `Forecast`: which readings are consistent with their window, where the
//! windows reset, and a forecast only when the readings support one.

use crate::quota::{QuotaReading, SAME_WINDOW_MS};

/// Readings this far apart can still check each other.
const RECENT_MS: i64 = 30 * 60_000;
/// Within one window usage never falls: a later reading lower by more than
/// this proves the earlier one was over-counted.
const FALL_TOLERANCE: f64 = 1.0;
/// A rise of the remaining share above this is a reset.
const RESET_RISE: f64 = 1.0;

/// No forecast before this share of the window has run.
pub const MIN_ELAPSED_PERCENT: f64 = 10.0;
/// No forecast from a reading older than this.
pub const MAX_READING_AGE_MS: i64 = 30 * 60_000;
/// Readings of the window must cover at least this much time, or this
/// share of the window when it is shorter.
const MIN_SPAN_MS: i64 = 2 * 3_600_000;
const MIN_SPAN_SHARE: f64 = 0.10;

fn is_recent(reading: &QuotaReading, newest: &QuotaReading) -> bool {
    newest.observed_at_ms - reading.observed_at_ms <= RECENT_MS && reading.same_window(newest)
}

/// A weekly window cannot roughly double within half an hour: a reading
/// that far above the lowest recent one is an unconfirmed over-count.
fn is_over_count(reading: &QuotaReading, floor: f64) -> bool {
    reading.window.window_minutes >= 2.0 * 24.0 * 60.0
        && reading.window.used_percent > floor * 1.5 + 3.0
}

/// The newest reading safe to show as the live meter.
pub fn current(readings: &[QuotaReading]) -> Option<&QuotaReading> {
    let newest = readings.last()?;
    let recent: Vec<&QuotaReading> = readings
        .iter()
        .rev()
        .take_while(|reading| is_recent(reading, newest))
        .collect();
    let floor = recent
        .iter()
        .map(|reading| reading.window.used_percent)
        .fold(f64::INFINITY, f64::min);
    recent
        .into_iter()
        .find(|reading| !is_over_count(reading, floor))
        .or(Some(newest))
}

/// The readings to chart, oldest first. A reading whose window started with
/// an older, superseded window is a provider replay and is left out, so it
/// cannot carve a dip; a later, lower reading of the same window proves the
/// higher ones before it were over-counts, left out too; and a trailing
/// jump is held back as `current` holds it.
pub fn consistent(readings: &[QuotaReading]) -> Vec<QuotaReading> {
    let mut kept: Vec<(&QuotaReading, usize)> = Vec::new();
    let mut starts: Vec<i64> = Vec::new();
    for reading in readings {
        let start = reading.window_start_ms();
        let near = |other: &i64| (start - other).abs() <= SAME_WINDOW_MS;
        if starts.last().is_some_and(near) {
            kept.push((reading, starts.len() - 1));
        } else if starts.iter().any(near) {
            continue;
        } else {
            starts.push(start);
            kept.push((reading, starts.len() - 1));
        }
    }
    let mut lowest_later: Vec<Option<f64>> = vec![None; starts.len()];
    let mut newest_first = Vec::new();
    for (reading, segment) in kept.into_iter().rev() {
        let used = reading.window.used_percent;
        if lowest_later[segment].is_some_and(|floor| used > floor + FALL_TOLERANCE) {
            continue;
        }
        lowest_later[segment] = Some(lowest_later[segment].map_or(used, |floor| floor.min(used)));
        newest_first.push(reading.clone());
    }
    let mut chronological: Vec<QuotaReading> = newest_first.iter().rev().cloned().collect();
    if let Some(newest) = chronological.last().cloned() {
        let floor = newest_first
            .iter()
            .take_while(|reading| is_recent(reading, &newest))
            .map(|reading| reading.window.used_percent)
            .fold(f64::INFINITY, f64::min);
        while chronological
            .last()
            .is_some_and(|last| is_over_count(last, floor))
        {
            chronological.pop();
        }
    }
    chronological
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reset {
    pub at_ms: i64,
    /// At the end of the window, as planned; otherwise the provider reset
    /// it early or corrected it.
    pub scheduled: bool,
}

/// Where the remaining share rose between two consistent readings.
pub fn resets(consistent: &[QuotaReading]) -> Vec<Reset> {
    consistent
        .windows(2)
        .filter(|pair| pair[0].window.used_percent - pair[1].window.used_percent > RESET_RISE)
        .map(|pair| Reset {
            at_ms: pair[1].observed_at_ms,
            scheduled: pair[0].window.elapsed_percent >= 80.0
                && pair[1].window.elapsed_percent <= 20.0,
        })
        .collect()
}

/// The readings of the latest window only, after its last reset.
pub fn this_window(consistent: &[QuotaReading]) -> &[QuotaReading] {
    let start = consistent
        .windows(2)
        .rposition(|pair| {
            !pair[1].same_window(&pair[0])
                || pair[0].window.used_percent - pair[1].window.used_percent > RESET_RISE
        })
        .map_or(0, |index| index + 1);
    &consistent[start..]
}

/// Where the window would end at the pace of the readings so far.
#[derive(Clone, Debug, PartialEq)]
pub struct Forecast {
    pub latest: QuotaReading,
    pub start_ms: i64,
    pub reset_ms: i64,
    /// Share used at reset at this pace; may exceed 100.
    pub projected_use: f64,
    /// When the window runs out before its reset.
    pub runs_out_ms: Option<i64>,
}

impl Forecast {
    pub fn remaining_at_reset(&self) -> f64 {
        (100.0 - self.projected_use).max(0.0)
    }
}

/// A forecast, or why there is none.
#[derive(Clone, Debug, PartialEq)]
pub enum Projection {
    Shown(Forecast),
    Withheld(&'static str),
}

/// A forecast from the consistent readings of a series, at `now_ms`: only
/// when the latest window has run long enough, its readings are recent and
/// cover enough time, and something was used.
pub fn project(consistent: &[QuotaReading], now_ms: i64) -> Projection {
    let window = this_window(consistent);
    let Some(latest) = window.last() else {
        return Projection::Withheld("No reading recorded for this window yet.");
    };
    let reset_ms = latest.resets_at_ms();
    if now_ms >= reset_ms {
        return Projection::Withheld("The window has reset since the last reading.");
    }
    if now_ms - latest.observed_at_ms > MAX_READING_AGE_MS {
        return Projection::Withheld("The last reading is too old to project.");
    }
    let elapsed = latest.window.elapsed_percent;
    if elapsed < MIN_ELAPSED_PERCENT {
        return Projection::Withheld("Too early in the window to project.");
    }
    let length_ms = latest.window.window_minutes * 60_000.0;
    let span = latest.observed_at_ms - window[0].observed_at_ms;
    if window.len() < 2 || (span as f64) < (MIN_SPAN_MS as f64).min(length_ms * MIN_SPAN_SHARE) {
        return Projection::Withheld("Not enough readings in this window yet.");
    }
    let used = latest.window.used_percent;
    if used <= 0.0 {
        return Projection::Withheld("Nothing used in this window yet.");
    }
    let projected_use = used / (elapsed / 100.0);
    let start_ms = latest.window_start_ms();
    let runs_out_ms = (projected_use > 100.0)
        .then(|| start_ms + (length_ms * 100.0 / projected_use).round() as i64);
    Projection::Shown(Forecast {
        latest: latest.clone(),
        start_ms,
        reset_ms,
        projected_use,
        runs_out_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::QuotaWindow;

    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 60 * MINUTE;
    const WEEK_MINUTES: f64 = 10_080.0;
    /// A window starting at this time.
    const START: i64 = 1_791_000_000_000;

    /// A weekly reading `hours` into the window that started at `start`.
    fn at(start: i64, hours: f64, used: f64) -> QuotaReading {
        let observed = start + (hours * HOUR as f64) as i64;
        QuotaReading {
            series: "claude".into(),
            observed_at_ms: observed,
            window: QuotaWindow {
                window_minutes: WEEK_MINUTES,
                used_percent: used,
                elapsed_percent: (observed - start) as f64 / (WEEK_MINUTES * MINUTE as f64) * 100.0,
                api_equivalent_spent: 0.0,
            },
        }
    }

    fn used(readings: &[QuotaReading]) -> Vec<f64> {
        readings
            .iter()
            .map(|reading| reading.window.used_percent)
            .collect()
    }

    #[test]
    fn a_new_cycle_starts_after_a_reset_and_is_told_apart() {
        let next = START + 7 * 24 * HOUR;
        let readings = [
            at(START, 150.0, 70.0),
            at(START, 166.0, 82.0),
            at(next, 1.0, 2.0),
            at(next, 3.0, 5.0),
        ];
        let kept = consistent(&readings);

        assert_eq!(used(&kept), [70.0, 82.0, 2.0, 5.0]);
        let found = resets(&kept);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].at_ms, readings[2].observed_at_ms);
        assert!(found[0].scheduled);
        assert_eq!(used(this_window(&kept)), [2.0, 5.0]);
    }

    #[test]
    fn a_replay_of_a_superseded_window_is_left_out() {
        // An early reset after 100 hours, then the provider served the old
        // window once more.
        let next = START + 100 * HOUR;
        let replay = at(START, 101.0, 85.0);
        let readings = [
            at(START, 90.0, 80.0),
            at(next, 0.2, 1.0),
            replay,
            at(next, 1.5, 3.0),
        ];

        let kept = consistent(&readings);
        assert_eq!(used(&kept), [80.0, 1.0, 3.0]);
        let found = resets(&kept);
        assert_eq!(found.len(), 1);
        assert!(!found[0].scheduled, "before the end of the window");
    }

    #[test]
    fn readings_disproved_by_a_lower_later_one_are_over_counts() {
        // Codex served 99% while enforcing 49%.
        let readings = [
            at(START, 10.0, 40.0),
            at(START, 11.0, 99.0),
            at(START, 12.0, 49.0),
            at(START, 13.0, 50.0),
        ];
        assert_eq!(used(&consistent(&readings)), [40.0, 49.0, 50.0]);
        assert!(resets(&consistent(&readings)).is_empty());
    }

    #[test]
    fn a_trailing_jump_waits_for_confirmation() {
        let readings = [
            at(START, 10.0, 20.0),
            at(START, 10.2, 21.0),
            at(START, 10.4, 60.0),
        ];

        assert_eq!(used(&consistent(&readings)), [20.0, 21.0]);
        assert_eq!(current(&readings).unwrap().window.used_percent, 21.0);
        // A short window may move that fast.
        let mut session: Vec<QuotaReading> = readings.to_vec();
        for reading in &mut session {
            reading.window.window_minutes = 300.0;
        }
        assert_eq!(current(&session).unwrap().window.used_percent, 60.0);
    }

    #[test]
    fn an_outage_leaves_a_gap_and_invents_no_reading() {
        let readings = [at(START, 10.0, 20.0), at(START, 30.0, 35.0)];
        let kept = consistent(&readings);
        assert_eq!(kept, readings);
        assert!(resets(&kept).is_empty());
    }

    #[test]
    fn a_forecast_needs_enough_recent_readings_in_a_window_that_has_run() {
        let now = |reading: &QuotaReading| reading.observed_at_ms + 5 * MINUTE;
        // Nothing recorded.
        assert_eq!(
            project(&[], START),
            Projection::Withheld("No reading recorded for this window yet.")
        );
        // One reading only.
        let single = [at(START, 48.0, 20.0)];
        assert_eq!(
            project(&single, now(&single[0])),
            Projection::Withheld("Not enough readings in this window yet.")
        );
        // Too early: 10 hours of a week is under 10%.
        let early = [at(START, 6.0, 5.0), at(START, 10.0, 8.0)];
        assert_eq!(
            project(&early, now(&early[1])),
            Projection::Withheld("Too early in the window to project.")
        );
        // Readings too close together.
        let close = [at(START, 47.5, 19.0), at(START, 48.0, 20.0)];
        assert_eq!(
            project(&close, now(&close[1])),
            Projection::Withheld("Not enough readings in this window yet.")
        );
        // An old reading.
        let enough = [at(START, 40.0, 15.0), at(START, 48.0, 20.0)];
        assert_eq!(
            project(&enough, enough[1].observed_at_ms + 31 * MINUTE),
            Projection::Withheld("The last reading is too old to project.")
        );
        // Past the reset.
        assert_eq!(
            project(&enough, START + 7 * 24 * HOUR + MINUTE),
            Projection::Withheld("The window has reset since the last reading.")
        );
        // Nothing used.
        let idle = [at(START, 40.0, 0.0), at(START, 48.0, 0.0)];
        assert_eq!(
            project(&idle, now(&idle[1])),
            Projection::Withheld("Nothing used in this window yet.")
        );
    }

    #[test]
    fn a_forecast_follows_the_average_pace_of_the_window() {
        // 20% used after 48 of 168 hours: 70% at reset.
        let calm = [at(START, 40.0, 15.0), at(START, 48.0, 20.0)];
        let Projection::Shown(forecast) = project(&calm, calm[1].observed_at_ms) else {
            panic!("no forecast");
        };
        assert!((forecast.projected_use - 70.0).abs() < 1e-9);
        assert!((forecast.remaining_at_reset() - 30.0).abs() < 1e-9);
        assert_eq!(forecast.runs_out_ms, None);
        assert_eq!(forecast.reset_ms, START + 168 * HOUR);

        // 60% after 48 hours: empty after 80 hours.
        let fast = [at(START, 40.0, 50.0), at(START, 48.0, 60.0)];
        let Projection::Shown(forecast) = project(&fast, fast[1].observed_at_ms) else {
            panic!("no forecast");
        };
        assert_eq!(forecast.runs_out_ms, Some(START + 80 * HOUR));
        assert_eq!(forecast.remaining_at_reset(), 0.0);
    }

    #[test]
    fn a_forecast_ignores_the_window_before_a_reset() {
        let next = START + 7 * 24 * HOUR;
        let readings = [at(START, 160.0, 90.0), at(next, 20.0, 10.0)];
        // Only one reading in the new window.
        assert_eq!(
            project(&consistent(&readings), readings[1].observed_at_ms),
            Projection::Withheld("Not enough readings in this window yet.")
        );
    }
}
