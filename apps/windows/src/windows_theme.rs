//! The Windows theme settings the dashboard follows. Adapted from Taskbar
//! Hub's `src/theme.rs` (Apache-2.0, see NOTICE).

use windows::{
    Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    },
    core::PCWSTR,
};

use crate::native::wide;

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const ACCENT: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";

/// Whether applications, such as the dashboard, use the light theme.
pub fn apps_use_light_theme() -> bool {
    read_value(PERSONALIZE, "AppsUseLightTheme").is_some_and(|value| value.first() == Some(&1))
}

/// Whether the taskbar and the notification area use the light theme.
pub fn system_uses_light_theme() -> bool {
    read_value(PERSONALIZE, "SystemUsesLightTheme").is_some_and(|value| value.first() == Some(&1))
}

/// The Windows accent color in the shade Windows 11 draws controls with:
/// a lighter one on dark surfaces, a darker one on light surfaces.
pub fn accent_shade(light: bool) -> Option<[u8; 3]> {
    // Eight RGBA swatches, from the lightest to the darkest; the fourth is
    // the accent itself.
    let swatches = read_value(ACCENT, "AccentPalette")?;
    let index = if light { 4 } else { 1 };
    let swatch = swatches.get(index * 4..index * 4 + 3)?;
    Some([swatch[0], swatch[1], swatch[2]])
}

/// The raw data of a value under `HKEY_CURRENT_USER`.
fn read_value(path: &str, name: &str) -> Option<Vec<u8>> {
    let path = wide(path);
    let name = wide(name);
    unsafe {
        let mut key = HKEY::default();
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            None,
            KEY_READ,
            &mut key,
        )
        .ok()
        .ok()?;
        let mut size = 0u32;
        let sized = RegQueryValueExW(
            key,
            PCWSTR::from_raw(name.as_ptr()),
            None,
            None,
            None,
            Some(&mut size),
        );
        let mut data = vec![0u8; size as usize];
        let read = sized.is_ok()
            && RegQueryValueExW(
                key,
                PCWSTR::from_raw(name.as_ptr()),
                None,
                None,
                Some(data.as_mut_ptr()),
                Some(&mut size),
            )
            .is_ok();
        let _ = RegCloseKey(key);
        read.then(|| {
            data.truncate(size as usize);
            data
        })
    }
}
