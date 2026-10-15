//! Where the widget goes, as pure functions over taskbar and monitor
//! geometry in device pixels, so every rule is tested on synthetic shapes.
//! The docking and floating rules follow Taskbar Hub's `src/hub/layout.rs`
//! (Apache-2.0, see NOTICE).
//!
//! The widget docks left of the notification area of the primary taskbar.
//! When that would cover a taskbar button or leave the taskbar, it floats
//! just above the taskbar instead, and docks again as soon as there is room.
//! Dragged elsewhere, it floats where it was dropped, remembered in logical
//! pixels relative to its monitor so another scale or position keeps it.

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{POINT, RECT};

use super::occupancy::intersection;

/// Logical gap to the notification area and between floating and taskbar.
pub const GAP: f64 = 8.0;
/// The primary monitor, whatever its device name.
pub const PRIMARY: &str = "primary";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Placement {
    /// Docked next to the notification area of the primary taskbar.
    #[default]
    NearTray,
    /// Floating at logical `x`, `y` from the top-left of `monitor`.
    Floating { monitor: String, x: i32, y: i32 },
}

/// A horizontal or vertical taskbar, in device pixels.
#[derive(Clone, Debug, Default)]
pub struct Taskbar {
    pub rect: RECT,
    pub tray: Option<RECT>,
    /// What its controls take; `None` when unknown.
    pub occupied: Option<Vec<RECT>>,
    /// The parts of `occupied` a widget may touch (the notification area).
    pub reserved: Vec<RECT>,
    /// Windows other programs placed in the taskbar, such as other widgets:
    /// this one packs left of them.
    pub obstacles: Vec<RECT>,
    pub scale: f64,
}

/// A monitor, in device pixels.
#[derive(Clone, Debug)]
pub struct Monitor {
    /// Device name, such as `\\.\DISPLAY2`.
    pub name: String,
    pub rect: RECT,
    pub work: RECT,
    pub primary: bool,
    pub scale: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Spot {
    /// Inside the taskbar at this screen rect.
    Docked(RECT),
    /// A topmost popup at this screen rect.
    Floating(RECT),
}

fn rect(left: i32, top: i32, width: i32, height: i32) -> RECT {
    RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

fn horizontal(rect: RECT) -> bool {
    (rect.right - rect.left) >= (rect.bottom - rect.top)
}

impl Taskbar {
    fn gap(&self) -> i32 {
        (GAP * self.scale).round() as i32
    }

    /// The taskbar's `rect` when it covers `widget`'s spot without a free
    /// control in the way.
    fn covers_buttons(&self, widget: RECT) -> bool {
        self.occupied.as_ref().is_some_and(|occupied| {
            occupied.iter().any(|item| {
                intersection(*item, widget).is_some()
                    && !self
                        .reserved
                        .iter()
                        .any(|tray| intersection(*item, *tray) == Some(*item))
            })
        })
    }
}

/// Where a widget of `width` × `height` device pixels goes near the tray of
/// `taskbar`, on `monitor`.
pub fn near_tray(taskbar: &Taskbar, monitor: RECT, width: i32, height: i32) -> Spot {
    let gap = taskbar.gap();
    let bar = taskbar.rect;
    if !horizontal(bar) {
        // Beside a vertical taskbar, at its top.
        let left = if bar.left <= monitor.left {
            bar.right + gap
        } else {
            bar.left - width - gap
        };
        return Spot::Floating(rect(left, bar.top + gap, width, height));
    }
    let tray_edge = taskbar.tray.map_or(bar.right, |tray| tray.left);
    let top = (bar.bottom - height).max(bar.top);
    let mut docked = rect(tray_edge - gap - width, top, width, height);
    let mut stepped = false;
    // Step left past the other programs' windows.
    while let Some(other) = taskbar
        .obstacles
        .iter()
        .filter(|other| intersection(**other, docked).is_some())
        .min_by_key(|other| other.left)
    {
        docked = rect(other.left - gap - width, top, width, height);
        stepped = true;
    }
    // Away from the tray, the buttons may reach the spot: dock there only
    // once they are known.
    let known = taskbar.occupied.is_some() || !stepped;
    let fits = known
        && docked.left >= bar.left
        && height <= bar.bottom - bar.top
        && !taskbar.covers_buttons(docked);
    if fits {
        Spot::Docked(docked)
    } else {
        Spot::Floating(above(taskbar, monitor, docked, width, height))
    }
}

/// Just outside the taskbar, aligned with `anchor` and within its width:
/// above it, or below a taskbar at the top of its monitor.
fn above(taskbar: &Taskbar, monitor: RECT, anchor: RECT, width: i32, height: i32) -> RECT {
    let gap = taskbar.gap() / 2;
    let bar = taskbar.rect;
    let left = anchor
        .left
        .clamp(bar.left, (bar.right - width).max(bar.left));
    let top = if bar.top > monitor.top {
        bar.top - height - gap
    } else {
        bar.bottom + gap
    };
    rect(left, top, width, height)
}

/// The monitor a floating placement names, the primary one when it is gone.
pub fn resolve<'a>(monitors: &'a [Monitor], name: &str) -> Option<&'a Monitor> {
    monitors
        .iter()
        .find(|monitor| {
            if name == PRIMARY {
                monitor.primary
            } else {
                monitor.name == name
            }
        })
        .or_else(|| monitors.iter().find(|monitor| monitor.primary))
        .or_else(|| monitors.first())
}

/// The screen rect of a floating widget, kept inside its monitor's work
/// area so it stays reachable after a scale or resolution change.
pub fn floating(monitor: &Monitor, x: i32, y: i32, width: i32, height: i32) -> RECT {
    let work = monitor.work;
    let left = monitor.rect.left + (f64::from(x) * monitor.scale).round() as i32;
    let top = monitor.rect.top + (f64::from(y) * monitor.scale).round() as i32;
    let left = left.clamp(work.left, (work.right - width).max(work.left));
    let top = top.clamp(work.top, (work.bottom - height).max(work.top));
    rect(left, top, width, height)
}

/// The placement of a widget dropped at `cursor` with its top-left at
/// `origin`: on the primary taskbar it docks near the tray; elsewhere it
/// floats there.
pub fn dropped(
    cursor: POINT,
    origin: POINT,
    taskbar: Option<&Taskbar>,
    monitors: &[Monitor],
) -> Placement {
    let on_taskbar = taskbar.is_some_and(|taskbar| {
        let bar = taskbar.rect;
        horizontal(bar)
            && cursor.x >= bar.left
            && cursor.x < bar.right
            && cursor.y >= bar.top
            && cursor.y < bar.bottom
    });
    if on_taskbar {
        return Placement::NearTray;
    }
    let monitor = monitors
        .iter()
        .find(|monitor| {
            let r = monitor.rect;
            cursor.x >= r.left && cursor.x < r.right && cursor.y >= r.top && cursor.y < r.bottom
        })
        .or_else(|| resolve(monitors, PRIMARY));
    let Some(monitor) = monitor else {
        return Placement::NearTray;
    };
    Placement::Floating {
        monitor: if monitor.primary {
            PRIMARY.to_string()
        } else {
            monitor.name.clone()
        },
        x: (f64::from(origin.x - monitor.rect.left) / monitor.scale).round() as i32,
        y: (f64::from(origin.y - monitor.rect.top) / monitor.scale).round() as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn r(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    const MONITOR: RECT = r(0, 0, 1920, 1080);

    fn taskbar() -> Taskbar {
        Taskbar {
            rect: r(0, 1032, 1920, 1080),
            tray: Some(r(1700, 1032, 1920, 1080)),
            occupied: Some(vec![r(760, 1032, 1160, 1080), r(1700, 1032, 1920, 1080)]),
            reserved: vec![r(1700, 1032, 1920, 1080)],
            obstacles: Vec::new(),
            scale: 1.0,
        }
    }

    fn monitor(name: &str, rect: RECT, primary: bool, scale: f64) -> Monitor {
        Monitor {
            name: name.into(),
            rect,
            work: RECT {
                bottom: rect.bottom - 48,
                ..rect
            },
            primary,
            scale,
        }
    }

    #[test]
    fn the_widget_docks_left_of_the_tray_at_the_bottom() {
        assert_eq!(
            near_tray(&taskbar(), MONITOR, 134, 46),
            Spot::Docked(r(1700 - 8 - 134, 1034, 1692, 1080))
        );
        // At 150 %, the taskbar is 72 high and the gap scales too.
        let mut scaled = taskbar();
        scaled.scale = 1.5;
        scaled.rect.top = 1008;
        scaled.tray = Some(r(1700, 1008, 1920, 1080));
        scaled.occupied = Some(vec![r(760, 1008, 1160, 1080)]);
        let Spot::Docked(spot) = near_tray(&scaled, MONITOR, 201, 69) else {
            panic!("not docked");
        };
        assert_eq!(spot.right, 1700 - 12);
    }

    #[test]
    fn a_full_taskbar_makes_it_float_above_and_room_docks_it_again() {
        let mut full = taskbar();
        full.occupied = Some(vec![r(200, 1032, 1690, 1080), r(1700, 1032, 1920, 1080)]);
        let Spot::Floating(spot) = near_tray(&full, MONITOR, 134, 46) else {
            panic!("docked over the buttons");
        };
        assert_eq!(spot.bottom, 1032 - 4, "just above the taskbar");
        assert!(spot.right <= 1920);
        assert!(matches!(
            near_tray(&taskbar(), MONITOR, 134, 46),
            Spot::Docked(_)
        ));
    }

    #[test]
    fn other_widgets_in_the_taskbar_are_stepped_over() {
        let mut shared = taskbar();
        shared.obstacles = vec![r(1500, 1032, 1690, 1080), r(1400, 1040, 1495, 1080)];
        assert_eq!(
            near_tray(&shared, MONITOR, 134, 46),
            Spot::Docked(r(1400 - 8 - 134, 1034, 1392, 1080))
        );
        // No room left of them before the buttons: float.
        shared.obstacles.push(r(1170, 1032, 1390, 1080));
        assert!(matches!(
            near_tray(&shared, MONITOR, 134, 46),
            Spot::Floating(_)
        ));
    }

    #[test]
    fn unknown_buttons_allow_docking_next_to_the_tray_only() {
        let mut unknown = taskbar();
        unknown.occupied = None;
        assert!(matches!(
            near_tray(&unknown, MONITOR, 134, 46),
            Spot::Docked(_)
        ));
        // Pushed away from the tray by other widgets, it waits above the
        // taskbar until the buttons are known.
        unknown.obstacles = vec![r(1500, 1032, 1690, 1080)];
        assert!(matches!(
            near_tray(&unknown, MONITOR, 134, 46),
            Spot::Floating(_)
        ));
    }

    #[test]
    fn a_vertical_or_top_taskbar_keeps_the_widget_outside_it() {
        let mut left = taskbar();
        left.rect = r(0, 0, 62, 1080);
        left.tray = None;
        assert_eq!(
            near_tray(&left, MONITOR, 134, 46),
            Spot::Floating(r(70, 8, 204, 54))
        );
        // A crowded taskbar at the top: the widget floats below it.
        let mut top = taskbar();
        top.rect = r(0, 0, 1920, 48);
        top.tray = Some(r(1700, 0, 1920, 48));
        top.occupied = Some(vec![r(0, 0, 1700, 48)]);
        let Spot::Floating(spot) = near_tray(&top, MONITOR, 134, 46) else {
            panic!("docked");
        };
        assert_eq!(spot.top, 48 + 4);
    }

    #[test]
    fn a_floating_widget_is_remembered_in_logical_pixels_of_its_monitor() {
        let monitors = [
            monitor(r"\\.\DISPLAY1", r(0, 0, 1920, 1080), true, 1.0),
            monitor(r"\\.\DISPLAY2", r(1920, 0, 1920 + 2560, 1440), false, 1.5),
        ];
        let placement = dropped(
            POINT { x: 2500, y: 700 },
            POINT { x: 2370, y: 690 },
            Some(&taskbar()),
            &monitors,
        );
        assert_eq!(
            placement,
            Placement::Floating {
                monitor: r"\\.\DISPLAY2".into(),
                x: 300,
                y: 460
            }
        );
        let Placement::Floating { monitor, x, y } = placement else {
            unreachable!()
        };
        let second = resolve(&monitors, &monitor).unwrap();
        assert_eq!(floating(second, x, y, 201, 69), r(2370, 690, 2571, 759));
        // Dropped on the taskbar, it docks again.
        assert_eq!(
            dropped(
                POINT { x: 1000, y: 1050 },
                POINT { x: 950, y: 1040 },
                Some(&taskbar()),
                &monitors
            ),
            Placement::NearTray
        );
    }

    #[test]
    fn a_floating_widget_stays_reachable_when_its_monitor_is_gone_or_shrinks() {
        let only = [monitor(r"\\.\DISPLAY1", r(0, 0, 1280, 720), true, 1.0)];
        let fallback = resolve(&only, r"\\.\DISPLAY2").unwrap();
        assert!(fallback.primary);
        // Saved for a larger screen: brought back inside the work area.
        assert_eq!(
            floating(fallback, 1800, 1000, 134, 46),
            r(1280 - 134, 672 - 46, 1280, 672)
        );
    }

    #[test]
    fn placements_read_and_write_as_tagged_json() {
        let floating = Placement::Floating {
            monitor: PRIMARY.into(),
            x: 10,
            y: -4,
        };
        let json = serde_json::to_string(&floating).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"floating","monitor":"primary","x":10,"y":-4}"#
        );
        assert_eq!(serde_json::from_str::<Placement>(&json).unwrap(), floating);
        assert_eq!(
            serde_json::from_str::<Placement>(r#"{"kind":"nearTray"}"#).unwrap(),
            Placement::NearTray
        );
    }
}
