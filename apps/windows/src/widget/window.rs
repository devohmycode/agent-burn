//! The widget's layered window: created without activation, presented with
//! `UpdateLayeredWindow`, docked as a child of the taskbar or floating as a
//! topmost popup. Adapted from Taskbar Hub's `src/hub/presenter.rs` and
//! `src/hub/host.rs` (Apache-2.0, see NOTICE).
//!
//! Every GDI object a presentation creates is deleted before it returns, so
//! repeated redraws and windows opened and closed leave no handle behind.

use std::cell::RefCell;

use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
        Graphics::{
            Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
            Gdi::*,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{
                ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, TOOLTIPS_CLASSW,
                TTDT_AUTOPOP, TTF_IDISHWND, TTF_SUBCLASS, TTM_ADDTOOLW, TTM_SETDELAYTIME,
                TTM_SETMAXTIPWIDTH, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
            },
            HiDpi::{GetDpiForMonitor, GetDpiForSystem, GetDpiForWindow, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, PWSTR},
};

use super::{canvas::Canvas, shell};
use crate::native::wide;

pub const CLASS_NAME: &str = "AgentBurnWidget";
/// `LPSTR_TEXTCALLBACKW`: the tooltip asks for its text each time it shows.
const TEXT_CALLBACK: PWSTR = PWSTR(usize::MAX as *mut u16);
/// Logical maximum width of a tooltip line before it wraps.
const TOOLTIP_WIDTH: f64 = 360.0;
/// How long a tooltip stays up, in milliseconds.
const TOOLTIP_DURATION: isize = 30_000;

thread_local! {
    /// Text handed to the tooltip; it must outlive the notification.
    static TOOLTIP_TEXT: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
}

/// The widget window and its tooltip.
#[derive(Debug)]
pub struct WidgetWindow {
    pub hwnd: HWND,
}

impl WidgetWindow {
    /// Creates the hidden widget window; `procedure` receives its messages.
    pub fn create(procedure: WNDPROC) -> Option<Self> {
        let class_name = wide(CLASS_NAME);
        unsafe {
            let instance = HINSTANCE(GetModuleHandleW(None).ok()?.0);
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_DBLCLKS,
                lpfnWndProc: procedure,
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
                ..Default::default()
            };
            // A second registration fails harmlessly.
            RegisterClassExW(&class);
            let title = wide("Agent Burn widget");
            // Created topmost: Windows may ignore a later request to join
            // the topmost band; the style is inert while docked.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                PCWSTR::from_raw(class_name.as_ptr()),
                PCWSTR::from_raw(title.as_ptr()),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance),
                None,
            )
            .ok()?;
            create_tooltip(hwnd);
            Some(Self { hwnd })
        }
    }

    /// Shows `canvas` in the window, keeping its position.
    pub fn present(&self, canvas: &Canvas) {
        present(self.hwnd, canvas.width(), canvas.height(), canvas.pixels());
    }

    /// A topmost popup at `rect` (screen device pixels).
    pub fn float_at(&self, rect: RECT) {
        unsafe {
            let topmost = GetWindowLongW(self.hwnd, GWL_EXSTYLE) & WS_EX_TOPMOST.0 as i32 != 0;
            if GetParent(self.hwnd).is_ok() || !topmost {
                shell::make_popup(self.hwnd, true);
            }
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    /// A child of `taskbar` at `rect` (screen device pixels).
    pub fn dock_at(&self, taskbar: HWND, rect: RECT) {
        unsafe {
            shell::embed_as_child(self.hwnd, taskbar);
            let mut origin = [POINT {
                x: rect.left,
                y: rect.top,
            }];
            MapWindowPoints(None, Some(taskbar), &mut origin);
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOP),
                origin[0].x,
                origin[0].y,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn docked(&self) -> bool {
        unsafe { GetParent(self.hwnd) }.is_ok()
    }

    /// Device pixels per logical pixel where the window is.
    pub fn scale(&self) -> f64 {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 { 1.0 } else { f64::from(dpi) / 96.0 }
    }

    pub fn destroy(self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Answers the tooltip's request for its text with `text`.
///
/// # Safety
/// `info` must be the `NMTTDISPINFOW` of a `TTN_GETDISPINFOW` notification.
pub unsafe fn fill_tooltip(info: *mut NMTTDISPINFOW, text: &str) {
    TOOLTIP_TEXT.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        *buffer = text.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe { (*info).lpszText = PWSTR(buffer.as_mut_ptr()) };
    });
}

pub use windows::Win32::UI::Controls::{NMTTDISPINFOW, TTN_GETDISPINFOW};

unsafe fn create_tooltip(owner: HWND) {
    let controls = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_WIN95_CLASSES,
    };
    unsafe {
        let _ = InitCommonControlsEx(&controls);
        let Ok(tooltip) = CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            PCWSTR::null(),
            WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            Some(owner),
            None,
            None,
            None,
        ) else {
            return;
        };
        let tool = TTTOOLINFOW {
            cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_IDISHWND | TTF_SUBCLASS,
            hwnd: owner,
            uId: owner.0 as usize,
            lpszText: TEXT_CALLBACK,
            ..Default::default()
        };
        SendMessageW(
            tooltip,
            TTM_ADDTOOLW,
            None,
            Some(LPARAM(&tool as *const _ as isize)),
        );
        let dpi = GetDpiForWindow(owner).max(96);
        let width = (TOOLTIP_WIDTH * f64::from(dpi) / 96.0).round() as isize;
        SendMessageW(tooltip, TTM_SETMAXTIPWIDTH, None, Some(LPARAM(width)));
        SendMessageW(
            tooltip,
            TTM_SETDELAYTIME,
            Some(WPARAM(TTDT_AUTOPOP as usize)),
            Some(LPARAM(TOOLTIP_DURATION)),
        );
    }
}

/// Presents premultiplied `pixels` in the layered window `hwnd`.
fn present(hwnd: HWND, width: u32, height: u32, pixels: &[u32]) {
    let (width, height) = (width as i32, height as i32);
    unsafe {
        let screen_dc = GetDC(None);
        let memory_dc = CreateCompatibleDC(Some(screen_dc));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(Some(memory_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .unwrap_or_default();
        if bitmap.is_invalid() || bits.is_null() {
            let _ = DeleteDC(memory_dc);
            ReleaseDC(None, screen_dc);
            return;
        }
        let old = SelectObject(memory_dc, bitmap.into());
        let window_pixels = std::slice::from_raw_parts_mut(bits as *mut u32, pixels.len());
        for (target, source) in window_pixels.iter_mut().zip(pixels) {
            // Windows passes mouse input through pixels of zero alpha; a
            // nearly transparent pixel keeps the whole widget clickable.
            *target = if source >> 24 == 0 {
                0x0100_0000
            } else {
                *source
            };
        }
        let origin = POINT { x: 0, y: 0 };
        let size = SIZE {
            cx: width,
            cy: height,
        };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            Some(screen_dc),
            None,
            Some(&size),
            Some(memory_dc),
            Some(&origin),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        SelectObject(memory_dc, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory_dc);
        ReleaseDC(None, screen_dc);
    }
}

/// Device pixels per logical pixel on `monitor`.
pub fn monitor_scale(monitor: HMONITOR) -> f64 {
    let (mut dpi_x, mut dpi_y) = (96, 96);
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_ok()
        && dpi_x > 0
    {
        (f64::from(dpi_x) / 96.0).clamp(0.25, 8.0)
    } else {
        let dpi = unsafe { GetDpiForSystem() };
        if dpi > 0 { f64::from(dpi) / 96.0 } else { 1.0 }
    }
}

fn covers(rect: RECT, monitor: RECT) -> bool {
    const EDGE: i32 = 2;
    rect.left <= monitor.left + EDGE
        && rect.top <= monitor.top + EDGE
        && rect.right >= monitor.right - EDGE
        && rect.bottom >= monitor.bottom - EDGE
}

/// A full-screen application (game, video, slides) covers `monitor`: a
/// floating widget hides meanwhile.
pub fn fullscreen_on(monitor: HMONITOR, monitor_rect: RECT) -> bool {
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.is_invalid()
            || !IsWindowVisible(foreground).as_bool()
            || IsIconic(foreground).as_bool()
        {
            return false;
        }
        let class = shell::class_name(foreground).unwrap_or_default();
        if matches!(
            class.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | CLASS_NAME
        ) {
            return false;
        }
        if MonitorFromWindow(foreground, MONITOR_DEFAULTTONULL) != monitor {
            return false;
        }
        let mut rect = RECT::default();
        if DwmGetWindowAttribute(
            foreground,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut _ as *mut std::ffi::c_void,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_err()
            && GetWindowRect(foreground, &mut rect).is_err()
        {
            return false;
        }
        covers(rect, monitor_rect)
    }
}

/// The default procedure for a widget without a host: never activated.
///
/// # Safety
/// Called by Windows with the arguments of a window message.
pub unsafe extern "system" fn passive_procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_screen_allows_a_small_edge_tolerance() {
        let monitor = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        assert!(covers(
            RECT {
                left: 1,
                top: -1,
                right: 1919,
                bottom: 1080
            },
            monitor
        ));
        assert!(!covers(
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1040
            },
            monitor
        ));
    }
}
