//! Start arguments: which part of the application runs in this process.

use std::{ffi::OsString, fmt, path::PathBuf};

/// Exit code for invalid start arguments (BSD `EX_USAGE`).
pub const EXIT_USAGE: u8 = 64;
/// Exit code for a mode this build does not provide yet.
pub const EXIT_UNAVAILABLE: u8 = 69;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartMode {
    /// The dashboard window (default).
    Dashboard,
    /// The taskbar widget alone (`--widget`).
    Widget,
    /// One bounded quota collection without any window (`--collect`).
    Collect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartOptions {
    pub mode: StartMode,
    /// `--engine <path>`: an `agent-burn.exe` other than the bundled one.
    pub engine: Option<PathBuf>,
    /// `--fixture <folder>`: saved engine outputs shown instead of running
    /// the engine, for visual checks.
    pub fixture: Option<PathBuf>,
    /// `--warp`: this process is the Direct3D 11 / WARP fallback.
    pub warp: bool,
    /// `--data <folder>`: the application folder (settings, history), as
    /// `AGENT_BURN_APP_DATA` sets it; for a scheduled collection under test.
    pub data: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsageError {
    ConflictingModes,
    MissingValue(&'static str),
    Unknown(OsString),
}

impl fmt::Display for UsageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConflictingModes => {
                formatter.write_str("--widget and --collect exclude each other")
            }
            Self::MissingValue(flag) => write!(formatter, "{flag} needs a path"),
            Self::Unknown(argument) => {
                write!(formatter, "unknown argument: {}", argument.display())
            }
        }
    }
}

/// Parses the arguments after the program name.
pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<StartOptions, UsageError> {
    let mut mode = None;
    let mut engine = None;
    let mut fixture = None;
    let mut warp = false;
    let mut data = None;
    let mut arguments = arguments.into_iter();
    let path_after = |arguments: &mut dyn Iterator<Item = OsString>, flag: &'static str| {
        arguments
            .next()
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .ok_or(UsageError::MissingValue(flag))
    };
    while let Some(argument) = arguments.next() {
        let chosen = match argument.to_str() {
            Some("--widget") => StartMode::Widget,
            Some("--collect") => StartMode::Collect,
            Some("--engine") => {
                engine = Some(path_after(&mut arguments, "--engine")?);
                continue;
            }
            Some("--fixture") => {
                fixture = Some(path_after(&mut arguments, "--fixture")?);
                continue;
            }
            Some("--warp") => {
                warp = true;
                continue;
            }
            Some("--data") => {
                data = Some(path_after(&mut arguments, "--data")?);
                continue;
            }
            _ => return Err(UsageError::Unknown(argument)),
        };
        if mode
            .replace(chosen)
            .is_some_and(|previous| previous != chosen)
        {
            return Err(UsageError::ConflictingModes);
        }
    }
    Ok(StartOptions {
        mode: mode.unwrap_or(StartMode::Dashboard),
        engine,
        fixture,
        warp,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn opens_the_dashboard_without_arguments() {
        assert_eq!(
            parse(args(&[])),
            Ok(StartOptions {
                mode: StartMode::Dashboard,
                engine: None,
                fixture: None,
                warp: false,
                data: None,
            })
        );
    }

    #[test]
    fn takes_another_application_folder() {
        let options = parse(args(&["--collect", "--data", r"C:\Temp\Agent Burn test"])).unwrap();
        assert_eq!(
            options.data,
            Some(PathBuf::from(r"C:\Temp\Agent Burn test"))
        );
        assert_eq!(
            parse(args(&["--data"])),
            Err(UsageError::MissingValue("--data"))
        );
    }

    #[test]
    fn selects_the_widget_and_the_collector() {
        assert_eq!(parse(args(&["--widget"])).unwrap().mode, StartMode::Widget);
        assert_eq!(
            parse(args(&["--collect"])).unwrap().mode,
            StartMode::Collect
        );
    }

    #[test]
    fn rejects_two_different_modes() {
        assert_eq!(
            parse(args(&["--widget", "--collect"])),
            Err(UsageError::ConflictingModes)
        );
    }

    #[test]
    fn accepts_an_engine_path_with_spaces() {
        let options = parse(args(&[
            "--collect",
            "--engine",
            r"C:\Program Files\Agent Burn\agent-burn.exe",
        ]))
        .unwrap();

        assert_eq!(options.mode, StartMode::Collect);
        assert_eq!(
            options.engine,
            Some(PathBuf::from(r"C:\Program Files\Agent Burn\agent-burn.exe"))
        );
    }

    #[test]
    fn reads_the_fixture_folder_and_the_warp_flag() {
        let options = parse(args(&["--fixture", r"tests\fixtures\quotas", "--warp"])).unwrap();

        assert_eq!(
            options.fixture,
            Some(PathBuf::from(r"tests\fixtures\quotas"))
        );
        assert!(options.warp);
        assert_eq!(options.mode, StartMode::Dashboard);
    }

    #[test]
    fn rejects_a_path_flag_without_a_path() {
        assert_eq!(
            parse(args(&["--engine"])),
            Err(UsageError::MissingValue("--engine"))
        );
        assert_eq!(
            parse(args(&["--fixture"])),
            Err(UsageError::MissingValue("--fixture"))
        );
    }

    #[test]
    fn rejects_unknown_arguments() {
        assert_eq!(
            parse(args(&["--dashboard"])),
            Err(UsageError::Unknown(OsString::from("--dashboard")))
        );
    }
}
