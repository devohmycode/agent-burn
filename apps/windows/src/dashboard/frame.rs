//! The native dashboard window: its title bar follows the Windows theme and,
//! on Windows 11 with OpenGL, the Mica backdrop shows through. Adapted from
//! Taskbar Hub's `src/dashboard/runner.rs` (Apache-2.0, see NOTICE).

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Dwm::{
        DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
        DWMWINDOWATTRIBUTE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
    },
    UI::Controls::MARGINS,
};

/// Turns Mica off, as on Windows 10, to check the opaque look.
pub const NO_MICA_ENV: &str = "AGENT_BURN_NO_MICA";

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeFrame {
    hwnd: isize,
    /// Mica is shown; the surfaces must leave the background transparent.
    pub mica: bool,
}

impl NativeFrame {
    /// The window, once created.
    pub fn hwnd(&self) -> Option<HWND> {
        (self.hwnd != 0).then_some(HWND(self.hwnd as *mut _))
    }

    /// Styles `window`; `mica` asks for the backdrop, which only Windows 11
    /// 22H2 and later can draw.
    pub fn new(window: &impl HasWindowHandle, mica: bool) -> Self {
        let Some(hwnd) = window_hwnd(window) else {
            return Self::default();
        };
        let wanted = mica && std::env::var_os(NO_MICA_ENV).is_none();
        let mica = wanted
            && unsafe {
                set_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW.0)
                    && DwmExtendFrameIntoClientArea(
                        hwnd,
                        &MARGINS {
                            cxLeftWidth: -1,
                            cxRightWidth: -1,
                            cyTopHeight: -1,
                            cyBottomHeight: -1,
                        },
                    )
                    .is_ok()
            };
        Self {
            hwnd: hwnd.0 as isize,
            mica,
        }
    }

    /// Draws the title bar dark or light.
    pub fn set_dark(self, dark: bool) {
        if self.hwnd != 0 {
            let hwnd = HWND(self.hwnd as *mut _);
            unsafe {
                set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, i32::from(dark));
            }
        }
    }
}

unsafe fn set_attribute(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: i32) -> bool {
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            std::ptr::from_ref(&value).cast(),
            std::mem::size_of_val(&value) as u32,
        )
        .is_ok()
    }
}

fn window_hwnd(window: &impl HasWindowHandle) -> Option<HWND> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(HWND(handle.hwnd.get() as *mut _)),
        _ => None,
    }
}
