//! Reports, without any input, what the background process of an
//! application folder shows: its hidden window, the widget (parent, place
//! next to the notification area), the notification-area icon, and the
//! dashboard window. Optionally posts one of its messages first.
//! `cargo run --example hub_probe -- <folder> [open|quit|refresh]`

use std::path::PathBuf;

use agent_burn_windows::{
    hub::instance::{self, WM_APP_QUIT, WM_APP_REFRESH, WM_APP_TRAY},
    widget::shell,
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, RECT, WPARAM},
        System::Threading::{GR_GDIOBJECTS, GetCurrentProcess, GetGuiResources},
        UI::{
            Shell::{NIN_SELECT, NOTIFYICONIDENTIFIER, Shell_NotifyIconGetRect},
            WindowsAndMessaging::{
                FindWindowExW, GWL_EXSTYLE, GetForegroundWindow, GetParent, GetWindowLongW,
                GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_LBUTTONDOWN,
                WM_LBUTTONUP, WS_EX_NOACTIVATE,
            },
        },
    },
    core::PCWSTR,
};

fn rect(rect: RECT) -> String {
    format!(
        "[{}, {}, {}, {}]",
        rect.left, rect.top, rect.right, rect.bottom
    )
}

fn main() {
    let mut args = std::env::args().skip(1);
    let folder = PathBuf::from(args.next().expect("the application folder"));
    let action = args.next();
    let hub = instance::hub_window(&folder);
    println!("background window: {}", hub.is_some());
    if let (Some(hub), Some("click")) = (hub, action.as_deref()) {
        // The left button pressed and released on the widget, as window
        // messages to it: no system input.
        let mut process = 0u32;
        unsafe { GetWindowThreadProcessId(hub, Some(&mut process)) };
        let class = agent_burn_windows::native::wide("AgentBurnWidget");
        for parent in [shell::primary_taskbar().map(|taskbar| taskbar.hwnd), None] {
            let mut after: Option<HWND> = None;
            while let Ok(hwnd) = unsafe {
                FindWindowExW(
                    parent,
                    after,
                    PCWSTR::from_raw(class.as_ptr()),
                    PCWSTR::null(),
                )
            } {
                if hwnd.is_invalid() {
                    break;
                }
                let mut owner = 0u32;
                unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
                if owner == process {
                    unsafe {
                        let _ = PostMessageW(Some(hwnd), WM_LBUTTONDOWN, WPARAM(1), LPARAM(0));
                        let _ = PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), LPARAM(0));
                    }
                    println!("posted click");
                    return;
                }
                after = Some(hwnd);
            }
        }
        println!("no widget to click");
        return;
    }
    if let (Some(hub), Some(action)) = (hub, action.as_deref()) {
        let (message, lparam) = match action {
            "open" => (WM_APP_TRAY, NIN_SELECT as isize),
            "quit" => (WM_APP_QUIT, 0),
            "refresh" => (WM_APP_REFRESH, 0),
            other => panic!("unknown action {other}"),
        };
        unsafe {
            let _ = PostMessageW(Some(hub), message, WPARAM(0), LPARAM(lparam));
        }
        println!("posted {action}");
        return;
    }
    let Some(hub) = hub else {
        return;
    };
    let mut process = 0u32;
    unsafe { GetWindowThreadProcessId(hub, Some(&mut process)) };
    println!("background process: {process}");
    // The widget: a window of class AgentBurnWidget of that process, either
    // a child of the taskbar or a top-level popup.
    let class = agent_burn_windows::native::wide("AgentBurnWidget");
    let mut widget: Option<HWND> = None;
    let mut parents: Vec<Option<HWND>> = vec![None];
    if let Some(taskbar) = shell::primary_taskbar() {
        parents.insert(0, Some(taskbar.hwnd));
    }
    'search: for parent in parents {
        let mut after: Option<HWND> = None;
        loop {
            let found = unsafe {
                FindWindowExW(
                    parent,
                    after,
                    PCWSTR::from_raw(class.as_ptr()),
                    PCWSTR::null(),
                )
            };
            let Ok(hwnd) = found else { break };
            if hwnd.is_invalid() {
                break;
            }
            let mut owner = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
            if owner == process {
                widget = Some(hwnd);
                break 'search;
            }
            after = Some(hwnd);
        }
    }
    match widget {
        None => println!("widget: none"),
        Some(hwnd) => {
            let parent = unsafe { GetParent(hwnd) }.ok();
            let parent_class = parent
                .and_then(shell::class_name)
                .unwrap_or_else(|| "none".into());
            let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
            let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
            println!(
                "widget: visible {visible}, parent {parent_class}, rect {}, no activation {}",
                shell::window_rect(hwnd).map(rect).unwrap_or_default(),
                ex_style & WS_EX_NOACTIVATE.0 != 0
            );
            if let Some(taskbar) = shell::primary_taskbar() {
                println!(
                    "taskbar {}, notification area {}",
                    rect(taskbar.rect),
                    shell::tray_rect(taskbar.hwnd).map(rect).unwrap_or_default()
                );
            }
        }
    }
    let identifier = NOTIFYICONIDENTIFIER {
        cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: hub,
        uID: 1,
        ..Default::default()
    };
    match unsafe { Shell_NotifyIconGetRect(&identifier) } {
        Ok(icon) => println!("notification icon: shown at {}", rect(icon)),
        Err(error) => println!("notification icon: not found ({error})"),
    }
    println!(
        "dashboard window: {}",
        instance::dashboard_window(&folder).is_some()
    );
    let foreground = unsafe { GetForegroundWindow() };
    let mut owner = 0u32;
    unsafe { GetWindowThreadProcessId(foreground, Some(&mut owner)) };
    println!(
        "foreground belongs to the background process: {}",
        owner == process
    );
    let _ = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
}
