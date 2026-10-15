//! The notification-area icon of the background process. Adapted from
//! Taskbar Hub's `src/tray_icon.rs` (Apache-2.0, see NOTICE).

use windows::{
    Win32::{
        Foundation::HWND,
        System::LibraryLoader::GetModuleFileNameW,
        UI::{
            Shell::{
                ExtractIconExW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
                NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
            },
            WindowsAndMessaging::{DestroyIcon, HICON},
        },
    },
    core::PCWSTR,
};

use super::instance::WM_APP_TRAY;

const ICON_ID: u32 = 1;

/// The application's small icon, embedded by `build.rs`.
fn app_icon() -> HICON {
    unsafe {
        let mut path = [0u16; 1024];
        let length = GetModuleFileNameW(None, &mut path) as usize;
        if length == 0 {
            return HICON::default();
        }
        let (mut large, mut small) = (HICON::default(), HICON::default());
        let extracted = ExtractIconExW(
            PCWSTR::from_raw(path.as_ptr()),
            0,
            Some(&mut large),
            Some(&mut small),
            1,
        );
        if extracted == 0 {
            return HICON::default();
        }
        if !large.is_invalid() && !small.is_invalid() {
            let _ = DestroyIcon(large);
        }
        if small.is_invalid() { large } else { small }
    }
}

fn data(hwnd: HWND, icon: HICON, tip: &str) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: icon,
        ..Default::default()
    };
    // At most 127 characters and the terminating zero.
    for (slot, unit) in data.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
        *slot = unit;
    }
    data
}

/// The icon of `hwnd`, which receives `WM_APP_TRAY` with the
/// `NOTIFYICON_VERSION_4` events: `NIN_SELECT`, `NIN_KEYSELECT` and
/// `WM_CONTEXTMENU` in the low word of `lParam`.
pub struct Tray {
    hwnd: HWND,
    icon: HICON,
    tip: String,
    added: bool,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            icon: app_icon(),
            tip: "Agent Burn".into(),
            added: false,
        }
    }

    /// Adds the icon; also after Explorer restarted and dropped it.
    pub fn add(&mut self) -> bool {
        let mut data = data(self.hwnd, self.icon, &self.tip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            self.added = Shell_NotifyIconW(NIM_ADD, &data).as_bool();
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
        }
        self.added
    }

    pub fn set_tip(&mut self, tip: &str) {
        if tip == self.tip {
            return;
        }
        self.tip = tip.to_string();
        if self.added {
            let data = data(self.hwnd, self.icon, &self.tip);
            unsafe {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
            }
        }
    }

    pub fn remove(&mut self) {
        if self.added {
            let data = data(self.hwnd, self.icon, &self.tip);
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            }
            self.added = false;
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        if !self.icon.is_invalid() {
            unsafe {
                let _ = DestroyIcon(self.icon);
            }
        }
    }
}
