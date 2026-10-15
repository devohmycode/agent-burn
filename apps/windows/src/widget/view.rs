//! What the widget shows: the quota of the chosen provider, as the saved
//! readings give it. The widget reads the same quota archive as the
//! dashboard and asks no provider anything.

use crate::{
    forecast,
    quota::{CLAUDE, CLAUDE_SESSION, CODEX, CURSOR, QuotaHistory},
    settings::Provider,
};

/// A reading older than this is shown as not current.
pub const STALE_AFTER_MS: i64 = 10 * 60_000;

#[derive(Clone, Debug, PartialEq)]
pub struct WidgetView {
    pub provider: Provider,
    pub rows: Vec<RowView>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RowView {
    /// `5h`, `7d` or `$`.
    pub label: &'static str,
    /// What the window is, for the tooltip.
    pub name: &'static str,
    pub state: RowState,
    /// The last failure to read it.
    pub failure: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RowState {
    /// Never read.
    Unknown,
    /// The window reset after the last reading: what is left now is not known.
    Reset { observed_at_ms: i64 },
    Read {
        remaining: f64,
        resets_at_ms: i64,
        observed_at_ms: i64,
        stale: bool,
    },
}

/// The windows of `provider`: series, row label and name.
fn windows(provider: Provider) -> &'static [(&'static str, &'static str, &'static str)] {
    match provider {
        Provider::Claude => &[
            (CLAUDE_SESSION, "5h", "5-hour session"),
            (CLAUDE, "7d", "Weekly limit"),
        ],
        Provider::Codex => &[(CODEX, "7d", "Weekly limit")],
        Provider::Cursor => &[(CURSOR, "$", "Promotional credits")],
    }
}

pub fn view(history: &QuotaHistory, provider: Provider, now_ms: i64) -> WidgetView {
    let rows = windows(provider)
        .iter()
        .map(|&(series, label, name)| {
            let readings = history
                .readings
                .get(series)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let state = match forecast::current(readings) {
                None => RowState::Unknown,
                Some(reading) if now_ms >= reading.resets_at_ms() => RowState::Reset {
                    observed_at_ms: reading.observed_at_ms,
                },
                Some(reading) => RowState::Read {
                    remaining: (100.0 - reading.window.used_percent).clamp(0.0, 100.0),
                    resets_at_ms: reading.resets_at_ms(),
                    observed_at_ms: reading.observed_at_ms,
                    stale: now_ms - reading.observed_at_ms > STALE_AFTER_MS,
                },
            };
            RowView {
                label,
                name,
                state,
                failure: history
                    .failures
                    .get(series)
                    .map(|failure| failure.message.clone()),
            }
        })
        .collect();
    WidgetView { provider, rows }
}

/// Time left as the widget writes it: the largest unit, `now` once passed.
pub fn short_duration(ms: i64) -> String {
    if ms <= 0 {
        return "now".into();
    }
    let minutes = ms / 60_000;
    if minutes >= 24 * 60 {
        format!("{}d", minutes / (24 * 60))
    } else if minutes >= 60 {
        format!("{}h", minutes / 60)
    } else {
        format!("{}m", minutes.max(1))
    }
}

impl RowView {
    /// The row's text: remaining share and time to reset.
    pub fn text(&self, now_ms: i64) -> String {
        match &self.state {
            RowState::Read {
                remaining,
                resets_at_ms,
                ..
            } => format!(
                "{remaining:.0}% · {}",
                short_duration(resets_at_ms - now_ms)
            ),
            RowState::Reset { .. } => "reset".into(),
            RowState::Unknown if self.failure.is_some() => "!".into(),
            RowState::Unknown => "--".into(),
        }
    }

    pub fn remaining(&self) -> Option<f64> {
        match self.state {
            RowState::Read { remaining, .. } => Some(remaining),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::quota::{Failure, QuotaReading, QuotaWindow};

    const NOW: i64 = 1_791_979_200_000;
    const MINUTE: i64 = 60_000;

    fn reading(series: &str, minutes_ago: i64, used: f64, minutes: f64, left: i64) -> QuotaReading {
        let observed = NOW - minutes_ago * MINUTE;
        let length = minutes * MINUTE as f64;
        let reset = NOW + left * MINUTE;
        QuotaReading {
            series: series.into(),
            observed_at_ms: observed,
            window: QuotaWindow {
                window_minutes: minutes,
                used_percent: used,
                elapsed_percent: (length - (reset - observed) as f64) / length * 100.0,
                api_equivalent_spent: 0.0,
            },
        }
    }

    fn history(readings: Vec<QuotaReading>) -> QuotaHistory {
        let mut by_series: BTreeMap<String, Vec<QuotaReading>> = BTreeMap::new();
        for reading in readings {
            by_series
                .entry(reading.series.clone())
                .or_default()
                .push(reading);
        }
        QuotaHistory {
            readings: by_series,
            failures: BTreeMap::new(),
        }
    }

    #[test]
    fn claude_shows_its_session_then_its_week() {
        let saved = history(vec![
            reading(CLAUDE_SESSION, 1, 40.0, 300.0, 130),
            reading(CLAUDE, 1, 25.0, 10_080.0, 3 * 24 * 60 + 5),
        ]);

        let shown = view(&saved, Provider::Claude, NOW);

        let texts: Vec<String> = shown.rows.iter().map(|row| row.text(NOW)).collect();
        assert_eq!(texts, ["60% · 2h", "75% · 3d"]);
        assert_eq!(shown.rows[0].label, "5h");
    }

    #[test]
    fn unknown_failed_reset_and_stale_rows_say_so() {
        let mut saved = history(vec![
            reading(CODEX, 30, 10.0, 10_080.0, 60),
            reading(CURSOR, 600, 10.0, 525_600.0, -5),
        ]);
        saved.failures.insert(
            CLAUDE.into(),
            Failure {
                message: "signed out".into(),
                at_ms: NOW,
            },
        );

        let codex = &view(&saved, Provider::Codex, NOW).rows[0];
        assert!(matches!(codex.state, RowState::Read { stale: true, .. }));
        assert_eq!(codex.text(NOW), "90% · 1h");
        assert_eq!(
            view(&saved, Provider::Cursor, NOW).rows[0].text(NOW),
            "reset"
        );
        let claude = view(&saved, Provider::Claude, NOW);
        assert_eq!(claude.rows[0].text(NOW), "--");
        assert_eq!(claude.rows[1].text(NOW), "!");
        assert_eq!(claude.rows[1].failure.as_deref(), Some("signed out"));
    }

    #[test]
    fn durations_keep_their_largest_unit() {
        assert_eq!(short_duration(0), "now");
        assert_eq!(short_duration(20_000), "1m");
        assert_eq!(short_duration(59 * MINUTE), "59m");
        assert_eq!(short_duration(61 * MINUTE), "1h");
        assert_eq!(short_duration(47 * 60 * MINUTE), "1d");
    }
}
