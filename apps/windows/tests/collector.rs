//! Quota collector tests. This executable is also their fake engine: started
//! with `FAKE_QUOTA_MODE_FILE`, it answers the quota-only calls as the mode
//! written in that file says, and logs each call.

use std::{
    env, fs,
    io::Write,
    panic,
    path::PathBuf,
    process::ExitCode,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use agent_burn_windows::{
    collector::{self, Collection},
    quota::{self, CLAUDE, CLAUDE_SESSION, CODEX, CURSOR},
    state::EngineOptions,
};
use serde_json::json;

const MODE_FILE: &str = "FAKE_QUOTA_MODE_FILE";
/// Fake secrets an engine error could carry.
const SECRETS: [&str; 5] = [
    "sk-ant-oat01-FAKEc0ffee1234567890abcdef",
    "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJsZWEifQ.c2lnbmF0dXJl",
    "lea.martin@example.com",
    "3f2a9c1e-7b4d-4e8a-9f00-1234abcd5678",
    "cookie-value-42",
];
const LOG_FILE: &str = "FAKE_QUOTA_LOG";

fn main() -> ExitCode {
    match env::var_os(MODE_FILE) {
        Some(path) => fake_engine(&fs::read_to_string(path).unwrap_or_default()),
        None => run_tests(),
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn window(minutes: u32, used: f64, elapsed: f64) -> serde_json::Value {
    json!({"windowMinutes": minutes, "usedPercent": used, "elapsedPercent": elapsed, "apiEquivalentSpent": 0})
}

fn fake_engine(mode: &str) -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if let Some(log) = env::var_os(LOG_FILE) {
        let line = format!(
            "{} quota_only={} force={}\n",
            args.join(" "),
            env::var("AGENT_BURN_QUOTA_ONLY").unwrap_or_default(),
            env::var("AGENT_BURN_FORCE_REFRESH").unwrap_or_default(),
        );
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .unwrap();
        file.write_all(line.as_bytes()).unwrap();
    }
    let provider = match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) {
        (Some("harness"), Some(agent)) => agent.to_string(),
        (Some("summary"), _) => "cursor".into(),
        _ => return ExitCode::from(64),
    };
    if mode.trim() == "secret" {
        eprintln!(
            "Error: CliError(\"401 Unauthorized for {} with Bearer {} (token={}, account {}, cookie={})\")",
            SECRETS[2], SECRETS[1], SECRETS[0], SECRETS[3], SECRETS[4]
        );
        return ExitCode::from(1);
    }
    let unavailable = || {
        eprintln!(
            "Error: CliError(\"Live quota unavailable; previous readings should be retained\")"
        );
        ExitCode::from(1)
    };
    let skip = json!({"agent": "cursor", "observedAt": now_ms()});
    let mode = mode.trim();
    if mode == "slow" || (mode == "hang-claude" && provider == "claude") {
        thread::sleep(Duration::from_millis(if mode == "slow" {
            1500
        } else {
            4000
        }));
    }
    // A meter that moves with the clock, so each collection is a change.
    let moving = (now_ms() / 1000 % 90) as f64;
    let answer = match (mode, provider.as_str()) {
        ("ok", "codex") => {
            json!({"agent": "codex", "observedAt": now_ms(), "window": window(10080, 20.0, 30.0)})
        }
        ("ok", "claude") => json!({
            "agent": "claude", "observedAt": now_ms(),
            "window": window(10080, 30.0, 30.0), "sessionWindow": window(300, 50.0, 40.0),
        }),
        ("ok" | "none", "cursor") => skip,
        ("partial", "codex") => {
            json!({"agent": "codex", "observedAt": now_ms(), "window": window(10080, 21.0, 30.1)})
        }
        ("partial", "cursor") => {
            json!({"agent": "cursor", "observedAt": now_ms(), "window": window(525_600, 10.0, 50.0)})
        }
        ("partial" | "none", _) => return unavailable(),
        ("invalid", "codex") => {
            json!({"agent": "codex", "observedAt": now_ms(), "window": window(10080, 150.0, 30.0)})
        }
        ("invalid", "claude") => {
            json!({"agent": "codex", "observedAt": now_ms(), "window": window(10080, 31.0, 30.0)})
        }
        ("invalid", "cursor") => {
            json!({"agent": "cursor", "observedAt": now_ms() - 86_400_000, "window": window(525_600, 11.0, 50.0)})
        }
        ("slow" | "hang-claude", agent) => json!({
            "agent": agent, "observedAt": now_ms(),
            "window": window(10080, moving, 30.0),
        }),
        _ => return ExitCode::from(70),
    };
    print!("{answer}");
    ExitCode::SUCCESS
}

struct Fake {
    _folder: tempfile::TempDir,
    folder: PathBuf,
    mode_file: PathBuf,
    log: PathBuf,
    options: EngineOptions,
}

impl Fake {
    fn new() -> Self {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().to_path_buf();
        let mode_file = path.join("mode.txt");
        let log = path.join("calls.log");
        let options = EngineOptions {
            engine: env::current_exe().unwrap(),
            env: vec![
                (MODE_FILE.into(), mode_file.display().to_string()),
                (LOG_FILE.into(), log.display().to_string()),
            ],
            ..EngineOptions::default()
        };
        Self {
            _folder: folder,
            folder: path.join("Agent Burn"),
            mode_file,
            log,
            options,
        }
    }

    fn mode(&self, mode: &str) {
        fs::write(&self.mode_file, mode).unwrap();
    }

    fn collect(&self, force: bool) -> Collection {
        collector::collect(&self.options, &self.folder, force)
    }

    fn history(&self) -> quota::QuotaHistory {
        quota::files(&self.folder)
            .history(&collector::source(&self.options))
            .unwrap()
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn used(&self, series: &str) -> Option<f64> {
        self.history()
            .latest(series)
            .map(|reading| reading.window.used_percent)
    }
}

fn results(collection: Collection) -> Vec<(&'static str, bool)> {
    match collection {
        Collection::Done(results) => results
            .into_iter()
            .map(|(provider, result)| (provider, result.is_ok()))
            .collect(),
        other => panic!("expected a collection, got {other:?}"),
    }
}

fn every_provider_is_read_live_and_saved() {
    let fake = Fake::new();
    fake.mode("ok");

    let done = results(fake.collect(false));

    assert_eq!(done, [(CODEX, true), (CLAUDE, true), (CURSOR, true)]);
    assert_eq!(fake.used(CODEX), Some(20.0));
    assert_eq!(fake.used(CLAUDE), Some(30.0));
    assert_eq!(fake.used(CLAUDE_SESSION), Some(50.0), "the session apart");
    assert_eq!(fake.used(CURSOR), None, "no promotional credits");
    let mut calls = fake.calls();
    calls.sort();
    assert_eq!(
        calls,
        [
            "harness claude --json --no-color quota_only=1 force=",
            "harness codex --json --no-color quota_only=1 force=",
            "summary --value --json --no-color quota_only=1 force=",
        ]
    );
}

fn a_collection_without_any_source_stays_usable() {
    let fake = Fake::new();
    fake.mode("none");

    let done = results(fake.collect(false));

    assert_eq!(done, [(CODEX, false), (CLAUDE, false), (CURSOR, true)]);
    let history = fake.history();
    assert!(history.readings.is_empty());
    assert!(
        history.failures[CODEX]
            .message
            .contains("Live quota unavailable")
    );
    assert!(history.failures.contains_key(CLAUDE));
    assert!(fake.folder.join(quota::STATE_FILE).exists());
}

fn a_partial_failure_keeps_the_last_reading() {
    let fake = Fake::new();
    fake.mode("ok");
    fake.collect(false);
    fake.mode("partial");

    let done = results(fake.collect(false));

    assert_eq!(done, [(CODEX, true), (CLAUDE, false), (CURSOR, true)]);
    assert_eq!(fake.used(CODEX), Some(21.0));
    assert_eq!(fake.used(CLAUDE), Some(30.0), "kept");
    assert_eq!(fake.used(CLAUDE_SESSION), Some(50.0), "kept");
    assert_eq!(fake.used(CURSOR), Some(10.0));
    let history = fake.history();
    assert!(history.failures.contains_key(CLAUDE));
    assert!(!history.failures.contains_key(CODEX));
}

fn invalid_readings_are_refused() {
    let fake = Fake::new();
    fake.mode("ok");
    fake.collect(false);
    fake.mode("invalid");

    let done = results(fake.collect(false));

    assert_eq!(done, [(CODEX, false), (CLAUDE, false), (CURSOR, false)]);
    assert_eq!(fake.used(CODEX), Some(20.0));
    assert_eq!(fake.used(CLAUDE), Some(30.0));
    assert_eq!(fake.used(CURSOR), None);
}

fn offline_reads_nothing() {
    let mut fake = Fake::new();
    fake.mode("ok");
    fake.options.offline = true;

    assert_eq!(fake.collect(false), Collection::Offline);
    assert!(fake.calls().is_empty());
}

fn a_second_collector_steps_aside_or_waits_and_nothing_is_lost() {
    let fake = Fake::new();
    fake.mode("slow");
    let (first, second) = thread::scope(|scope| {
        let first = scope.spawn(|| fake.collect(false));
        thread::sleep(Duration::from_millis(300));
        let second = scope.spawn(|| fake.collect(false));
        (first.join().unwrap(), second.join().unwrap())
    });
    assert!(matches!(first, Collection::Done(_)), "{first:?}");
    assert_eq!(second, Collection::Busy);
    assert_eq!(fake.calls().len(), 3, "the busy collector called nothing");

    // Forced, the second waits for the first, then reads again.
    let (first, second) = thread::scope(|scope| {
        let first = scope.spawn(|| fake.collect(true));
        thread::sleep(Duration::from_millis(300));
        let second = scope.spawn(|| fake.collect(true));
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_eq!(
        results(first),
        [(CODEX, true), (CLAUDE, true), (CURSOR, true)]
    );
    assert_eq!(
        results(second),
        [(CODEX, true), (CLAUDE, true), (CURSOR, true)]
    );
    assert!(
        fake.calls()
            .iter()
            .skip(3)
            .all(|call| call.ends_with("force=1"))
    );
    // Three collections, 1.5 s apart: each one's readings are kept.
    let history = fake.history();
    assert_eq!(
        history.readings[CODEX].len(),
        3,
        "{:?}",
        history.readings[CODEX]
    );
    assert_eq!(history.readings[CLAUDE].len(), 3);
}

fn each_provider_is_saved_as_soon_as_it_answers() {
    let fake = Fake::new();
    fake.mode("hang-claude");
    let started = Instant::now();
    thread::scope(|scope| {
        let collection = scope.spawn(|| fake.collect(false));
        while fake.used(CODEX).is_none() {
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "codex not saved early"
            );
            thread::sleep(Duration::from_millis(50));
        }
        assert!(fake.used(CLAUDE).is_none(), "claude still running");
        assert!(!collection.is_finished());
        assert!(matches!(collection.join().unwrap(), Collection::Done(_)));
    });
    assert!(fake.used(CLAUDE).is_some());
}

fn forced_requests_during_a_collection_make_one_forced_collection() {
    let fake = Fake::new();
    fake.mode("slow");
    let watcher = collector::Watcher::start(fake.folder.clone(), || {});
    // No reading yet: the watcher collects at once.
    watcher.set_options(fake.options.clone());
    let started = Instant::now();
    while !watcher.view().collecting {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "no collection started"
        );
        thread::sleep(Duration::from_millis(20));
    }
    for _ in 0..3 {
        watcher.collect_now(true);
    }
    // The first collection, then a single forced one.
    while fake.calls().len() < 6 || watcher.view().collecting {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{:?}",
            fake.calls()
        );
        thread::sleep(Duration::from_millis(50));
    }
    thread::sleep(Duration::from_millis(500));
    let calls = fake.calls();
    assert_eq!(calls.len(), 6, "{calls:?}");
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.ends_with("force=1"))
            .count(),
        3
    );
    assert!(calls[..3].iter().all(|call| call.ends_with("force=")));
}

fn engine_errors_leave_no_secret_on_screen_or_on_disk() {
    let fake = Fake::new();
    fake.mode("secret");

    let Collection::Done(results) = fake.collect(false) else {
        panic!("no collection");
    };

    let mut shown: Vec<String> = results
        .into_iter()
        .filter_map(|(_, result)| result.err())
        .collect();
    assert_eq!(shown.len(), 3);
    for entry in fs::read_dir(&fake.folder).unwrap() {
        shown.push(fs::read_to_string(entry.unwrap().path()).unwrap_or_default());
    }
    for text in &shown {
        for secret in SECRETS {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }
    assert!(
        shown[0].contains("401 Unauthorized for [email] with Bearer [token]"),
        "{}",
        shown[0]
    );
}

fn run_tests() -> ExitCode {
    let tests: [(&str, fn()); 9] = [
        (
            "engine_errors_leave_no_secret_on_screen_or_on_disk",
            engine_errors_leave_no_secret_on_screen_or_on_disk,
        ),
        (
            "forced_requests_during_a_collection_make_one_forced_collection",
            forced_requests_during_a_collection_make_one_forced_collection,
        ),
        (
            "every_provider_is_read_live_and_saved",
            every_provider_is_read_live_and_saved,
        ),
        (
            "a_collection_without_any_source_stays_usable",
            a_collection_without_any_source_stays_usable,
        ),
        (
            "a_partial_failure_keeps_the_last_reading",
            a_partial_failure_keeps_the_last_reading,
        ),
        ("invalid_readings_are_refused", invalid_readings_are_refused),
        ("offline_reads_nothing", offline_reads_nothing),
        (
            "a_second_collector_steps_aside_or_waits_and_nothing_is_lost",
            a_second_collector_steps_aside_or_waits_and_nothing_is_lost,
        ),
        (
            "each_provider_is_saved_as_soon_as_it_answers",
            each_provider_is_saved_as_soon_as_it_answers,
        ),
    ];
    let mut failed = 0;
    for (name, test) in tests {
        let result = panic::catch_unwind(test);
        println!(
            "test {name} ... {}",
            if result.is_ok() { "ok" } else { "FAILED" }
        );
        failed += usize::from(result.is_err());
    }
    println!(
        "test result: {}. {} passed; {failed} failed",
        if failed == 0 { "ok" } else { "FAILED" },
        tests.len() - failed
    );
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
