//! Engine client tests. This executable is also their fake engine: started
//! with `FAKE_ENGINE_MODE`, it plays one engine behavior instead of testing.

use std::{
    env, fs,
    io::Write,
    os::windows::process::CommandExt,
    panic,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    thread,
    time::{Duration, Instant},
};

use agent_burn_windows::cli_client::{
    self, EngineError, EngineLocation, EngineRequest, OUTPUT_LIMIT,
};
use serde_json::Value;
use windows::Win32::{
    Foundation::{CloseHandle, WAIT_TIMEOUT},
    System::{
        Console::GetConsoleWindow,
        Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    },
};

const MODE: &str = "FAKE_ENGINE_MODE";
const PID_FILE: &str = "FAKE_ENGINE_PID_FILE";
const CHILD_PID_FILE: &str = "FAKE_ENGINE_CHILD_PID_FILE";

fn main() -> ExitCode {
    match env::var(MODE) {
        Ok(mode) => fake_engine(&mode),
        Err(_) => run_tests(),
    }
}

fn fake_engine(mode: &str) -> ExitCode {
    if let Some(path) = env::var_os(PID_FILE) {
        fs::write(path, std::process::id().to_string()).unwrap();
    }
    match mode {
        "echo" => {
            let report = serde_json::json!({
                "args": env::args().skip(1).collect::<Vec<_>>(),
                "console": !unsafe { GetConsoleWindow() }.is_invalid(),
                "cwd": env::current_dir().unwrap(),
            });
            print!("{report}");
            ExitCode::SUCCESS
        }
        "text" => {
            print!("Agent Burn usage summary");
            ExitCode::SUCCESS
        }
        "fail" => {
            eprintln!("Error: CliError(\"No valid Claude data directories found\")");
            ExitCode::from(1)
        }
        "flood" => {
            let chunk = vec![b'x'; 1024 * 1024];
            let mut stdout = std::io::stdout().lock();
            for _ in 0..=OUTPUT_LIMIT / chunk.len() {
                if stdout.write_all(&chunk).is_err() {
                    break;
                }
            }
            thread::sleep(Duration::from_secs(60));
            ExitCode::SUCCESS
        }
        "noisy" => {
            let line = "WARN  pricing mismatch\n".repeat(1000);
            for _ in 0..250 {
                eprint!("{line}");
            }
            print!("{{\"ok\":true}}");
            ExitCode::SUCCESS
        }
        "sleep" => {
            thread::sleep(Duration::from_secs(60));
            ExitCode::SUCCESS
        }
        // A caller without a console, like the windowed application.
        "detached-client" => {
            let request = EngineRequest::new(["summary"]).env(MODE, "echo");
            let report: Value = cli_client::run_json(&this_engine(), &request).unwrap();
            print!("{report}");
            ExitCode::SUCCESS
        }
        "spawn" => {
            let mut child = Command::new(env::current_exe().unwrap())
                .env(MODE, "sleep")
                .env(PID_FILE, env::var_os(CHILD_PID_FILE).unwrap())
                .spawn()
                .unwrap();
            let _ = child.wait();
            ExitCode::SUCCESS
        }
        _ => ExitCode::from(2),
    }
}

fn fake(mode: &str) -> EngineRequest {
    EngineRequest::new(["summary", "--json"]).env(MODE, mode)
}

fn this_engine() -> PathBuf {
    env::current_exe().unwrap()
}

fn passes_arguments_verbatim_from_the_profile_folder() {
    let args = [
        "summary",
        "--since",
        "2026-10-01",
        r"C:\Données agents\folder with spaces",
        "quote\"inside",
        "",
        r"trailing\",
    ];
    let request = EngineRequest::new(args).env(MODE, "echo");

    let report: Value = cli_client::run_json(&this_engine(), &request).unwrap();

    assert_eq!(report["args"], serde_json::json!(args));
    assert_eq!(
        report["cwd"].as_str().map(PathBuf::from),
        env::var_os("USERPROFILE").map(PathBuf::from)
    );
}

/// A console engine started by a process without a console gets a console
/// window of its own unless the client asks for none.
fn opens_no_console_window_for_a_windowed_caller() {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let output = Command::new(this_engine())
        .env(MODE, "detached-client")
        .creation_flags(DETACHED_PROCESS)
        .output()
        .unwrap();

    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["console"], false);
}

fn runs_an_engine_whose_path_has_spaces_and_accents() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("Agent Burn Été");
    fs::create_dir(&folder).unwrap();
    fs::copy(this_engine(), folder.join(cli_client::ENGINE_FILE_NAME)).unwrap();
    let engine = cli_client::bundled_engine_next_to(&folder.join("agent-burn-windows.exe"));

    let resolved = cli_client::resolve_engine(&EngineLocation::Custom(engine.clone())).unwrap();
    let report: Value = cli_client::run_json(&resolved, &fake("echo")).unwrap();

    assert_eq!(resolved, engine);
    assert_eq!(report["args"], serde_json::json!(["summary", "--json"]));
}

fn reports_a_missing_engine() {
    let missing = Path::new(r"C:\nowhere\agent-burn.exe");

    let error = cli_client::run(missing, &fake("echo")).unwrap_err();

    assert!(matches!(error, EngineError::NotFound(path) if path == missing));
}

fn rejects_output_that_is_not_json() {
    let error = cli_client::run_json::<Value>(&this_engine(), &fake("text")).unwrap_err();

    assert!(matches!(error, EngineError::InvalidJson(_)));
}

fn reports_the_exit_code_and_message() {
    let error = cli_client::run(&this_engine(), &fake("fail")).unwrap_err();

    assert!(matches!(
        error,
        EngineError::Failed { code: Some(1), ref stderr }
            if stderr.contains("No valid Claude data directories found")
    ));
}

fn drains_a_noisy_stderr() {
    let output = cli_client::run(&this_engine(), &fake("noisy")).unwrap();

    assert_eq!(output.stdout, b"{\"ok\":true}");
    assert!(output.stderr.len() <= 64 * 1024);
}

fn stops_an_engine_flooding_stdout() {
    let pids = tempfile::tempdir().unwrap();
    let pid_file = pids.path().join("engine.pid");
    let started = Instant::now();

    let error =
        cli_client::run(&this_engine(), &fake("flood").env(PID_FILE, &pid_file)).unwrap_err();

    assert!(matches!(error, EngineError::OutputTooLarge));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_gone(&pid_file);
}

fn stops_an_engine_past_its_timeout() {
    let pids = tempfile::tempdir().unwrap();
    let pid_file = pids.path().join("engine.pid");
    let request = fake("sleep")
        .env(PID_FILE, &pid_file)
        .timeout(Duration::from_secs(1));
    let started = Instant::now();

    let error = cli_client::run(&this_engine(), &request).unwrap_err();

    assert!(matches!(error, EngineError::TimedOut));
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_gone(&pid_file);
}

fn stops_the_processes_an_engine_started() {
    let pids = tempfile::tempdir().unwrap();
    let pid_file = pids.path().join("engine.pid");
    let child_pid_file = pids.path().join("child.pid");
    let request = fake("spawn")
        .env(PID_FILE, &pid_file)
        .env(CHILD_PID_FILE, &child_pid_file)
        .timeout(Duration::from_secs(3));
    let started = Instant::now();

    let error = cli_client::run(&this_engine(), &request).unwrap_err();

    assert!(matches!(error, EngineError::TimedOut));
    // A surviving child would keep the pipes open for its whole 60 s sleep.
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_gone(&pid_file);
    assert_gone(&child_pid_file);
}

/// The process whose id `pid_file` holds has ended.
fn assert_gone(pid_file: &Path) {
    let pid: u32 = fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while is_running(pid) {
        assert!(Instant::now() < deadline, "process {pid} is still running");
        thread::sleep(Duration::from_millis(50));
    }
}

fn is_running(pid: u32) -> bool {
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else {
        return false;
    };
    let running = unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT;
    let _ = unsafe { CloseHandle(handle) };
    running
}

fn run_tests() -> ExitCode {
    let tests: &[(&str, fn())] = &[
        (
            "passes_arguments_verbatim_from_the_profile_folder",
            passes_arguments_verbatim_from_the_profile_folder,
        ),
        (
            "opens_no_console_window_for_a_windowed_caller",
            opens_no_console_window_for_a_windowed_caller,
        ),
        (
            "runs_an_engine_whose_path_has_spaces_and_accents",
            runs_an_engine_whose_path_has_spaces_and_accents,
        ),
        ("reports_a_missing_engine", reports_a_missing_engine),
        (
            "rejects_output_that_is_not_json",
            rejects_output_that_is_not_json,
        ),
        (
            "reports_the_exit_code_and_message",
            reports_the_exit_code_and_message,
        ),
        ("drains_a_noisy_stderr", drains_a_noisy_stderr),
        (
            "stops_an_engine_flooding_stdout",
            stops_an_engine_flooding_stdout,
        ),
        (
            "stops_an_engine_past_its_timeout",
            stops_an_engine_past_its_timeout,
        ),
        (
            "stops_the_processes_an_engine_started",
            stops_the_processes_an_engine_started,
        ),
    ];
    // `cargo test -- <filter>`; flags such as `--nocapture` are ignored.
    let filters: Vec<String> = env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-') && arg.parse::<u32>().is_err())
        .collect();
    let (mut passed, mut failed) = (0, 0);
    for (name, test) in tests {
        if !filters.is_empty() && !filters.iter().any(|filter| name.contains(filter.as_str())) {
            continue;
        }
        let started = Instant::now();
        if panic::catch_unwind(test).is_ok() {
            passed += 1;
            println!("test {name} ... ok ({} ms)", started.elapsed().as_millis());
        } else {
            failed += 1;
            println!("test {name} ... FAILED");
        }
    }
    let verdict = if failed == 0 { "ok" } else { "FAILED" };
    println!("\ntest result: {verdict}. {passed} passed; {failed} failed");
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
