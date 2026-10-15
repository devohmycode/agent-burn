//! Daily usage: API-equivalent cost per day, split by agent, over the
//! selected period. Every day of the period keeps its place, recorded or
//! not; daily totals are the engine's, never recomputed. The engine reports
//! models per agent for the whole period only, so no per-day model split is
//! drawn.

use std::collections::BTreeMap;

use eframe::egui;

use crate::{
    dashboard::format,
    dates::{Date, days_between},
    models::SummaryReport,
    state::Snapshot,
    ui::{
        components::{
            bars::{self, Column, Segment, Series},
            fluent::{card, section},
        },
        series,
        theme::{danger, is_light, muted},
    },
};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn short(date: Date) -> String {
    format!(
        "{} {}",
        MONTHS[usize::from(date.month.clamp(1, 12)) - 1],
        date.day
    )
}

/// Past this many days, columns cover a week each.
const MAX_DAILY_COLUMNS: usize = 120;

#[derive(Debug, PartialEq)]
pub struct SeriesInfo {
    /// The agent, or `None` for agents sharing "Other".
    pub agent: Option<String>,
    pub name: String,
    pub slot: Option<usize>,
}

#[derive(Debug, PartialEq)]
pub struct Day {
    pub date: Date,
    /// The engine reported this day.
    pub recorded: bool,
    /// Cost per series index, in series order.
    pub by_series: Vec<(usize, f64)>,
    pub cost: Option<f64>,
    pub tokens: Option<u64>,
}

#[derive(Debug, PartialEq)]
pub struct UsageView {
    pub series: Vec<SeriesInfo>,
    pub days: Vec<Day>,
}

/// A chart column: one day, or one week (Monday to Sunday) for long periods.
#[derive(Debug, PartialEq)]
pub struct ColumnData {
    pub first: Date,
    pub last: Date,
    pub recorded: bool,
    pub by_series: Vec<(usize, f64)>,
}

impl UsageView {
    /// `bounds` are the period's first and last days; without them the days
    /// the report covers are used.
    pub fn new(report: &SummaryReport, bounds: Option<(Date, Date)>) -> Self {
        let recorded: BTreeMap<Date, (f64, Option<u64>)> = report
            .daily
            .iter()
            .filter_map(|day| Some((Date::parse(&day.date)?, (day.cost, day.tokens))))
            .collect();
        let bounds = bounds.or_else(|| {
            let period = report.subscription.as_ref()?.period.as_ref();
            period
                .and_then(|period| Some((Date::parse(&period.from)?, Date::parse(&period.to)?)))
                .or_else(|| Some((*recorded.keys().next()?, *recorded.keys().next_back()?)))
        });
        let mut agents: Vec<&str> = report
            .agents
            .iter()
            .map(|agent| agent.agent.as_str())
            .collect();
        agents.sort_by_key(|agent| (series::order(agent), agent.to_string()));
        let mut series_list: Vec<SeriesInfo> = Vec::new();
        let mut index_of = BTreeMap::new();
        for agent in agents {
            let slot = series::slot(agent);
            let index = match slot {
                Some(_) => {
                    series_list.push(SeriesInfo {
                        agent: Some(agent.to_string()),
                        name: series::name(agent),
                        slot,
                    });
                    series_list.len() - 1
                }
                None => {
                    if let Some(other) = series_list.iter().position(|entry| entry.agent.is_none())
                    {
                        other
                    } else {
                        series_list.push(SeriesInfo {
                            agent: None,
                            name: "Other".into(),
                            slot: None,
                        });
                        series_list.len() - 1
                    }
                }
            };
            index_of.insert(agent.to_string(), index);
        }
        let mut per_day: BTreeMap<Date, BTreeMap<usize, f64>> = BTreeMap::new();
        for agent in &report.agents {
            let index = index_of[&agent.agent];
            for day in &agent.daily {
                if let Some(date) = Date::parse(&day.date) {
                    *per_day.entry(date).or_default().entry(index).or_default() += day.cost;
                }
            }
        }
        let days = bounds
            .map(|(first, last)| {
                days_between(first, last)
                    .map(|date| {
                        let row = recorded.get(&date);
                        Day {
                            date,
                            recorded: row.is_some(),
                            by_series: per_day
                                .get(&date)
                                .map(|costs| {
                                    costs.iter().map(|(index, cost)| (*index, *cost)).collect()
                                })
                                .unwrap_or_default(),
                            cost: row.map(|(cost, _)| *cost),
                            tokens: row.and_then(|(_, tokens)| *tokens),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            series: series_list,
            days,
        }
    }

    pub fn columns(&self) -> Vec<ColumnData> {
        if self.days.len() <= MAX_DAILY_COLUMNS {
            return self
                .days
                .iter()
                .map(|day| ColumnData {
                    first: day.date,
                    last: day.date,
                    recorded: day.recorded,
                    by_series: day.by_series.clone(),
                })
                .collect();
        }
        let mut columns: Vec<ColumnData> = Vec::new();
        for day in &self.days {
            let monday = day.date.add_days(-day.date.weekday_from_monday());
            if columns.last().is_none_or(|column| {
                column.first.add_days(-column.first.weekday_from_monday()) != monday
            }) {
                columns.push(ColumnData {
                    first: day.date,
                    last: day.date,
                    recorded: false,
                    by_series: Vec::new(),
                });
            }
            let column = columns.last_mut().expect("a column was just pushed");
            column.last = day.date;
            column.recorded |= day.recorded;
            for (index, cost) in &day.by_series {
                match column
                    .by_series
                    .iter_mut()
                    .find(|(existing, _)| existing == index)
                {
                    Some((_, total)) => *total += cost,
                    None => column.by_series.push((*index, *cost)),
                }
            }
        }
        for column in &mut columns {
            column.by_series.sort_by_key(|(index, _)| *index);
        }
        columns
    }
}

pub fn show(ui: &mut egui::Ui, snapshot: &Snapshot, today: Date) {
    let summary = match &snapshot.summary {
        Ok(summary) => summary,
        Err(error) => {
            card(ui, |ui| {
                ui.colored_label(
                    danger(),
                    format!("The usage report could not be read: {error}"),
                );
            });
            return;
        }
    };
    if summary.agents.is_empty() {
        card(ui, |ui| {
            ui.label("No agent usage was found for this period.");
        });
        return;
    }
    let view = UsageView::new(summary, snapshot.selection.period.bounds(today));
    let light = is_light();
    let series_list: Vec<Series> = view
        .series
        .iter()
        .map(|entry| Series {
            name: entry.name.clone(),
            color: series::color(entry.slot, light),
        })
        .collect();
    let columns: Vec<Column> = view
        .columns()
        .into_iter()
        .map(|column| {
            let (label, title) = if column.first == column.last {
                (short(column.first), column.first.to_string())
            } else {
                (
                    short(column.first),
                    format!("{} to {}", column.first, column.last),
                )
            };
            Column {
                label,
                title,
                segments: column
                    .by_series
                    .iter()
                    .map(|(series, value)| Segment {
                        series: *series,
                        value: *value,
                    })
                    .collect(),
                recorded: column.recorded,
            }
        })
        .collect();
    let weekly = view.days.len() != columns.len();

    section(
        ui,
        if weekly {
            "Weekly API-equivalent cost"
        } else {
            "Daily API-equivalent cost"
        },
        |ui| {
            card(ui, |ui| {
                bars::legend(ui, &series_list);
                bars::stacked_bars(
                    ui,
                    ui.id().with("daily"),
                    &series_list,
                    &columns,
                    format::money,
                );
                ui.label(
                    egui::RichText::new(
                        "A period without a bar has no recorded usage. Hover a bar for its amounts.",
                    )
                    .color(muted())
                    .size(12.0),
                );
                egui::CollapsingHeader::new("Show as a table")
                    .id_salt("daily-table")
                    .show(ui, |ui| {
                        egui::Grid::new("daily-grid")
                            .num_columns(3)
                            .spacing([24.0, 6.0])
                            .show(ui, |ui| {
                                for heading in ["Day", "Cost", "Tokens"] {
                                    ui.label(
                                        egui::RichText::new(heading).color(muted()).size(12.0),
                                    );
                                }
                                ui.end_row();
                                for day in view.days.iter().rev() {
                                    ui.label(day.date.to_string());
                                    match (day.cost, day.tokens) {
                                        (Some(cost), tokens) => {
                                            ui.label(format::money(cost))
                                                .on_hover_text(format::exact_money(cost));
                                            ui.label(
                                                tokens.map_or_else(|| "–".into(), format::tokens),
                                            );
                                        }
                                        (None, _) => {
                                            ui.label(
                                                egui::RichText::new("no usage recorded")
                                                    .color(muted()),
                                            );
                                            ui.label("");
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                    });
            });
        },
    );

    section(ui, "Models by agent", |ui| {
        for agent in &summary.agents {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(
                        rect,
                        2.0,
                        series::color(series::slot(&agent.agent), light),
                    );
                    ui.label(egui::RichText::new(series::name(&agent.agent)).strong());
                    ui.label(
                        egui::RichText::new(format!(
                            "{} · {} tokens",
                            format::money(agent.total_cost),
                            format::tokens(agent.total_tokens)
                        ))
                        .color(muted()),
                    );
                });
                egui::Grid::new(("models", &agent.agent))
                    .num_columns(4)
                    .spacing([24.0, 6.0])
                    .show(ui, |ui| {
                        for heading in ["Model", "Cost", "Tokens", "Share"] {
                            ui.label(egui::RichText::new(heading).color(muted()).size(12.0));
                        }
                        ui.end_row();
                        for model in &agent.models {
                            ui.label(&model.model);
                            ui.label(format::money(model.total_cost))
                                .on_hover_text(format::exact_money(model.total_cost));
                            ui.label(format::tokens(model.total_tokens));
                            ui.label(
                                model
                                    .percentage
                                    .map_or_else(|| "unknown".into(), format::percent),
                            );
                            ui.end_row();
                        }
                    });
                if let Some(breakdown) = &agent.token_breakdown {
                    ui.label(
                        egui::RichText::new(format!(
                            "Input {} · output {} · cache write {} · cache read {}",
                            format::tokens(breakdown.input),
                            format::tokens(breakdown.output),
                            format::tokens(breakdown.cache_write),
                            format::tokens(breakdown.cache_read)
                        ))
                        .color(muted())
                        .size(12.0),
                    );
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(scenario: &str) -> SummaryReport {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(scenario)
            .join("summary.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn period(report: &SummaryReport) -> (Date, Date) {
        let period = report
            .subscription
            .as_ref()
            .unwrap()
            .period
            .as_ref()
            .unwrap();
        (
            Date::parse(&period.from).unwrap(),
            Date::parse(&period.to).unwrap(),
        )
    }

    #[test]
    fn every_day_of_the_period_keeps_its_place() {
        let report = fixture("live-accounts");
        let (from, to) = period(&report);

        let view = UsageView::new(&report, Some((from.add_days(-2), to.add_days(1))));

        assert_eq!(view.days.len() as i64, to.days() - from.days() + 4);
        assert_eq!(view.days[0].date, from.add_days(-2));
        assert!(!view.days[0].recorded);
        assert_eq!(view.days[0].cost, None);
        assert!(view.days[0].by_series.is_empty());
        assert_eq!(view.days.last().unwrap().date, to.add_days(1));
    }

    #[test]
    fn daily_totals_are_the_engine_values() {
        let report = fixture("live-accounts");

        let view = UsageView::new(&report, None);

        for row in &report.daily {
            let day = view
                .days
                .iter()
                .find(|day| day.date.to_string() == row.date)
                .unwrap();
            assert!(day.recorded);
            assert_eq!(day.cost, Some(row.cost));
            assert_eq!(day.tokens, row.tokens);
            let split: f64 = day.by_series.iter().map(|(_, cost)| cost).sum();
            assert!(
                (split - row.cost).abs() < 1e-9,
                "{} {split} {}",
                row.date,
                row.cost
            );
        }
    }

    #[test]
    fn agents_keep_their_slot_order() {
        let view = UsageView::new(&fixture("live-accounts"), None);

        let names: Vec<&str> = view
            .series
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, ["Codex", "Claude", "Cursor"]);
    }

    #[test]
    fn no_source_draws_nothing() {
        let view = UsageView::new(&fixture("no-source"), None);

        assert!(view.days.is_empty());
        assert!(view.columns().is_empty());
    }

    #[test]
    fn long_periods_group_whole_weeks() {
        let report = fixture("live-accounts");
        let (from, to) = period(&report);
        let first = Date::parse("2026-01-01").unwrap();

        let view = UsageView::new(&report, Some((first, to)));
        let columns = view.columns();

        assert!(view.days.len() > MAX_DAILY_COLUMNS);
        assert!(
            columns
                .iter()
                .skip(1)
                .all(|column| column.first.weekday_from_monday() == 0)
        );
        assert_eq!(columns[0].first, first);
        assert_eq!(columns.last().unwrap().last, to);
        let grouped: f64 = columns
            .iter()
            .flat_map(|column| column.by_series.iter().map(|(_, cost)| cost))
            .sum();
        let daily: f64 = report
            .daily
            .iter()
            .filter(|day| Date::parse(&day.date).unwrap() >= from)
            .map(|day| day.cost)
            .sum();
        assert!((grouped - daily).abs() < 1e-9);
    }
}
