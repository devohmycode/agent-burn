//! Windows shell facts and window styles the widget needs: taskbars, the
//! notification area, monitors, and moving a window into or out of the
//! taskbar. Adapted from Taskbar Hub's `src/native_interop.rs` (Apache-2.0,
//! see NOTICE).

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, RECT},
        Graphics::Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
        },
        UI::WindowsAndMessaging::*,
    },
    core::{BOOL, PCWSTR},
};

use crate::native::wide;

const WS_POPUP_STYLE: u32 = 0x8000_0000;
const WS_CHILD_STYLE: u32 = 0x4000_0000;
const WS_CLIPSIBLINGS_STYLE: u32 = 0x0400_0000;

#[derive(Clone, Copy, Debug)]
pub struct TaskbarWindow {
    pub hwnd: HWND,
    pub rect: RECT,
}

#[derive(Clone, Copy, Debug)]
pub struct DisplayMonitor {
    pub handle: HMONITOR,
    pub rect: RECT,
    pub work: RECT,
    pub primary: bool,
}

pub fn is_horizontal(rect: RECT) -> bool {
    (rect.right - rect.left) >= (rect.bottom - rect.top)
}

/// The device name of a monitor, such as `\\.\DISPLAY2`.
pub fn monitor_device_name(handle: HMONITOR) -> Option<String> {
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(handle, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) }
        .as_bool()
        .then(|| {
            let length = info
                .szDevice
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(info.szDevice.len());
            String::from_utf16_lossy(&info.szDevice[..length])
        })
}

/// Every connected monitor, the primary one first.
pub fn find_monitors() -> Vec<DisplayMonitor> {
    unsafe extern "system" fn each(
        monitor: HMONITOR,
        _dc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let found = unsafe { &mut *(data.0 as *mut Vec<DisplayMonitor>) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            found.push(DisplayMonitor {
                handle: monitor,
                rect: info.rcMonitor,
                work: info.rcWork,
                primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        BOOL(1)
    }
    let mut found: Vec<DisplayMonitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(each),
            LPARAM(&mut found as *mut _ as isize),
        );
    }
    found.sort_by_key(|display| (!display.primary, display.rect.left, display.rect.top));
    found
}

/// The main taskbar and those of the other monitors.
pub fn find_taskbars() -> Vec<TaskbarWindow> {
    let mut taskbars = Vec::new();
    for class_name in ["Shell_TrayWnd", "Shell_SecondaryTrayWnd"] {
        let class = wide(class_name);
        let mut after: Option<HWND> = None;
        loop {
            let found = unsafe {
                FindWindowExW(
                    None,
                    after,
                    PCWSTR::from_raw(class.as_ptr()),
                    PCWSTR::null(),
                )
            };
            let hwnd = match found {
                Ok(hwnd) if !hwnd.is_invalid() => hwnd,
                _ => break,
            };
            if let Some(rect) = window_rect(hwnd) {
                taskbars.push(TaskbarWindow { hwnd, rect });
            }
            after = Some(hwnd);
        }
    }
    taskbars
}

/// The taskbar of the primary monitor.
pub fn primary_taskbar() -> Option<TaskbarWindow> {
    let class = wide("Shell_TrayWnd");
    let hwnd = unsafe { FindWindowW(PCWSTR::from_raw(class.as_ptr()), PCWSTR::null()) }.ok()?;
    Some(TaskbarWindow {
        hwnd,
        rect: window_rect(hwnd)?,
    })
}

pub fn find_child(parent: HWND, class_name: &str) -> Option<HWND> {
    let class = wide(class_name);
    match unsafe {
        FindWindowExW(
            Some(parent),
            None,
            PCWSTR::from_raw(class.as_ptr()),
            PCWSTR::null(),
        )
    } {
        Ok(hwnd) if !hwnd.is_invalid() => Some(hwnd),
        _ => None,
    }
}

/// Visible windows that programs other than Explorer and this one placed
/// directly in `taskbar`, such as other taskbar widgets.
pub fn foreign_children(taskbar: HWND) -> Vec<RECT> {
    struct Search {
        taskbar: HWND,
        explorer: u32,
        found: Vec<RECT>,
    }
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
        let search = unsafe { &mut *(data.0 as *mut Search) };
        let mut owner = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
        let direct = unsafe { GetParent(hwnd) }.ok() == Some(search.taskbar);
        if direct
            && owner != search.explorer
            && owner != std::process::id()
            && unsafe { IsWindowVisible(hwnd) }.as_bool()
            && let Some(rect) = window_rect(hwnd)
        {
            search.found.push(rect);
        }
        BOOL(1)
    }
    let mut explorer = 0u32;
    unsafe { GetWindowThreadProcessId(taskbar, Some(&mut explorer)) };
    let mut search = Search {
        taskbar,
        explorer,
        found: Vec::new(),
    };
    unsafe {
        let _ = EnumChildWindows(
            Some(taskbar),
            Some(each),
            LPARAM(&mut search as *mut Search as isize),
        );
    }
    search.found
}

/// The notification area of `taskbar`.
pub fn tray_rect(taskbar: HWND) -> Option<RECT> {
    find_child(taskbar, "TrayNotifyWnd").and_then(window_rect)
}

/// `GetWindowRect` returns at once even if Explorer hangs, unlike
/// `SHAppBarMessage`.
pub fn window_rect(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }
        .ok()
        .map(|()| rect)
}

pub fn class_name(hwnd: HWND) -> Option<String> {
    let mut name = [0u16; 128];
    let length = unsafe { GetClassNameW(hwnd, &mut name) };
    (length > 0).then(|| String::from_utf16_lossy(&name[..length as usize]))
}

/// Hosts the layered widget inside the taskbar: it then shares the
/// taskbar's visibility and z-order instead of competing with it as a
/// topmost popup.
pub fn embed_as_child(hwnd: HWND, parent: HWND) {
    unsafe {
        let current_parent = GetParent(hwnd).ok();
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE);
        let _ = SetWindowLongW(
            hwnd,
            GWL_EXSTYLE,
            (ex_style | WS_EX_TOOLWINDOW.0 as i32 | WS_EX_NOACTIVATE.0 as i32)
                & !(WS_EX_TOPMOST.0 as i32),
        );
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let new_style = (style & !WS_POPUP_STYLE) | WS_CHILD_STYLE | WS_CLIPSIBLINGS_STYLE;
        let _ = SetWindowLongW(hwnd, GWL_STYLE, new_style as i32);
        if current_parent != Some(parent) {
            let _ = SetParent(hwnd, Some(parent));
            // Windows 11 can leave a reparented layered surface beneath the
            // taskbar's composition visual: rebinding the layered style once
            // after the parent change brings it back above.
            if GetParent(hwnd).ok() == Some(parent) {
                let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE);
                if ex_style & WS_EX_LAYERED.0 as i32 != 0 {
                    let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, ex_style & !(WS_EX_LAYERED.0 as i32));
                    let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, ex_style);
                }
            }
        }
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

/// Makes the widget a top-level popup again, topmost when floating.
pub fn make_popup(hwnd: HWND, topmost: bool) {
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let detaching = style & WS_CHILD_STYLE != 0;
        let screen_rect = if detaching { window_rect(hwnd) } else { None };
        let restore_visibility = detaching && style & WS_VISIBLE.0 != 0;
        if restore_visibility {
            // Do not show the old client position while detaching.
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if detaching {
            let _ = SetParent(hwnd, None);
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let new_style = (style & !WS_CHILD_STYLE & !WS_CLIPSIBLINGS_STYLE) | WS_POPUP_STYLE;
        let _ = SetWindowLongW(hwnd, GWL_STYLE, new_style as i32);
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE);
        let ex_style = if topmost {
            ex_style | WS_EX_TOPMOST.0 as i32
        } else {
            ex_style & !(WS_EX_TOPMOST.0 as i32)
        };
        let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, ex_style);
        let mut flags = SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED;
        let (x, y) = match screen_rect {
            Some(rect) => (rect.left, rect.top),
            None => {
                flags |= SWP_NOMOVE;
                (0, 0)
            }
        };
        if restore_visibility {
            flags |= SWP_SHOWWINDOW;
        }
        let _ = SetWindowPos(
            hwnd,
            Some(if topmost {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            }),
            x,
            y,
            0,
            0,
            flags,
        );
    }
}

/// The message Explorer broadcasts when it creates the taskbar again, e.g.
/// after a restart: notification icons and docked windows must come back.
pub fn taskbar_created_message() -> u32 {
    static MESSAGE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MESSAGE.get_or_init(|| {
        let name = wide("TaskbarCreated");
        unsafe { RegisterWindowMessageW(PCWSTR::from_raw(name.as_ptr())) }
    })
}
