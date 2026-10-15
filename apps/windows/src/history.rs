//! Reports kept between runs, in the application folder.
//!
//! - `metrics-history.json` holds the cost and tokens of every agent for
//!   every day a report showed. A day is replaced, never added to another
//!   reading of it, and never expires, so the days the logs no longer hold
//!   (Claude Code deletes old transcripts) stay counted. A report of
//!   everything replaces the days it holds; a report of a shorter period
//!   only fills the days the history lacks, and a report of everything is
//!   read once a day to bring them up to date.
//! - `report-cache.json` holds the latest reports of each selection, shown
//!   at start and while a selection is collected again.
//!
//! Both files keep a `.bak` copy of their last valid version. The rules
//! follow the macOS application (`MetricsArchive.swift`, `ReportCache.swift`
//! and `UsageJournal.swift`).

use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    dates::Date,
    files::{self, Stored, Versioned},
    models::{AgentUsage, DailyUsage, HarnessReport, SummaryReport},
    state::{Period, Selection, Snapshot},
};

pub const ARCHIVE_FILE: &str = "metrics-history.json";
pub const CACHE_FILE: &str = "report-cache.json";
/// The lock every process takes before writing these files.
pub const LOCK_NAME: &str = "history";
const LOCK_WAIT: Duration = Duration::from_secs(10);
/// Engine settings remembered, the most recent first.
const SOURCES_KEPT: usize = 4;
/// Summaries remembered for each engine setting.
const SUMMARIES_KEPT: usize = 12;

/// How a report's days enter the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ingest {
    /// A report of everything: its days replace the saved ones.
    Replace,
    /// A shorter period: its days only fill days the history lacks.
    Fill,
}

impl Ingest {
    pub fn for_period(period: Period) -> Self {
        if period == Period::All {
            Self::Replace
        } else {
            Self::Fill
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsArchive {
    pub version: u32,
    /// Agent, then day (`YYYY-MM-DD`).
    pub agents: BTreeMap<String, BTreeMap<String, DailyUsage>>,
    /// The day of the last report of everything.
    pub complete_on: Option<String>,
}

impl Default for MetricsArchive {
    fn default() -> Self {
        Self {
            version: Self::VERSION,
            agents: BTreeMap::new(),
            complete_on: None,
        }
    }
}

impl Versioned for MetricsArchive {
    const VERSION: u32 = 1;
    fn version(&self) -> u32 {
        self.version
    }
}

impl MetricsArchive {
    /// Adds the days of `report`; returns whether anything changed. Days
    /// with an unreadable date or a cost that is not a finite positive
    /// amount are left out.
    pub fn ingest(&mut self, report: &SummaryReport, ingest: Ingest) -> bool {
        let mut changed = false;
        for usage in &report.agents {
            for day in &usage.daily {
                let Some(date) = Date::parse(&day.date) else {
                    continue;
                };
                if !day.cost.is_finite() || day.cost < 0.0 {
                    continue;
                }
                let date = date.to_string();
                let days = self.agents.entry(usage.agent.clone()).or_default();
                let old = days.get(&date);
                if old.is_some() && ingest == Ingest::Fill {
                    continue;
                }
                let row = DailyUsage {
                    date: date.clone(),
                    cost: day.cost,
                    tokens: day.tokens.or(old.and_then(|old| old.tokens)),
                    cursor_models_cost: day
                        .cursor_models_cost
                        .or(old.and_then(|old| old.cursor_models_cost)),
                    cursor_models_tokens: day
                        .cursor_models_tokens
                        .or(old.and_then(|old| old.cursor_models_tokens)),
                };
                if old != Some(&row) {
                    days.insert(date, row);
                    changed = true;
                }
            }
        }
        changed
    }

    /// `live` with the saved days of the same period and agent that it no
    /// longer reports, and how many such days there are. The days it reports
    /// keep the engine's values, so with nothing to add the report is
    /// unchanged.
    pub fn merge(
        &self,
        live: &SummaryReport,
        bounds: Option<(Date, Date)>,
        agent: Option<&str>,
    ) -> (SummaryReport, usize) {
        let bounds = bounds.map(|(since, until)| (since.to_string(), until.to_string()));
        let within = |date: &str| {
            bounds
                .as_ref()
                .is_none_or(|(since, until)| since.as_str() <= date && date <= until.as_str())
        };
        let mut report = live.clone();
        let mut kept = 0;
        for (name, days) in &self.agents {
            if agent.is_some_and(|agent| agent != name) {
                continue;
            }
            let logged: HashSet<&str> = live
                .agents
                .iter()
                .find(|usage| usage.agent == *name)
                .map(|usage| usage.daily.iter().map(|day| day.date.as_str()).collect())
                .unwrap_or_default();
            let extra: Vec<&DailyUsage> = days
                .values()
                .filter(|day| within(&day.date) && !logged.contains(day.date.as_str()))
                .collect();
            if extra.is_empty() {
                continue;
            }
            kept += extra.len();
            let cost: f64 = extra.iter().map(|day| day.cost).sum();
            let tokens: u64 = extra.iter().map(|day| day.tokens.unwrap_or(0)).sum();
            let index = match report.agents.iter().position(|usage| usage.agent == *name) {
                Some(index) => index,
                None => {
                    report.agents.push(AgentUsage {
                        agent: name.clone(),
                        total_cost: 0.0,
                        total_tokens: 0,
                        models: Vec::new(),
                        daily: Vec::new(),
                        token_breakdown: None,
                    });
                    report.agents.len() - 1
                }
            };
            let usage = &mut report.agents[index];
            usage.total_cost += cost;
            usage.total_tokens += tokens;
            usage.daily.extend(extra.iter().map(|day| (*day).clone()));
            usage.daily.sort_by(|a, b| a.date.cmp(&b.date));
            report.totals.total_cost += cost;
            report.totals.total_tokens += tokens;
            for day in extra {
                add_day(&mut report.daily, day);
            }
        }
        report.daily.sort_by(|a, b| a.date.cmp(&b.date));
        (report, kept)
    }
}

/// Adds `day` to the row of its date in `daily`, or as a new row.
fn add_day(daily: &mut Vec<DailyUsage>, day: &DailyUsage) {
    let sum_cost = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    };
    let sum_tokens = |a: Option<u64>, b: Option<u64>| match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    };
    match daily.iter_mut().find(|row| row.date == day.date) {
        Some(row) => {
            row.cost += day.cost;
            row.tokens = sum_tokens(row.tokens, day.tokens);
            row.cursor_models_cost = sum_cost(row.cursor_models_cost, day.cursor_models_cost);
            row.cursor_models_tokens =
                sum_tokens(row.cursor_models_tokens, day.cursor_models_tokens);
        }
        None => daily.push(day.clone()),
    }
}

/// A report as the engine printed it, when it was read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Saved<T> {
    /// The selection (summaries) or the harness name.
    pub key: String,
    pub saved_at_ms: i64,
    /// The local day it was read, `YYYY-MM-DD`.
    pub saved_on: String,
    pub report: T,
}

/// The saved reports of one set of engine settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceReports {
    pub source: String,
    /// The most recent first.
    pub summaries: Vec<Saved<SummaryReport>>,
    pub harnesses: Vec<Saved<HarnessReport>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportCache {
    pub version: u32,
    /// The most recently saved first.
    pub sources: Vec<SourceReports>,
}

impl Default for ReportCache {
    fn default() -> Self {
        Self {
            version: Self::VERSION,
            sources: Vec::new(),
        }
    }
}

impl Versioned for ReportCache {
    const VERSION: u32 = 1;
    fn version(&self) -> u32 {
        self.version
    }
}

impl ReportCache {
    fn source_mut(&mut self, source: &str) -> &mut SourceReports {
        let reports = match self.sources.iter().position(|saved| saved.source == source) {
            Some(index) => self.sources.remove(index),
            None => SourceReports {
                source: source.to_string(),
                summaries: Vec::new(),
                harnesses: Vec::new(),
            },
        };
        self.sources.insert(0, reports);
        self.sources.truncate(SOURCES_KEPT);
        &mut self.sources[0]
    }

    fn remember<T>(list: &mut Vec<Saved<T>>, saved: Saved<T>, kept: usize) {
        list.retain(|old| old.key != saved.key);
        list.insert(0, saved);
        list.truncate(kept);
    }
}

fn now_ms(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

fn at_ms(ms: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64)
}

/// The saved reports of the application folder.
#[derive(Debug)]
pub struct History {
    folder: PathBuf,
}

impl History {
    pub fn new(folder: PathBuf) -> Self {
        Self { folder }
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    pub fn archive_path(&self) -> PathBuf {
        self.folder.join(ARCHIVE_FILE)
    }

    pub fn cache_path(&self) -> PathBuf {
        self.folder.join(CACHE_FILE)
    }

    /// Whether a report of everything should be read today, besides the
    /// selection, to bring the saved days up to date.
    pub fn needs_complete(&self, selection: &Selection, today: Date) -> bool {
        if selection.is_complete() {
            return false;
        }
        match files::load::<MetricsArchive>(&self.archive_path()) {
            Stored::Found { value, .. } => value.complete_on != Some(today.to_string()),
            Stored::Missing | Stored::Damaged(_) => true,
            Stored::Locked(_) => false,
        }
    }

    /// The saved reports of `selection`'s engine settings.
    fn source_reports(&self, selection: &Selection) -> Option<SourceReports> {
        let Stored::Found { value: cache, .. } = files::load::<ReportCache>(&self.cache_path())
        else {
            return None;
        };
        cache
            .sources
            .into_iter()
            .find(|saved| saved.source == selection.options.source_key())
    }

    /// The last saved summary of `selection`, with the saved days, when it
    /// was saved and how many days it keeps. A period counted from today is
    /// only shown on the day it was read.
    fn saved_summary(
        &self,
        source: &SourceReports,
        selection: &Selection,
        today: Date,
    ) -> Option<(SummaryReport, SystemTime, usize)> {
        let today_text = today.to_string();
        let summary = source.summaries.iter().find(|saved| {
            saved.key == selection.key()
                && (!selection.period.counts_from_today() || saved.saved_on == today_text)
        })?;
        let mut report = summary.report.clone();
        let mut kept_days = 0;
        if let Stored::Found { value: archive, .. } =
            files::load::<MetricsArchive>(&self.archive_path())
        {
            (report, kept_days) = archive.merge(
                &report,
                selection.period.bounds(today),
                selection.agent.as_deref(),
            );
        }
        Some((report, at_ms(summary.saved_at_ms), kept_days))
    }

    /// The last saved reports of `selection`, with the saved days, marked
    /// as saved.
    pub fn saved(&self, selection: &Selection, today: Date) -> Option<Snapshot> {
        let source = self.source_reports(selection)?;
        let (report, saved_at, kept_days) = self.saved_summary(&source, selection, today)?;
        let harnesses = selection
            .harnesses()
            .into_iter()
            .filter_map(|name| {
                source
                    .harnesses
                    .iter()
                    .find(|saved| saved.key == name)
                    .map(|saved| (name, Ok(saved.report.clone())))
            })
            .collect();
        let mut snapshot = Snapshot::collected(selection, Ok(report), harnesses);
        snapshot.collected_at = saved_at;
        snapshot.saved = true;
        snapshot.kept_days = kept_days;
        Some(snapshot)
    }

    /// Keeps the last valid reports in view where the new ones failed,
    /// dated, with the failure.
    fn fall_back(&self, snapshot: &mut Snapshot, today: Date) {
        let failed = snapshot.summary.is_err()
            || snapshot.harnesses.iter().any(|(_, report)| report.is_err());
        if !failed {
            return;
        }
        let Some(source) = self.source_reports(&snapshot.selection) else {
            return;
        };
        if snapshot.summary.is_err()
            && let Some((report, saved_at, kept_days)) =
                self.saved_summary(&source, &snapshot.selection, today)
            && let Err(error) = std::mem::replace(&mut snapshot.summary, Ok(report))
        {
            snapshot.summary_failure = Some((error, saved_at));
            snapshot.kept_days = kept_days;
        }
        for (name, report) in &mut snapshot.harnesses {
            if report.is_err()
                && let Some(saved) = source.harnesses.iter().find(|saved| saved.key == *name)
                && let Err(error) = std::mem::replace(report, Ok(saved.report.clone()))
            {
                snapshot
                    .harness_saved
                    .push((name, at_ms(saved.saved_at_ms), error));
            }
        }
    }

    /// Saves what `snapshot` read, and `complete`, a report of everything
    /// read besides it; returns `snapshot` with the saved days its period
    /// covers and the problems met.
    pub fn record(
        &self,
        mut snapshot: Snapshot,
        complete: Option<SummaryReport>,
        today: Date,
    ) -> Snapshot {
        let lock = files::Lock::acquire(&self.folder, LOCK_NAME, LOCK_WAIT);
        if let Err(error) = &lock {
            snapshot.notices.push(format!(
                "The saved history was not updated ({error}); it is shown as it was."
            ));
        }
        let writable = lock.is_ok();
        let selection = snapshot.selection.clone();

        let archive = self.update_archive(&snapshot, complete.as_ref(), today, writable);
        let (archive, notices) = archive;
        snapshot.notices.extend(notices);
        if writable && let Err(error) = self.update_cache(&snapshot, today) {
            snapshot.notices.push(format!(
                "The latest reports could not be saved for the next start ({error})."
            ));
        }
        if let (Some(archive), Ok(summary)) = (archive, &mut snapshot.summary) {
            let (merged, kept) = archive.merge(
                summary,
                selection.period.bounds(today),
                selection.agent.as_deref(),
            );
            *summary = merged;
            snapshot.kept_days = kept;
        }
        self.fall_back(&mut snapshot, today);
        snapshot
    }

    /// The saved days with what was read added, and the problems met.
    fn update_archive(
        &self,
        snapshot: &Snapshot,
        complete: Option<&SummaryReport>,
        today: Date,
        writable: bool,
    ) -> (Option<MetricsArchive>, Vec<String>) {
        let path = self.archive_path();
        let mut notices = Vec::new();
        // A damaged file is replaced as soon as possible, once kept aside.
        let mut changed = false;
        let mut archive = match files::load::<MetricsArchive>(&path) {
            Stored::Missing => MetricsArchive::default(),
            Stored::Found { value, from_backup } => {
                if from_backup {
                    notices.push(
                        "The daily cost history was damaged; its last good copy is used.".into(),
                    );
                    changed = true;
                }
                value
            }
            Stored::Damaged(reason) => {
                notices.push(format!(
                    "The daily cost history could not be read ({reason}); a new one is started."
                ));
                changed = true;
                MetricsArchive::default()
            }
            Stored::Locked(reason) => {
                notices.push(format!(
                    "The daily cost history is left untouched: {reason}."
                ));
                return (None, notices);
            }
        };
        let selection = &snapshot.selection;
        let today = today.to_string();
        if let Ok(live) = &snapshot.summary {
            changed |= archive.ingest(live, Ingest::for_period(selection.period));
            if selection.is_complete() && archive.complete_on.as_deref() != Some(today.as_str()) {
                archive.complete_on = Some(today.clone());
                changed = true;
            }
        }
        if let Some(complete) = complete {
            changed |= archive.ingest(complete, Ingest::Replace);
            if archive.complete_on.as_deref() != Some(today.as_str()) {
                archive.complete_on = Some(today);
                changed = true;
            }
        }
        if changed && writable {
            match files::save(&path, &archive) {
                Ok(Some(kept)) => notices.push(format!(
                    "The damaged daily cost history was kept as {}.",
                    kept.display()
                )),
                Ok(None) => {}
                Err(error) => notices.push(format!(
                    "Daily costs could not be saved ({error}); the last saved history is kept."
                )),
            }
        }
        (Some(archive), notices)
    }

    /// Remembers the reports `snapshot` read, as the engine printed them.
    fn update_cache(&self, snapshot: &Snapshot, today: Date) -> Result<(), String> {
        let summary = snapshot.summary.as_ref().ok();
        let harnesses: Vec<(&str, &HarnessReport)> = snapshot
            .harnesses
            .iter()
            .filter_map(|(name, report)| Some((*name, report.as_ref().ok()?)))
            .collect();
        if summary.is_none() && harnesses.is_empty() {
            return Ok(());
        }
        let path = self.cache_path();
        let mut cache = match files::load::<ReportCache>(&path) {
            Stored::Found { value, .. } => value,
            Stored::Missing | Stored::Damaged(_) => ReportCache::default(),
            Stored::Locked(reason) => return Err(reason),
        };
        let saved_at_ms = now_ms(snapshot.collected_at);
        let saved_on = today.to_string();
        let source = cache.source_mut(&snapshot.selection.options.source_key());
        if let Some(report) = summary {
            ReportCache::remember(
                &mut source.summaries,
                Saved {
                    key: snapshot.selection.key(),
                    saved_at_ms,
                    saved_on: saved_on.clone(),
                    report: report.clone(),
                },
                SUMMARIES_KEPT,
            );
        }
        for (name, report) in harnesses {
            ReportCache::remember(
                &mut source.harnesses,
                Saved {
                    key: name.to_string(),
                    saved_at_ms,
                    saved_on: saved_on.clone(),
                    report: report.clone(),
                },
                usize::MAX,
            );
        }
        files::save(&path, &cache).map(|_| ())
    }
}

/// The application folder's history, when there is a folder.
pub fn open(folder: Option<&Path>) -> Option<History> {
    folder.map(|folder| History::new(folder.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{models::HarnessReport, state::EngineOptions};

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    /// `(date, cost, tokens)`.
    type Day<'a> = (&'a str, f64, u64);

    /// A summary with `days` per agent.
    fn report(days: &[(&str, &[Day])]) -> SummaryReport {
        let agents: Vec<_> = days
            .iter()
            .map(|(agent, days)| {
                let cost: f64 = days.iter().map(|day| day.1).sum();
                let tokens: u64 = days.iter().map(|day| day.2).sum();
                json!({
                    "agent": agent,
                    "totalCost": cost,
                    "totalTokens": tokens,
                    "daily": days
                        .iter()
                        .map(|(date, cost, tokens)| json!({"date": date, "cost": cost, "tokens": tokens}))
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        let mut by_day: BTreeMap<&str, (f64, u64)> = BTreeMap::new();
        for (_, days) in days {
            for (date, cost, tokens) in *days {
                let row = by_day.entry(date).or_default();
                row.0 += cost;
                row.1 += tokens;
            }
        }
        let cost: f64 = by_day.values().map(|row| row.0).sum();
        let tokens: u64 = by_day.values().map(|row| row.1).sum();
        serde_json::from_value(json!({
            "totals": {"totalCost": cost, "totalTokens": tokens},
            "agents": agents,
            "daily": by_day
                .iter()
                .map(|(date, (cost, tokens))| json!({"date": date, "cost": cost, "tokens": tokens}))
                .collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    fn cost(archive: &MetricsArchive, agent: &str, day: &str) -> Option<f64> {
        archive.agents.get(agent)?.get(day).map(|day| day.cost)
    }

    fn selection(period: Period) -> Selection {
        Selection {
            period,
            ..Selection::default()
        }
    }

    fn snapshot(selection: Selection, summary: SummaryReport) -> Snapshot {
        Snapshot {
            selection,
            summary: Ok(summary),
            harnesses: Vec::new(),
            collected_at: SystemTime::now(),
            saved: false,
            kept_days: 0,
            notices: Vec::new(),
            summary_failure: None,
            harness_saved: Vec::new(),
        }
    }

    #[test]
    fn a_complete_report_replaces_days_and_a_shorter_one_only_fills() {
        let mut archive = MetricsArchive::default();
        archive.ingest(
            &report(&[(
                "claude",
                &[("2026-10-12", 1.0, 10), ("2026-10-13", 2.0, 20)],
            )]),
            Ingest::Replace,
        );

        // Today's report saw 13 October again, and 14 October.
        archive.ingest(
            &report(&[(
                "claude",
                &[("2026-10-13", 9.0, 90), ("2026-10-14", 3.0, 30)],
            )]),
            Ingest::Fill,
        );
        assert_eq!(cost(&archive, "claude", "2026-10-13"), Some(2.0));
        assert_eq!(cost(&archive, "claude", "2026-10-14"), Some(3.0));

        archive.ingest(
            &report(&[("claude", &[("2026-10-13", 2.5, 25)])]),
            Ingest::Replace,
        );
        assert_eq!(cost(&archive, "claude", "2026-10-13"), Some(2.5));
        // A day the complete report no longer holds is kept.
        assert_eq!(cost(&archive, "claude", "2026-10-12"), Some(1.0));
    }

    #[test]
    fn a_day_seen_by_several_periods_is_counted_once() {
        let mut archive = MetricsArchive::default();
        let day = report(&[("codex", &[("2026-10-14", 4.0, 40)])]);
        for _ in 0..3 {
            archive.ingest(&day, Ingest::Fill);
            archive.ingest(&day, Ingest::Replace);
        }

        assert_eq!(archive.agents["codex"].len(), 1);
        let (merged, kept) = archive.merge(&day, None, None);
        assert_eq!(kept, 0);
        assert_eq!(merged, day);
    }

    #[test]
    fn unreadable_days_are_left_out() {
        let mut archive = MetricsArchive::default();
        let mut bad = report(&[("claude", &[("2026-10-14", 1.0, 1), ("someday", 1.0, 1)])]);
        bad.agents[0].daily[0].cost = f64::NAN;

        assert!(!archive.ingest(&bad, Ingest::Replace));
        assert!(archive.agents.get("claude").is_none_or(BTreeMap::is_empty));
    }

    #[test]
    fn days_the_logs_lost_are_added_to_the_report() {
        let mut archive = MetricsArchive::default();
        archive.ingest(
            &report(&[
                (
                    "claude",
                    &[("2026-09-01", 5.0, 50), ("2026-10-13", 2.0, 20)],
                ),
                ("cursor", &[("2026-09-02", 1.5, 15)]),
            ]),
            Ingest::Replace,
        );
        // The engine no longer finds September: Claude deleted its logs and
        // Cursor is offline.
        let live = report(&[(
            "claude",
            &[("2026-10-13", 2.0, 20), ("2026-10-14", 1.0, 10)],
        )]);

        let (merged, kept) = archive.merge(&live, None, None);

        assert_eq!(kept, 2);
        assert_eq!(merged.totals.total_cost, 3.0 + 5.0 + 1.5);
        assert_eq!(merged.totals.total_tokens, 30 + 50 + 15);
        let claude = &merged.agents[0];
        assert_eq!(claude.total_cost, 8.0);
        assert_eq!(
            claude
                .daily
                .iter()
                .map(|day| day.date.as_str())
                .collect::<Vec<_>>(),
            ["2026-09-01", "2026-10-13", "2026-10-14"]
        );
        let cursor = merged
            .agents
            .iter()
            .find(|usage| usage.agent == "cursor")
            .unwrap();
        assert_eq!((cursor.total_cost, cursor.models.len()), (1.5, 0));
        assert_eq!(merged.daily.first().unwrap().date, "2026-09-01");
        assert_eq!(merged.daily.len(), 4);
    }

    #[test]
    fn saved_days_follow_the_period_and_the_agent() {
        let mut archive = MetricsArchive::default();
        archive.ingest(
            &report(&[
                (
                    "claude",
                    &[("2026-09-01", 5.0, 50), ("2026-10-02", 2.0, 20)],
                ),
                ("codex", &[("2026-10-03", 1.0, 10)]),
            ]),
            Ingest::Replace,
        );
        let live = report(&[("claude", &[("2026-10-14", 1.0, 10)])]);
        let october = Some((date("2026-10-01"), date("2026-10-14")));

        let (month, kept) = archive.merge(&live, october, None);
        assert_eq!((month.totals.total_cost, kept), (4.0, 2));

        let (claude, kept) = archive.merge(&live, october, Some("claude"));
        assert_eq!((claude.totals.total_cost, kept), (3.0, 1));
        assert_eq!(claude.agents.len(), 1);
    }

    #[test]
    fn the_latest_reports_come_back_at_the_next_start() {
        let folder = tempfile::tempdir().unwrap();
        let history = History::new(folder.path().to_path_buf());
        let today = date("2026-10-14");
        let all = selection(Period::All);
        let day = report(&[("claude", &[("2026-10-14", 1.0, 10)])]);

        let shown = history.record(snapshot(all.clone(), day.clone()), None, today);
        assert!(shown.notices.is_empty(), "{:?}", shown.notices);

        let restarted = History::new(folder.path().to_path_buf());
        let saved = restarted.saved(&all, today).unwrap();
        assert!(saved.saved);
        assert_eq!(saved.summary.unwrap(), day);
        // Another engine setting has nothing saved.
        let offline = Selection {
            options: EngineOptions {
                offline: true,
                ..EngineOptions::default()
            },
            ..all
        };
        assert!(restarted.saved(&offline, today).is_none());
    }

    #[test]
    fn a_period_counted_from_today_is_not_shown_the_next_day() {
        let folder = tempfile::tempdir().unwrap();
        let history = History::new(folder.path().to_path_buf());
        let today = selection(Period::Today);
        let day = report(&[("claude", &[("2026-10-13", 1.0, 10)])]);
        history.record(snapshot(today.clone(), day), None, date("2026-10-13"));

        assert!(history.saved(&today, date("2026-10-13")).is_some());
        assert!(history.saved(&today, date("2026-10-14")).is_none());
    }

    #[test]
    fn a_shorter_period_asks_for_a_complete_report_once_a_day() {
        let folder = tempfile::tempdir().unwrap();
        let history = History::new(folder.path().to_path_buf());
        let month = selection(Period::ThisMonth);
        let today = date("2026-10-14");
        assert!(history.needs_complete(&month, today));
        assert!(!history.needs_complete(&selection(Period::All), today));

        let complete = report(&[("claude", &[("2026-10-01", 3.0, 30)])]);
        let shown = history.record(
            snapshot(
                month.clone(),
                report(&[("claude", &[("2026-10-14", 1.0, 10)])]),
            ),
            Some(complete),
            today,
        );

        assert!(!history.needs_complete(&month, today));
        assert!(history.needs_complete(&month, date("2026-10-15")));
        // The month shows the day the complete report brought.
        assert_eq!(shown.kept_days, 1);
        assert_eq!(shown.summary.unwrap().totals.total_cost, 4.0);
    }

    #[test]
    fn a_damaged_history_is_recovered_from_its_copy_and_kept_aside() {
        let folder = tempfile::tempdir().unwrap();
        let history = History::new(folder.path().to_path_buf());
        let today = date("2026-10-14");
        let all = selection(Period::All);
        let old = report(&[("claude", &[("2026-09-01", 5.0, 50)])]);
        history.record(snapshot(all.clone(), old), None, today);
        // A second save makes the first one the copy.
        history.record(
            snapshot(
                all.clone(),
                report(&[("claude", &[("2026-10-13", 1.0, 10)])]),
            ),
            None,
            today,
        );
        std::fs::write(history.archive_path(), b"{\"version\":1,\"agents\":{").unwrap();

        let shown = history.record(snapshot(all.clone(), report(&[])), None, today);

        assert_eq!(shown.kept_days, 1, "{:?}", shown.notices);
        assert!(shown.notices[0].contains("last good copy"));
        assert!(
            shown
                .notices
                .iter()
                .any(|notice| notice.contains("damaged-"))
        );
        let Stored::Found { value, from_backup } =
            files::load::<MetricsArchive>(&history.archive_path())
        else {
            panic!("the history was not rewritten");
        };
        assert!(!from_backup);
        assert_eq!(cost(&value, "claude", "2026-09-01"), Some(5.0));
    }

    #[test]
    fn a_failed_save_keeps_the_last_valid_history() {
        let folder = tempfile::tempdir().unwrap();
        let history = History::new(folder.path().to_path_buf());
        let today = date("2026-10-14");
        let all = selection(Period::All);
        history.record(
            snapshot(
                all.clone(),
                report(&[("claude", &[("2026-10-13", 2.0, 20)])]),
            ),
            None,
            today,
        );
        let before = std::fs::read(history.archive_path()).unwrap();
        // A read-only history cannot be replaced.
        files::tests::set_read_only(&history.archive_path(), true);

        let shown = history.record(
            snapshot(all, report(&[("claude", &[("2026-10-14", 1.0, 10)])])),
            None,
            today,
        );

        assert!(
            shown
                .notices
                .iter()
                .any(|notice| notice.starts_with("Daily costs could not be saved")),
            "{:?}",
            shown.notices
        );
        assert_eq!(std::fs::read(history.archive_path()).unwrap(), before);
        files::tests::set_read_only(&history.archive_path(), false);
        // What was read is still shown, with the saved days.
        assert_eq!(shown.summary.unwrap().totals.total_cost, 3.0);
    }

    #[test]
    fn a_failed_report_keeps_the_last_valid_one_in_view_with_its_date() {
        let folder = tempfile::tempdir().unwrap();
        let history = History::new(folder.path().to_path_buf());
        let today = date("2026-10-14");
        let all = selection(Period::All);
        let good = report(&[("claude", &[("2026-10-14", 2.0, 20)])]);
        let harness: HarnessReport = serde_json::from_value(serde_json::json!({
            "agent": "claude",
            "apiEquivalentPerMonth": 10.0,
            "liveLimits": true,
            "window": {"windowMinutes": 10080, "usedPercent": 30.0, "elapsedPercent": 50.0, "apiEquivalentSpent": 3.0},
        }))
        .unwrap();
        let mut first = snapshot(all.clone(), good.clone());
        first.harnesses = vec![("claude", Ok(harness.clone()))];
        let saved_at = first.collected_at;
        history.record(first, None, today);

        let mut failed = Snapshot::collected(&all, Err("the engine timed out".into()), Vec::new());
        failed.harnesses = vec![("claude", Err("network unreachable".into()))];
        let shown = history.record(failed, None, today);

        assert_eq!(shown.summary.as_ref().unwrap(), &good);
        let (error, at) = shown.summary_failure.clone().unwrap();
        assert_eq!(error, "the engine timed out");
        assert_eq!(now_ms(at), now_ms(saved_at));
        assert_eq!(shown.harness("claude").unwrap().as_ref().unwrap(), &harness);
        assert_eq!(shown.harness_saved.len(), 1);
        assert_eq!(shown.harness_saved[0].2, "network unreachable");
        assert_eq!(now_ms(shown.harness_time("claude")), now_ms(saved_at));
        // Without anything saved, the failure stays as it is.
        let other = Selection {
            agent: Some("codex".into()),
            ..all
        };
        let failed = Snapshot::collected(&other, Err("no source".into()), Vec::new());
        assert!(history.record(failed, None, today).summary.is_err());
    }

    #[test]
    fn saved_files_keep_no_secret_a_report_carried() {
        use crate::redact::tests::{SECRETS, leaked};
        let folder = tempfile::tempdir().unwrap();
        let history = History::new(folder.path().to_path_buf());
        // Fields the application does not know are dropped when decoding.
        let with_secrets: SummaryReport = serde_json::from_value(json!({
            "totals": {"totalCost": 1.0, "totalTokens": 10},
            "accessToken": SECRETS[0],
            "account": {"email": SECRETS[2], "id": SECRETS[3]},
            "agents": [{
                "agent": "claude", "totalCost": 1.0, "totalTokens": 10,
                "sessionKey": SECRETS[1],
                "daily": [{"date": "2026-10-14", "cost": 1.0, "tokens": 10, "cookie": SECRETS[6]}],
            }],
        }))
        .unwrap();

        history.record(
            snapshot(selection(Period::All), with_secrets),
            None,
            date("2026-10-14"),
        );

        for entry in std::fs::read_dir(folder.path()).unwrap() {
            let text = std::fs::read_to_string(entry.unwrap().path()).unwrap_or_default();
            assert!(leaked(&text).is_empty(), "{text}");
        }
    }

    #[test]
    fn an_interrupted_write_loses_nothing_and_is_cleaned_up() {
        let folder = tempfile::tempdir().unwrap();
        let history = History::new(folder.path().to_path_buf());
        let today = date("2026-10-14");
        let all = selection(Period::All);
        history.record(
            snapshot(
                all.clone(),
                report(&[("claude", &[("2026-09-01", 5.0, 50)])]),
            ),
            None,
            today,
        );
        // A write cut short an hour ago left its temporary file behind.
        let abandoned = folder.path().join(format!(".{ARCHIVE_FILE}.999-0.tmp"));
        std::fs::write(&abandoned, b"{\"version\":1,\"agents\":{\"claude").unwrap();
        let hour_ago = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&abandoned)
            .unwrap()
            .set_modified(hour_ago)
            .unwrap();

        let shown = history.record(
            snapshot(all, report(&[("claude", &[("2026-10-14", 1.0, 10)])])),
            None,
            today,
        );

        assert!(shown.notices.is_empty(), "{:?}", shown.notices);
        assert_eq!(
            shown.summary.unwrap().totals.total_cost,
            6.0,
            "the saved day is kept"
        );
        assert!(!abandoned.exists(), "the abandoned file is removed");
    }

    #[test]
    fn an_unreadable_history_or_folder_is_never_written_over() {
        let folder = tempfile::tempdir().unwrap();
        let history = History::new(folder.path().to_path_buf());
        // A directory where the file should be cannot be read as one.
        std::fs::create_dir(history.archive_path()).unwrap();
        std::fs::write(history.archive_path().join("keep.txt"), b"kept").unwrap();
        let live = report(&[("claude", &[("2026-10-14", 1.0, 10)])]);

        let shown = history.record(
            snapshot(selection(Period::All), live.clone()),
            None,
            date("2026-10-14"),
        );

        assert_eq!(shown.summary.unwrap(), live);
        assert!(
            shown.notices[0].contains("left untouched"),
            "{:?}",
            shown.notices
        );
        assert_eq!(
            std::fs::read(history.archive_path().join("keep.txt")).unwrap(),
            b"kept"
        );

        // An application folder that is a file: nothing saved, nothing lost,
        // the report still shown.
        let blocked = folder.path().join("not-a-folder");
        std::fs::write(&blocked, b"a file").unwrap();
        let shown = History::new(blocked.clone()).record(
            snapshot(selection(Period::All), live.clone()),
            None,
            date("2026-10-14"),
        );
        assert_eq!(shown.summary.unwrap(), live);
        assert!(!shown.notices.is_empty());
        assert_eq!(std::fs::read(&blocked).unwrap(), b"a file");
    }

    #[test]
    fn a_history_from_a_newer_version_is_left_untouched() {
        let folder = tempfile::tempdir().unwrap();
        let history = History::new(folder.path().to_path_buf());
        let newer = br#"{"version":2,"agents":{},"days":[]}"#;
        std::fs::write(history.archive_path(), newer).unwrap();
        let live = report(&[("claude", &[("2026-10-14", 1.0, 10)])]);

        let shown = history.record(
            snapshot(selection(Period::All), live.clone()),
            None,
            date("2026-10-14"),
        );

        assert_eq!(shown.summary.unwrap(), live);
        assert!(shown.notices[0].contains("newer Agent Burn"));
        assert_eq!(std::fs::read(history.archive_path()).unwrap(), newer);
    }
}
