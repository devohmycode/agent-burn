//! Preferences kept between runs, in `%LOCALAPPDATA%\Agent Burn\settings.json`.
//!
//! Only what the user chose is saved. A file that cannot be decoded is kept
//! aside, never overwritten, and the defaults are used. Paths and plans are
//! checked before they reach the engine: an invalid Claude folder would make
//! the engine fail the whole report.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{files, state::EngineOptions};

const FILE_NAME: &str = "settings.json";
const VERSION: u32 = 1;
/// Overrides the settings folder, for tests and visual checks.
pub const DIRECTORY_ENV: &str = "AGENT_BURN_APP_DATA";

/// The quota the widget shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Provider {
    #[default]
    Claude,
    Codex,
    Cursor,
}

impl Provider {
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Cursor];

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Cursor => "Cursor",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub version: u32,
    /// An `agent-burn.exe` other than the one beside the application.
    pub engine_path: Option<PathBuf>,
    /// An engine configuration file (`--config`).
    pub config_path: Option<PathBuf>,
    /// Claude folders (`CLAUDE_CONFIG_DIR`); empty for the default.
    pub claude_dirs: Vec<PathBuf>,
    /// Codex homes (`CODEX_HOME`); empty for the default.
    pub codex_dirs: Vec<PathBuf>,
    /// Cursor data folder or `state.vscdb` (`CURSOR_DATA_DIR`).
    pub cursor_dir: Option<PathBuf>,
    /// Plans as the engine names them, or a monthly price; `None` detects.
    pub claude_plan: Option<String>,
    pub codex_plan: Option<String>,
    pub cursor_plan: Option<String>,
    pub offline: bool,
    pub widget_enabled: bool,
    pub widget_provider: Provider,
    /// Quotas keep being collected after the dashboard closes.
    pub background_collection: bool,
    /// The widget and the notification-area icon start at sign-in.
    pub start_at_sign_in: bool,
    /// The dashboard opens too at sign-in.
    pub open_dashboard_at_sign_in: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: VERSION,
            engine_path: None,
            config_path: None,
            claude_dirs: Vec::new(),
            codex_dirs: Vec::new(),
            cursor_dir: None,
            claude_plan: None,
            codex_plan: None,
            cursor_plan: None,
            offline: false,
            widget_enabled: true,
            widget_provider: Provider::Claude,
            background_collection: false,
            start_at_sign_in: false,
            open_dashboard_at_sign_in: false,
        }
    }
}

/// The settings folder: `AGENT_BURN_APP_DATA`, else `%LOCALAPPDATA%\Agent Burn`.
pub fn directory() -> Option<PathBuf> {
    if let Some(folder) = std::env::var_os(DIRECTORY_ENV).filter(|folder| !folder.is_empty()) {
        return Some(PathBuf::from(folder));
    }
    crate::native::local_app_data().map(|folder| folder.join("Agent Burn"))
}

/// What loading found.
#[derive(Debug)]
pub struct Loaded {
    pub settings: Settings,
    /// Something the user should know about the file.
    pub notice: Option<String>,
    /// False when saving would destroy a file that could not be kept aside.
    pub writable: bool,
}

pub fn load(folder: &Path) -> Loaded {
    let path = folder.join(FILE_NAME);
    let text = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                settings: Settings::default(),
                notice: None,
                writable: true,
            };
        }
        Err(error) => {
            return Loaded {
                settings: Settings::default(),
                notice: Some(format!(
                    "The settings file could not be read ({error}). Defaults are in use and changes will not be saved over it."
                )),
                writable: false,
            };
        }
    };
    match serde_json::from_slice::<Settings>(&text) {
        Ok(settings) => Loaded {
            settings,
            notice: None,
            writable: true,
        },
        Err(error) => {
            // The same damaged file is kept once, however often it is read.
            match files::keep_aside(&path, &text) {
                Ok(kept) => Loaded {
                    settings: Settings::default(),
                    notice: Some(format!(
                        "The settings file was damaged ({error}). It was kept as {}; defaults are in use.",
                        kept.display()
                    )),
                    writable: true,
                },
                Err(copy_error) => Loaded {
                    settings: Settings::default(),
                    notice: Some(format!(
                        "The settings file was damaged ({error}) and could not be kept aside ({copy_error}). Defaults are in use and changes will not be saved over it."
                    )),
                    writable: false,
                },
            }
        }
    }
}

/// Writes `settings` to a temporary file, then moves it over the old one,
/// so a failed save leaves the previous file whole.
pub fn save(folder: &Path, settings: &Settings) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
    files::write_atomically(&folder.join(FILE_NAME), &json).map_err(|error| error.to_string())
}

/// A setting the engine cannot use; it is left out until fixed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub setting: &'static str,
    pub message: String,
}

impl Settings {
    /// The engine options these settings give, with `bundled` as the engine
    /// when none was chosen, and the settings that had to be left out.
    pub fn engine_options(&self, bundled: PathBuf) -> (EngineOptions, Vec<Problem>) {
        let mut problems = Vec::new();
        let mut problem = |setting: &'static str, message: String| {
            problems.push(Problem { setting, message });
        };
        let engine = match &self.engine_path {
            Some(path) if path.is_file() => path.clone(),
            Some(path) => {
                problem(
                    "engine",
                    format!("No file at {}; the bundled engine is used.", path.display()),
                );
                bundled
            }
            None => bundled,
        };
        let config = match &self.config_path {
            Some(path) if path.is_file() => Some(path.clone()),
            Some(path) => {
                problem(
                    "config",
                    format!("No file at {}; it is ignored.", path.display()),
                );
                None
            }
            None => None,
        };
        let mut env = Vec::new();
        let claude: Vec<String> = self
            .claude_dirs
            .iter()
            .filter(|dir| {
                let usable = usable_list_entry(dir) && is_claude_folder(dir);
                if !usable {
                    problem(
                        "claude",
                        format!(
                            "{} is not a Claude folder (it needs a projects folder); it is ignored.",
                            dir.display()
                        ),
                    );
                }
                usable
            })
            .map(|dir| dir.display().to_string())
            .collect();
        if !claude.is_empty() {
            env.push(("CLAUDE_CONFIG_DIR".to_string(), claude.join(",")));
        }
        let codex: Vec<String> = self
            .codex_dirs
            .iter()
            .filter(|dir| {
                let usable = usable_list_entry(dir) && dir.is_dir();
                if !usable {
                    problem(
                        "codex",
                        format!("{} is not a folder; it is ignored.", dir.display()),
                    );
                }
                usable
            })
            .map(|dir| dir.display().to_string())
            .collect();
        if !codex.is_empty() {
            env.push(("CODEX_HOME".to_string(), codex.join(",")));
        }
        if let Some(dir) = &self.cursor_dir {
            if dir.exists() {
                env.push(("CURSOR_DATA_DIR".to_string(), dir.display().to_string()));
            } else {
                problem(
                    "cursor",
                    format!("Nothing at {}; it is ignored.", dir.display()),
                );
            }
        }
        let mut plan = |setting: &'static str, value: &Option<String>, names: &[&str]| {
            let value = value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            if names.contains(&value.to_ascii_lowercase().as_str()) || is_price(value) {
                Some(value.to_ascii_lowercase())
            } else {
                problem(
                    setting,
                    format!(
                        "\"{value}\" is not a plan ({} or a monthly price); the plan is detected instead.",
                        names.join(", ")
                    ),
                );
                None
            }
        };
        let claude_plan = plan("claudePlan", &self.claude_plan, &CLAUDE_PLANS);
        let codex_plan = plan("codexPlan", &self.codex_plan, &CODEX_PLANS);
        let cursor_plan = plan("cursorPlan", &self.cursor_plan, &CURSOR_PLANS);
        (
            EngineOptions {
                engine,
                offline: self.offline,
                config,
                claude_plan,
                codex_plan,
                cursor_plan,
                env,
            },
            problems,
        )
    }
}

pub const CLAUDE_PLANS: [&str; 3] = ["pro", "max-5x", "max-20x"];
pub const CODEX_PLANS: [&str; 2] = ["plus", "pro"];
pub const CURSOR_PLANS: [&str; 3] = ["pro", "pro+", "ultra"];

/// The engine splits its folder lists on commas.
fn usable_list_entry(dir: &Path) -> bool {
    !dir.as_os_str().is_empty() && !dir.to_string_lossy().contains(',')
}

/// The engine's rule: a folder holding `projects`, or `projects` itself.
fn is_claude_folder(dir: &Path) -> bool {
    dir.join("projects").is_dir()
        || (dir.file_name().is_some_and(|name| name == "projects") && dir.is_dir())
}

fn is_price(value: &str) -> bool {
    value
        .trim_start_matches('$')
        .parse::<f64>()
        .is_ok_and(|price| price.is_finite() && price > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn a_first_run_uses_the_defaults() {
        let dir = folder();

        let loaded = load(dir.path());

        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.notice.is_none());
        assert!(loaded.writable);
    }

    #[test]
    fn saved_settings_come_back() {
        let dir = folder();
        let settings = Settings {
            offline: true,
            codex_plan: Some("pro".into()),
            claude_dirs: vec![PathBuf::from(r"D:\Données agents\claude")],
            widget_provider: Provider::Cursor,
            ..Settings::default()
        };

        save(dir.path(), &settings).unwrap();
        let loaded = load(dir.path());

        assert_eq!(loaded.settings, settings);
        assert!(loaded.notice.is_none());
    }

    #[test]
    fn a_damaged_file_is_kept_aside_and_never_blocks() {
        let dir = folder();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "{ \"offline\": ").unwrap();

        let loaded = load(dir.path());

        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.writable);
        assert!(loaded.notice.as_deref().unwrap().contains("damaged"));
        let kept: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("settings.damaged-")
            })
            .collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(
            std::fs::read_to_string(kept[0].path()).unwrap(),
            "{ \"offline\": "
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ \"offline\": ");
    }

    #[test]
    fn a_damaged_file_is_kept_once_however_often_it_is_read() {
        let dir = folder();
        std::fs::write(dir.path().join(FILE_NAME), "not json").unwrap();

        for _ in 0..3 {
            let loaded = load(dir.path());
            assert!(loaded.notice.is_some());
        }

        let kept = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("settings.damaged-")
            })
            .count();
        assert_eq!(kept, 1);
    }

    #[test]
    fn missing_and_unknown_fields_are_tolerated() {
        let dir = folder();
        std::fs::write(
            dir.path().join(FILE_NAME),
            r#"{"offline": true, "futureOption": 3}"#,
        )
        .unwrap();

        let loaded = load(dir.path());

        assert!(loaded.settings.offline);
        assert!(loaded.settings.widget_enabled);
        assert!(loaded.notice.is_none());
    }

    #[test]
    fn a_failed_save_leaves_the_previous_file_whole() {
        let dir = folder();
        save(dir.path(), &Settings::default()).unwrap();
        let before = std::fs::read(dir.path().join(FILE_NAME)).unwrap();
        // A read-only file cannot be replaced.
        crate::files::tests::set_read_only(&dir.path().join(FILE_NAME), true);

        let result = save(
            dir.path(),
            &Settings {
                offline: true,
                ..Settings::default()
            },
        );

        assert!(result.is_err());
        assert_eq!(std::fs::read(dir.path().join(FILE_NAME)).unwrap(), before);
        crate::files::tests::set_read_only(&dir.path().join(FILE_NAME), false);
    }

    #[test]
    fn valid_folders_reach_the_engine() {
        let dir = folder();
        let claude = dir.path().join("Claude perso");
        std::fs::create_dir_all(claude.join("projects")).unwrap();
        let codex = dir.path().join("Codex perso");
        std::fs::create_dir_all(&codex).unwrap();
        let settings = Settings {
            claude_dirs: vec![claude.clone(), claude.join("projects")],
            codex_dirs: vec![codex.clone()],
            claude_plan: Some("Max-5x".into()),
            cursor_plan: Some("$40".into()),
            ..Settings::default()
        };

        let (options, problems) = settings.engine_options(PathBuf::from("agent-burn.exe"));

        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            options.env,
            [
                (
                    "CLAUDE_CONFIG_DIR".to_string(),
                    format!("{},{}", claude.display(), claude.join("projects").display())
                ),
                ("CODEX_HOME".to_string(), codex.display().to_string()),
            ]
        );
        assert_eq!(options.claude_plan.as_deref(), Some("max-5x"));
        assert_eq!(options.cursor_plan.as_deref(), Some("$40"));
    }

    #[test]
    fn invalid_settings_are_named_and_left_out() {
        let dir = folder();
        let not_claude = dir.path().join("empty");
        std::fs::create_dir_all(&not_claude).unwrap();
        let settings = Settings {
            engine_path: Some(dir.path().join("missing.exe")),
            config_path: Some(dir.path().join("missing.json")),
            claude_dirs: vec![not_claude],
            codex_dirs: vec![PathBuf::from(r"C:\a,b")],
            cursor_dir: Some(dir.path().join("nothing")),
            codex_plan: Some("team".into()),
            ..Settings::default()
        };

        let (options, problems) = settings.engine_options(PathBuf::from("bundled.exe"));

        assert_eq!(options.engine, PathBuf::from("bundled.exe"));
        assert_eq!(options.config, None);
        assert!(options.env.is_empty());
        assert_eq!(options.codex_plan, None);
        let named: Vec<&str> = problems.iter().map(|problem| problem.setting).collect();
        assert_eq!(
            named,
            ["engine", "config", "claude", "codex", "cursor", "codexPlan"]
        );
    }
}
