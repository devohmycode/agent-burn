//! The dashboard shell: navigation, the page header with the period and the
//! refresh state, and the pages. The shell layout and the live theme follow
//! Taskbar Hub's `src/dashboard/app.rs` (Apache-2.0, see NOTICE).

use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use eframe::egui;

use super::{
    Launch, Surface, clock, format, frame::NativeFrame, overview, quota_history, quotas,
    settings_page::SettingsPage, usage,
};
use crate::{
    cli_client::{self, EngineLocation},
    collector,
    dates::Date,
    history::History,
    hub, quota, scheduled,
    settings::{self, Settings},
    startup,
    state::{self, Period, Selection, Snapshot, Store, View},
    ui::{
        components::{
            fluent::{navigation_item, page_title},
            text_field,
        },
        theme::{self, Fluent, configure_fluent_style, danger, muted},
    },
    update,
};

/// Width of the navigation pane.
const NAVIGATION_WIDTH: f32 = 200.0;
/// How often the Windows theme and accent are read again.
const THEME_REFRESH: Duration = Duration::from_secs(1);
/// Sets the zoom factor at start, to check other display scales.
const ZOOM_ENV: &str = "AGENT_BURN_APP_ZOOM";
/// Appends one JSON line per finished collection: frames drawn meanwhile,
/// the longest of them and the slow ones, to check that collecting never
/// blocks the window.
const TRACE_ENV: &str = "AGENT_BURN_APP_TRACE";
/// Opens on a page: `overview`, `quotas`, `usage` or `settings`.
const PAGE_ENV: &str = "AGENT_BURN_APP_PAGE";
/// Scrolls the page down by this many points at start, to check what lies
/// below the first screen.
const SCROLL_ENV: &str = "AGENT_BURN_APP_SCROLL";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Overview,
    Quotas,
    Usage,
    Settings,
}

impl Page {
    const ALL: [Self; 4] = [Self::Overview, Self::Quotas, Self::Usage, Self::Settings];

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Quotas => "Quotas",
            Self::Usage => "Usage",
            Self::Settings => "Settings",
        }
    }
}

pub struct DashboardApp {
    frame: NativeFrame,
    fluent: Fluent,
    /// The Windows theme and the settings file stamp, as the watching
    /// thread last read them.
    outside: Arc<std::sync::Mutex<Outside>>,
    store: Store,
    /// Collects the live meters, or only reads them for fixtures.
    quotas: Option<collector::Watcher>,
    quota_range: quota_history::Range,
    page: Page,
    /// The engine beside the application, or the one `--engine` named.
    bundled_engine: PathBuf,
    engine_override: bool,
    settings_folder: Option<PathBuf>,
    settings: Settings,
    settings_page: SettingsPage,
    settings_writable: bool,
    /// Something the user should know about the settings file.
    settings_notice: Option<String>,
    save_error: Option<String>,
    problems: Vec<settings::Problem>,
    custom_since: String,
    custom_until: String,
    custom_error: Option<String>,
    /// The folder the scheduled task is managed from; none for fixtures
    /// and another application folder.
    schedule_folder: Option<PathBuf>,
    /// The application folder this dashboard shares with the background
    /// process; none for fixtures.
    hub_folder: Option<PathBuf>,
    /// The settings file as last read or written here.
    settings_stamp: Option<(SystemTime, u64)>,
    /// The last result of registering or removing it.
    schedule: Arc<std::sync::Mutex<Option<Result<scheduled::Schedule, String>>>>,
    /// The last result of writing or removing the sign-in entry.
    startup: Arc<std::sync::Mutex<Option<Result<startup::Startup, String>>>>,
    /// The last update check; none while it runs or before it starts.
    update: Arc<std::sync::Mutex<Option<update::Check>>>,
    repaint: egui::Context,
    frames: FrameWatch,
    trace: Option<PathBuf>,
    start_scroll: Option<(f32, Instant)>,
}

impl DashboardApp {
    pub fn new(context: &egui::Context, frame: NativeFrame, launch: &Launch) -> Self {
        theme::install_fonts(context);
        let fluent = Fluent::from_system(frame.mica);
        configure_fluent_style(context, fluent);
        frame.set_dark(!fluent.light);
        if let Some(zoom) = std::env::var(ZOOM_ENV)
            .ok()
            .and_then(|zoom| zoom.parse::<f32>().ok())
            .filter(|zoom| (0.5..=4.0).contains(zoom))
        {
            // As Windows does at that display scale: bigger text, and a
            // window grown to keep the same room in points.
            context.set_zoom_factor(zoom);
            context.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                super::initial_size()[0],
                super::initial_size()[1],
            )));
        }

        let (bundled_engine, engine_override) = match &launch.engine {
            EngineLocation::Custom(path) => (path.clone(), true),
            EngineLocation::Bundled => (
                std::env::current_exe()
                    .map(|app| cli_client::bundled_engine_next_to(&app))
                    .unwrap_or_else(|_| PathBuf::from(cli_client::ENGINE_FILE_NAME)),
                false,
            ),
        };
        let settings_folder = settings::directory();
        let loaded = match &settings_folder {
            Some(folder) => settings::load(folder),
            None => settings::Loaded {
                settings: Settings::default(),
                notice: Some(
                    "No folder for the settings was found; changes will not be kept.".into(),
                ),
                writable: false,
            },
        };
        // Fixtures are shown as captured: nothing is saved nor added.
        let history = settings_folder
            .as_deref()
            .filter(|_| launch.fixture.is_none())
            .map(|folder| Arc::new(History::new(folder.to_path_buf())));
        let collect = match &launch.fixture {
            Some(folder) => state::fixture_collector(folder.clone()),
            None => state::engine_collector(history.clone()),
        };
        // With fixtures, the saved readings are shown but none is collected.
        let quotas = settings_folder.as_ref().map(|folder| {
            let repaint = context.clone();
            let on_change = move || repaint.request_repaint();
            match &launch.fixture {
                None => collector::Watcher::start(folder.clone(), on_change),
                Some(_) => collector::Watcher::reading(folder.clone(), on_change),
            }
        });
        let restore = history.map(|history| {
            Arc::new(move |selection: &Selection| history.saved(selection, Date::today()))
                as state::Restore
        });
        let hub_folder = settings_folder.clone().filter(|_| launch.fixture.is_none());
        let stamp = settings_folder
            .as_deref()
            .and_then(|folder| settings_stamp(&folder.join("settings.json")));
        let outside = watch_outside(
            context,
            frame.mica,
            settings_folder
                .as_deref()
                .map(|folder| folder.join("settings.json")),
        );
        let schedule_folder = settings_folder
            .clone()
            .filter(|_| launch.fixture.is_none())
            .filter(|_| std::env::var_os(settings::DIRECTORY_ENV).is_none());
        let mut app = Self {
            frame,
            fluent,
            outside,
            store: Store::with_restore(Selection::default(), collect, restore),
            quotas,
            quota_range: quota_history::Range::initial(),
            page: initial_page(),
            bundled_engine,
            engine_override,
            settings_folder,
            settings_page: SettingsPage::new(&loaded.settings),
            settings: loaded.settings,
            settings_writable: loaded.writable,
            settings_notice: loaded.notice,
            save_error: None,
            problems: Vec::new(),
            custom_since: String::new(),
            custom_until: String::new(),
            custom_error: None,
            schedule_folder,
            hub_folder,
            settings_stamp: stamp,
            schedule: Arc::default(),
            startup: Arc::default(),
            update: Arc::default(),
            repaint: context.clone(),
            frames: FrameWatch::default(),
            trace: std::env::var_os(TRACE_ENV).map(PathBuf::from),
            start_scroll: std::env::var(SCROLL_ENV)
                .ok()
                .and_then(|offset| offset.parse().ok())
                .map(|offset| (offset, Instant::now())),
        };
        let repaint = context.clone();
        app.store.subscribe(move || repaint.request_repaint());
        let selection = Selection {
            options: app.engine_options(),
            ..Selection::default()
        };
        if let Some(quotas) = &app.quotas {
            quotas.set_options(selection.options.clone());
        }
        app.store.select(selection);
        app.check_update();
        // Points the task and the sign-in entry at this copy of the
        // application, wherever it is.
        app.sync_schedule();
        app.sync_startup();
        if let (Some(folder), Some(hwnd)) = (&app.hub_folder, app.frame.hwnd()) {
            hub::instance::mark_dashboard(hwnd, folder);
        }
        app
    }

    fn engine_options(&mut self) -> state::EngineOptions {
        let (mut options, problems) = self.settings.engine_options(self.bundled_engine.clone());
        if self.engine_override {
            options.engine = self.bundled_engine.clone();
        }
        self.problems = problems;
        options
    }

    /// Saves `settings` and collects again when they change the reports.
    fn apply_settings(&mut self, settings: Settings) {
        if settings == self.settings {
            return;
        }
        let schedule_changed =
            settings.background_collection != self.settings.background_collection;
        let startup_changed = (
            settings.start_at_sign_in,
            settings.open_dashboard_at_sign_in,
        ) != (
            self.settings.start_at_sign_in,
            self.settings.open_dashboard_at_sign_in,
        );
        let gone_online = self.settings.offline && !settings.offline;
        self.settings = settings;
        if gone_online {
            self.check_update();
        }
        if schedule_changed {
            self.sync_schedule();
        }
        if startup_changed {
            self.sync_startup();
        }
        self.save_error = match (&self.settings_folder, self.settings_writable) {
            (Some(folder), true) => settings::save(folder, &self.settings).err(),
            _ => None,
        };
        if let Some(folder) = &self.settings_folder {
            self.settings_stamp = settings_stamp(&folder.join("settings.json"));
        }
        if let Some(folder) = &self.hub_folder {
            hub::instance::tell_hub(folder, hub::instance::WM_APP_SETTINGS);
        }
        let options = self.engine_options();
        if let Some(quotas) = &self.quotas {
            quotas.set_options(options.clone());
        }
        let view = self.store.view();
        if options != view.selection.options {
            self.store.select(Selection {
                options,
                ..view.selection
            });
        }
    }

    /// Registers or removes the scheduled collection to match the setting,
    /// off the window's thread. Fixtures and another application folder
    /// (`AGENT_BURN_APP_DATA`) never touch it.
    fn sync_schedule(&self) {
        let Some(folder) = self.schedule_folder.clone() else {
            return;
        };
        let Ok(executable) = std::env::current_exe() else {
            return;
        };
        let enabled = self.settings.background_collection;
        let status = Arc::clone(&self.schedule);
        let repaint = self.repaint.clone();
        *state::lock(&status) = None;
        std::thread::spawn(move || {
            let plan = scheduled::Plan::for_executable(executable);
            let result = scheduled::ensure(enabled, &plan, &folder);
            *state::lock(&status) = Some(result);
            repaint.request_repaint();
        });
    }

    /// Writes or removes the sign-in entry to match the settings, as for
    /// the scheduled task.
    fn sync_startup(&self) {
        if self.schedule_folder.is_none() {
            return;
        }
        let Ok(executable) = std::env::current_exe() else {
            return;
        };
        let (enabled, open) = (
            self.settings.start_at_sign_in,
            self.settings.open_dashboard_at_sign_in,
        );
        *state::lock(&self.startup) = Some(startup::ensure(
            startup::RUN_KEY,
            enabled,
            open,
            &executable,
        ));
    }

    /// Looks for a newer Windows release off the window's thread; never
    /// while offline or for fixtures.
    fn check_update(&self) {
        if self.hub_folder.is_none() || self.settings.offline {
            return;
        }
        let status = Arc::clone(&self.update);
        let repaint = self.repaint.clone();
        *state::lock(&status) = None;
        std::thread::spawn(move || {
            let result = update::check();
            *state::lock(&status) = Some(result);
            repaint.request_repaint();
        });
    }

    fn update_section(&self, ui: &mut egui::Ui) {
        if self.hub_folder.is_none() {
            return;
        }
        let check = state::lock(&self.update).clone();
        let text = match (&check, self.settings.offline) {
            (_, true) => "Offline: updates are not checked.".to_string(),
            (None, false) => "Checking for updates…".to_string(),
            (Some(update::Check::Current), false) => {
                format!("Agent Burn {} is the latest version.", update::CURRENT)
            }
            (Some(update::Check::Available(release)), false) => format!(
                "Agent Burn {} is available (this is {}). Extract the new archive over this folder or beside it; the settings and histories stay in your user folder.",
                release.version,
                update::CURRENT
            ),
            (Some(update::Check::Failed(error)), false) => {
                format!("Updates could not be checked: {error}.")
            }
        };
        crate::ui::components::fluent::section(ui, "Updates", |ui| {
            crate::ui::components::fluent::card(ui, |ui| {
                ui.label(text);
                self.update_buttons(ui, &check);
            });
        });
    }

    fn update_buttons(&self, ui: &mut egui::Ui, check: &Option<update::Check>) {
        ui.horizontal(|ui| {
            if let Some(update::Check::Available(release)) = check {
                if ui.button("Download").clicked() {
                    update::open(&release.archive);
                }
                if ui.button("Release notes").clicked() {
                    update::open(&release.page);
                }
            }
            if !self.settings.offline && check.is_some() && ui.button("Check for updates").clicked()
            {
                self.check_update();
            }
        });
    }

    fn startup_section(&self, ui: &mut egui::Ui) {
        if self.schedule_folder.is_none() {
            return;
        }
        let (color, text) = match &*state::lock(&self.startup) {
            None => return,
            Some(Ok(startup::Startup::Off | startup::Startup::Removed)) => {
                (muted(), "Agent Burn does not start at sign-in.".to_string())
            }
            Some(Ok(_)) => (
                muted(),
                "Agent Burn starts at sign-in (Windows Settings › Apps › Startup lists it as “Agent Burn”).".to_string(),
            ),
            Some(Err(error)) => (
                danger(),
                format!("The sign-in entry could not be updated: {error}"),
            ),
        };
        ui.colored_label(color, text);
    }

    /// What became of the scheduled collection.
    fn schedule_section(&self, ui: &mut egui::Ui) {
        if self.schedule_folder.is_none() {
            return;
        }
        let text = match &*state::lock(&self.schedule) {
            None => (muted(), "Updating the scheduled task…".to_string()),
            Some(Ok(scheduled::Schedule::Off | scheduled::Schedule::Removed)) => (
                muted(),
                "No scheduled task: quotas are collected only while Agent Burn is open."
                    .to_string(),
            ),
            Some(Ok(_)) => (
                muted(),
                format!(
                    "Scheduled task “{}” is on: quotas are collected every minute while you are signed in.",
                    scheduled::TASK_NAME
                ),
            ),
            Some(Err(error)) => (
                danger(),
                format!("The scheduled task could not be updated: {error}"),
            ),
        };
        ui.colored_label(text.0, text.1);
    }

    /// Takes the settings the background process changed (from the widget's
    /// menu).
    fn reload_settings(&mut self) {
        let Some(folder) = self.settings_folder.clone() else {
            return;
        };
        let stamp = settings_stamp(&folder.join("settings.json"));
        if stamp == self.settings_stamp {
            return;
        }
        self.settings_stamp = stamp;
        let loaded = settings::load(&folder);
        if loaded.settings != self.settings {
            self.settings_page = SettingsPage::new(&loaded.settings);
            self.settings = loaded.settings;
            let options = self.engine_options();
            if let Some(quotas) = &self.quotas {
                quotas.set_options(options.clone());
            }
            let view = self.store.view();
            if options != view.selection.options {
                self.store.select(Selection {
                    options,
                    ..view.selection
                });
            }
        }
    }

    /// Ends Agent Burn: the dashboard, the widget and the icon.
    fn quit(&self, context: &egui::Context) {
        if let Some(folder) = &self.hub_folder {
            hub::instance::tell_hub(folder, hub::instance::WM_APP_QUIT);
        }
        context.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Follows the Windows theme and accent.
    fn refresh_theme(&mut self, context: &egui::Context) {
        let (fluent, stamp) = *state::lock(&self.outside);
        if stamp != self.settings_stamp {
            self.reload_settings();
        }
        if fluent != self.fluent {
            self.fluent = fluent;
            configure_fluent_style(context, fluent);
            self.frame.set_dark(!fluent.light);
        }
    }

    fn shell(&mut self, ui: &mut egui::Ui, view: &View) {
        let height = ui.available_height();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.allocate_ui_with_layout(
                egui::vec2(NAVIGATION_WIDTH, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_height(height);
                    egui::Frame::new()
                        .inner_margin(egui::Margin {
                            left: 8,
                            right: 8,
                            top: 12,
                            bottom: 8,
                        })
                        .show(ui, |ui| {
                            ui.set_width(NAVIGATION_WIDTH - 16.0);
                            ui.spacing_mut().item_spacing.y = 4.0;
                            ui.label(
                                egui::RichText::new("Agent Burn")
                                    .font(crate::ui::components::fluent::semibold(15.0))
                                    .color(theme::menu_text()),
                            );
                            ui.add_space(12.0);
                            for page in Page::ALL {
                                if navigation_item(ui, self.page == page, page.title()).clicked() {
                                    self.page = page;
                                }
                            }
                            ui.add_space(16.0);
                            if let Some(update::Check::Available(release)) =
                                &*state::lock(&self.update)
                            {
                                let offer = ui
                                    .link(format!("Version {} is available", release.version))
                                    .on_hover_text("Opens the download of the Windows archive.");
                                if offer.clicked() {
                                    update::open(&release.archive);
                                }
                                ui.add_space(8.0);
                            }
                            let quit = ui.button("Quit Agent Burn").on_hover_text(
                                "Closes this window, the taskbar widget and the notification-area icon. Closing the window alone keeps them.",
                            );
                            if quit.clicked() {
                                self.quit(ui.ctx());
                            }
                        });
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_height(height);
                    ui.spacing_mut().item_spacing.x = 9.0;
                    self.page(ui, view);
                },
            );
        });
    }

    fn page(&mut self, ui: &mut egui::Ui, view: &View) {
        page_title(ui, self.page.title());
        ui.horizontal(|ui| {
            ui.add_space(28.0);
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - 28.0).max(100.0));
                self.toolbar(ui, view);
            });
        });
        ui.add_space(8.0);
        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
        // Applied while the page fills, so a short first frame does not
        // clamp it.
        if let Some((offset, since)) = self.start_scroll {
            if since.elapsed() < Duration::from_secs(2) {
                scroll = scroll.vertical_scroll_offset(offset);
                ui.ctx().request_repaint();
            } else {
                self.start_scroll = None;
            }
        }
        scroll.show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: 28,
                    right: 28,
                    top: 0,
                    bottom: 20,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    match self.page {
                        Page::Overview => show_or_wait(ui, view, |ui, snapshot| {
                            overview::show(ui, snapshot);
                        }),
                        Page::Quotas => {
                            let readings =
                                self.quotas.as_ref().map(|watcher| watcher.view().history);
                            show_or_wait(ui, view, |ui, snapshot| {
                                quotas::show(ui, snapshot, readings.as_deref(), SystemTime::now());
                            });
                            if let Some(watcher) = &self.quotas {
                                let now = SystemTime::now();
                                let now_ms = quota::now_ms();
                                quota_history::show(
                                    ui,
                                    &watcher.view().history,
                                    &mut self.quota_range,
                                    now_ms,
                                    clock::local_midnight_ms(now).unwrap_or(now_ms),
                                );
                            }
                        }
                        Page::Usage => show_or_wait(ui, view, |ui, snapshot| {
                            usage::show(ui, snapshot, Date::today());
                        }),
                        Page::Settings => {
                            let edited = self.settings_page.show(
                                ui,
                                &self.settings,
                                &self.problems,
                                &self.bundled_engine,
                                self.engine_override,
                            );
                            if let Some(settings) = edited {
                                self.apply_settings(settings);
                            }
                            self.schedule_section(ui);
                            self.startup_section(ui);
                            self.update_section(ui);
                            self.settings_file_section(ui);
                        }
                    }
                });
        });
    }

    /// Period and agent for the cost pages, refresh and collection state.
    fn toolbar(&mut self, ui: &mut egui::Ui, view: &View) {
        ui.horizontal_wrapped(|ui| {
            if matches!(self.page, Page::Overview | Page::Usage) {
                self.period_picker(ui, view);
                self.agent_picker(ui, view);
            }
            let refresh_key = ui.input(|input| input.key_pressed(egui::Key::F5));
            let shift = ui.input(|input| input.modifiers.shift);
            let clicked = ui
                .button("Refresh")
                .on_hover_text("F5. With Shift: read every live meter again, bypassing the readings shared with Claude Code.")
                .clicked();
            if clicked || refresh_key {
                self.store.refresh();
                if let Some(quotas) = &self.quotas {
                    quotas.collect_now(shift);
                }
            }
            if view.collecting {
                ui.spinner();
                ui.label(egui::RichText::new("Collecting…").color(muted()));
            }
            if let Some(snapshot) = &view.snapshot {
                let now = SystemTime::now();
                if snapshot.saved {
                    ui.label(
                        egui::RichText::new(format!(
                            "Showing the report saved {} ({})",
                            clock::local_date_time(snapshot.collected_at),
                            format::ago(snapshot.collected_at, now)
                        ))
                        .color(theme::caution()),
                    );
                } else if !view.collecting && snapshot.summary_failure.is_none() {
                    ui.label(
                        egui::RichText::new(format!(
                            "Updated {} ({})",
                            clock::local_time(snapshot.collected_at),
                            format::ago(snapshot.collected_at, now)
                        ))
                        .color(muted()),
                    );
                }
            }
        });
        if let Some(snapshot) = &view.snapshot {
            if let Some((error, saved_at)) = &snapshot.summary_failure {
                ui.colored_label(
                    theme::caution(),
                    format!(
                        "The report could not be updated ({error}). Showing the one saved {} ({}).",
                        clock::local_date_time(*saved_at),
                        format::ago(*saved_at, SystemTime::now())
                    ),
                );
            }
            for notice in &snapshot.notices {
                ui.colored_label(theme::caution(), notice);
            }
            if snapshot.kept_days > 0 && matches!(self.page, Page::Overview | Page::Usage) {
                ui.label(
                    egui::RichText::new(format!(
                        "Includes {} agent-day{} saved from earlier reports that the logs no longer hold; model details cover the logged days only.",
                        snapshot.kept_days,
                        if snapshot.kept_days == 1 { "" } else { "s" },
                    ))
                    .color(muted())
                    .size(12.0),
                );
            }
        }
        if self.page == Page::Quotas {
            ui.label(
                egui::RichText::new("Quota windows run on their own schedule; the usage period does not apply here.")
                    .color(muted())
                    .size(12.0),
            );
            if let Some(quotas) = &self.quotas {
                self.live_meters(ui, &quotas.view());
            }
        }
        if self.settings.offline {
            ui.label(
                egui::RichText::new(
                    "Offline: no live quotas or pricing downloads, and Cursor provides no new report.",
                )
                .color(theme::caution()),
            );
        }
        if let Some(notice) = &self.settings_notice {
            ui.colored_label(theme::caution(), notice);
        }
        if let Some(error) = &self.save_error {
            ui.colored_label(danger(), format!("Last change not saved: {error}"));
        }
        if self.page == Page::Settings {
            for problem in &self.problems {
                ui.colored_label(danger(), &problem.message);
            }
        } else if !self.problems.is_empty() {
            ui.colored_label(
                theme::caution(),
                format!(
                    "{} setting{} cannot be used and {} ignored; see Settings.",
                    self.problems.len(),
                    if self.problems.len() == 1 { "" } else { "s" },
                    if self.problems.len() == 1 {
                        "is"
                    } else {
                        "are"
                    },
                ),
            );
        }
        if let Some(error) = &self.custom_error {
            ui.colored_label(danger(), error);
        }
        if matches!(self.page, Page::Overview | Page::Usage)
            && (matches!(view.selection.period, Period::Custom { .. }) || self.custom_editing())
        {
            self.custom_range(ui, view);
        }
    }

    /// When the live meters were last read, and what failed.
    fn live_meters(&self, ui: &mut egui::Ui, quotas: &collector::QuotaView) {
        let history = &quotas.history;
        let latest = [quota::CODEX, quota::CLAUDE, quota::CURSOR]
            .into_iter()
            .filter_map(|series| history.latest(series))
            .map(|reading| reading.observed_at())
            .max();
        let now = SystemTime::now();
        let text = match (quotas.collecting, latest, self.settings.offline) {
            _ if !quotas.collects => {
                "Saved readings, shown without collecting new ones (fixture view).".to_string()
            }
            (_, _, true) => "Offline: the live meters are not read.".to_string(),
            (true, _, _) => "Reading the live meters…".to_string(),
            (false, Some(at), _) => format!(
                "Live meters read {} ({}); read again every minute while Agent Burn is open.",
                clock::local_time(at),
                format::ago(at, now)
            ),
            (false, None, _) => "No live meter read yet.".to_string(),
        };
        ui.label(egui::RichText::new(text).color(muted()).size(12.0));
        for (series, failure) in &history.failures {
            ui.colored_label(
                theme::caution(),
                format!("{}: {}", quota::series_name(series), failure.message),
            );
        }
        if let Some(error) = &quotas.error {
            ui.colored_label(danger(), format!("Saved quota readings: {error}"));
        }
    }

    fn custom_editing(&self) -> bool {
        !self.custom_since.is_empty() || !self.custom_until.is_empty()
    }

    fn period_picker(&mut self, ui: &mut egui::Ui, view: &View) {
        let current = view.selection.period;
        let mut chosen = current;
        let mut custom = false;
        egui::ComboBox::from_id_salt("period")
            .width(160.0)
            .selected_text(current.label())
            .show_ui(ui, |ui| {
                for period in Period::NAMED {
                    ui.selectable_value(&mut chosen, period, period.label());
                }
                custom = ui
                    .selectable_label(matches!(current, Period::Custom { .. }), "Custom range…")
                    .clicked();
            });
        if custom {
            let today = Date::today();
            let (since, until) = current
                .bounds(today)
                .unwrap_or((today.add_days(-29), today));
            self.custom_since = since.to_string();
            self.custom_until = until.to_string();
        }
        if chosen != current {
            self.custom_since.clear();
            self.custom_until.clear();
            self.custom_error = None;
            self.store.select(Selection {
                period: chosen,
                ..view.selection.clone()
            });
        }
    }

    fn custom_range(&mut self, ui: &mut egui::Ui, view: &View) {
        if !self.custom_editing()
            && let Period::Custom { since, until } = view.selection.period
        {
            self.custom_since = since.to_string();
            self.custom_until = until.to_string();
        }
        ui.horizontal(|ui| {
            ui.label("From");
            let since = text_field::singleline(&mut self.custom_since)
                .desired_width(110.0)
                .hint_text("YYYY-MM-DD")
                .show(ui)
                .response;
            ui.label("to");
            let until = text_field::singleline(&mut self.custom_until)
                .desired_width(110.0)
                .hint_text("YYYY-MM-DD")
                .show(ui)
                .response;
            ui.label(egui::RichText::new("both days included").color(muted()));
            let entered = (since.lost_focus() || until.lost_focus())
                && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if ui.button("Apply").clicked() || entered {
                match (
                    Date::parse(&self.custom_since),
                    Date::parse(&self.custom_until),
                ) {
                    (Some(since), Some(until)) if since <= until => {
                        self.custom_error = None;
                        self.store.select(Selection {
                            period: Period::Custom { since, until },
                            ..view.selection.clone()
                        });
                    }
                    (Some(_), Some(_)) => {
                        self.custom_error =
                            Some("The first day must not be after the last.".into());
                    }
                    _ => {
                        self.custom_error = Some("Enter both days as YYYY-MM-DD.".into());
                    }
                }
            }
        });
    }

    fn agent_picker(&mut self, ui: &mut egui::Ui, view: &View) {
        let mut agents: Vec<String> = view
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.summary.as_ref().ok())
            .map(|summary| {
                summary
                    .agents
                    .iter()
                    .map(|agent| agent.agent.clone())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(agent) = &view.selection.agent
            && !agents.contains(agent)
        {
            agents.push(agent.clone());
        }
        agents.sort_by_key(|agent| (crate::ui::series::order(agent), agent.clone()));
        let current = view.selection.agent.clone();
        let mut chosen = current.clone();
        egui::ComboBox::from_id_salt("agent")
            .width(140.0)
            .selected_text(
                current
                    .as_deref()
                    .map_or_else(|| "All agents".into(), crate::ui::series::name),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, None, "All agents");
                for agent in agents {
                    let name = crate::ui::series::name(&agent);
                    ui.selectable_value(&mut chosen, Some(agent), name);
                }
            });
        if chosen != current {
            self.store.select(Selection {
                agent: chosen,
                ..view.selection.clone()
            });
        }
    }

    fn settings_file_section(&self, ui: &mut egui::Ui) {
        crate::ui::components::fluent::section(ui, "Settings file", |ui| {
            crate::ui::components::fluent::card(ui, |ui| {
                match &self.settings_folder {
                    Some(folder) => ui.label(format!(
                        "Saved in {}",
                        folder.join("settings.json").display()
                    )),
                    None => ui.label("Not saved: no settings folder."),
                };
            });
        });
    }
}

type Outside = (Fluent, Option<(SystemTime, u64)>);

/// Reads the Windows theme and the settings file stamp every second off the
/// window's thread, and wakes the window only when one changes. Ends with
/// the window.
fn watch_outside(
    context: &egui::Context,
    mica: bool,
    settings: Option<PathBuf>,
) -> Arc<std::sync::Mutex<Outside>> {
    let read = move || {
        (
            Fluent::from_system(mica),
            settings.as_deref().and_then(settings_stamp),
        )
    };
    let outside = Arc::new(std::sync::Mutex::new(read()));
    let watched = Arc::downgrade(&outside);
    let repaint = context.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(THEME_REFRESH);
            let Some(outside) = watched.upgrade() else {
                return;
            };
            let now = read();
            let mut last = state::lock(&outside);
            if *last != now {
                *last = now;
                repaint.request_repaint();
            }
        }
    });
    outside
}

fn settings_stamp(path: &std::path::Path) -> Option<(SystemTime, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

fn initial_page() -> Page {
    match std::env::var(PAGE_ENV).as_deref() {
        Ok("quotas") => Page::Quotas,
        Ok("usage") => Page::Usage,
        Ok("settings") => Page::Settings,
        _ => Page::Overview,
    }
}

fn show_or_wait(ui: &mut egui::Ui, view: &View, show: impl FnOnce(&mut egui::Ui, &Snapshot)) {
    match &view.snapshot {
        Some(snapshot) => show(ui, snapshot),
        None if view.collecting => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading usage…");
            });
        }
        None => {
            ui.label("No report yet. Press Refresh.");
        }
    }
}

impl Surface for DashboardApp {
    fn draw(&mut self, ui: &mut egui::Ui) {
        let view = self.store.view();
        if let Some((frames, longest)) = self.frames.frame(view.collecting)
            && let Some(path) = &self.trace
        {
            trace(path, &view.snapshot, frames, longest, &self.frames.slow);
        }
        if view.collecting && self.trace.is_some() {
            // Keeps frames coming, so a blocked window would show in the trace.
            ui.ctx().request_repaint();
        }
        self.refresh_theme(ui.ctx());
        let background = ui.visuals().panel_fill;
        ui.painter().rect_filled(ui.max_rect(), 0.0, background);
        self.shell(ui, &view);
        // Keeps times such as "2 min ago" and the Live label current.
        ui.ctx().request_repaint_after(Duration::from_secs(60));
    }

    fn on_close(&mut self, _context: &egui::Context) {}
}

/// Frames slower than this are listed in the trace.
const SLOW_FRAME: Duration = Duration::from_millis(50);

/// Frame timing while a collection runs.
#[derive(Default)]
struct FrameWatch {
    last_frame: Option<Instant>,
    collecting: bool,
    started: Option<Instant>,
    frames: u32,
    longest: Duration,
    /// Slow frames: milliseconds since the collection started, duration.
    slow: Vec<(u128, u128)>,
}

impl FrameWatch {
    /// Records a frame; returns the totals when a collection just ended.
    fn frame(&mut self, collecting: bool) -> Option<(u32, Duration)> {
        let now = Instant::now();
        let gap = self.last_frame.map(|last| now - last);
        self.last_frame = Some(now);
        let ended = match (self.collecting, collecting) {
            (false, true) => {
                self.started = Some(now);
                self.frames = 0;
                self.longest = Duration::ZERO;
                self.slow.clear();
                None
            }
            (true, false) => Some((self.frames, self.longest)),
            _ => None,
        };
        if collecting && let Some(gap) = gap.filter(|_| self.collecting) {
            self.frames += 1;
            self.longest = self.longest.max(gap);
            if gap > SLOW_FRAME && self.slow.len() < 20 {
                let since = self
                    .started
                    .map_or(0, |started| (now - started).as_millis());
                self.slow.push((since, gap.as_millis()));
            }
        }
        self.collecting = collecting;
        ended
    }
}

fn trace(
    path: &PathBuf,
    snapshot: &Option<Arc<Snapshot>>,
    frames: u32,
    longest: Duration,
    slow: &[(u128, u128)],
) {
    let collected_at_ms = snapshot.as_ref().and_then(|snapshot| {
        snapshot
            .collected_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .map(|since| since.as_millis())
    });
    let line = serde_json::json!({
        "collectedAtMs": collected_at_ms,
        "framesDuringCollection": frames,
        "longestFrameMs": longest.as_secs_f64() * 1000.0,
        "slowFrames": slow
            .iter()
            .map(|(at, duration)| serde_json::json!({ "atMs": at, "durationMs": duration }))
            .collect::<Vec<_>>(),
    });
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_settings_change_wakes_the_window_and_the_watch_ends_with_it() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("settings.json");
        std::fs::write(&path, "{}").unwrap();
        let context = egui::Context::default();
        let outside = watch_outside(&context, false, Some(path.clone()));
        let before = state::lock(&outside).1;
        assert!(before.is_some());
        std::fs::write(&path, "{\"version\":1}").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while state::lock(&outside).1 == before {
            assert!(Instant::now() < deadline, "the change was not seen");
            std::thread::sleep(Duration::from_millis(50));
        }
        let watched = Arc::downgrade(&outside);
        drop(outside);
        assert!(watched.upgrade().is_none());
    }
}
