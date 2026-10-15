//! The light quota collection, after the macOS `QuotaCollector.swift`: three
//! engine calls in quota-only mode (`AGENT_BURN_QUOTA_ONLY=1`), which read
//! the live meters of Codex, Claude and Cursor without scanning any log.
//! Each provider's answer is checked (provider, units, observation time,
//! window) and saved as soon as it arrives, whatever the other calls do.
//! One collection runs at a time across every Agent Burn process.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, mpsc},
    thread,
    time::{Duration, Instant, SystemTime},
};

use serde::Deserialize;

use crate::{
    cli_client::{self, EngineRequest},
    files::Lock,
    models::SummaryReport,
    quota::{
        self, CLAUDE, CLAUDE_SESSION, CODEX, CURSOR, Outcome, QuotaHistory, QuotaReading,
        QuotaWindow,
    },
    settings,
    state::EngineOptions,
};

/// The engine reads live meters only, never the logs.
pub const QUOTA_ONLY_ENV: &str = "AGENT_BURN_QUOTA_ONLY";
/// The engine reads every meter again, bypassing shared readings.
pub const FORCE_REFRESH_ENV: &str = "AGENT_BURN_FORCE_REFRESH";
/// The lock held for a whole collection.
pub const COLLECTOR_LOCK: &str = "quota-collector";
const CALL_TIMEOUT: Duration = Duration::from_secs(45);
/// A forced collection waits this long for a running one.
const FORCE_WAIT: Duration = Duration::from_secs(60);

/// The providers, in the order their calls start.
pub const PROVIDERS: [&str; 3] = [CODEX, CLAUDE, CURSOR];

#[derive(Clone, Debug, PartialEq)]
pub enum Collection {
    /// Background collection is turned off in the settings.
    Disabled,
    /// Offline: no live meter is read.
    Offline,
    /// Another collection is running.
    Busy,
    /// Each provider: the readings saved, or why there are none.
    Done(Vec<(&'static str, Result<usize, String>)>),
    /// The collection could not start.
    Failed(String),
}

/// The quota-only output of `harness <agent>` and `summary --value`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Collected {
    agent: String,
    observed_at: f64,
    window: Option<QuotaWindow>,
    session_window: Option<QuotaWindow>,
}

/// The settings that decide which accounts are read: the series of other
/// settings are kept apart. The engine and the plans are left out, so
/// moving or updating the application keeps the same series.
pub fn source_settings(options: &EngineOptions) -> String {
    let mut parts = vec![
        options
            .config
            .as_ref()
            .map_or_else(String::new, |config| config.display().to_string()),
    ];
    parts.extend(
        options
            .env
            .iter()
            .map(|(key, value)| format!("{key}={value}")),
    );
    parts.join("|")
}

/// The archive name of these settings' readings.
pub fn source(options: &EngineOptions) -> String {
    quota::source_name(&source_settings(options))
}

fn request(options: &EngineOptions, provider: &str, force: bool) -> EngineRequest {
    let mut args: Vec<String> = match provider {
        CURSOR => vec!["summary".into(), "--value".into()],
        harness => vec!["harness".into(), harness.into()],
    };
    args.extend(["--json".into(), "--no-color".into()]);
    if let Some(config) = &options.config {
        args.extend(["--config".into(), config.display().to_string()]);
    }
    let mut request = options
        .env
        .iter()
        .fold(EngineRequest::new(args), |request, (key, value)| {
            request.env(key, value)
        })
        .env(QUOTA_ONLY_ENV, "1")
        .timeout(CALL_TIMEOUT);
    if force {
        request = request.env(FORCE_REFRESH_ENV, "1");
    }
    request
}

/// What one provider's answer gives: readings to save and failures.
fn interpret(
    provider: &'static str,
    answer: Result<Collected, String>,
    now_ms: i64,
) -> (Vec<QuotaReading>, Vec<(&'static str, Outcome)>) {
    let failed = |reason: String| {
        (
            Vec::new(),
            vec![(
                provider,
                Outcome::Failed(format!(
                    "The live quota could not be read ({reason}). The last reading is kept."
                )),
            )],
        )
    };
    let collected = match answer {
        Ok(collected) => collected,
        Err(reason) => return failed(reason),
    };
    let observed = collected.observed_at;
    let fresh =
        observed.is_finite() && (observed - now_ms as f64).abs() <= quota::MAX_CLOCK_SKEW_MS as f64;
    if collected.agent != provider || !fresh {
        return failed("the engine gave a reading of another provider or time".into());
    }
    let reading = |series: &str, window: QuotaWindow| QuotaReading {
        series: series.to_string(),
        observed_at_ms: observed as i64,
        window,
    };
    match (provider, collected.window) {
        // No promotional credits: nothing to follow, nothing wrong.
        (CURSOR, None) => (Vec::new(), vec![(provider, Outcome::Nothing)]),
        (_, Some(window)) if window.is_valid() => {
            let mut readings = vec![reading(provider, window)];
            if provider == CLAUDE
                && let Some(session) = collected.session_window.filter(QuotaWindow::is_valid)
            {
                readings.push(reading(CLAUDE_SESSION, session));
            }
            (readings, Vec::new())
        }
        _ => failed("the engine gave an invalid window".into()),
    }
}

/// One bounded collection with `options`, saved in `folder`. A `force`d
/// collection waits for a running one and has the engine read every meter
/// again; otherwise a running collection makes this one return `Busy`.
pub fn collect(options: &EngineOptions, folder: &Path, force: bool) -> Collection {
    if options.offline {
        return Collection::Offline;
    }
    let wait = if force { FORCE_WAIT } else { Duration::ZERO };
    let _lock = match Lock::acquire(folder, COLLECTOR_LOCK, wait) {
        Ok(lock) => lock,
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => return Collection::Busy,
        Err(error) => return Collection::Failed(error.to_string()),
    };
    let source = source(options);
    let files = quota::files(folder);
    let (sender, answers) = mpsc::channel();
    thread::scope(|scope| {
        for provider in PROVIDERS {
            let sender = sender.clone();
            scope.spawn(move || {
                let answer = cli_client::run_json::<Collected>(
                    &options.engine,
                    &request(options, provider, force),
                )
                .map_err(|error| error.to_string());
                let _ = sender.send((provider, answer));
            });
        }
        drop(sender);
        let mut done = Vec::new();
        // Each answer is saved at once, before the slower calls end.
        for (provider, answer) in answers {
            let now = quota::now_ms();
            let (readings, outcomes) = interpret(provider, answer, now);
            let failure = outcomes.iter().find_map(|(_, outcome)| match outcome {
                Outcome::Failed(message) => Some(message.clone()),
                Outcome::Nothing => None,
            });
            let saved = files.commit(&source, readings, &outcomes, now);
            done.push((
                provider,
                match (saved, failure) {
                    (Err(error), _) => Err(format!("The readings could not be saved: {error}")),
                    (Ok(_), Some(failure)) => Err(failure),
                    (Ok(accepted), None) => Ok(accepted.len()),
                },
            ));
        }
        done.sort_by_key(|(provider, _)| PROVIDERS.iter().position(|known| known == provider));
        Collection::Done(done)
    })
}

/// Saves the live meters a summary report carries (Claude's session and
/// week, Cursor's promotional credits), as the macOS application does.
pub fn record_accounts(
    folder: &Path,
    options: &EngineOptions,
    summary: &SummaryReport,
) -> Result<(), String> {
    let now = quota::now_ms();
    let readings = quota::account_readings(summary, now);
    if readings.is_empty() {
        return Ok(());
    }
    quota::files(folder)
        .commit(&source(options), readings, &[], now)
        .map(|_| ())
}

/// How often the watcher looks at the files and the clock.
const CHECK_EVERY: Duration = Duration::from_secs(5);
/// Readings older than this are collected again, at most this often.
const FRESH_FOR: Duration = Duration::from_secs(60);

/// What the dashboard shows of the live meters.
#[derive(Clone, Debug, Default)]
pub struct QuotaView {
    pub history: Arc<QuotaHistory>,
    /// False when the readings are only shown (fixtures).
    pub collects: bool,
    pub collecting: bool,
    /// The last collection this process made.
    pub last: Option<Collection>,
    /// Why the saved readings could not be read.
    pub error: Option<String>,
}

/// Collects the live meters while the dashboard is open: when the latest
/// readings of Codex or Claude are over a minute old or failed (a scheduled
/// collection may have read them already), and on request. Reloads the
/// saved readings when another process wrote them.
#[derive(Clone)]
pub struct Watcher {
    shared: Arc<Watch>,
}

struct Watch {
    folder: PathBuf,
    /// False: the saved readings are shown, none is collected.
    collects: bool,
    state: Mutex<WatchState>,
    wake: Condvar,
    on_change: Box<dyn Fn() + Send + Sync>,
}

/// A source and, for each quota file, when it changed and its size.
type Stamp = (String, Vec<Option<(SystemTime, u64)>>);

#[derive(Default)]
struct WatchState {
    options: Option<EngineOptions>,
    /// The options changed: read the files again without waiting.
    changed: bool,
    requested: bool,
    /// The next collection reads every meter again.
    forced: bool,
    last_attempt: Option<Instant>,
    view: QuotaView,
    /// The files as last read, to read them again only when they change.
    stamp: Option<Stamp>,
}

impl Watcher {
    pub fn start(folder: PathBuf, on_change: impl Fn() + Send + Sync + 'static) -> Self {
        Self::spawn(folder, true, on_change)
    }

    /// Shows the saved readings and follows their changes, collecting none.
    pub fn reading(folder: PathBuf, on_change: impl Fn() + Send + Sync + 'static) -> Self {
        Self::spawn(folder, false, on_change)
    }

    fn spawn(
        folder: PathBuf,
        collects: bool,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let watcher = Self {
            shared: Arc::new(Watch {
                folder,
                collects,
                state: Mutex::new(WatchState {
                    view: QuotaView {
                        collects,
                        ..QuotaView::default()
                    },
                    ..WatchState::default()
                }),
                wake: Condvar::new(),
                on_change: Box::new(on_change),
            }),
        };
        let running = watcher.clone();
        thread::spawn(move || running.run());
        watcher
    }

    /// The settings to collect with; another source shows other readings.
    pub fn set_options(&self, options: EngineOptions) {
        let mut state = lock(&self.shared.state);
        if state.options.as_ref() != Some(&options) {
            state.options = Some(options);
            state.changed = true;
            self.shared.wake.notify_all();
        }
    }

    /// Collects now; during a collection, once it ends. A `force`d one
    /// has the engine read every meter again (`AGENT_BURN_FORCE_REFRESH`).
    /// Requests made meanwhile make a single collection.
    pub fn collect_now(&self, force: bool) {
        let mut state = lock(&self.shared.state);
        state.requested = true;
        state.forced |= force;
        drop(state);
        self.shared.wake.notify_all();
    }

    pub fn view(&self) -> QuotaView {
        lock(&self.shared.state).view.clone()
    }

    fn run(&self) {
        let mut state = lock(&self.shared.state);
        loop {
            if !state.requested && !std::mem::take(&mut state.changed) {
                state = self
                    .shared
                    .wake
                    .wait_timeout(state, CHECK_EVERY)
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .0;
            }
            let Some(options) = state.options.clone() else {
                continue;
            };
            drop(state);
            if self.reload(&options) {
                (self.shared.on_change)();
            }
            state = lock(&self.shared.state);
            let retry_due = state
                .last_attempt
                .is_none_or(|attempt| attempt.elapsed() >= FRESH_FOR);
            let due = state.requested || (retry_due && overdue(&state.view.history));
            if !self.shared.collects || options.offline || !due {
                state.requested = false;
                continue;
            }
            state.requested = false;
            let force = std::mem::take(&mut state.forced);
            state.view.collecting = true;
            drop(state);
            (self.shared.on_change)();
            let collection = collect(&options, &self.shared.folder, force);
            self.reload(&options);
            state = lock(&self.shared.state);
            state.last_attempt = Some(Instant::now());
            state.view.collecting = false;
            state.view.last = Some(collection);
            drop(state);
            (self.shared.on_change)();
            state = lock(&self.shared.state);
        }
    }

    /// Reads the saved readings again when the files or the source changed;
    /// returns whether it did.
    fn reload(&self, options: &EngineOptions) -> bool {
        let files = quota::files(&self.shared.folder);
        let source = source(options);
        let stamp = Some((
            source.clone(),
            [files.state_path(), files.journal_path()]
                .iter()
                .map(|path| {
                    let metadata = std::fs::metadata(path).ok()?;
                    Some((metadata.modified().ok()?, metadata.len()))
                })
                .collect(),
        ));
        if lock(&self.shared.state).stamp == stamp {
            return false;
        }
        let read = files.history(&source);
        let mut state = lock(&self.shared.state);
        state.stamp = stamp;
        match read {
            Ok(history) => {
                state.view.history = Arc::new(history);
                state.view.error = None;
            }
            Err(error) => state.view.error = Some(error),
        }
        true
    }
}

/// Whether Codex or Claude has no reading from the last minute, or failed.
fn overdue(history: &QuotaHistory) -> bool {
    let now = quota::now_ms();
    [CODEX, CLAUDE].into_iter().any(|series| {
        history.failures.contains_key(series)
            || history
                .latest(series)
                .is_none_or(|latest| now - latest.observed_at_ms >= FRESH_FOR.as_millis() as i64)
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The `--collect` mode, which the scheduled task runs: one collection with
/// the saved settings, when background collection is on, and the
/// engine beside the application unless `engine` names another.
pub fn run_once(engine: Option<PathBuf>) -> Collection {
    let Some(folder) = settings::directory() else {
        return Collection::Failed("no application folder".into());
    };
    let loaded = settings::load(&folder);
    // A task left behind by a copy that was not started since the option
    // was turned off collects nothing.
    if !loaded.settings.background_collection {
        return Collection::Disabled;
    }
    let bundled = std::env::current_exe()
        .map(|app| cli_client::bundled_engine_next_to(&app))
        .unwrap_or_else(|_| PathBuf::from(cli_client::ENGINE_FILE_NAME));
    // Invalid settings are left out, as in the dashboard.
    let (mut options, _problems) = loaded.settings.engine_options(bundled);
    if let Some(engine) = engine {
        options.engine = engine;
    }
    collect(&options, &folder, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_791_979_200_000;

    fn window(used: f64) -> QuotaWindow {
        QuotaWindow {
            window_minutes: 10_080.0,
            used_percent: used,
            elapsed_percent: 30.0,
            api_equivalent_spent: 0.0,
        }
    }

    fn collected(agent: &str, at: i64, window: Option<QuotaWindow>) -> Collected {
        Collected {
            agent: agent.into(),
            observed_at: at as f64,
            window,
            session_window: None,
        }
    }

    fn failed(outcomes: &[(&str, Outcome)]) -> bool {
        matches!(outcomes, [(_, Outcome::Failed(_))])
    }

    #[test]
    fn readings_of_another_provider_or_time_are_refused() {
        let (readings, outcomes) =
            interpret(CODEX, Ok(collected(CLAUDE, NOW, Some(window(5.0)))), NOW);
        assert!(readings.is_empty() && failed(&outcomes));

        let stale = NOW - 91_000;
        let (readings, outcomes) =
            interpret(CODEX, Ok(collected(CODEX, stale, Some(window(5.0)))), NOW);
        assert!(readings.is_empty() && failed(&outcomes));

        let (readings, outcomes) =
            interpret(CODEX, Ok(collected(CODEX, NOW, Some(window(150.0)))), NOW);
        assert!(readings.is_empty() && failed(&outcomes));

        let (readings, outcomes) = interpret(CODEX, Ok(collected(CODEX, NOW, None)), NOW);
        assert!(
            readings.is_empty() && failed(&outcomes),
            "a harness needs its window"
        );
    }

    #[test]
    fn claude_keeps_its_session_apart_and_cursor_may_have_nothing() {
        let mut answer = collected(CLAUDE, NOW, Some(window(12.0)));
        answer.session_window = Some(QuotaWindow {
            window_minutes: 300.0,
            ..window(40.0)
        });
        let (readings, outcomes) = interpret(CLAUDE, Ok(answer), NOW);
        let series: Vec<&str> = readings
            .iter()
            .map(|reading| reading.series.as_str())
            .collect();
        assert_eq!(series, [CLAUDE, CLAUDE_SESSION]);
        assert!(outcomes.is_empty());

        let (readings, outcomes) = interpret(CURSOR, Ok(collected(CURSOR, NOW, None)), NOW);
        assert!(readings.is_empty());
        assert_eq!(outcomes, [(CURSOR, Outcome::Nothing)]);
    }

    #[test]
    fn another_account_folder_starts_other_series() {
        let folder = tempfile::tempdir().unwrap();
        let usual = EngineOptions::default();
        let other = EngineOptions {
            env: vec![("CLAUDE_CONFIG_DIR".into(), r"D:\work\.claude".into())],
            ..EngineOptions::default()
        };
        assert_ne!(source(&usual), source(&other));
        let files = quota::files(folder.path());
        let reading = |used| QuotaReading {
            series: CLAUDE.into(),
            observed_at_ms: NOW,
            window: window(used),
        };
        files
            .commit(&source(&usual), vec![reading(10.0)], &[], NOW)
            .unwrap();
        files
            .commit(&source(&other), vec![reading(90.0)], &[], NOW)
            .unwrap();

        let shown = files.history(&source(&other)).unwrap();
        assert_eq!(shown.readings[CLAUDE].len(), 1);
        assert_eq!(shown.latest(CLAUDE).unwrap().window.used_percent, 90.0);
        assert_eq!(
            files
                .history(&source(&usual))
                .unwrap()
                .latest(CLAUDE)
                .unwrap()
                .window
                .used_percent,
            10.0
        );
    }

    #[test]
    fn calls_ask_for_live_meters_only() {
        let options = EngineOptions {
            config: Some(PathBuf::from(r"C:\cfg.json")),
            env: vec![("CODEX_HOME".into(), r"D:\codex".into())],
            claude_plan: Some("max-5x".into()),
            ..EngineOptions::default()
        };
        let cursor = request(&options, CURSOR, false);
        let args: Vec<String> = cursor
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "summary",
                "--value",
                "--json",
                "--no-color",
                "--config",
                r"C:\cfg.json"
            ]
        );
        let env: Vec<(String, String)> = request(&options, CODEX, true)
            .env
            .iter()
            .map(|(key, value)| (key.to_string_lossy().into(), value.to_string_lossy().into()))
            .collect();
        assert!(env.contains(&(QUOTA_ONLY_ENV.into(), "1".into())));
        assert!(env.contains(&(FORCE_REFRESH_ENV.into(), "1".into())));
        assert!(env.contains(&("CODEX_HOME".into(), r"D:\codex".into())));
        // Moving the application or changing a plan keeps the same series.
        let moved = EngineOptions {
            engine: PathBuf::from(r"E:\elsewhere\agent-burn.exe"),
            claude_plan: None,
            ..options.clone()
        };
        assert_eq!(source(&options), source(&moved));
    }
}
