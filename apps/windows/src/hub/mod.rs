//! The background process (`--widget`): the taskbar widget, the
//! notification-area icon and the quota collection while Agent Burn runs.
//! There is one per application folder; the dashboard is a separate process
//! it opens, so closing the dashboard window leaves the widget and the icon,
//! and only "Quit Agent Burn" ends both.
//!
//! The widget shows the saved quota readings and makes no request of
//! its own: the collection that feeds it is the dashboard's, this process's
//! and the scheduled task's, coordinated by the collector's lock.

pub mod instance;
pub mod menu;
pub mod tray;

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::NMHDR,
            Input::KeyboardAndMouse::{ReleaseCapture, SetCapture},
            Shell::NIN_SELECT,
            WindowsAndMessaging::*,
        },
    },
    core::PCWSTR,
};

use self::{
    instance::{WM_APP_QUIT, WM_APP_QUOTAS, WM_APP_REFRESH, WM_APP_SETTINGS, WM_APP_TRAY},
    menu::Command,
    tray::Tray,
};
use crate::{
    cli_client,
    collector::Watcher,
    dashboard::{clock, format},
    files,
    native::wide,
    quota,
    settings::{self, Settings},
    state::EngineOptions,
    widget::{
        draw,
        layout::{self, Placement, Spot},
        occupancy, shell,
        view::{self, RowState, WidgetView},
        window::{self, NMTTDISPINFOW, TTN_GETDISPINFOW, WidgetWindow},
    },
    windows_theme,
};

/// `NIN_SELECT | NINF_KEY`: the icon chosen with the keyboard.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const TICK: usize = 1;
/// How often the settings, the taskbar and the clock are looked at.
const TICK_MS: u32 = 2000;
/// Where the widget is, owned by this process.
const WIDGET_FILE: &str = "widget.json";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct WidgetFile {
    version: u32,
    placement: Placement,
}

fn load_placement(folder: &Path) -> Placement {
    std::fs::read(folder.join(WIDGET_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<WidgetFile>(&bytes).ok())
        .map(|file| file.placement)
        .unwrap_or_default()
}

fn save_placement(folder: &Path, placement: &Placement) {
    let file = WidgetFile {
        version: 1,
        placement: placement.clone(),
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&file) {
        let _ = files::write_atomically(&folder.join(WIDGET_FILE), &bytes);
    }
}

fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    start_cursor: POINT,
    start_origin: POINT,
    moving: bool,
}

/// What was last drawn, to draw again only when it changes.
type Drawn = (WidgetView, bool, u64, i64, bool);

struct Hub {
    folder: PathBuf,
    hwnd: HWND,
    engine: Option<PathBuf>,
    settings: Settings,
    settings_stamp: Option<(SystemTime, u64)>,
    placement: Placement,
    watcher: Watcher,
    tray: Tray,
    widget: Option<WidgetWindow>,
    drawn: Option<Drawn>,
    placed: Option<(Spot, isize)>,
    drag: Option<Drag>,
}

thread_local! {
    static HUB: RefCell<Option<Hub>> = const { RefCell::new(None) };
}

/// Runs `action` on the hub, unless it is in use further up the stack
/// (a message sent while it works): then the message is left alone.
fn with_hub<R>(action: impl FnOnce(&mut Hub) -> R) -> Option<R> {
    HUB.with(|hub| {
        let mut hub = hub.try_borrow_mut().ok()?;
        hub.as_mut().map(action)
    })
}

fn engine_options(settings: &Settings, engine: &Option<PathBuf>) -> EngineOptions {
    let bundled = std::env::current_exe()
        .map(|app| cli_client::bundled_engine_next_to(&app))
        .unwrap_or_else(|_| PathBuf::from(cli_client::ENGINE_FILE_NAME));
    let (mut options, _problems) = settings.engine_options(bundled);
    if let Some(engine) = engine {
        options.engine = engine.clone();
    }
    options
}

/// The background process's entry point.
pub fn run(engine: Option<PathBuf>) -> ExitCode {
    let Some(folder) = settings::directory() else {
        return ExitCode::FAILURE;
    };
    // Another one already runs for this folder.
    let Some(_instance) = instance::claim("Hub", &folder) else {
        return ExitCode::SUCCESS;
    };
    let Some(hwnd) = create_hub_window(&folder) else {
        return ExitCode::FAILURE;
    };
    let loaded = settings::load(&folder);
    let target = hwnd.0 as isize;
    let watcher = Watcher::start(folder.clone(), move || unsafe {
        let _ = PostMessageW(
            Some(HWND(target as *mut _)),
            WM_APP_QUOTAS,
            WPARAM(0),
            LPARAM(0),
        );
    });
    watcher.set_options(engine_options(&loaded.settings, &engine));
    occupancy::spawn_reader();
    let mut tray = Tray::new(hwnd);
    tray.add();
    HUB.with(|hub| {
        *hub.borrow_mut() = Some(Hub {
            settings_stamp: file_stamp(&folder.join("settings.json")),
            placement: load_placement(&folder),
            folder,
            hwnd,
            engine,
            settings: loaded.settings,
            watcher,
            tray,
            widget: None,
            drawn: None,
            placed: None,
            drag: None,
        })
    });
    with_hub(refresh);
    unsafe {
        SetTimer(Some(hwnd), TICK, TICK_MS, None);
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    HUB.with(|hub| {
        if let Some(mut hub) = hub.borrow_mut().take() {
            hub.tray.remove();
            if let Some(widget) = hub.widget.take() {
                widget.destroy();
            }
        }
    });
    ExitCode::SUCCESS
}

fn create_hub_window(folder: &Path) -> Option<HWND> {
    let class_name = wide(instance::HUB_CLASS);
    let title = wide(instance::hub_title(folder));
    unsafe {
        let module = HINSTANCE(GetModuleHandleW(None).ok()?.0);
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(hub_procedure),
            hInstance: module,
            lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassExW(&class);
        // A hidden top-level window: message-only windows do not receive the
        // TaskbarCreated broadcast after an Explorer restart.
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR::from_raw(class_name.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(module),
            None,
        )
        .ok()
    }
}

impl Hub {
    fn reload_settings(&mut self, force: bool) {
        let stamp = file_stamp(&self.folder.join("settings.json"));
        if !force && stamp == self.settings_stamp {
            return;
        }
        self.settings_stamp = stamp;
        let loaded = settings::load(&self.folder);
        if loaded.settings != self.settings {
            self.settings = loaded.settings;
            self.watcher
                .set_options(engine_options(&self.settings, &self.engine));
            self.drawn = None;
        }
    }

    /// Changes the settings file, as the dashboard would.
    fn change_settings(&mut self, change: impl FnOnce(&mut Settings)) {
        let mut loaded = settings::load(&self.folder);
        change(&mut loaded.settings);
        if loaded.writable {
            let _ = settings::save(&self.folder, &loaded.settings);
        }
        self.settings = loaded.settings;
        self.settings_stamp = file_stamp(&self.folder.join("settings.json"));
        self.drawn = None;
    }

    fn view(&self) -> WidgetView {
        view::view(
            &self.watcher.view().history,
            self.settings.widget_provider,
            quota::now_ms(),
        )
    }

    fn open_dashboard(&self) {
        match instance::dashboard_window(&self.folder) {
            Some(hwnd) => instance::activate(hwnd),
            None => {
                let mut arguments = Vec::new();
                if let Some(engine) = &self.engine {
                    arguments.extend(["--engine".to_string(), engine.display().to_string()]);
                }
                let _ = instance::spawn(&arguments);
            }
        }
    }

    fn quit(&mut self) {
        if let Some(dashboard) = instance::dashboard_window(&self.folder) {
            unsafe {
                let _ = PostMessageW(Some(dashboard), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        self.tray.remove();
        if let Some(widget) = self.widget.take() {
            widget.destroy();
        }
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    fn menu_state(&self) -> menu::State {
        menu::State {
            provider: self.settings.widget_provider,
            widget_shown: self.settings.widget_enabled,
            floating: matches!(self.placement, Placement::Floating { .. }),
        }
    }

    fn run_command(&mut self, command: Command) {
        match command {
            Command::OpenDashboard => self.open_dashboard(),
            Command::Refresh => self.watcher.collect_now(true),
            Command::Show(provider) => {
                self.change_settings(|settings| settings.widget_provider = provider)
            }
            Command::ToggleWidget => {
                self.change_settings(|settings| settings.widget_enabled = !settings.widget_enabled)
            }
            Command::DockNearTray => {
                self.placement = Placement::NearTray;
                save_placement(&self.folder, &self.placement);
                self.placed = None;
            }
            Command::Quit => {
                self.quit();
                return;
            }
        }
        refresh(self);
    }
}

/// Where the primary taskbar is and what its buttons take.
fn taskbar_geometry() -> Option<(shell::TaskbarWindow, layout::Taskbar, RECT)> {
    let taskbar = shell::primary_taskbar()?;
    let monitor = unsafe { MonitorFromWindow(taskbar.hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = unsafe { GetMonitorInfoW(monitor, &mut info) };
    let occupancy = occupancy::cached(taskbar);
    Some((
        taskbar,
        layout::Taskbar {
            rect: taskbar.rect,
            tray: shell::tray_rect(taskbar.hwnd),
            reserved: occupancy
                .as_ref()
                .map(|sample| sample.reserved.clone())
                .unwrap_or_default(),
            occupied: occupancy.map(|sample| sample.occupied),
            obstacles: shell::foreign_children(taskbar.hwnd),
            scale: window::monitor_scale(monitor),
        },
        info.rcMonitor,
    ))
}

fn monitors() -> Vec<layout::Monitor> {
    shell::find_monitors()
        .into_iter()
        .map(|display| layout::Monitor {
            name: shell::monitor_device_name(display.handle).unwrap_or_default(),
            rect: display.rect,
            work: display.work,
            primary: display.primary,
            scale: window::monitor_scale(display.handle),
        })
        .collect()
}

fn device(logical: f64, scale: f64) -> i32 {
    (logical * scale).round() as i32
}

/// Shows, moves and draws the widget as the settings, the placement, the
/// taskbar and the readings say; does nothing when nothing changed.
fn refresh(hub: &mut Hub) {
    let alive = hub
        .widget
        .as_ref()
        .is_some_and(|widget| unsafe { IsWindow(Some(widget.hwnd)) }.as_bool());
    if !alive {
        // Explorer took the docked window with it when it restarted.
        hub.widget = None;
        hub.drawn = None;
        hub.placed = None;
    }
    if !hub.settings.widget_enabled {
        if let Some(widget) = hub.widget.take() {
            widget.destroy();
        }
        hub.drawn = None;
        hub.placed = None;
        update_tip(hub);
        return;
    }
    if hub.widget.is_none() {
        hub.widget = WidgetWindow::create(Some(widget_procedure));
    }
    if hub.drag.is_some_and(|drag| drag.moving) {
        return;
    }
    let target = match &hub.placement {
        Placement::NearTray => taskbar_geometry().map(|(taskbar, geometry, monitor)| {
            let scale = geometry.scale;
            let spot = layout::near_tray(
                &geometry,
                monitor,
                device(draw::WIDTH, scale),
                device(draw::HEIGHT, scale),
            );
            (scale, spot, taskbar.hwnd.0 as isize)
        }),
        Placement::Floating { monitor, x, y } => {
            let monitors = monitors();
            layout::resolve(&monitors, monitor).and_then(|display| {
                let handle = shell::find_monitors()
                    .into_iter()
                    .find(|found| found.rect == display.rect)?;
                if window::fullscreen_on(handle.handle, display.rect) {
                    return None;
                }
                let scale = display.scale;
                let rect = layout::floating(
                    display,
                    *x,
                    *y,
                    device(draw::WIDTH, scale),
                    device(draw::HEIGHT, scale),
                );
                Some((scale, Spot::Floating(rect), 0))
            })
        }
    };
    let Some(widget) = &hub.widget else {
        return;
    };
    let Some((scale, spot, parent)) = target else {
        widget.hide();
        hub.placed = None;
        return;
    };
    let now = quota::now_ms();
    let view = hub.view();
    let dark = !windows_theme::system_uses_light_theme();
    let drawn: Drawn = (
        view.clone(),
        dark,
        scale.to_bits(),
        now / 60_000,
        matches!(spot, Spot::Floating(_)),
    );
    let moved = hub.placed != Some((spot, parent));
    if moved {
        match spot {
            Spot::Docked(rect) => widget.dock_at(HWND(parent as *mut _), rect),
            Spot::Floating(rect) => widget.float_at(rect),
        }
        hub.placed = Some((spot, parent));
    }
    // A moved or reparented layered window shows fresh pixels.
    if moved || hub.drawn.as_ref() != Some(&drawn) {
        let floating = matches!(spot, Spot::Floating(_));
        widget.present(&draw::draw(&view, dark, scale, now, floating));
        hub.drawn = Some(drawn);
    }
    update_tip(hub);
}

fn update_tip(hub: &mut Hub) {
    let view = hub.view();
    hub.tray.set_tip(&tray_tip(&view));
}

/// The notification-area tooltip: one line.
pub fn tray_tip(view: &WidgetView) -> String {
    let rows: Vec<String> = view
        .rows
        .iter()
        .map(|row| match row.remaining() {
            Some(left) => format!("{} {left:.0}% left", row.label),
            None => format!("{} {}", row.label, row.text(0)),
        })
        .collect();
    format!("Agent Burn · {} {}", view.provider.label(), rows.join(", "))
}

/// The widget's tooltip: each window, its reset and the reading time.
pub fn widget_tip(view: &WidgetView, now_ms: i64) -> String {
    let now = clock::from_millis(now_ms).unwrap_or_else(SystemTime::now);
    let mut lines = vec![format!("{} quota", view.provider.label())];
    let mut read: Option<i64> = None;
    for row in &view.rows {
        let line = match &row.state {
            RowState::Read {
                remaining,
                resets_at_ms,
                observed_at_ms,
                stale,
            } => {
                read = Some(read.map_or(*observed_at_ms, |known| known.max(*observed_at_ms)));
                let reset = clock::from_millis(*resets_at_ms)
                    .map(|at| {
                        format!(
                            "resets in {} ({})",
                            format::duration(Duration::from_millis(
                                (*resets_at_ms - now_ms).max(0) as u64
                            )),
                            clock::local_date_time(at)
                        )
                    })
                    .unwrap_or_default();
                let stale = if *stale { " — not current" } else { "" };
                format!("{}: {remaining:.0}% left, {reset}{stale}", row.name)
            }
            RowState::Reset { .. } => format!("{}: reset since the last reading", row.name),
            RowState::Unknown => format!("{}: no reading yet", row.name),
        };
        lines.push(line);
        if let Some(failure) = &row.failure {
            lines.push(format!("  {failure}"));
        }
    }
    if let Some(at) = read.and_then(clock::from_millis) {
        lines.push(format!(
            "Read {} ({})",
            clock::local_time(at),
            format::ago(at, now)
        ));
    }
    lines.push("Click to open Agent Burn · right-click for options · drag to move".into());
    lines.join("\n")
}

fn cursor() -> POINT {
    let mut point = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut point);
    }
    point
}

/// Ends a drag: the widget docks again on the primary taskbar, or floats
/// where it was dropped.
fn drop_widget(hub: &mut Hub) {
    let Some(widget) = &hub.widget else {
        return;
    };
    let Some(rect) = shell::window_rect(widget.hwnd) else {
        return;
    };
    let geometry = taskbar_geometry().map(|(_, geometry, _)| geometry);
    hub.placement = layout::dropped(
        cursor(),
        POINT {
            x: rect.left,
            y: rect.top,
        },
        geometry.as_ref(),
        &monitors(),
    );
    save_placement(&hub.folder, &hub.placement);
    hub.placed = None;
}

unsafe extern "system" fn widget_procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN => {
            let origin = shell::window_rect(hwnd).map(|rect| POINT {
                x: rect.left,
                y: rect.top,
            });
            with_hub(|hub| {
                if let Some(origin) = origin {
                    hub.drag = Some(Drag {
                        start_cursor: cursor(),
                        start_origin: origin,
                        moving: false,
                    });
                }
            });
            unsafe { SetCapture(hwnd) };
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let point = cursor();
            let step = with_hub(|hub| {
                let drag = hub.drag.as_mut()?;
                let (dx, dy) = (point.x - drag.start_cursor.x, point.y - drag.start_cursor.y);
                let (cx, cy) =
                    unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
                let started = !drag.moving && (dx.abs() > cx || dy.abs() > cy);
                drag.moving |= started;
                drag.moving
                    .then_some((started, drag.start_origin.x + dx, drag.start_origin.y + dy))
            })
            .flatten();
            if let Some((started, x, y)) = step {
                unsafe {
                    if started {
                        // Out of the taskbar, to travel over the desktop.
                        shell::make_popup(hwnd, true);
                    }
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        x,
                        y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            unsafe {
                let _ = ReleaseCapture();
            }
            with_hub(|hub| match hub.drag.take() {
                Some(drag) if drag.moving => {
                    drop_widget(hub);
                    refresh(hub);
                }
                _ => hub.open_dashboard(),
            });
            return LRESULT(0);
        }
        WM_CAPTURECHANGED => {
            with_hub(|hub| {
                if hub.drag.take().is_some_and(|drag| drag.moving) {
                    hub.placed = None;
                    refresh(hub);
                }
            });
        }
        WM_RBUTTONUP => {
            show_menu();
            return LRESULT(0);
        }
        WM_NOTIFY => {
            let header = lparam.0 as *const NMHDR;
            if !header.is_null() && unsafe { (*header).code } == TTN_GETDISPINFOW {
                let text =
                    with_hub(|hub| widget_tip(&hub.view(), quota::now_ms())).unwrap_or_default();
                unsafe { window::fill_tooltip(lparam.0 as *mut NMTTDISPINFOW, &text) };
                return LRESULT(0);
            }
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// The menu, shown without holding the hub: its modal loop dispatches the
/// hub's timer meanwhile.
fn show_menu() {
    let Some((owner, state)) = with_hub(|hub| (hub.hwnd, hub.menu_state())) else {
        return;
    };
    if let Some(command) = menu::show(owner, state) {
        with_hub(|hub| hub.run_command(command));
    }
}

unsafe extern "system" fn hub_procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == shell::taskbar_created_message() && message != 0 {
        with_hub(|hub| {
            hub.tray.add();
            if let Some(widget) = hub.widget.take() {
                widget.destroy();
            }
            hub.drawn = None;
            hub.placed = None;
            refresh(hub);
        });
        return LRESULT(0);
    }
    match message {
        WM_TIMER if wparam.0 == TICK => {
            with_hub(|hub| {
                hub.reload_settings(false);
                refresh(hub);
            });
            return LRESULT(0);
        }
        WM_APP_QUOTAS => {
            with_hub(refresh);
            return LRESULT(0);
        }
        WM_APP_SETTINGS => {
            with_hub(|hub| {
                hub.reload_settings(true);
                refresh(hub);
            });
            return LRESULT(0);
        }
        WM_APP_REFRESH => {
            with_hub(|hub| hub.watcher.collect_now(true));
            return LRESULT(0);
        }
        WM_APP_QUIT => {
            with_hub(Hub::quit);
            return LRESULT(0);
        }
        WM_APP_TRAY => {
            match (lparam.0 & 0xFFFF) as u32 {
                NIN_SELECT | NIN_KEYSELECT => {
                    with_hub(|hub| hub.open_dashboard());
                }
                WM_CONTEXTMENU => show_menu(),
                _ => {}
            }
            return LRESULT(0);
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE | WM_DPICHANGED => {
            with_hub(|hub| {
                hub.drawn = None;
                hub.placed = None;
                refresh(hub);
            });
        }
        WM_ENDSESSION if wparam.0 != 0 => {
            with_hub(Hub::quit);
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::draw::tests::{NOW, claude};

    #[test]
    fn the_tooltips_say_what_the_widget_draws() {
        let view = claude(40.0, 25.0);

        assert_eq!(
            tray_tip(&view),
            "Agent Burn · Claude 5h 60% left, 7d 75% left"
        );
        let tip = widget_tip(&view, NOW);
        assert!(
            tip.starts_with("Claude quota\n5-hour session: 60% left, resets in "),
            "{tip}"
        );
        assert!(tip.contains("Weekly limit: 75% left"), "{tip}");
        assert!(tip.contains("\nRead "), "{tip}");
    }

    #[test]
    fn a_missing_or_damaged_placement_docks_near_the_tray() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(load_placement(folder.path()), Placement::NearTray);
        let floating = Placement::Floating {
            monitor: layout::PRIMARY.into(),
            x: 40,
            y: 80,
        };
        save_placement(folder.path(), &floating);
        assert_eq!(load_placement(folder.path()), floating);
        std::fs::write(folder.path().join(WIDGET_FILE), b"{oops").unwrap();
        assert_eq!(load_placement(folder.path()), Placement::NearTray);
    }
}
