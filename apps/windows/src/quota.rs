//! Quota readings kept between runs, in the application folder.
//!
//! - `quota-readings.csv` is the archive of readings, only ever appended to:
//!   a crash or a bad write never loses earlier rows. Its columns are those
//!   of the macOS application's `QuotaJournal.swift`. A reading is written
//!   when the meter moves, when a new window starts, and at least every 15
//!   minutes; the last reading of an unchanged stretch is written before
//!   the next change, so a chart keeps its flat parts flat.
//! - `quota-state.json` holds, for each series, the latest accepted reading
//!   and the last one written, and the last failure; it has a `.bak` copy
//!   and is rebuilt from the archive when both are lost.
//!
//! Series: `codex` and `claude` (weekly windows), `claude-session` (Claude's
//! 5-hour session, kept apart) and `cursor` (promotional credits). Readings
//! are grouped by a hash of the source settings, so another account folder
//! starts other series. Every write holds the `quota` lock.

use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    dates::Date,
    files::{self, Stored, Versioned},
    models::{ClaudeAccount, CursorAccount, SummaryReport},
};

pub const JOURNAL_FILE: &str = "quota-readings.csv";
pub const STATE_FILE: &str = "quota-state.json";
pub const LOCK_NAME: &str = "quota";
const LOCK_WAIT: Duration = Duration::from_secs(10);

pub const CODEX: &str = "codex";
pub const CLAUDE: &str = "claude";
pub const CLAUDE_SESSION: &str = "claude-session";
pub const CURSOR: &str = "cursor";

/// A reading this far from the local clock is refused.
pub const MAX_CLOCK_SKEW_MS: i64 = 90_000;
/// An unchanged meter is still written this often.
const HEARTBEAT_MS: i64 = 15 * 60_000;
/// Readings whose windows start this close belong to the same window.
pub const SAME_WINDOW_MS: i64 = 5 * 60_000;
/// A smaller move of the meter is not a change.
const MIN_CHANGE: f64 = 0.05;

const HEADER: [&str; 10] = [
    "observed_at",
    "observed_at_ms",
    "source",
    "agent",
    "window_minutes",
    "used_percent",
    "remaining_percent",
    "elapsed_percent",
    "resets_at",
    "api_equivalent_spent",
];

/// A quota window as the engine's quota-only output prints it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaWindow {
    pub window_minutes: f64,
    pub used_percent: f64,
    /// Share of the window already run, 0–100.
    pub elapsed_percent: f64,
    #[serde(default)]
    pub api_equivalent_spent: f64,
}

impl QuotaWindow {
    pub fn is_valid(&self) -> bool {
        self.window_minutes.is_finite()
            && self.window_minutes > 0.0
            && self.used_percent.is_finite()
            && (0.0..=100.0).contains(&self.used_percent)
            && self.elapsed_percent.is_finite()
            && (0.0..=100.0).contains(&self.elapsed_percent)
            && self.api_equivalent_spent.is_finite()
    }

    fn length_ms(&self) -> f64 {
        self.window_minutes * 60_000.0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaReading {
    pub series: String,
    pub observed_at_ms: i64,
    pub window: QuotaWindow,
}

impl QuotaReading {
    pub fn is_valid(&self) -> bool {
        self.observed_at_ms > 0 && self.window.is_valid()
    }

    pub fn observed_at(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(self.observed_at_ms.max(0) as u64)
    }

    /// When the window of this reading started.
    pub fn window_start_ms(&self) -> i64 {
        self.observed_at_ms
            - (self.window.elapsed_percent / 100.0 * self.window.length_ms()).round() as i64
    }

    /// When the window of this reading resets.
    pub fn resets_at_ms(&self) -> i64 {
        self.observed_at_ms
            + ((100.0 - self.window.elapsed_percent) / 100.0 * self.window.length_ms()).round()
                as i64
    }

    pub fn same_window(&self, other: &Self) -> bool {
        (self.window_start_ms() - other.window_start_ms()).abs() <= SAME_WINDOW_MS
    }
}

/// How the dashboard names a series.
pub fn series_name(series: &str) -> &str {
    match series {
        CODEX => "Codex",
        CLAUDE => "Claude (week)",
        CLAUDE_SESSION => "Claude (session)",
        CURSOR => "Cursor",
        other => other,
    }
}

/// The source settings as a short name: readings of other account folders
/// form other series, and no path is written in the archive.
pub fn source_name(settings: &str) -> String {
    // FNV-1a, 64 bits.
    let hash = settings
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    format!("s{hash:016x}")
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesState {
    pub latest: Option<QuotaReading>,
    /// The last reading written to the archive.
    pub stored: Option<QuotaReading>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub message: String,
    pub at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaState {
    pub version: u32,
    /// Source, then series.
    pub sources: BTreeMap<String, BTreeMap<String, SeriesState>>,
    pub failures: BTreeMap<String, BTreeMap<String, Failure>>,
}

impl Default for QuotaState {
    fn default() -> Self {
        Self {
            version: Self::VERSION,
            sources: BTreeMap::new(),
            failures: BTreeMap::new(),
        }
    }
}

impl Versioned for QuotaState {
    const VERSION: u32 = 1;
    fn version(&self) -> u32 {
        self.version
    }
}

impl QuotaState {
    /// Accepts `reading` when it is valid and newer than the latest of its
    /// series, and clears that series' failure; returns the rows to write to
    /// the archive, or `None` when the reading is refused.
    pub fn record(&mut self, source: &str, reading: QuotaReading) -> Option<Vec<QuotaReading>> {
        if !reading.is_valid() {
            return None;
        }
        let series = self
            .sources
            .entry(source.to_string())
            .or_default()
            .entry(reading.series.clone())
            .or_default();
        if series
            .latest
            .as_ref()
            .is_some_and(|latest| reading.observed_at_ms <= latest.observed_at_ms)
        {
            return None;
        }
        let mut rows = Vec::new();
        let write = match &series.stored {
            None => true,
            Some(stored) => {
                let moved = !reading.same_window(stored)
                    || (reading.window.used_percent - stored.window.used_percent).abs()
                        >= MIN_CHANGE;
                // The end of the unchanged stretch, before the change.
                if moved
                    && let Some(latest) = &series.latest
                    && latest != stored
                {
                    rows.push(latest.clone());
                }
                moved || reading.observed_at_ms - stored.observed_at_ms >= HEARTBEAT_MS
            }
        };
        if write {
            rows.push(reading.clone());
            series.stored = Some(reading.clone());
        }
        if let Some(failures) = self.failures.get_mut(source) {
            failures.remove(&reading.series);
        }
        series.latest = Some(reading);
        Some(rows)
    }

    pub fn fail(&mut self, source: &str, series: &str, message: String, at_ms: i64) {
        self.failures
            .entry(source.to_string())
            .or_default()
            .insert(series.to_string(), Failure { message, at_ms });
    }

    pub fn clear_failure(&mut self, source: &str, series: &str) {
        if let Some(failures) = self.failures.get_mut(source) {
            failures.remove(series);
        }
    }

    /// The state the archive rows give: each series' last row is both its
    /// latest and its last written reading.
    fn rebuilt(rows: &[(String, QuotaReading)]) -> Self {
        let mut state = Self::default();
        for (source, reading) in rows {
            let series = state
                .sources
                .entry(source.clone())
                .or_default()
                .entry(reading.series.clone())
                .or_default();
            if series
                .latest
                .as_ref()
                .is_none_or(|latest| reading.observed_at_ms > latest.observed_at_ms)
            {
                series.latest = Some(reading.clone());
                series.stored = Some(reading.clone());
            }
        }
        state
    }
}

/// A change to the failures of a series.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The series could not be read now: why.
    Failed(String),
    /// Nothing to read (e.g. no promotional credits); no failure.
    Nothing,
}

/// What one source's quotas hold, for display.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuotaHistory {
    /// Readings of each series, oldest first, the latest one last.
    pub readings: BTreeMap<String, Vec<QuotaReading>>,
    pub failures: BTreeMap<String, Failure>,
}

impl QuotaHistory {
    pub fn latest(&self, series: &str) -> Option<&QuotaReading> {
        self.readings.get(series)?.last()
    }
}

/// The quota files of the application folder.
#[derive(Clone, Debug)]
pub struct QuotaFiles {
    folder: PathBuf,
}

impl QuotaFiles {
    pub fn new(folder: PathBuf) -> Self {
        Self { folder }
    }

    pub fn journal_path(&self) -> PathBuf {
        self.folder.join(JOURNAL_FILE)
    }

    pub fn state_path(&self) -> PathBuf {
        self.folder.join(STATE_FILE)
    }

    /// Records `readings` of `source` and the `outcomes` of series that gave
    /// none; returns the readings accepted. Holds the `quota` lock, reads the
    /// files again and writes them back, so concurrent writers never drop
    /// each other's readings.
    pub fn commit(
        &self,
        source: &str,
        readings: Vec<QuotaReading>,
        outcomes: &[(&str, Outcome)],
        now_ms: i64,
    ) -> Result<Vec<QuotaReading>, String> {
        let _lock = files::Lock::acquire(&self.folder, LOCK_NAME, LOCK_WAIT)
            .map_err(|error| error.to_string())?;
        let mut state = self.load_state()?;
        let mut rows = Vec::new();
        let mut accepted = Vec::new();
        for reading in readings {
            if let Some(new_rows) = state.record(source, reading.clone()) {
                rows.extend(new_rows);
                accepted.push(reading);
            }
        }
        for (series, outcome) in outcomes {
            match outcome {
                Outcome::Failed(message) => state.fail(source, series, message.clone(), now_ms),
                Outcome::Nothing => state.clear_failure(source, series),
            }
        }
        self.append(source, &rows)
            .map_err(|error| format!("the quota archive could not be written: {error}"))?;
        files::save(&self.state_path(), &state)?;
        Ok(accepted)
    }

    /// The state, its copy, or the state the archive gives.
    fn load_state(&self) -> Result<QuotaState, String> {
        match files::load::<QuotaState>(&self.state_path()) {
            Stored::Found { value, .. } => Ok(value),
            Stored::Locked(reason) => Err(reason),
            Stored::Missing | Stored::Damaged(_) => {
                let rows = self.rows().map_err(|error| error.to_string())?;
                Ok(QuotaState::rebuilt(&rows))
            }
        }
    }

    /// Appends `readings` to the archive, its header first when it is new.
    fn append(&self, source: &str, readings: &[QuotaReading]) -> io::Result<()> {
        if readings.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.folder)?;
        let path = self.journal_path();
        let mut text = String::new();
        match std::fs::read(&path) {
            Ok(existing) if existing.last().is_some_and(|byte| *byte != b'\n') => {
                // A row cut short by a crash stays alone on its line.
                text.push('\n');
            }
            Ok(existing) if existing.is_empty() => text.push_str(&line(&HEADER.map(String::from))),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                text.push_str(&line(&HEADER.map(String::from)));
            }
            Err(error) => return Err(error),
        }
        for reading in readings {
            text.push_str(&line(&fields(source, reading)));
        }
        let mut file = files::retry_transient(|| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
        })?;
        file.write_all(text.as_bytes())?;
        file.sync_all()
    }

    /// Every readable archive row, as `(source, reading)`, in file order.
    fn rows(&self) -> io::Result<Vec<(String, QuotaReading)>> {
        match std::fs::read_to_string(self.journal_path()) {
            Ok(text) => Ok(text.lines().skip(1).filter_map(parse_row).collect()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    /// The readings and failures of `source`.
    pub fn history(&self, source: &str) -> Result<QuotaHistory, String> {
        let state = match files::load::<QuotaState>(&self.state_path()) {
            Stored::Found { value, .. } => Some(value),
            Stored::Locked(reason) => return Err(reason),
            Stored::Missing | Stored::Damaged(_) => None,
        };
        let rows = self.rows().map_err(|error| error.to_string())?;
        let mut history = QuotaHistory::default();
        for (row_source, reading) in rows {
            if row_source == source {
                history
                    .readings
                    .entry(reading.series.clone())
                    .or_default()
                    .push(reading);
            }
        }
        for readings in history.readings.values_mut() {
            readings.sort_by_key(|reading| reading.observed_at_ms);
            readings.dedup_by_key(|reading| reading.observed_at_ms);
        }
        if let Some(state) = state {
            for (series, series_state) in state.sources.get(source).into_iter().flatten() {
                if let Some(latest) = &series_state.latest {
                    let readings = history.readings.entry(series.clone()).or_default();
                    if readings
                        .last()
                        .is_none_or(|last| last.observed_at_ms < latest.observed_at_ms)
                    {
                        readings.push(latest.clone());
                    }
                }
            }
            history.failures = state.failures.get(source).cloned().unwrap_or_default();
        }
        Ok(history)
    }
}

fn fields(source: &str, reading: &QuotaReading) -> [String; 10] {
    let window = &reading.window;
    [
        iso(reading.observed_at_ms),
        reading.observed_at_ms.to_string(),
        source.to_string(),
        reading.series.clone(),
        number(window.window_minutes),
        number(window.used_percent),
        number(100.0 - window.used_percent),
        number(window.elapsed_percent),
        iso(reading.resets_at_ms()),
        number(window.api_equivalent_spent),
    ]
}

fn parse_row(text: &str) -> Option<(String, QuotaReading)> {
    let fields = split(text);
    if fields.len() < HEADER.len() {
        return None;
    }
    let number = |index: usize| fields[index].parse::<f64>().ok();
    let reading = QuotaReading {
        series: fields[3].clone(),
        observed_at_ms: fields[1].parse().ok()?,
        window: QuotaWindow {
            window_minutes: number(4)?,
            used_percent: number(5)?,
            elapsed_percent: number(7)?,
            api_equivalent_spent: number(9)?,
        },
    };
    (reading.is_valid() && !fields[2].is_empty() && !reading.series.is_empty())
        .then(|| (fields[2].clone(), reading))
}

fn number(value: f64) -> String {
    if value == value.round() && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value:.4}")
    }
}

/// UTC time as `2026-10-14T13:45:12.345Z`.
fn iso(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rest = ms.rem_euclid(86_400_000);
    let date = Date::from_days(days);
    format!(
        "{date}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

fn line(fields: &[String]) -> String {
    let quoted: Vec<String> = fields
        .iter()
        .map(|field| {
            if field.contains([',', '"', '\n', '\r']) {
                format!("\"{}\"", field.replace('"', "\"\""))
            } else {
                field.clone()
            }
        })
        .collect();
    quoted.join(",") + "\n"
}

fn split(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        match (quoted, character) {
            (true, '"') if characters.peek() == Some(&'"') => {
                field.push('"');
                characters.next();
            }
            (true, '"') => quoted = false,
            (false, '"') => quoted = true,
            (false, ',') => fields.push(std::mem::take(&mut field)),
            (_, character) => field.push(character),
        }
    }
    fields.push(field);
    fields
}

/// Readings of the live meters a summary report carries: Claude's session
/// and week, and Cursor's promotional credits.
pub fn account_readings(summary: &SummaryReport, now_ms: i64) -> Vec<QuotaReading> {
    let mut readings = Vec::new();
    if let Some(account) = &summary.claude_account {
        readings.extend(claude_reading(
            account,
            CLAUDE_SESSION,
            account.session_used_percent,
            account.session_resets_at_ms,
            300.0,
            now_ms,
        ));
        readings.extend(claude_reading(
            account,
            CLAUDE,
            account.weekly_used_percent,
            account.weekly_resets_at_ms,
            10_080.0,
            now_ms,
        ));
    }
    if let Some(account) = &summary.cursor_account {
        readings.extend(cursor_reading(account, now_ms));
    }
    readings
}

fn claude_reading(
    account: &ClaudeAccount,
    series: &str,
    used: Option<f64>,
    reset_ms: Option<i64>,
    minutes: f64,
    now_ms: i64,
) -> Option<QuotaReading> {
    let (used, reset_ms) = (used?, reset_ms?);
    let observed = account.observed_at_ms.unwrap_or(now_ms);
    let length = minutes * 60_000.0;
    let elapsed = (length - (reset_ms - observed) as f64) / length * 100.0;
    let reading = QuotaReading {
        series: series.to_string(),
        observed_at_ms: observed,
        window: QuotaWindow {
            window_minutes: minutes,
            used_percent: used,
            elapsed_percent: elapsed,
            api_equivalent_spent: 0.0,
        },
    };
    (reset_ms > observed && reading.is_valid()).then_some(reading)
}

/// Whether promotional credits remain: Cursor spends them before the
/// included allowance, and only they make a quota worth following.
pub fn cursor_has_promotional_credits(account: &CursorAccount) -> bool {
    account.grants.iter().any(|grant| {
        grant.kind.as_deref() == Some("promo") && grant.remaining_usd.is_some_and(|left| left > 0.0)
    })
}

/// Cursor's promotional credits as a window that ends when the first of
/// them expires (or the billing cycle ends), at most a year long; as the
/// macOS application and the engine read it.
fn cursor_reading(account: &CursorAccount, now_ms: i64) -> Option<QuotaReading> {
    if !cursor_has_promotional_credits(account) {
        return None;
    }
    let promo: Vec<_> = account
        .grants
        .iter()
        .filter(|grant| grant.kind.as_deref() == Some("promo"))
        .collect();
    let used = account.active_percent_used.or_else(|| {
        let live: Vec<_> = promo
            .iter()
            .filter(|grant| grant.remaining_usd.is_some_and(|left| left > 0.0))
            .collect();
        let remaining: f64 = live.iter().filter_map(|grant| grant.remaining_usd).sum();
        let total: f64 = live.iter().filter_map(|grant| grant.total_usd).sum();
        (total > 0.0).then(|| ((total - remaining) / total * 100.0).clamp(0.0, 100.0))
    })?;
    let reset_ms = promo
        .iter()
        .filter(|grant| grant.remaining_usd.is_some_and(|left| left > 0.0))
        .filter_map(|grant| grant.expires_at_ms)
        .min()
        .or(account.billing_cycle_end_ms)?;
    if reset_ms <= now_ms {
        return None;
    }
    let start_ms = (reset_ms - 365 * 86_400_000).min(now_ms);
    let length = (reset_ms - start_ms) as f64;
    let spent: f64 = promo
        .iter()
        .filter_map(|grant| Some((grant.total_usd? - grant.remaining_usd?).max(0.0)))
        .sum();
    let reading = QuotaReading {
        series: CURSOR.to_string(),
        observed_at_ms: now_ms,
        window: QuotaWindow {
            window_minutes: length / 60_000.0,
            used_percent: used,
            elapsed_percent: (now_ms - start_ms) as f64 / length * 100.0,
            api_equivalent_spent: spent,
        },
    };
    reading.is_valid().then_some(reading)
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

/// The quota files of `folder`.
pub fn files(folder: &Path) -> QuotaFiles {
    QuotaFiles::new(folder.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread};

    use super::*;

    const HOUR: i64 = 3_600_000;
    /// 14 October 2026, 12:00 UTC.
    const NOON: i64 = 1_791_979_200_000;

    fn reading(series: &str, at: i64, used: f64, elapsed: f64) -> QuotaReading {
        QuotaReading {
            series: series.into(),
            observed_at_ms: at,
            window: QuotaWindow {
                window_minutes: 10_080.0,
                used_percent: used,
                elapsed_percent: elapsed,
                api_equivalent_spent: 0.0,
            },
        }
    }

    /// A weekly reading `minutes` after noon, in a window that started a day
    /// before noon.
    fn weekly(series: &str, minutes: i64, used: f64) -> QuotaReading {
        let at = NOON + minutes * 60_000;
        let elapsed = (at - (NOON - 24 * HOUR)) as f64 / (10_080.0 * 60_000.0) * 100.0;
        reading(series, at, used, elapsed)
    }

    #[test]
    fn invalid_and_older_readings_are_refused() {
        let mut state = QuotaState::default();
        assert!(state.record("s", weekly(CODEX, 0, 101.0)).is_none());
        assert!(state.record("s", weekly(CODEX, 0, f64::NAN)).is_none());
        assert!(state.record("s", reading(CODEX, NOON, 5.0, -1.0)).is_none());
        let mut no_length = weekly(CODEX, 0, 5.0);
        no_length.window.window_minutes = 0.0;
        assert!(state.record("s", no_length).is_none());

        assert!(state.record("s", weekly(CODEX, 10, 5.0)).is_some());
        assert!(
            state.record("s", weekly(CODEX, 10, 6.0)).is_none(),
            "same time"
        );
        assert!(state.record("s", weekly(CODEX, 5, 6.0)).is_none(), "older");
    }

    #[test]
    fn an_unchanged_meter_is_written_every_fifteen_minutes_and_its_end_before_a_change() {
        let mut state = QuotaState::default();
        let mut written = Vec::new();
        for minute in 0..20 {
            written.extend(state.record("s", weekly(CLAUDE, minute, 10.0)).unwrap());
        }
        written.extend(state.record("s", weekly(CLAUDE, 20, 12.0)).unwrap());

        let minutes: Vec<i64> = written
            .iter()
            .map(|reading| (reading.observed_at_ms - NOON) / 60_000)
            .collect();
        // First, heartbeat at 15, the last unchanged at 19, the change at 20.
        assert_eq!(minutes, [0, 15, 19, 20]);
        assert_eq!(
            state.sources["s"][CLAUDE]
                .latest
                .as_ref()
                .unwrap()
                .window
                .used_percent,
            12.0
        );
    }

    #[test]
    fn a_new_window_is_written_at_once() {
        let mut state = QuotaState::default();
        state.record("s", weekly(CODEX, 0, 80.0)).unwrap();
        // A day later in a window that started just now: a reset.
        let reset = reading(CODEX, NOON + 2 * 60_000, 80.0, 0.01);
        assert_eq!(state.record("s", reset.clone()).unwrap(), [reset]);
    }

    #[test]
    fn a_reading_clears_the_failure_of_its_series_only() {
        let mut state = QuotaState::default();
        state.fail("s", CODEX, "offline".into(), NOON);
        state.fail("s", CLAUDE, "expired".into(), NOON);
        state.record("s", weekly(CODEX, 1, 3.0)).unwrap();

        assert!(!state.failures["s"].contains_key(CODEX));
        assert_eq!(state.failures["s"][CLAUDE].message, "expired");
    }

    #[test]
    fn the_archive_keeps_every_series_apart_and_survives_a_lost_state() {
        let folder = tempfile::tempdir().unwrap();
        let files = QuotaFiles::new(folder.path().to_path_buf());
        files
            .commit(
                "s",
                vec![weekly(CLAUDE, 0, 10.0), {
                    let mut session = weekly(CLAUDE_SESSION, 0, 40.0);
                    session.window.window_minutes = 300.0;
                    session.window.elapsed_percent = 50.0;
                    session
                }],
                &[(CODEX, Outcome::Failed("not signed in".into()))],
                NOON,
            )
            .unwrap();
        files
            .commit("other", vec![weekly(CLAUDE, 1, 90.0)], &[], NOON)
            .unwrap();

        let history = files.history("s").unwrap();
        assert_eq!(history.latest(CLAUDE).unwrap().window.used_percent, 10.0);
        assert_eq!(
            history.latest(CLAUDE_SESSION).unwrap().window.used_percent,
            40.0
        );
        assert_eq!(history.failures[CODEX].message, "not signed in");

        std::fs::remove_file(files.state_path()).unwrap();
        std::fs::remove_file(files::backup_path(&files.state_path())).ok();
        let rebuilt = files.history("s").unwrap();
        assert_eq!(rebuilt.readings, history.readings);
        // A reading older than the archived one is still refused after the
        // state is rebuilt.
        let accepted = files
            .commit("s", vec![weekly(CLAUDE, -5, 1.0)], &[], NOON)
            .unwrap();
        assert!(accepted.is_empty());
    }

    #[test]
    fn a_row_cut_short_is_skipped_and_the_next_rows_stay_readable() {
        let folder = tempfile::tempdir().unwrap();
        let files = QuotaFiles::new(folder.path().to_path_buf());
        files
            .commit("s", vec![weekly(CODEX, 0, 1.0)], &[], NOON)
            .unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(files.journal_path())
            .unwrap();
        file.write_all(b"2026-10-14T12:01:00.000Z,1791979260000,s,codex,100")
            .unwrap();
        drop(file);

        files
            .commit("s", vec![weekly(CODEX, 2, 2.0)], &[], NOON)
            .unwrap();

        let readings = &files.history("s").unwrap().readings[CODEX];
        let used: Vec<f64> = readings
            .iter()
            .map(|reading| reading.window.used_percent)
            .collect();
        assert_eq!(used, [1.0, 2.0]);
    }

    #[test]
    fn concurrent_writers_lose_no_reading() {
        let folder = tempfile::tempdir().unwrap();
        let files = Arc::new(QuotaFiles::new(folder.path().to_path_buf()));
        let writers: Vec<_> = [CODEX, CLAUDE, CURSOR, CLAUDE_SESSION]
            .into_iter()
            .map(|series| {
                let files = Arc::clone(&files);
                thread::spawn(move || {
                    for minute in 0..25 {
                        // Each reading moves the meter, so each is written.
                        files
                            .commit("s", vec![weekly(series, minute, minute as f64)], &[], NOON)
                            .unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }

        let history = files.history("s").unwrap();
        let Stored::Found { value: state, .. } = files::load::<QuotaState>(&files.state_path())
        else {
            panic!("no state");
        };
        let last = NOON + 24 * 60_000;
        for series in [CODEX, CLAUDE, CURSOR, CLAUDE_SESSION] {
            assert_eq!(history.readings[series].len(), 25, "{series}");
            // No writer replaced the state with one that missed a reading.
            let saved = &state.sources["s"][series];
            assert_eq!(
                saved.latest.as_ref().unwrap().observed_at_ms,
                last,
                "{series}"
            );
            assert_eq!(
                saved.stored.as_ref().unwrap().observed_at_ms,
                last,
                "{series}"
            );
        }
    }

    #[test]
    fn the_archive_rows_read_back_as_written() {
        let original = reading(CURSOR, NOON + 1, 12.3456, 7.5);
        let row = line(&fields("s, \"quoted\"", &original));
        assert!(row.starts_with("2026-10-14T12:00:00.001Z,1791979200001,\"s, \"\"quoted\"\"\","));

        let (source, parsed) = parse_row(row.trim_end()).unwrap();
        assert_eq!(source, "s, \"quoted\"");
        assert_eq!(parsed, original);
    }

    #[test]
    fn claude_and_cursor_meters_of_a_summary_become_readings() {
        let summary: SummaryReport = serde_json::from_value(serde_json::json!({
            "totals": {"totalCost": 0, "totalTokens": 0},
            "claudeAccount": {
                "sessionUsedPercent": 40,
                "sessionResetsAtMs": NOON + 3 * HOUR,
                "weeklyUsedPercent": 12,
                "weeklyResetsAtMs": NOON + 84 * HOUR,
                "observedAtMs": NOON,
            },
            "cursorAccount": {
                "billingCycleEndMs": NOON + 400 * 24 * HOUR,
                "grants": [
                    {"kind": "promo", "totalUSD": 100, "remainingUSD": 25, "expiresAtMs": NOON + 30 * 24 * HOUR},
                    {"kind": "promo", "totalUSD": 50, "remainingUSD": 0, "expiresAtMs": NOON + HOUR},
                ],
            },
        }))
        .unwrap();

        let readings = account_readings(&summary, NOON);

        let series: Vec<&str> = readings
            .iter()
            .map(|reading| reading.series.as_str())
            .collect();
        assert_eq!(series, [CLAUDE_SESSION, CLAUDE, CURSOR]);
        assert_eq!(readings[0].window.elapsed_percent, 40.0);
        assert_eq!(readings[1].window.elapsed_percent, 50.0);
        assert_eq!(readings[0].resets_at_ms(), NOON + 3 * HOUR);
        // Spent grants count; the window ends when the live grant expires.
        assert_eq!(readings[2].window.used_percent, 75.0);
        assert_eq!(readings[2].resets_at_ms(), NOON + 30 * 24 * HOUR);
        assert_eq!(readings[2].window.api_equivalent_spent, 125.0);
    }

    #[test]
    fn source_names_hide_paths_and_tell_settings_apart() {
        let name = source_name(r"|CLAUDE_CONFIG_DIR=C:\Users\me\.claude");
        assert!(!name.contains("Users"));
        assert_eq!(name.len(), 17);
        assert_ne!(name, source_name(""));
        assert_eq!(name, source_name(r"|CLAUDE_CONFIG_DIR=C:\Users\me\.claude"));
    }
}
