//! Starting Agent Burn when the user signs in: a value of the current
//! user's `Run` key, which needs no administrator rights. It starts the
//! background process (widget and notification-area icon), and the
//! dashboard too when the user asks for it. The value is written again when
//! the application starts from another place, and removed when the option
//! is turned off.

use std::path::Path;

use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR},
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPEN_CREATE_OPTIONS, REG_SZ, RRF_RT_REG_SZ,
            RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW, RegSetValueExW,
        },
    },
    core::PCWSTR,
};

use crate::native::wide;

/// Where Windows looks for programs to start at sign-in.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const VALUE_NAME: &str = "Agent Burn";

/// The command line that starts `executable` at sign-in.
pub fn command(executable: &Path, open_dashboard: bool) -> String {
    let path = format!("\"{}\"", executable.display());
    if open_dashboard {
        path
    } else {
        format!("{path} --widget")
    }
}

fn error(code: WIN32_ERROR) -> String {
    windows::core::Error::from(code.to_hresult()).message()
}

/// The command registered under `key`, if any.
pub fn read(key: &str) -> Result<Option<String>, String> {
    let (key, name) = (wide(key), wide(VALUE_NAME));
    let mut size = 0u32;
    unsafe {
        let sized = RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key.as_ptr()),
            PCWSTR::from_raw(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        );
        if sized == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if sized.is_err() {
            return Err(error(sized));
        }
        let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
        let read = RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key.as_ptr()),
            PCWSTR::from_raw(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        );
        if read.is_err() {
            return Err(error(read));
        }
        let length = buffer
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(buffer.len());
        Ok(Some(String::from_utf16_lossy(&buffer[..length])))
    }
}

/// Registers `command` under `key`.
pub fn write(key: &str, command: &str) -> Result<(), String> {
    let (key, name) = (wide(key), wide(VALUE_NAME));
    let value: Vec<u8> = wide(command)
        .iter()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    unsafe {
        let mut handle = HKEY::default();
        let created = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPEN_CREATE_OPTIONS(0),
            KEY_SET_VALUE,
            None,
            &mut handle,
            None,
        );
        if created.is_err() {
            return Err(error(created));
        }
        let written = RegSetValueExW(
            handle,
            PCWSTR::from_raw(name.as_ptr()),
            None,
            REG_SZ,
            Some(&value),
        );
        let _ = RegCloseKey(handle);
        if written.is_err() {
            return Err(error(written));
        }
    }
    Ok(())
}

/// Removes the value; a missing one is not an error.
pub fn remove(key: &str) -> Result<(), String> {
    let (key, name) = (wide(key), wide(VALUE_NAME));
    let removed = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key.as_ptr()),
            PCWSTR::from_raw(name.as_ptr()),
        )
    };
    if removed.is_ok() || removed == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(error(removed))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Startup {
    Registered,
    /// Registered again, for this copy or another choice.
    Updated,
    AlreadyRegistered,
    Removed,
    Off,
}

/// Makes the value under `key` match the settings.
pub fn ensure(
    key: &str,
    enabled: bool,
    open_dashboard: bool,
    executable: &Path,
) -> Result<Startup, String> {
    let current = read(key)?;
    if !enabled {
        return match current {
            Some(_) => remove(key).map(|()| Startup::Removed),
            None => Ok(Startup::Off),
        };
    }
    let wanted = command(executable, open_dashboard);
    match current {
        Some(existing) if existing == wanted => Ok(Startup::AlreadyRegistered),
        Some(_) => write(key, &wanted).map(|()| Startup::Updated),
        None => write(key, &wanted).map(|()| Startup::Registered),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use windows::Win32::System::Registry::RegDeleteTreeW;

    use super::*;

    /// A key of its own, removed afterwards: the real `Run` key is never
    /// touched by the tests.
    struct TestKey(String);

    impl TestKey {
        fn new(name: &str) -> Self {
            Self(format!(
                r"Software\AgentBurnTests\{name}-{}",
                std::process::id()
            ))
        }
    }

    impl Drop for TestKey {
        fn drop(&mut self) {
            let key = wide(&self.0);
            unsafe {
                let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR::from_raw(key.as_ptr()));
            }
            let parent = wide(r"Software\AgentBurnTests");
            unsafe {
                let _ = windows::Win32::System::Registry::RegDeleteKeyW(
                    HKEY_CURRENT_USER,
                    PCWSTR::from_raw(parent.as_ptr()),
                );
            }
        }
    }

    #[test]
    fn the_command_starts_the_widget_or_the_dashboard() {
        let executable = PathBuf::from(r"C:\Program Files\Agent Burn\agent-burn-windows.exe");
        assert_eq!(
            command(&executable, false),
            r#""C:\Program Files\Agent Burn\agent-burn-windows.exe" --widget"#
        );
        assert_eq!(
            command(&executable, true),
            r#""C:\Program Files\Agent Burn\agent-burn-windows.exe""#
        );
    }

    #[test]
    fn turning_it_on_and_off_is_reversible_and_follows_the_application() {
        let key = TestKey::new("startup");
        let here = PathBuf::from(r"C:\Apps\Agent Burn\agent-burn-windows.exe");
        assert_eq!(ensure(&key.0, false, false, &here), Ok(Startup::Off));

        assert_eq!(ensure(&key.0, true, false, &here), Ok(Startup::Registered));
        assert_eq!(read(&key.0).unwrap(), Some(command(&here, false)));
        assert_eq!(
            ensure(&key.0, true, false, &here),
            Ok(Startup::AlreadyRegistered)
        );
        // Moved, or the dashboard asked for at sign-in: written again.
        let moved = PathBuf::from(r"D:\Tools\Agent Burn\agent-burn-windows.exe");
        assert_eq!(ensure(&key.0, true, false, &moved), Ok(Startup::Updated));
        assert_eq!(ensure(&key.0, true, true, &moved), Ok(Startup::Updated));
        assert_eq!(read(&key.0).unwrap(), Some(command(&moved, true)));

        assert_eq!(ensure(&key.0, false, true, &moved), Ok(Startup::Removed));
        assert_eq!(read(&key.0).unwrap(), None);
        assert_eq!(remove(&key.0), Ok(()), "removing twice is fine");
    }
}
