//! The menu of the widget and of the notification-area icon: the same
//! actions as the dashboard offers.

use windows::{
    Win32::{
        Foundation::{HWND, POINT},
        UI::WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HMENU, MENU_ITEM_FLAGS,
            MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, PostMessageW,
            SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_NULL,
        },
    },
    core::PCWSTR,
};

use crate::{native::wide, settings::Provider};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    OpenDashboard,
    Refresh,
    Show(Provider),
    ToggleWidget,
    DockNearTray,
    Quit,
}

const OPEN: usize = 1;
const REFRESH: usize = 2;
const PROVIDER: usize = 10;
const TOGGLE_WIDGET: usize = 20;
const DOCK: usize = 21;
const QUIT: usize = 30;

/// What the menu shows checked or enabled.
#[derive(Clone, Copy, Debug)]
pub struct State {
    pub provider: Provider,
    pub widget_shown: bool,
    pub floating: bool,
}

fn command(id: usize) -> Option<Command> {
    Some(match id {
        OPEN => Command::OpenDashboard,
        REFRESH => Command::Refresh,
        TOGGLE_WIDGET => Command::ToggleWidget,
        DOCK => Command::DockNearTray,
        QUIT => Command::Quit,
        id if (PROVIDER..PROVIDER + Provider::ALL.len()).contains(&id) => {
            Command::Show(Provider::ALL[id - PROVIDER])
        }
        _ => return None,
    })
}

unsafe fn append(menu: HMENU, flags: MENU_ITEM_FLAGS, id: usize, text: &str) {
    let text = wide(text);
    unsafe {
        let _ = AppendMenuW(menu, flags, id, PCWSTR::from_raw(text.as_ptr()));
    }
}

/// Shows the menu at the pointer for `owner` and returns the chosen command.
/// The owner comes to the front first, as Windows requires for the menu to
/// close when the user clicks elsewhere.
pub fn show(owner: HWND, state: State) -> Option<Command> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let providers = CreatePopupMenu().ok()?;
        for (index, provider) in Provider::ALL.into_iter().enumerate() {
            let flags = if provider == state.provider {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            append(providers, flags, PROVIDER + index, provider.label());
        }
        append(menu, MF_STRING, OPEN, "Open Agent Burn");
        append(menu, MF_STRING, REFRESH, "Refresh quotas now");
        append(menu, MF_SEPARATOR, 0, "");
        append(
            menu,
            MF_STRING | MF_POPUP,
            providers.0 as usize,
            "Widget shows",
        );
        let shown = if state.widget_shown {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        append(menu, shown, TOGGLE_WIDGET, "Show the widget");
        let dock = if state.widget_shown && state.floating {
            MF_STRING
        } else {
            MF_STRING | MF_GRAYED
        };
        append(menu, dock, DOCK, "Dock next to the notification area");
        append(menu, MF_SEPARATOR, 0, "");
        append(menu, MF_STRING, QUIT, "Quit Agent Burn");
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let _ = SetForegroundWindow(owner);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            cursor.x,
            cursor.y,
            None,
            owner,
            None,
        );
        let _ = PostMessageW(Some(owner), WM_NULL, Default::default(), Default::default());
        // Destroys the submenu too.
        let _ = DestroyMenu(menu);
        command(chosen.0 as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_item_maps_to_its_command() {
        assert_eq!(command(OPEN), Some(Command::OpenDashboard));
        assert_eq!(command(PROVIDER + 1), Some(Command::Show(Provider::Codex)));
        assert_eq!(command(PROVIDER + 3), None);
        assert_eq!(command(0), None, "the menu was closed");
        assert_eq!(command(QUIT), Some(Command::Quit));
    }
}
