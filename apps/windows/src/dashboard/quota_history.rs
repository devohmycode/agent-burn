//! The quota history: for each series, the readings collected, the
//! window resets, the even pace and a forecast only when the readings
//! support one. A forecast is a projection of recorded readings, never a
//! limit reported by the provider, and says so.

use std::time::Duration;

use eframe::egui;

use crate::{
    dashboard::{clock, format},
    forecast::{self, Projection},
    quota::{self, CLAUDE, CLAUDE_SESSION, CODEX, CURSOR, QuotaHistory, QuotaReading},
    ui::{
        components::{
            fluent::{card, section, semibold},
            quota_chart::{self, Chart},
        },
        series,
        theme::{caution, danger, is_light, menu_text, muted},
    },
};

/// Readings further apart than this leave a gap on the chart.
const GAP_MS: i64 = 30 * 60_000;
const DAY_MS: i64 = 86_400_000;
/// The series, in the order of the cards.
const SERIES: [&str; 4] = [CLAUDE, CLAUDE_SESSION, CODEX, CURSOR];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Range {
    /// From the start of the current window to its reset.
    #[default]
    Window,
    Today,
    Week,
    Month,
}

/// Opens the history on a range: `window`, `today`, `week` or `month`, for
/// visual checks.
pub const RANGE_ENV: &str = "AGENT_BURN_APP_QUOTA_RANGE";

impl Range {
    const ALL: [Self; 4] = [Self::Window, Self::Today, Self::Week, Self::Month];

    /// The range named by `AGENT_BURN_APP_QUOTA_RANGE`, else the window.
    pub fn initial() -> Self {
        match std::env::var(RANGE_ENV).as_deref() {
            Ok("today") => Self::Today,
            Ok("week") => Self::Week,
            Ok("month") => Self::Month,
            _ => Self::Window,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Window => "Current window",
            Self::Today => "Today",
            Self::Week => "Last 7 days",
            Self::Month => "Last 30 days",
        }
    }
}

/// What one series' chart shows.
#[derive(Debug, PartialEq)]
pub struct SeriesView {
    pub series: &'static str,
    pub readings: Vec<QuotaReading>,
    pub resets: Vec<forecast::Reset>,
    pub from_ms: i64,
    pub to_ms: i64,
    pub pace: Option<(i64, i64)>,
    pub projection: Projection,
}

/// The chart of `series` over `range`, or `None` without any reading.
pub fn series_view(
    history: &QuotaHistory,
    series: &'static str,
    range: Range,
    now_ms: i64,
    midnight_ms: i64,
) -> Option<SeriesView> {
    let readings = forecast::consistent(history.readings.get(series)?);
    let latest = readings.last()?.clone();
    let projection = forecast::project(&readings, now_ms);
    let (from_ms, to_ms, pace) = match range {
        Range::Window => {
            let start = latest.window_start_ms();
            let reset = latest.resets_at_ms();
            (start, reset.max(now_ms), Some((start, reset)))
        }
        Range::Today => (midnight_ms, now_ms, None),
        Range::Week => (now_ms - 7 * DAY_MS, now_ms, None),
        Range::Month => (now_ms - 30 * DAY_MS, now_ms, None),
    };
    let resets = forecast::resets(&readings)
        .into_iter()
        .filter(|reset| (from_ms..=to_ms).contains(&reset.at_ms))
        .collect();
    Some(SeriesView {
        series,
        readings,
        resets,
        from_ms,
        to_ms,
        pace,
        projection,
    })
}

/// The sentence under a chart about the forecast.
pub fn projection_text(projection: &Projection) -> (String, bool) {
    match projection {
        Projection::Shown(forecast) => {
            let reset = clock::from_millis(forecast.reset_ms)
                .map_or_else(String::new, clock::local_date_time);
            let text = match forecast.runs_out_ms.and_then(clock::from_millis) {
                Some(runs_out) => format!(
                    "At the average pace of this window, it runs out around {}, {} before it resets ({reset}).",
                    clock::local_date_time(runs_out),
                    format::duration(Duration::from_millis(
                        (forecast.reset_ms - forecast.runs_out_ms.unwrap_or(forecast.reset_ms))
                            .max(0) as u64
                    )),
                ),
                None => format!(
                    "At the average pace of this window, about {:.0}% would be left when it resets ({reset}).",
                    forecast.remaining_at_reset()
                ),
            };
            (
                format!(
                    "{text} A projection of the recorded readings, not a limit reported by the provider."
                ),
                forecast.runs_out_ms.is_some(),
            )
        }
        Projection::Withheld(reason) => (format!("No forecast: {reason}"), false),
    }
}

fn agent_of(series: &str) -> &str {
    if series == CLAUDE_SESSION {
        CLAUDE
    } else {
        series
    }
}

pub fn show(
    ui: &mut egui::Ui,
    history: &QuotaHistory,
    range: &mut Range,
    now_ms: i64,
    midnight_ms: i64,
) {
    section(ui, "Quota history", |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Show").color(muted()));
            egui::ComboBox::from_id_salt("quota-range")
                .width(150.0)
                .selected_text(range.label())
                .show_ui(ui, |ui| {
                    for option in Range::ALL {
                        ui.selectable_value(range, option, option.label());
                    }
                });
        });
        ui.add_space(4.0);
        let mut missing = Vec::new();
        for series in SERIES {
            match series_view(history, series, *range, now_ms, midnight_ms) {
                Some(view) => {
                    card(ui, |ui| show_series(ui, &view, now_ms));
                    ui.add_space(6.0);
                }
                None => missing.push(quota::series_name(series)),
            }
        }
        if !missing.is_empty() {
            ui.label(
                egui::RichText::new(format!(
                    "No reading recorded yet: {}. Cursor is followed only while promotional credits remain.",
                    missing.join(", ")
                ))
                .color(muted())
                .size(12.0),
            );
        }
    });
}

fn show_series(ui: &mut egui::Ui, view: &SeriesView, now_ms: i64) {
    let color = series::color(series::slot(agent_of(view.series)), is_light());
    let latest = view.readings.last();
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(quota::series_name(view.series))
                .font(semibold(15.0))
                .color(menu_text()),
        );
        if let Some(latest) = latest {
            let now = clock::from_millis(now_ms).unwrap_or_else(std::time::SystemTime::now);
            ui.label(
                egui::RichText::new(format!(
                    "{:.0}% left · read {} ({})",
                    100.0 - latest.window.used_percent,
                    clock::local_date_time(latest.observed_at()),
                    format::ago(latest.observed_at(), now)
                ))
                .color(muted())
                .size(12.0),
            );
        }
    });
    let shown = match &view.projection {
        Projection::Shown(forecast) if view.pace.is_some() => Some(forecast),
        _ => None,
    };
    quota_chart::show(
        ui,
        &Chart {
            readings: &view.readings,
            resets: &view.resets,
            from_ms: view.from_ms,
            to_ms: view.to_ms,
            pace: view.pace,
            forecast: shown,
            now_ms,
            color,
            gap_ms: GAP_MS,
        },
    );
    quota_chart::legend(ui, color, shown.is_some());
    let in_range = view
        .readings
        .iter()
        .filter(|reading| (view.from_ms..=view.to_ms).contains(&reading.observed_at_ms))
        .count();
    let resets = match view.resets.len() {
        0 => String::new(),
        1 => " · 1 reset".into(),
        count => format!(" · {count} resets"),
    };
    ui.label(
        egui::RichText::new(format!(
            "{in_range} reading{} in this range{resets}",
            if in_range == 1 { "" } else { "s" }
        ))
        .color(muted())
        .size(12.0),
    );
    let (text, warns) = projection_text(&view.projection);
    let color = match &view.projection {
        Projection::Shown(_) if warns => danger(),
        Projection::Shown(_) => menu_text(),
        Projection::Withheld(_) => muted(),
    };
    ui.label(egui::RichText::new(text).color(color).size(12.0));
    if view.resets.iter().any(|reset| !reset.scheduled) {
        ui.label(
            egui::RichText::new(
                "An early reset: the provider restored the quota before the end of its window, or corrected it.",
            )
            .color(caution())
            .size(12.0),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::quota::QuotaWindow;

    const HOUR: i64 = 3_600_000;
    const START: i64 = 1_791_000_000_000;

    fn at(series: &str, start: i64, hours: f64, used: f64) -> QuotaReading {
        let observed = start + (hours * HOUR as f64) as i64;
        QuotaReading {
            series: series.into(),
            observed_at_ms: observed,
            window: QuotaWindow {
                window_minutes: 10_080.0,
                used_percent: used,
                elapsed_percent: (observed - start) as f64 / (10_080.0 * 60_000.0) * 100.0,
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
    fn the_current_window_runs_from_its_start_to_its_reset_with_the_even_pace() {
        let saved = history(vec![
            at(CODEX, START, 40.0, 15.0),
            at(CODEX, START, 48.0, 20.0),
        ]);
        let now = START + 48 * HOUR;

        let view = series_view(&saved, CODEX, Range::Window, now, now - HOUR).unwrap();

        assert_eq!((view.from_ms, view.to_ms), (START, START + 168 * HOUR));
        assert_eq!(view.pace, Some((START, START + 168 * HOUR)));
        assert!(matches!(view.projection, Projection::Shown(_)));
        let (text, warns) = projection_text(&view.projection);
        assert!(text.contains("about 30% would be left"), "{text}");
        assert!(text.contains("not a limit reported by the provider"));
        assert!(!warns);
        // Other ranges draw no pace.
        let week = series_view(&saved, CODEX, Range::Week, now, now - HOUR).unwrap();
        assert_eq!(week.pace, None);
    }

    #[test]
    fn a_series_without_readings_has_no_chart() {
        let saved = history(vec![at(CODEX, START, 40.0, 15.0)]);
        assert!(series_view(&saved, CLAUDE, Range::Window, START, START).is_none());
        // One reading: a chart, but no forecast.
        let view = series_view(&saved, CODEX, Range::Window, START + 40 * HOUR, START).unwrap();
        assert_eq!(
            projection_text(&view.projection).0,
            "No forecast: Not enough readings in this window yet."
        );
    }

    #[test]
    fn a_run_out_before_the_reset_is_a_warning() {
        let saved = history(vec![
            at(CLAUDE, START, 40.0, 50.0),
            at(CLAUDE, START, 48.0, 60.0),
        ]);
        let view = series_view(&saved, CLAUDE, Range::Window, START + 48 * HOUR, START).unwrap();

        let (text, warns) = projection_text(&view.projection);
        assert!(
            text.starts_with("At the average pace of this window, it runs out around"),
            "{text}"
        );
        assert!(warns);
    }

    #[test]
    fn resets_outside_the_range_are_left_out() {
        let next = START + 168 * HOUR;
        let saved = history(vec![
            at(CODEX, START, 160.0, 80.0),
            at(CODEX, next, 1.0, 2.0),
            at(CODEX, next, 30.0, 10.0),
        ]);
        let now = next + 30 * HOUR;

        let month = series_view(&saved, CODEX, Range::Month, now, now).unwrap();
        assert_eq!(month.resets.len(), 1);
        let today = series_view(&saved, CODEX, Range::Today, now, now - 2 * HOUR).unwrap();
        assert!(today.resets.is_empty());
    }
}
