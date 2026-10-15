//! One background process and one dashboard per application folder, and
//! how each finds the other: named mutexes, the background process's hidden
//! window, and a property on the dashboard window.

use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM},
        System::Threading::CreateMutexW,
        UI::WindowsAndMessaging::{
            EnumWindows, FindWindowW, GetPropW, IsIconic, PostMessageW, SW_RESTORE,
            SetForegroundWindow, SetPropW, ShowWindow, WM_APP,
        },
    },
    core::{BOOL, PCWSTR},
};

use crate::native::wide;

/// The class of the background process's hidden window.
pub const HUB_CLASS: &str = "AgentBurnHub";
/// The property naming the folder of a dashboard window.
const FOLDER_PROPERTY: &str = "AgentBurn.Folder";

/// Messages the background process understands.
pub const WM_APP_QUOTAS: u32 = WM_APP + 1;
pub const WM_APP_TRAY: u32 = WM_APP + 2;
/// Quit Agent Burn: the dashboard and the background process.
pub const WM_APP_QUIT: u32 = WM_APP + 3;
/// The settings file changed.
pub const WM_APP_SETTINGS: u32 = WM_APP + 4;
/// Read the live meters now.
pub const WM_APP_REFRESH: u32 = WM_APP + 5;

/// A short, stable name for the application folder.
pub fn folder_key(folder: &Path) -> u32 {
    let text = folder.display().to_string().to_lowercase();
    // FNV-1a, 32 bits.
    text.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}

/// A named mutex held for the life of the process.
#[derive(Debug)]
pub struct Instance(HANDLE);

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Claims being the only `kind` of process for `folder`; `None` when
/// another one already is.
pub fn claim(kind: &str, folder: &Path) -> Option<Instance> {
    let name = wide(format!(
        "Local\\AgentBurn.{kind}.{:08x}",
        folder_key(folder)
    ));
    unsafe {
        let handle = CreateMutexW(None, true, PCWSTR::from_raw(name.as_ptr())).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return None;
        }
        Some(Instance(handle))
    }
}

pub fn hub_title(folder: &Path) -> String {
    format!("Agent Burn background {:08x}", folder_key(folder))
}

/// The hidden window of the background process of `folder`.
pub fn hub_window(folder: &Path) -> Option<HWND> {
    let (class, title) = (wide(HUB_CLASS), wide(hub_title(folder)));
    unsafe {
        FindWindowW(
            PCWSTR::from_raw(class.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
        )
    }
    .ok()
    .filter(|hwnd| !hwnd.is_invalid())
}

/// Posts `message` to the background process of `folder`; false without one.
pub fn tell_hub(folder: &Path, message: u32) -> bool {
    hub_window(folder).is_some_and(|hwnd| {
        unsafe { PostMessageW(Some(hwnd), message, Default::default(), Default::default()) }.is_ok()
    })
}

/// Marks `hwnd` as the dashboard of `folder`.
pub fn mark_dashboard(hwnd: HWND, folder: &Path) {
    let name = wide(FOLDER_PROPERTY);
    unsafe {
        let _ = SetPropW(
            hwnd,
            PCWSTR::from_raw(name.as_ptr()),
            Some(HANDLE(folder_key(folder) as usize as *mut _)),
        );
    }
}

/// The dashboard window of `folder`.
pub fn dashboard_window(folder: &Path) -> Option<HWND> {
    struct Search {
        key: usize,
        found: Option<HWND>,
    }
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
        let search = unsafe { &mut *(data.0 as *mut Search) };
        let name = wide(FOLDER_PROPERTY);
        let value = unsafe { GetPropW(hwnd, PCWSTR::from_raw(name.as_ptr())) };
        if value.0 as usize == search.key {
            search.found = Some(hwnd);
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut search = Search {
        key: folder_key(folder) as usize,
        found: None,
    };
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut search as *mut Search as isize));
    }
    search.found
}

/// Brings the dashboard window `hwnd` to the front. The caller has just
/// received the user's request, which lets Windows hand over the focus.
pub fn activate(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Starts this executable again with `arguments`, without waiting for it.
pub fn spawn(arguments: &[String]) -> io::Result<()> {
    let executable: PathBuf = std::env::current_exe()?;
    Command::new(executable).args(arguments).spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_have_stable_distinct_keys() {
        let usual = Path::new(r"C:\Users\Léa\AppData\Local\Agent Burn");
        assert_eq!(
            folder_key(usual),
            folder_key(Path::new(r"c:\users\léa\appdata\local\agent burn"))
        );
        assert_ne!(
            folder_key(usual),
            folder_key(Path::new(r"C:\Temp\Agent Burn test"))
        );
    }

    #[test]
    fn a_second_claim_on_the_same_folder_is_refused_until_the_first_ends() {
        let folder = tempfile::tempdir().unwrap();
        let first = claim("Test", folder.path()).unwrap();
        assert!(claim("Test", folder.path()).is_none());
        assert!(claim("Other", folder.path()).is_some(), "another kind");
        drop(first);
        assert!(claim("Test", folder.path()).is_some());
    }
}
