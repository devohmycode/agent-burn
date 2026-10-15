//! The one usage state the dashboard and the widget read.
//!
//! Collections run on a worker thread. A request for the selection already
//! being collected joins that collection; a request for another selection
//! waits for it, and only the latest selection is collected next. A result
//! for a selection the user has left is dropped, never shown. The engine
//! options are part of the selection, so a result read with old settings is
//! dropped too.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::SystemTime,
};

use crate::{
    cli_client::{self, EngineRequest},
    collector,
    dates::Date,
    history::History,
    models::{HarnessReport, SummaryReport},
};

/// The usage period of the cost views. Quota windows ignore it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Period {
    /// Everything the logs hold.
    #[default]
    All,
    Today,
    Yesterday,
    /// Since Monday.
    ThisWeek,
    ThisMonth,
    ThisYear,
    /// Today and the 6 days before.
    Last7Days,
    /// Today and the 29 days before.
    Last30Days,
    /// Both days included.
    Custom {
        since: Date,
        until: Date,
    },
}

impl Period {
    /// The periods the engine names (`--range`), plus everything.
    pub const NAMED: [Self; 8] = [
        Self::All,
        Self::Today,
        Self::Yesterday,
        Self::ThisWeek,
        Self::ThisMonth,
        Self::ThisYear,
        Self::Last7Days,
        Self::Last30Days,
    ];

    pub fn label(self) -> String {
        match self {
            Self::All => "All time".into(),
            Self::Today => "Today".into(),
            Self::Yesterday => "Yesterday".into(),
            Self::ThisWeek => "This week".into(),
            Self::ThisMonth => "This month".into(),
            Self::ThisYear => "This year".into(),
            Self::Last7Days => "Last 7 days".into(),
            Self::Last30Days => "Last 30 days".into(),
            Self::Custom { since, until } => format!("{since} to {until}"),
        }
    }

    /// A name that tells periods apart in saved reports.
    pub fn key(self) -> String {
        match self {
            Self::All => "all".into(),
            Self::Custom { since, until } => format!("{since}..{until}"),
            named => named.args()[1].clone(),
        }
    }

    /// The period moves with the day: a report of it read on another day
    /// covers other days.
    pub fn counts_from_today(self) -> bool {
        !matches!(self, Self::All | Self::Custom { .. })
    }

    pub fn args(self) -> Vec<String> {
        let range = match self {
            Self::All => return Vec::new(),
            Self::Custom { since, until } => {
                return vec![
                    "--since".into(),
                    since.to_string(),
                    "--until".into(),
                    until.to_string(),
                ];
            }
            Self::Today => "today",
            Self::Yesterday => "yesterday",
            Self::ThisWeek => "wtd",
            Self::ThisMonth => "mtd",
            Self::ThisYear => "ytd",
            Self::Last7Days => "week",
            Self::Last30Days => "month",
        };
        vec!["--range".into(), range.into()]
    }

    /// First and last day, as the engine computes them from `today`; `None`
    /// for everything.
    pub fn bounds(self, today: Date) -> Option<(Date, Date)> {
        let since = match self {
            Self::All => return None,
            Self::Custom { since, until } => return Some((since, until)),
            Self::Yesterday => {
                let yesterday = today.add_days(-1);
                return Some((yesterday, yesterday));
            }
            Self::Today => today,
            Self::ThisWeek => today.add_days(-today.weekday_from_monday()),
            Self::ThisMonth => Date::new(today.year, today.month, 1)?,
            Self::ThisYear => Date::new(today.year, 1, 1)?,
            Self::Last7Days => today.add_days(-6),
            Self::Last30Days => today.add_days(-29),
        };
        Some((since, today))
    }
}

/// Engine settings that shape every report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct EngineOptions {
    pub engine: PathBuf,
    /// No live meters, no pricing download, no Cursor usage.
    pub offline: bool,
    /// `--config`.
    pub config: Option<PathBuf>,
    pub claude_plan: Option<String>,
    pub codex_plan: Option<String>,
    pub cursor_plan: Option<String>,
    /// Source folders (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `CURSOR_DATA_DIR`).
    pub env: Vec<(String, String)>,
}

impl EngineOptions {
    /// Tells apart the settings that read different reports. The engine's
    /// place is left out: a moved, updated or missing engine keeps the
    /// saved reports in view.
    pub fn source_key(&self) -> String {
        let path = |path: &Option<PathBuf>| {
            path.as_ref()
                .map_or_else(String::new, |path| path.display().to_string())
        };
        let text = |text: &Option<String>| text.clone().unwrap_or_default();
        let mut parts = vec![
            self.offline.to_string(),
            path(&self.config),
            text(&self.claude_plan),
            text(&self.codex_plan),
            text(&self.cursor_plan),
        ];
        parts.extend(self.env.iter().map(|(key, value)| format!("{key}={value}")));
        parts.join("|")
    }

    fn request(&self, mut args: Vec<String>, summary: bool) -> EngineRequest {
        if self.offline {
            args.push("--offline".into());
        }
        if let Some(config) = &self.config {
            args.extend(["--config".into(), config.display().to_string()]);
        }
        let plans = [
            ("--claude-plan", &self.claude_plan, true),
            ("--codex-plan", &self.codex_plan, true),
            // `harness` has no Cursor plan option.
            ("--cursor-plan", &self.cursor_plan, summary),
        ];
        for (flag, plan, accepted) in plans {
            if let (Some(plan), true) = (plan, accepted) {
                args.extend([flag.to_string(), plan.clone()]);
            }
        }
        self.env
            .iter()
            .fold(EngineRequest::new(args), |request, (key, value)| {
                request.env(key, value)
            })
    }
}

/// What the user asked to see.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Selection {
    pub period: Period,
    /// One agent (`claude`, `codex`, `cursor`, …), or every detected one.
    pub agent: Option<String>,
    pub options: EngineOptions,
}

impl Selection {
    /// The period and agent, for saved reports.
    pub fn key(&self) -> String {
        format!(
            "{}|{}",
            self.period.key(),
            self.agent.as_deref().unwrap_or("*")
        )
    }

    /// Every day of every agent.
    pub fn is_complete(&self) -> bool {
        self.period == Period::All && self.agent.is_none()
    }

    pub fn summary_request(&self) -> EngineRequest {
        let mut args: Vec<String> = ["summary", "--value", "--breakdown", "--json", "--no-color"]
            .map(String::from)
            .into();
        args.extend(self.period.args());
        if let Some(agent) = &self.agent {
            args.extend(["--agents".into(), agent.clone()]);
        }
        self.options.request(args, true)
    }

    pub fn harness_request(&self, harness: &str) -> EngineRequest {
        let args = ["harness", harness, "--value", "--json", "--no-color"]
            .map(String::from)
            .into();
        self.options.request(args, false)
    }

    /// Harness reports carry the weekly quotas; they ignore the period.
    pub fn harnesses(&self) -> Vec<&'static str> {
        ["claude", "codex"]
            .into_iter()
            .filter(|harness| self.agent.as_deref().is_none_or(|agent| agent == *harness))
            .collect()
    }
}

/// A report, or why it could not be read.
pub type Report<T> = Result<T, String>;

/// Everything one collection read, for one selection.
#[derive(Debug)]
pub struct Snapshot {
    pub selection: Selection,
    pub summary: Report<SummaryReport>,
    pub harnesses: Vec<(&'static str, Report<HarnessReport>)>,
    /// When the collection ended.
    pub collected_at: SystemTime,
    /// Reports saved by an earlier collection, shown until a new one ends.
    pub saved: bool,
    /// Days the summary keeps from earlier reports: the logs no longer
    /// hold them, and they have no model details.
    pub kept_days: usize,
    /// Problems with the saved history.
    pub notices: Vec<String>,
    /// The summary could not be read now: why, and when the summary shown
    /// instead was saved.
    pub summary_failure: Option<(String, SystemTime)>,
    /// Harness reports shown from an earlier collection: name, when it was
    /// saved, and why the new one failed.
    pub harness_saved: Vec<(&'static str, SystemTime, String)>,
}

impl Snapshot {
    /// What a collection read, before the saved history is applied.
    pub fn collected(
        selection: &Selection,
        summary: Report<SummaryReport>,
        harnesses: Vec<(&'static str, Report<HarnessReport>)>,
    ) -> Self {
        Self {
            selection: selection.clone(),
            summary,
            harnesses,
            collected_at: SystemTime::now(),
            saved: false,
            kept_days: 0,
            notices: Vec::new(),
            summary_failure: None,
            harness_saved: Vec::new(),
        }
    }

    /// When the harness report `name` was read: its saving time when it is
    /// an earlier one.
    pub fn harness_time(&self, name: &str) -> SystemTime {
        self.harness_saved
            .iter()
            .find(|(saved, _, _)| *saved == name)
            .map_or(self.collected_at, |(_, at, _)| *at)
    }

    pub fn harness(&self, name: &str) -> Option<&Report<HarnessReport>> {
        self.harnesses
            .iter()
            .find(|(harness, _)| *harness == name)
            .map(|(_, report)| report)
    }
}

/// Produces a snapshot; runs on a worker thread.
pub type Collect = Arc<dyn Fn(&Selection) -> Snapshot + Send + Sync>;

/// Collects with the engine named by the selection: the summary and each
/// harness run as separate engine processes, side by side. With a
/// `history`, what was read is saved, the saved days the logs lost are
/// shown, and once a day a report of everything is read besides a shorter
/// selection to keep the saved days complete.
pub fn engine_collector(history: Option<Arc<History>>) -> Collect {
    Arc::new(move |selection: &Selection| {
        let engine = &selection.options.engine;
        let today = Date::today();
        let complete = history
            .as_ref()
            .is_some_and(|history| history.needs_complete(selection, today))
            .then(|| Selection {
                period: Period::All,
                agent: None,
                options: selection.options.clone(),
            });
        let run_summary = |selection: &Selection| {
            cli_client::run_json::<SummaryReport>(engine, &selection.summary_request())
                .map_err(|error| error.to_string())
        };
        let run_harness = |harness: &'static str| {
            cli_client::run_json::<HarnessReport>(engine, &selection.harness_request(harness))
                .map_err(|error| error.to_string())
        };
        thread::scope(|scope| {
            let harnesses: Vec<_> = selection
                .harnesses()
                .into_iter()
                .map(|harness| (harness, scope.spawn(move || run_harness(harness))))
                .collect();
            let complete = complete
                .as_ref()
                .map(|complete| scope.spawn(move || run_summary(complete)));
            let summary = run_summary(selection);
            let harnesses = harnesses
                .into_iter()
                .map(|(harness, handle)| {
                    let report = handle
                        .join()
                        .unwrap_or_else(|_| Err("harness collection panicked".into()));
                    (harness, report)
                })
                .collect();
            let complete = complete.and_then(|handle| handle.join().ok()?.ok());
            let mut snapshot = Snapshot::collected(selection, summary, harnesses);
            let Some(history) = &history else {
                return snapshot;
            };
            if let Ok(summary) = &snapshot.summary
                && let Err(error) =
                    collector::record_accounts(history.folder(), &selection.options, summary)
            {
                snapshot.notices.push(format!(
                    "The live meters of this report could not be saved ({error})."
                ));
            }
            history.record(snapshot, complete, today)
        })
    })
}

/// Reads saved engine outputs (`summary.json`, `harness-claude.json`,
/// `harness-codex.json`) from `folder` instead of running the engine: the
/// captured fixtures, shown for visual checks.
pub fn fixture_collector(folder: PathBuf) -> Collect {
    Arc::new(move |selection: &Selection| {
        let read = |name: &str| -> Option<Result<String, String>> {
            let path = folder.join(name);
            path.exists()
                .then(|| std::fs::read_to_string(&path).map_err(|error| error.to_string()))
        };
        let summary =
            read("summary.json").map_or_else(|| Err(stderr_of(&folder, "summary")), decode);
        let harnesses = selection
            .harnesses()
            .into_iter()
            .map(|harness| {
                let name = format!("harness-{harness}.json");
                let report = read(&name).map_or_else(
                    || Err(stderr_of(&folder, &format!("harness-{harness}"))),
                    decode,
                );
                (harness, report)
            })
            .collect();
        Snapshot::collected(selection, summary, harnesses)
    })
}

fn decode<T: serde::de::DeserializeOwned>(text: Result<String, String>) -> Report<T> {
    text.and_then(|text| serde_json::from_str(&text).map_err(|error| error.to_string()))
}

fn stderr_of(folder: &Path, command: &str) -> String {
    std::fs::read_to_string(folder.join(format!("{command}.stderr.txt")))
        .map(|text| cli_client::engine_message(&text))
        .unwrap_or_else(|_| format!("no {command} report in this fixture"))
}

/// What a reader sees at one moment.
#[derive(Clone, Debug)]
pub struct View {
    pub selection: Selection,
    /// The latest snapshot for `selection`; `None` before the first one.
    pub snapshot: Option<Arc<Snapshot>>,
    pub collecting: bool,
}

/// The shared state handle; clones refer to the same state.
#[derive(Clone)]
pub struct Store {
    shared: Arc<Shared>,
}

/// Finds the saved reports of a selection; runs on the worker thread.
pub type Restore = Arc<dyn Fn(&Selection) -> Option<Snapshot> + Send + Sync>;

struct Shared {
    state: Mutex<State>,
    collect: Collect,
    restore: Option<Restore>,
    listeners: Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,
}

struct State {
    selection: Selection,
    snapshot: Option<Arc<Snapshot>>,
    /// The selection being collected.
    running: Option<Selection>,
    /// A request for another selection arrived during the collection.
    queued: bool,
}

impl Store {
    pub fn new(selection: Selection, collect: Collect) -> Self {
        Self::with_restore(selection, collect, None)
    }

    /// A store that shows the saved reports of a selection that has none
    /// yet, while it is collected.
    pub fn with_restore(selection: Selection, collect: Collect, restore: Option<Restore>) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    selection,
                    snapshot: None,
                    running: None,
                    queued: false,
                }),
                collect,
                restore,
                listeners: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Calls `listener` from the worker thread after each change, e.g. to
    /// repaint a window.
    pub fn subscribe(&self, listener: impl Fn() + Send + Sync + 'static) {
        lock(&self.shared.listeners).push(Box::new(listener));
    }

    pub fn view(&self) -> View {
        let state = lock(&self.shared.state);
        View {
            selection: state.selection.clone(),
            snapshot: state
                .snapshot
                .clone()
                .filter(|snapshot| snapshot.selection == state.selection),
            collecting: state.running.is_some(),
        }
    }

    /// Shows `selection` and collects it.
    pub fn select(&self, selection: Selection) {
        let mut state = lock(&self.shared.state);
        state.selection = selection;
        self.request(&mut state);
        drop(state);
        self.notify();
    }

    /// Collects the current selection again.
    pub fn refresh(&self) {
        let mut state = lock(&self.shared.state);
        self.request(&mut state);
        drop(state);
        self.notify();
    }

    fn request(&self, state: &mut State) {
        match &state.running {
            None => self.start(state),
            Some(running) if *running == state.selection => {}
            Some(_) => state.queued = true,
        }
    }

    fn start(&self, state: &mut State) {
        let selection = state.selection.clone();
        state.running = Some(selection.clone());
        let store = self.clone();
        thread::spawn(move || {
            if let Some(restore) = &store.shared.restore
                && store.lacks_snapshot(&selection)
                && let Some(saved) = restore(&selection)
            {
                store.offer(saved);
            }
            let snapshot = (store.shared.collect)(&selection);
            store.finish(snapshot);
        });
    }

    /// Whether `selection` is shown without a snapshot of its own.
    fn lacks_snapshot(&self, selection: &Selection) -> bool {
        let state = lock(&self.shared.state);
        state.selection == *selection
            && state
                .snapshot
                .as_ref()
                .is_none_or(|snapshot| snapshot.selection != *selection)
    }

    /// Shows saved reports, unless the selection changed or has a snapshot.
    fn offer(&self, saved: Snapshot) {
        let mut state = lock(&self.shared.state);
        let wanted = state.selection == saved.selection
            && state
                .snapshot
                .as_ref()
                .is_none_or(|snapshot| snapshot.selection != state.selection);
        if wanted {
            state.snapshot = Some(Arc::new(saved));
        }
        drop(state);
        if wanted {
            self.notify();
        }
    }

    fn finish(&self, snapshot: Snapshot) {
        let mut state = lock(&self.shared.state);
        let collected = state.running.take();
        let current = collected.as_ref() == Some(&state.selection);
        if current {
            state.snapshot = Some(Arc::new(snapshot));
        }
        if std::mem::take(&mut state.queued) && !current {
            self.start(&mut state);
        }
        drop(state);
        self.notify();
    }

    fn notify(&self) {
        for listener in lock(&self.shared.listeners).iter() {
            listener();
        }
    }
}

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        sync::mpsc,
        time::{Duration, Instant},
    };

    use super::*;

    /// A collector that records each selection and waits for `release`.
    struct Gate {
        calls: Arc<Mutex<Vec<Selection>>>,
        release: mpsc::Sender<()>,
    }

    fn gated() -> (Gate, Collect) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (release, released) = mpsc::channel::<()>();
        let released = Mutex::new(released);
        let recorded = Arc::clone(&calls);
        let collect: Collect = Arc::new(move |selection: &Selection| {
            lock(&recorded).push(selection.clone());
            lock(&released).recv().unwrap();
            Snapshot::collected(selection, Err("fake".into()), Vec::new())
        });
        (Gate { calls, release }, collect)
    }

    impl Gate {
        fn calls(&self) -> Vec<Selection> {
            lock(&self.calls).clone()
        }
    }

    fn wait_until(condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "condition not reached");
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn period(period: Period) -> Selection {
        Selection {
            period,
            ..Selection::default()
        }
    }

    fn args(request: &EngineRequest) -> Vec<String> {
        request
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    #[test]
    fn collects_off_the_calling_thread() {
        let (gate, collect) = gated();
        let store = Store::new(Selection::default(), collect);
        let started = Instant::now();

        store.refresh();

        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(store.view().collecting);
        wait_until(|| gate.calls().len() == 1);
        gate.release.send(()).unwrap();
        wait_until(|| !store.view().collecting);
        assert!(store.view().snapshot.is_some());
    }

    #[test]
    fn requests_for_the_running_selection_join_it() {
        let (gate, collect) = gated();
        let store = Store::new(Selection::default(), collect);

        for _ in 0..5 {
            store.refresh();
        }
        wait_until(|| gate.calls().len() == 1);
        gate.release.send(()).unwrap();
        wait_until(|| !store.view().collecting);

        assert_eq!(gate.calls().len(), 1);
    }

    #[test]
    fn a_result_for_a_left_selection_is_dropped() {
        let (gate, collect) = gated();
        let store = Store::new(period(Period::All), collect);
        store.refresh();
        wait_until(|| gate.calls().len() == 1);

        store.select(period(Period::Today));
        gate.release.send(()).unwrap();
        wait_until(|| gate.calls().len() == 2);

        assert!(store.view().snapshot.is_none());
        gate.release.send(()).unwrap();
        wait_until(|| !store.view().collecting);
        let snapshot = store.view().snapshot.unwrap();
        assert_eq!(snapshot.selection, period(Period::Today));
    }

    #[test]
    fn a_result_read_with_old_settings_is_dropped() {
        let (gate, collect) = gated();
        let store = Store::new(Selection::default(), collect);
        store.refresh();
        wait_until(|| gate.calls().len() == 1);

        let offline = Selection {
            options: EngineOptions {
                offline: true,
                ..EngineOptions::default()
            },
            ..Selection::default()
        };
        store.select(offline.clone());
        gate.release.send(()).unwrap();
        wait_until(|| gate.calls().len() == 2);

        assert!(store.view().snapshot.is_none());
        gate.release.send(()).unwrap();
        wait_until(|| !store.view().collecting);
        assert_eq!(store.view().snapshot.unwrap().selection, offline);
    }

    #[test]
    fn only_the_latest_waiting_selection_is_collected() {
        let (gate, collect) = gated();
        let store = Store::new(period(Period::All), collect);
        store.refresh();
        wait_until(|| gate.calls().len() == 1);

        store.select(period(Period::Today));
        store.select(period(Period::ThisWeek));
        store.select(period(Period::ThisMonth));
        gate.release.send(()).unwrap();
        wait_until(|| gate.calls().len() == 2);
        gate.release.send(()).unwrap();
        wait_until(|| !store.view().collecting);

        assert_eq!(
            gate.calls(),
            [period(Period::All), period(Period::ThisMonth)]
        );
    }

    #[test]
    fn returning_to_the_running_selection_keeps_its_result() {
        let (gate, collect) = gated();
        let store = Store::new(period(Period::All), collect);
        store.refresh();
        wait_until(|| gate.calls().len() == 1);

        store.select(period(Period::Today));
        store.select(period(Period::All));
        gate.release.send(()).unwrap();
        wait_until(|| !store.view().collecting);

        assert_eq!(gate.calls(), [period(Period::All)]);
        assert_eq!(
            store.view().snapshot.unwrap().selection,
            period(Period::All)
        );
    }

    #[test]
    fn saved_reports_show_until_the_collection_ends() {
        let (gate, collect) = gated();
        let restore: Restore = Arc::new(|selection: &Selection| {
            let mut saved = Snapshot::collected(selection, Err("saved".into()), Vec::new());
            saved.saved = true;
            Some(saved)
        });
        let store = Store::with_restore(Selection::default(), collect, Some(restore));

        store.refresh();
        wait_until(|| gate.calls().len() == 1);
        assert!(store.view().snapshot.unwrap().saved);
        gate.release.send(()).unwrap();
        wait_until(|| !store.view().collecting);
        assert!(!store.view().snapshot.unwrap().saved);

        // Collected again, the selection keeps its collected snapshot.
        store.refresh();
        wait_until(|| gate.calls().len() == 2);
        assert!(!store.view().snapshot.unwrap().saved);
        gate.release.send(()).unwrap();
        wait_until(|| !store.view().collecting);
    }

    #[test]
    fn every_reader_sees_the_same_snapshot() {
        let (gate, collect) = gated();
        let store = Store::new(Selection::default(), collect);
        let widget = store.clone();
        store.refresh();
        wait_until(|| gate.calls().len() == 1);
        gate.release.send(()).unwrap();
        wait_until(|| !store.view().collecting);

        let dashboard_view = store.view().snapshot.unwrap();
        let widget_view = widget.view().snapshot.unwrap();

        assert!(Arc::ptr_eq(&dashboard_view, &widget_view));
        assert_eq!(dashboard_view.collected_at, widget_view.collected_at);
    }

    #[test]
    fn listeners_hear_each_change() {
        let (gate, collect) = gated();
        let store = Store::new(Selection::default(), collect);
        let heard = Arc::new(Mutex::new(0));
        let counter = Arc::clone(&heard);
        store.subscribe(move || *lock(&counter) += 1);

        store.refresh();
        wait_until(|| gate.calls().len() == 1);
        gate.release.send(()).unwrap();
        wait_until(|| !store.view().collecting);

        assert_eq!(*lock(&heard), 2);
    }

    #[test]
    fn a_single_agent_skips_the_other_harness() {
        let selection = Selection {
            period: Period::ThisMonth,
            agent: Some("codex".into()),
            ..Selection::default()
        };

        assert_eq!(selection.harnesses(), ["codex"]);
        assert_eq!(
            args(&selection.summary_request()),
            [
                "summary",
                "--value",
                "--breakdown",
                "--json",
                "--no-color",
                "--range",
                "mtd",
                "--agents",
                "codex"
            ]
        );
    }

    #[test]
    fn a_custom_period_passes_both_inclusive_bounds() {
        let period = Period::Custom {
            since: date("2026-10-01"),
            until: date("2026-10-13"),
        };

        assert_eq!(
            period.args(),
            ["--since", "2026-10-01", "--until", "2026-10-13"]
        );
        assert_eq!(
            period.bounds(date("2026-12-31")),
            Some((date("2026-10-01"), date("2026-10-13")))
        );
    }

    #[test]
    fn named_periods_match_the_engine_ranges() {
        let today = date("2026-10-15"); // a Thursday

        let bounds = |period: Period| period.bounds(today);

        assert_eq!(bounds(Period::All), None);
        assert_eq!(bounds(Period::Today), Some((today, today)));
        assert_eq!(
            bounds(Period::Yesterday),
            Some((date("2026-10-14"), date("2026-10-14")))
        );
        assert_eq!(bounds(Period::ThisWeek), Some((date("2026-10-12"), today)));
        assert_eq!(bounds(Period::ThisMonth), Some((date("2026-10-01"), today)));
        assert_eq!(bounds(Period::ThisYear), Some((date("2026-01-01"), today)));
        assert_eq!(bounds(Period::Last7Days), Some((date("2026-10-09"), today)));
        assert_eq!(
            bounds(Period::Last30Days),
            Some((date("2026-09-16"), today))
        );
    }

    #[test]
    fn settings_reach_both_commands() {
        let selection = Selection {
            options: EngineOptions {
                offline: true,
                config: Some(PathBuf::from(r"C:\Données\agent-burn.json")),
                claude_plan: Some("max-5x".into()),
                cursor_plan: Some("ultra".into()),
                env: vec![("CODEX_HOME".into(), r"D:\codex home".into())],
                ..EngineOptions::default()
            },
            ..Selection::default()
        };

        let summary = selection.summary_request();
        let harness = selection.harness_request("claude");

        assert!(args(&summary).ends_with(&[
            "--offline".to_string(),
            "--config".into(),
            r"C:\Données\agent-burn.json".into(),
            "--claude-plan".into(),
            "max-5x".into(),
            "--cursor-plan".into(),
            "ultra".into(),
        ]));
        assert!(!args(&harness).contains(&"--cursor-plan".to_string()));
        assert_eq!(
            harness.env,
            [(
                OsString::from("CODEX_HOME"),
                OsString::from(r"D:\codex home")
            )]
        );
    }
}
