//! Which parts of the taskbar its buttons take, read through UI Automation
//! on a thread of its own: Windows 11's taskbar buttons have no windows to
//! measure. Neither the widget's thread nor anything else waits for it; the
//! widget reads the last sample. Adapted from Taskbar Hub's
//! `src/hub/collision.rs` (Apache-2.0, see NOTICE).

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::RECT,
    System::{
        Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx},
        Variant::{VARIANT, VARIANT_0_0, VT_I4},
    },
    UI::{Accessibility::*, WindowsAndMessaging::IsWindowVisible},
};

use super::shell::{self, TaskbarWindow};

/// Shortest and longest pause between two readings: it doubles while the
/// taskbar stays the same, and falls back as soon as it changes.
const READ_PAUSE: (Duration, Duration) = (Duration::from_secs(2), Duration::from_secs(6));
/// A sample older than the longest pause plus a slow reading is not trusted.
const MAX_SAMPLE_AGE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
struct Handle(isize);

static SAMPLES: Mutex<Vec<(Handle, Occupancy, Instant)>> = Mutex::new(Vec::new());

/// What a taskbar's controls take, in screen device pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Occupancy {
    pub bounds: RECT,
    /// App buttons, Start, Search, Widgets and the notification area.
    pub occupied: Vec<RECT>,
    /// The notification area: a widget may touch it.
    pub reserved: Vec<RECT>,
}

pub fn intersection(a: RECT, b: RECT) -> Option<RECT> {
    let rect = RECT {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right),
        bottom: a.bottom.min(b.bottom),
    };
    (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
}

fn next_pause(pause: Duration, unchanged: bool) -> Duration {
    if unchanged {
        (pause * 2).min(READ_PAUSE.1)
    } else {
        READ_PAUSE.0
    }
}

/// The last sample of `taskbar`, while it is recent and the taskbar has not
/// moved; `None` when unknown, never an empty taskbar.
pub fn cached(taskbar: TaskbarWindow) -> Option<Occupancy> {
    if !unsafe { IsWindowVisible(taskbar.hwnd) }.as_bool() {
        return None;
    }
    let samples = SAMPLES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    samples
        .iter()
        .find(|(handle, sample, at)| {
            handle.0 == taskbar.hwnd.0 as isize
                && sample.bounds == taskbar.rect
                && at.elapsed() <= MAX_SAMPLE_AGE
        })
        .map(|(_, sample, _)| sample.clone())
}

// Controls that take room, by role: localized names and Windows 11 class
// names would miss other shells.
const SPACE_ROLES: [UIA_CONTROLTYPE_ID; 8] = [
    UIA_ButtonControlTypeId,
    UIA_SplitButtonControlTypeId,
    UIA_CheckBoxControlTypeId,
    UIA_RadioButtonControlTypeId,
    UIA_ListItemControlTypeId,
    UIA_TabItemControlTypeId,
    UIA_MenuItemControlTypeId,
    UIA_EditControlTypeId,
];

struct Reader {
    automation: IUIAutomation2,
    request: IUIAutomationCacheRequest,
    condition: IUIAutomationCondition,
}

impl Reader {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            let automation: IUIAutomation2 =
                CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;
            automation.SetConnectionTimeout(500)?;
            automation.SetTransactionTimeout(1000)?;
            let request = automation.CreateCacheRequest()?;
            request.SetTreeScope(TreeScope_Element)?;
            request.SetAutomationElementMode(AutomationElementMode_None)?;
            for property in [
                UIA_BoundingRectanglePropertyId,
                UIA_IsOffscreenPropertyId,
                UIA_ControlTypePropertyId,
                UIA_ProcessIdPropertyId,
                UIA_ClassNamePropertyId,
            ] {
                request.AddProperty(property)?;
            }
            // Only the controls that take room: Explorer then marshals a few
            // dozen elements instead of the whole taskbar tree.
            let mut condition: Option<IUIAutomationCondition> = None;
            for role in SPACE_ROLES {
                let mut value = VARIANT::default();
                let inner: &mut VARIANT_0_0 = &mut value.Anonymous.Anonymous;
                inner.vt = VT_I4;
                inner.Anonymous.lVal = role.0;
                let this = automation.CreatePropertyCondition(UIA_ControlTypePropertyId, &value)?;
                condition = Some(match condition {
                    Some(previous) => automation.CreateOrCondition(&previous, &this)?,
                    None => this,
                });
            }
            let condition = match condition {
                Some(condition) => condition,
                None => automation.CreateTrueCondition()?,
            };
            Ok(Self {
                automation,
                request,
                condition,
            })
        }
    }

    fn read(&self, taskbar: TaskbarWindow) -> windows::core::Result<Option<Occupancy>> {
        unsafe {
            let bounds = taskbar.rect;
            if !IsWindowVisible(taskbar.hwnd).as_bool() || !taskbar_shown(bounds) {
                return Ok(None);
            }
            let root = self.automation.ElementFromHandle(taskbar.hwnd)?;
            let elements =
                root.FindAllBuildCache(TreeScope_Descendants, &self.condition, &self.request)?;
            let tray = shell::tray_rect(taskbar.hwnd).and_then(|rect| intersection(rect, bounds));
            let count = elements.Length()?;
            // An absent accessibility tree is unknown, never proof of room.
            if count == 0 {
                return Ok(None);
            }
            let (mut occupied, mut reserved) = (Vec::new(), Vec::new());
            let mut found_control = false;
            for index in 0..count {
                let element = elements.GetElement(index)?;
                if element.CachedProcessId()? == std::process::id() as i32
                    || !SPACE_ROLES.contains(&element.CachedControlType()?)
                {
                    continue;
                }
                found_control = true;
                if element.CachedIsOffscreen()?.as_bool() {
                    continue;
                }
                if let Some(rect) = intersection(element.CachedBoundingRectangle()?, bounds) {
                    let in_tray = element
                        .CachedClassName()?
                        .to_string()
                        .starts_with("SystemTray.")
                        || tray.is_some_and(|tray| intersection(rect, tray) == Some(rect));
                    if in_tray {
                        reserved.push(rect);
                    }
                    occupied.push(rect);
                }
            }
            if !found_control {
                return Ok(None);
            }
            // The notification area, even where its icons lack UI Automation.
            if let Some(tray) = tray {
                occupied.push(tray);
                reserved.push(tray);
            }
            // A tree read while the taskbar moved (auto-hide, DPI) is dropped.
            if shell::window_rect(taskbar.hwnd) != Some(bounds) {
                return Ok(None);
            }
            Ok(Some(Occupancy {
                bounds,
                occupied,
                reserved,
            }))
        }
    }
}

/// Whether the taskbar is on screen, not slid away by auto-hide.
fn taskbar_shown(bounds: RECT) -> bool {
    let horizontal = shell::is_horizontal(bounds);
    let thickness = if horizontal {
        bounds.bottom - bounds.top
    } else {
        bounds.right - bounds.left
    };
    thickness > 2
        && shell::find_monitors().iter().any(|monitor| {
            intersection(bounds, monitor.rect).is_some_and(|visible| {
                let shown = if horizontal {
                    visible.bottom - visible.top
                } else {
                    visible.right - visible.left
                };
                shown >= thickness
            })
        })
}

/// Starts the reader thread; it runs until the process ends.
pub fn spawn_reader() {
    std::thread::spawn(|| {
        if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
            return;
        }
        loop {
            if let Ok(reader) = Reader::new() {
                let mut pause = READ_PAUSE.0;
                loop {
                    let next: Vec<(Handle, Occupancy, Instant)> = shell::find_taskbars()
                        .into_iter()
                        .filter_map(|taskbar| {
                            let sample = reader.read(taskbar).ok().flatten()?;
                            Some((Handle(taskbar.hwnd.0 as isize), sample, Instant::now()))
                        })
                        .collect();
                    let mut samples = SAMPLES
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let unchanged = samples.len() == next.len()
                        && samples
                            .iter()
                            .zip(&next)
                            .all(|((a, x, _), (b, y, _))| a.0 == b.0 && x == y);
                    pause = next_pause(pause, unchanged);
                    *samples = next;
                    drop(samples);
                    std::thread::sleep(pause);
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_space_out_while_the_taskbar_stays_the_same() {
        let (short, long) = READ_PAUSE;
        assert_eq!(next_pause(short, true), short * 2);
        assert_eq!(next_pause(long, true), long);
        assert_eq!(next_pause(long, false), short);
        assert!(MAX_SAMPLE_AGE > long, "a sample outlives the longest pause");
    }

    #[test]
    fn intersections_are_empty_when_rects_only_touch() {
        let a = RECT {
            left: 0,
            top: 0,
            right: 10,
            bottom: 10,
        };
        let b = RECT {
            left: 10,
            top: 0,
            right: 20,
            bottom: 10,
        };
        assert_eq!(intersection(a, b), None);
        let c = RECT {
            left: 5,
            top: 5,
            right: 15,
            bottom: 15,
        };
        assert_eq!(
            intersection(a, c),
            Some(RECT {
                left: 5,
                top: 5,
                right: 10,
                bottom: 10
            })
        );
    }
}
