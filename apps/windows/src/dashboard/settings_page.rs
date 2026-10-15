//! The settings page. Switches and lists apply at once; a text field
//! applies when it loses focus or on Enter. Settings the engine cannot use
//! are listed under their section and left out of every report.

use std::path::{Path, PathBuf};

use eframe::egui;

use crate::{
    settings::{CLAUDE_PLANS, CODEX_PLANS, CURSOR_PLANS, Problem, Provider, Settings},
    ui::{
        components::{
            fluent::{CONTROL_WIDTH, choice_row, row, section},
            text_field,
            toggle::Toggle,
        },
        theme::{danger, muted},
    },
};

pub struct SettingsPage {
    engine: String,
    config: String,
    claude_dirs: String,
    codex_dirs: String,
    cursor_dir: String,
    claude_price: String,
    codex_price: String,
    cursor_price: String,
    claude_price_mode: bool,
    codex_price_mode: bool,
    cursor_price_mode: bool,
}

/// A plan as chosen in its list.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PlanChoice {
    Detect,
    Named(String),
    Price,
}

fn path_text(path: Option<&PathBuf>) -> String {
    path.map(|path| path.display().to_string())
        .unwrap_or_default()
}

fn lines(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_lines(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn optional_path(text: &str) -> Option<PathBuf> {
    let text = text.trim().trim_matches('"');
    (!text.is_empty()).then(|| PathBuf::from(text))
}

fn plan_choice(value: Option<&str>, names: &[&str]) -> PlanChoice {
    match value {
        None => PlanChoice::Detect,
        Some(value) if names.contains(&value) => PlanChoice::Named(value.to_string()),
        Some(_) => PlanChoice::Price,
    }
}

fn plan_label(name: &str) -> String {
    match name {
        "max-5x" => "Max 5×".into(),
        "max-20x" => "Max 20×".into(),
        "pro+" => "Pro+".into(),
        other => {
            let mut chars = other.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        }
    }
}

impl SettingsPage {
    pub fn new(settings: &Settings) -> Self {
        let price = |plan: &Option<String>, names: &[&str]| match plan {
            Some(plan) if !names.contains(&plan.as_str()) => plan.clone(),
            _ => String::new(),
        };
        Self {
            engine: path_text(settings.engine_path.as_ref()),
            config: path_text(settings.config_path.as_ref()),
            claude_dirs: lines(&settings.claude_dirs),
            codex_dirs: lines(&settings.codex_dirs),
            cursor_dir: path_text(settings.cursor_dir.as_ref()),
            claude_price: price(&settings.claude_plan, &CLAUDE_PLANS),
            codex_price: price(&settings.codex_plan, &CODEX_PLANS),
            cursor_price: price(&settings.cursor_plan, &CURSOR_PLANS),
            claude_price_mode: plan_choice(settings.claude_plan.as_deref(), &CLAUDE_PLANS)
                == PlanChoice::Price,
            codex_price_mode: plan_choice(settings.codex_plan.as_deref(), &CODEX_PLANS)
                == PlanChoice::Price,
            cursor_price_mode: plan_choice(settings.cursor_plan.as_deref(), &CURSOR_PLANS)
                == PlanChoice::Price,
        }
    }

    /// Draws the page; returns the settings when a change applies.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        settings: &Settings,
        problems: &[Problem],
        bundled: &Path,
        engine_override: bool,
    ) -> Option<Settings> {
        let mut edited = settings.clone();
        let show_problems = |ui: &mut egui::Ui, names: &[&str]| {
            for problem in problems
                .iter()
                .filter(|problem| names.contains(&problem.setting))
            {
                ui.colored_label(danger(), &problem.message);
            }
        };

        section(ui, "Engine", |ui| {
            let detail = if engine_override {
                format!("Set by --engine for this session: {}", bundled.display())
            } else {
                format!(
                    "Leave empty for the engine beside the application: {}",
                    bundled.display()
                )
            };
            row(ui, "Engine executable", &detail, |ui| {
                if path_field(ui, &mut self.engine, "Bundled engine") {
                    edited.engine_path = optional_path(&self.engine);
                }
            });
            row(
                ui,
                "Configuration file",
                "Optional agent-burn.json passed with --config.",
                |ui| {
                    if path_field(ui, &mut self.config, "No configuration file") {
                        edited.config_path = optional_path(&self.config);
                    }
                },
            );
            row(
                ui,
                "Offline mode",
                "No live quotas and no pricing downloads. Cursor provides no new report offline.",
                |ui| {
                    Toggle::new(&mut edited.offline).show(ui);
                },
            );
            show_problems(ui, &["engine", "config"]);
        });

        section(ui, "Sources", |ui| {
            row(
                ui,
                "Claude folders",
                "One per line; empty for %USERPROFILE%\\.claude. A WSL install can be read through \\\\wsl.localhost\\<distribution>\\home\\<user>\\.claude.",
                |ui| {
                    if lines_field(ui, &mut self.claude_dirs) {
                        edited.claude_dirs = parse_lines(&self.claude_dirs);
                    }
                },
            );
            row(
                ui,
                "Codex homes",
                "One per line; empty for %USERPROFILE%\\.codex.",
                |ui| {
                    if lines_field(ui, &mut self.codex_dirs) {
                        edited.codex_dirs = parse_lines(&self.codex_dirs);
                    }
                },
            );
            row(
                ui,
                "Cursor data folder",
                "Empty for %APPDATA%\\Cursor.",
                |ui| {
                    if path_field(ui, &mut self.cursor_dir, "Default folder") {
                        edited.cursor_dir = optional_path(&self.cursor_dir);
                    }
                },
            );
            show_problems(ui, &["claude", "codex", "cursor"]);
        });

        section(ui, "Plans", |ui| {
            plan_row(
                ui,
                "Claude plan",
                &mut edited.claude_plan,
                &mut self.claude_price,
                &mut self.claude_price_mode,
                &CLAUDE_PLANS,
            );
            plan_row(
                ui,
                "Codex plan",
                &mut edited.codex_plan,
                &mut self.codex_price,
                &mut self.codex_price_mode,
                &CODEX_PLANS,
            );
            plan_row(
                ui,
                "Cursor plan",
                &mut edited.cursor_plan,
                &mut self.cursor_price,
                &mut self.cursor_price_mode,
                &CURSOR_PLANS,
            );
            show_problems(ui, &["claudePlan", "codexPlan", "cursorPlan"]);
        });

        section(ui, "Taskbar widget", |ui| {
            row(
                ui,
                "Show the widget",
                "A compact quota meter next to the notification area. Drag it to float it anywhere; right-click it for options.",
                |ui| {
                    Toggle::new(&mut edited.widget_enabled).show(ui);
                },
            );
            let choices: Vec<(Provider, &str)> = Provider::ALL
                .iter()
                .map(|provider| (*provider, provider.label()))
                .collect();
            choice_row(
                ui,
                "Quota shown",
                "The provider whose quota the widget displays.",
                &mut edited.widget_provider,
                &choices,
            );
        });

        section(ui, "Background collection", |ui| {
            row(
                ui,
                "Collect quotas after closing",
                "A Task Scheduler task of your account records the quotas every minute while you are signed in, also once Agent Burn is closed. It needs no administrator rights; turning this off removes it.",
                |ui| {
                    Toggle::new(&mut edited.background_collection).show(ui);
                },
            );
        });

        section(ui, "Startup and closing", |ui| {
            row(
                ui,
                "Start when you sign in",
                "Starts the taskbar widget and the notification-area icon when you sign in to Windows, for your account only.",
                |ui| {
                    Toggle::new(&mut edited.start_at_sign_in).show(ui);
                },
            );
            if edited.start_at_sign_in {
                row(
                    ui,
                    "Open the dashboard too",
                    "Off: only the widget and the icon start; click either to open the dashboard.",
                    |ui| {
                        Toggle::new(&mut edited.open_dashboard_at_sign_in).show(ui);
                    },
                );
            }
            ui.label(
                egui::RichText::new(
                    "Closing this window keeps the widget and the icon. “Quit Agent Burn” (navigation pane, or the icon’s menu) ends them. Background collection is separate: while it is on, quotas are still read after quitting.",
                )
                .color(muted())
                .size(12.0),
            );
        });

        (edited != *settings).then_some(edited)
    }
}

/// A one-line path field; true when its text applies.
fn path_field(ui: &mut egui::Ui, text: &mut String, hint: &str) -> bool {
    text_field::singleline(text)
        .desired_width(CONTROL_WIDTH)
        .hint_text(hint)
        .show(ui)
        .response
        .lost_focus()
}

/// A one-path-per-line field; true when its text applies.
fn lines_field(ui: &mut egui::Ui, text: &mut String) -> bool {
    ui.add(
        egui::TextEdit::multiline(text)
            .desired_width(CONTROL_WIDTH)
            .desired_rows(2)
            .hint_text("Default folder"),
    )
    .lost_focus()
}

/// A plan list, with a price field while "Monthly price" is chosen. Until a
/// price is typed, the plan is detected.
fn plan_row(
    ui: &mut egui::Ui,
    title: &str,
    plan: &mut Option<String>,
    price: &mut String,
    price_mode: &mut bool,
    names: &[&str],
) {
    let current = if *price_mode {
        PlanChoice::Price
    } else {
        plan_choice(plan.as_deref(), names)
    };
    let mut chosen = current.clone();
    row(
        ui,
        title,
        "Detected from the sign-in when possible; a monthly price in dollars also works.",
        |ui| {
            if *price_mode
                && text_field::singleline(price)
                    .desired_width(90.0)
                    .hint_text("$/month")
                    .show(ui)
                    .response
                    .lost_focus()
            {
                let typed = price.trim();
                *plan = (!typed.is_empty()).then(|| typed.to_string());
            }
            let selected = match &chosen {
                PlanChoice::Detect => "Detect".to_string(),
                PlanChoice::Named(name) => plan_label(name),
                PlanChoice::Price => "Monthly price".to_string(),
            };
            egui::ComboBox::from_id_salt(title)
                .width(150.0)
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut chosen, PlanChoice::Detect, "Detect");
                    for name in names {
                        ui.selectable_value(
                            &mut chosen,
                            PlanChoice::Named((*name).to_string()),
                            plan_label(name),
                        );
                    }
                    ui.selectable_value(&mut chosen, PlanChoice::Price, "Monthly price");
                });
        },
    );
    if chosen != current {
        *price_mode = chosen == PlanChoice::Price;
        *plan = match &chosen {
            PlanChoice::Detect => None,
            PlanChoice::Named(name) => Some(name.clone()),
            PlanChoice::Price => {
                let typed = price.trim();
                (!typed.is_empty()).then(|| typed.to_string())
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::components::keyboard::sequence;

    /// Runs the page over `keys`, one per frame, as a user would.
    fn drive(settings: &Settings, keys: &[egui::Key]) -> Option<Settings> {
        let context = egui::Context::default();
        crate::ui::theme::install_fonts(&context);
        let mut page = SettingsPage::new(settings);
        let mut edited = None;
        for input in sequence(keys) {
            let mut output = context.run_ui(input, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if let Some(changed) =
                        page.show(ui, settings, &[], Path::new("agent-burn.exe"), false)
                    {
                        edited = Some(changed);
                    }
                });
            });
            output.textures_delta.clear();
        }
        edited
    }

    #[test]
    fn the_keyboard_alone_turns_offline_mode_on_and_it_survives_a_restart() {
        // Engine field, configuration field, then the offline switch.
        let edited = drive(
            &Settings::default(),
            &[
                egui::Key::Tab,
                egui::Key::Tab,
                egui::Key::Tab,
                egui::Key::Space,
            ],
        )
        .unwrap();
        assert!(edited.offline);

        let folder = tempfile::tempdir().unwrap();
        crate::settings::save(folder.path(), &edited).unwrap();
        let reloaded = crate::settings::load(folder.path());

        assert_eq!(reloaded.settings, edited);
        assert!(reloaded.settings.offline);
    }

    #[test]
    fn moving_focus_alone_changes_nothing() {
        assert_eq!(
            drive(&Settings::default(), &[egui::Key::Tab, egui::Key::Tab]),
            None
        );
    }

    #[test]
    fn folder_lists_skip_blank_lines() {
        assert_eq!(
            parse_lines("  C:\\a  \n\n\\\\wsl.localhost\\Ubuntu\\home\\me\\.claude\n"),
            [
                PathBuf::from("C:\\a"),
                PathBuf::from("\\\\wsl.localhost\\Ubuntu\\home\\me\\.claude")
            ]
        );
    }

    #[test]
    fn quoted_paths_lose_their_quotes() {
        assert_eq!(
            optional_path("\"C:\\Program Files\\agent-burn.exe\""),
            Some(PathBuf::from("C:\\Program Files\\agent-burn.exe"))
        );
        assert_eq!(optional_path("   "), None);
    }

    #[test]
    fn plans_map_to_their_choice() {
        assert_eq!(plan_choice(None, &CLAUDE_PLANS), PlanChoice::Detect);
        assert_eq!(
            plan_choice(Some("max-5x"), &CLAUDE_PLANS),
            PlanChoice::Named("max-5x".into())
        );
        assert_eq!(plan_choice(Some("150"), &CLAUDE_PLANS), PlanChoice::Price);
        assert_eq!(plan_label("max-20x"), "Max 20×");
    }
}
