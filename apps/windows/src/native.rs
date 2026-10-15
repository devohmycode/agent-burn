//! Small Win32 helpers shared by the windows of the application.

use std::{ffi::OsStr, os::windows::ffi::OsStrExt, path::PathBuf};

use windows::{
    Win32::{
        System::Com::CoTaskMemFree,
        UI::{
            Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
            WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
        },
    },
    core::PCWSTR,
};

/// `text` as a NUL-terminated UTF-16 string.
pub fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

/// `%LOCALAPPDATA%`, from the known folder rather than the environment.
pub fn local_app_data() -> Option<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None).ok()?;
        let result = path.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(path.0.cast()));
        result
    }
}

/// A blocking error box, for failures that leave no window to show them in.
pub fn error_box(title: &str, message: &str) {
    let title = wide(title);
    let message = wide(message);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR::from_raw(message.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}
