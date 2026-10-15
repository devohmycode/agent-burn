//! Runs the bundled `agent-burn.exe` engine: separate arguments, no shell, no
//! console window, bounded output and time, and no process left behind.

use std::{
    ffi::OsString,
    fmt,
    io::{self, Read},
    os::windows::{io::AsRawHandle, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde::de::DeserializeOwned;
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::CREATE_NO_WINDOW,
    },
};

pub const ENGINE_FILE_NAME: &str = "agent-burn.exe";
/// Largest report accepted on stdout.
pub const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
/// Stderr kept for error messages; the rest is drained and dropped.
const STDERR_LIMIT: usize = 64 * 1024;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Where the engine comes from: next to the application, or a binary the
/// user selected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum EngineLocation {
    #[default]
    Bundled,
    Custom(PathBuf),
}

/// The engine `agent-burn.exe` beside the application executable `app`.
pub fn bundled_engine_next_to(app: &Path) -> PathBuf {
    app.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(ENGINE_FILE_NAME)
}

/// The engine file for `location`, checked to exist.
pub fn resolve_engine(location: &EngineLocation) -> Result<PathBuf, EngineError> {
    let path = match location {
        EngineLocation::Bundled => {
            let app = std::env::current_exe().map_err(EngineError::Io)?;
            bundled_engine_next_to(&app)
        }
        EngineLocation::Custom(path) => path.clone(),
    };
    if path.is_file() {
        Ok(path)
    } else {
        Err(EngineError::NotFound(path))
    }
}

/// One engine invocation. Arguments reach the engine verbatim, one by one.
#[derive(Clone, Debug)]
pub struct EngineRequest {
    pub args: Vec<OsString>,
    /// Variables added to the inherited environment.
    pub env: Vec<(OsString, OsString)>,
    pub timeout: Duration,
}

impl EngineRequest {
    pub fn new<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            env: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// What a successful run printed.
#[derive(Debug)]
pub struct EngineOutput {
    pub stdout: Vec<u8>,
    /// Warnings, at most 64 KiB.
    pub stderr: String,
}

#[derive(Debug)]
pub enum EngineError {
    NotFound(PathBuf),
    Spawn(io::Error),
    Io(io::Error),
    /// Non-zero exit; `stderr` holds the engine's message.
    Failed {
        code: Option<i32>,
        stderr: String,
    },
    TimedOut,
    OutputTooLarge,
    InvalidJson(serde_json::Error),
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(path) => write!(
                formatter,
                "engine not found: {}. Put agent-burn.exe beside the application, or choose an engine in Settings",
                path.display()
            ),
            Self::Spawn(error) => write!(formatter, "engine could not start: {error}"),
            Self::Io(error) => write!(formatter, "engine I/O failed: {error}"),
            Self::Failed { code, stderr } => match code {
                Some(code) => write!(
                    formatter,
                    "engine exited with code {code}: {}",
                    engine_message(stderr)
                ),
                None => write!(formatter, "engine stopped: {}", engine_message(stderr)),
            },
            Self::TimedOut => formatter.write_str("engine timed out"),
            Self::OutputTooLarge => write!(
                formatter,
                "engine output exceeded {} MiB",
                OUTPUT_LIMIT / 1024 / 1024
            ),
            Self::InvalidJson(error) => write!(formatter, "engine output is not a report: {error}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// The engine's message without its Rust debug wrapping, nor secrets or
/// account identifiers (see `redact`): it is shown and saved.
pub fn engine_message(stderr: &str) -> String {
    crate::redact::redact(&unwrapped(stderr))
}

/// The engine prints `Error: CliError("…")` with escaped quotes and
/// backslashes.
fn unwrapped(stderr: &str) -> String {
    let stderr = stderr.trim();
    stderr
        .strip_prefix("Error: CliError(\"")
        .and_then(|rest| rest.strip_suffix("\")"))
        .map_or_else(
            || stderr.to_string(),
            |inner| {
                inner
                    .replace("\\\\", "\u{0}")
                    .replace("\\\"", "\"")
                    .replace('\u{0}', "\\")
            },
        )
}

/// Runs the engine and decodes its stdout as `T`.
pub fn run_json<T: DeserializeOwned>(
    engine: &Path,
    request: &EngineRequest,
) -> Result<T, EngineError> {
    let output = run(engine, request)?;
    serde_json::from_slice(&output.stdout).map_err(EngineError::InvalidJson)
}

/// Runs the engine to completion, within `request.timeout` and
/// [`OUTPUT_LIMIT`]. The engine and anything it starts are stopped on every
/// path out of this function.
pub fn run(engine: &Path, request: &EngineRequest) -> Result<EngineOutput, EngineError> {
    if !engine.is_file() {
        return Err(EngineError::NotFound(engine.to_path_buf()));
    }
    let mut command = Command::new(engine);
    command
        .args(&request.args)
        .envs(request.env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW.0);
    // The engine reads `.agent-burn\agent-burn.json` from its working
    // directory; the profile folder makes that independent of the launcher.
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|path| Path::new(path).is_dir()) {
        command.current_dir(profile);
    }
    let job = Job::new().map_err(EngineError::Spawn)?;
    let mut child = command.spawn().map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => EngineError::NotFound(engine.to_path_buf()),
        _ => EngineError::Spawn(error),
    })?;
    if let Err(error) = job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(EngineError::Spawn(error));
    }

    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = child.stdout.take().map(|pipe| {
        let overflow = Arc::clone(&overflow);
        thread::spawn(move || read_limited(pipe, OUTPUT_LIMIT, Some(&overflow)))
    });
    let stderr = child
        .stderr
        .take()
        .map(|pipe| thread::spawn(move || read_limited(pipe, STDERR_LIMIT, None)));

    let ending = wait(&mut child, &job, request.timeout, &overflow);
    // Stops what the engine may have started, so both pipes reach their end.
    job.terminate();
    let stdout = join(stdout);
    let stderr = String::from_utf8_lossy(&join(stderr)).trim().to_string();
    match ending? {
        Ending::TimedOut => Err(EngineError::TimedOut),
        Ending::Overflow => Err(EngineError::OutputTooLarge),
        Ending::Exited(status) if status.success() => Ok(EngineOutput { stdout, stderr }),
        Ending::Exited(status) => Err(EngineError::Failed {
            code: status.code(),
            stderr,
        }),
    }
}

enum Ending {
    Exited(ExitStatus),
    TimedOut,
    Overflow,
}

fn wait(
    child: &mut Child,
    job: &Job,
    timeout: Duration,
    overflow: &AtomicBool,
) -> Result<Ending, EngineError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().map_err(EngineError::Io)? {
            return Ok(Ending::Exited(status));
        }
        let ending = if overflow.load(Ordering::Acquire) {
            Ending::Overflow
        } else if Instant::now() >= deadline {
            Ending::TimedOut
        } else {
            thread::sleep(POLL_INTERVAL);
            continue;
        };
        job.terminate();
        let _ = child.kill();
        child.wait().map_err(EngineError::Io)?;
        return Ok(ending);
    }
}

/// Reads `pipe` to its end, keeping at most `limit` bytes. Past the limit it
/// raises `overflow` and stops when one is given, or keeps draining.
fn read_limited(mut pipe: impl Read, limit: usize, overflow: Option<&AtomicBool>) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let room = limit.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(room)]);
        if read > room
            && let Some(overflow) = overflow
        {
            overflow.store(true, Ordering::Release);
            break;
        }
    }
    kept
}

fn join(reader: Option<thread::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default()
}

/// A job object holding the engine and its descendants. Closing it, even when
/// the application crashes, ends every process still inside.
struct Job(HANDLE);

impl Job {
    fn new() -> io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(None, None) }.map_err(io::Error::from)?;
        let job = Self(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(io::Error::from)?;
        Ok(job)
    }

    fn assign(&self, child: &Child) -> io::Result<()> {
        unsafe { AssignProcessToJobObject(self.0, HANDLE(child.as_raw_handle())) }
            .map_err(io::Error::from)
    }

    fn terminate(&self) {
        let _ = unsafe { TerminateJobObject(self.0, 1) };
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_engine_sits_next_to_the_application() {
        assert_eq!(
            bundled_engine_next_to(Path::new(
                r"C:\Program Files\Agent Burn\agent-burn-windows.exe"
            )),
            PathBuf::from(r"C:\Program Files\Agent Burn\agent-burn.exe")
        );
    }

    #[test]
    fn custom_engine_must_exist() {
        let missing = PathBuf::from(r"C:\nowhere\agent-burn.exe");

        let error = resolve_engine(&EngineLocation::Custom(missing.clone())).unwrap_err();

        assert!(matches!(error, EngineError::NotFound(path) if path == missing));
    }

    #[test]
    fn engine_errors_lose_their_debug_wrapping() {
        assert_eq!(
            engine_message(
                r#"Error: CliError("No valid Claude data directories found in CLAUDE_CONFIG_DIR: C:\\Fixture\\error\\missing")"#
            ),
            r"No valid Claude data directories found in CLAUDE_CONFIG_DIR: C:\Fixture\error\missing"
        );
        assert_eq!(
            engine_message("Unsupported agent 'cursor' for harness.\n"),
            "Unsupported agent 'cursor' for harness."
        );
    }

    #[test]
    fn keeps_draining_past_the_limit_without_overflow_flag() {
        let data = [b'x'; 10];

        assert_eq!(read_limited(&data[..], 4, None), b"xxxx");
    }

    #[test]
    fn raises_overflow_past_the_limit() {
        let data = [b'x'; 10];
        let overflow = AtomicBool::new(false);

        let kept = read_limited(&data[..], 4, Some(&overflow));

        assert_eq!(kept, b"xxxx");
        assert!(overflow.load(Ordering::Acquire));
    }
}
