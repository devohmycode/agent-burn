//! The dashboard's Windows 11 (Fluent) look: light or dark with Windows, the
//! Windows accent, Segoe UI, and room for the Mica backdrop. Adapted from
//! Taskbar Hub's `src/ui/theme.rs` (Apache-2.0, see NOTICE); its icon fonts
//! and translations are left out.

use std::path::PathBuf;

use eframe::egui;

use crate::ui::tokens::{CONTROL_CORNER_RADIUS, CONTROL_HEIGHT, DROPDOWN_CORNER_RADIUS};

/// Font family of titles, Segoe UI Semibold when installed.
pub const SEMIBOLD: &str = "semibold";
/// Forces `light` or `dark` instead of the Windows app theme.
pub const THEME_ENV: &str = "AGENT_BURN_APP_THEME";

/// The colors of the shared components, set by `configure_fluent_style`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Palette {
    accent: egui::Color32,
    menu_surface: egui::Color32,
    muted: egui::Color32,
    selected_menu_fill: egui::Color32,
    success: egui::Color32,
    caution: egui::Color32,
    danger: egui::Color32,
    toggle_inactive: egui::Color32,
    toggle_inactive_hover: egui::Color32,
    toggle_knob: egui::Color32,
    toggle_label: egui::Color32,
    section_surface: egui::Color32,
    section_border: egui::Color32,
    track: egui::Color32,
    menu_hover: egui::Color32,
    menu_text: egui::Color32,
    focus: egui::Color32,
}

thread_local! {
    /// Per thread: the dashboard draws on one thread, and parallel tests may
    /// each use a different palette.
    static PALETTE: std::cell::Cell<Palette> =
        std::cell::Cell::new(fluent_palette(Fluent::DARK_DEFAULT));
}

fn palette() -> Palette {
    PALETTE.with(std::cell::Cell::get)
}

/// How the dashboard looks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fluent {
    /// Follows the Windows app theme.
    pub light: bool,
    /// The Windows accent color, in the shade Windows uses for this theme.
    pub accent: egui::Color32,
    /// The window shows the Mica backdrop behind transparent surfaces.
    pub mica: bool,
}

impl Fluent {
    const DARK_DEFAULT: Self = Self {
        light: false,
        accent: egui::Color32::from_rgb(0x60, 0xCD, 0xFF),
        mica: false,
    };

    /// The current Windows theme and accent color. `AGENT_BURN_APP_THEME`
    /// (`light` or `dark`) overrides the theme, to check both looks.
    pub fn from_system(mica: bool) -> Self {
        let light = match std::env::var(THEME_ENV).as_deref() {
            Ok("light") => true,
            Ok("dark") => false,
            _ => crate::windows_theme::apps_use_light_theme(),
        };
        let accent = crate::windows_theme::accent_shade(light).map_or_else(
            || {
                if light {
                    egui::Color32::from_rgb(0x00, 0x5F, 0xB8)
                } else {
                    egui::Color32::from_rgb(0x60, 0xCD, 0xFF)
                }
            },
            |[r, g, b]| egui::Color32::from_rgb(r, g, b),
        );
        Self {
            light,
            accent,
            mica,
        }
    }
}

const fn translucent(r: u8, g: u8, b: u8, a: u8) -> egui::Color32 {
    let r = ((r as u16 * a as u16 + 127) / 255) as u8;
    let g = ((g as u16 * a as u16 + 127) / 255) as u8;
    let b = ((b as u16 * a as u16 + 127) / 255) as u8;
    egui::Color32::from_rgba_premultiplied(r, g, b, a)
}

fn fluent_palette(fluent: Fluent) -> Palette {
    let accent = fluent.accent;
    if fluent.light {
        Palette {
            accent,
            menu_surface: egui::Color32::from_rgb(0xF3, 0xF3, 0xF3),
            muted: egui::Color32::from_rgb(0x5D, 0x5D, 0x5D),
            selected_menu_fill: translucent(0, 0, 0, 10),
            success: egui::Color32::from_rgb(0x0F, 0x7B, 0x0F),
            caution: egui::Color32::from_rgb(0x9D, 0x5D, 0x00),
            danger: egui::Color32::from_rgb(0xC4, 0x2B, 0x1C),
            toggle_inactive: egui::Color32::from_rgb(0x86, 0x86, 0x86),
            toggle_inactive_hover: egui::Color32::from_rgb(0x6E, 0x6E, 0x6E),
            toggle_knob: egui::Color32::WHITE,
            toggle_label: egui::Color32::from_rgb(0x1A, 0x1A, 0x1A),
            section_surface: translucent(255, 255, 255, 179),
            section_border: translucent(0, 0, 0, 15),
            track: translucent(0, 0, 0, 24),
            menu_hover: translucent(0, 0, 0, 6),
            menu_text: egui::Color32::from_rgb(0x1A, 0x1A, 0x1A),
            focus: egui::Color32::from_rgb(0x1A, 0x1A, 0x1A),
        }
    } else {
        Palette {
            accent,
            menu_surface: egui::Color32::from_rgb(0x20, 0x20, 0x20),
            muted: egui::Color32::from_rgb(0xC5, 0xC5, 0xC5),
            selected_menu_fill: translucent(255, 255, 255, 15),
            success: egui::Color32::from_rgb(0x6C, 0xCB, 0x5F),
            caution: egui::Color32::from_rgb(0xFC, 0xE1, 0x00),
            danger: egui::Color32::from_rgb(0xFF, 0x99, 0xA4),
            toggle_inactive: egui::Color32::from_rgb(0x9A, 0x9A, 0x9A),
            toggle_inactive_hover: egui::Color32::from_rgb(0xB5, 0xB5, 0xB5),
            toggle_knob: egui::Color32::from_rgb(0x1F, 0x1F, 0x1F),
            toggle_label: egui::Color32::WHITE,
            section_surface: translucent(255, 255, 255, 13),
            section_border: translucent(0, 0, 0, 40),
            track: translucent(255, 255, 255, 28),
            menu_hover: translucent(255, 255, 255, 9),
            menu_text: egui::Color32::WHITE,
            focus: egui::Color32::WHITE,
        }
    }
}

/// Installs the visuals of `fluent`. Fonts are installed once, by
/// [`install_fonts`].
pub fn configure_fluent_style(context: &egui::Context, fluent: Fluent) {
    PALETTE.with(|current| current.set(fluent_palette(fluent)));
    let theme = if fluent.light {
        egui::Theme::Light
    } else {
        egui::Theme::Dark
    };
    context.set_theme(theme);
    let mut visuals = if fluent.light {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    let text = menu_text();
    // Mica shows through wherever nothing is painted.
    visuals.panel_fill = if fluent.mica {
        egui::Color32::TRANSPARENT
    } else {
        menu_surface()
    };
    visuals.window_fill = if fluent.light {
        egui::Color32::from_rgb(0xF9, 0xF9, 0xF9)
    } else {
        egui::Color32::from_rgb(0x2C, 0x2C, 0x2C)
    };
    visuals.window_stroke = egui::Stroke::new(1.0, section_border());
    let (rest, hover, press) = if fluent.light {
        (
            translucent(255, 255, 255, 179),
            translucent(249, 249, 249, 128),
            translucent(249, 249, 249, 77),
        )
    } else {
        (
            translucent(255, 255, 255, 15),
            translucent(255, 255, 255, 21),
            translucent(255, 255, 255, 8),
        )
    };
    let border = egui::Stroke::new(
        1.0,
        if fluent.light {
            translucent(0, 0, 0, 20)
        } else {
            translucent(255, 255, 255, 18)
        },
    );
    for (state, fill) in [
        (&mut visuals.widgets.inactive, rest),
        (&mut visuals.widgets.hovered, hover),
        (&mut visuals.widgets.active, press),
        (&mut visuals.widgets.open, press),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = border;
        state.fg_stroke = egui::Stroke::new(1.0, text);
    }
    // egui draws a focused widget with the `active` style: give it the
    // keyboard focus ring.
    visuals.widgets.active.bg_stroke = focus_stroke();
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, text);
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, section_border());
    visuals.selection.bg_fill = fluent.accent.gamma_multiply(0.6);
    visuals.selection.stroke = egui::Stroke::new(1.0, text);
    visuals.hyperlink_color = fluent.accent;
    visuals.faint_bg_color = rest;
    visuals.extreme_bg_color = if fluent.light {
        egui::Color32::WHITE
    } else {
        egui::Color32::from_rgb(0x1C, 0x1C, 0x1C)
    };
    visuals.error_fg_color = danger();
    visuals.warn_fg_color = caution();
    round_controls(&mut visuals);
    context.set_visuals_of(theme, visuals);
    set_spacing(context, theme);
}

fn round_controls(visuals: &mut egui::Visuals) {
    let radius = egui::CornerRadius::same(CONTROL_CORNER_RADIUS);
    visuals.widgets.inactive.corner_radius = radius;
    visuals.widgets.hovered.corner_radius = radius;
    visuals.widgets.active.corner_radius = radius;
    visuals.widgets.open.corner_radius = radius;
    visuals.menu_corner_radius = egui::CornerRadius::same(DROPDOWN_CORNER_RADIUS);
}

fn set_spacing(context: &egui::Context, theme: egui::Theme) {
    let mut style = (*context.style_of(theme)).clone();
    style.spacing.item_spacing = egui::vec2(9.0, 8.0);
    // Keep intrinsic button content below CONTROL_HEIGHT so interact_size can
    // define one exact visual height for every button.
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = CONTROL_HEIGHT;
    style.spacing.indent = 16.0;
    context.set_style_of(theme, style);
}

/// Segoe UI, Segoe UI Semibold and Consolas from Windows, with egui's own
/// fonts behind them for any missing glyph.
pub fn install_fonts(context: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let defaults = |fonts: &egui::FontDefinitions, family: egui::FontFamily| {
        fonts.families.get(&family).cloned().unwrap_or_default()
    };
    let proportional_defaults = defaults(&fonts, egui::FontFamily::Proportional);
    let monospace_defaults = defaults(&fonts, egui::FontFamily::Monospace);

    let mut proportional = Vec::new();
    if load_windows_font(&mut fonts, "segoe-ui", "segoeui.ttf") {
        proportional.push("segoe-ui".to_string());
    }
    proportional.extend(proportional_defaults);
    let mut semibold = Vec::new();
    if load_windows_font(&mut fonts, "segoe-ui-semibold", "seguisb.ttf") {
        semibold.push("segoe-ui-semibold".to_string());
    }
    semibold.extend(proportional.iter().cloned());
    let mut monospace = Vec::new();
    if load_windows_font(&mut fonts, "consolas", "consola.ttf") {
        monospace.push("consolas".to_string());
    }
    monospace.extend(monospace_defaults);

    fonts
        .families
        .insert(egui::FontFamily::Proportional, proportional);
    fonts
        .families
        .insert(egui::FontFamily::Name(SEMIBOLD.into()), semibold);
    fonts
        .families
        .insert(egui::FontFamily::Monospace, monospace);
    context.set_fonts(fonts);
}

fn load_windows_font(fonts: &mut egui::FontDefinitions, name: &str, file_name: &str) -> bool {
    let windows_directory = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let Ok(bytes) = std::fs::read(windows_directory.join("Fonts").join(file_name)) else {
        return false;
    };
    fonts
        .font_data
        .insert(name.into(), egui::FontData::from_owned(bytes).into());
    true
}

/// The 2-point outline of a control that has the keyboard focus.
pub fn focus_stroke() -> egui::Stroke {
    egui::Stroke::new(2.0, palette().focus)
}

pub fn accent() -> egui::Color32 {
    palette().accent
}

pub fn menu_surface() -> egui::Color32 {
    palette().menu_surface
}

pub fn muted() -> egui::Color32 {
    palette().muted
}

pub fn selected_menu_fill() -> egui::Color32 {
    palette().selected_menu_fill
}

pub fn success() -> egui::Color32 {
    palette().success
}

pub fn caution() -> egui::Color32 {
    palette().caution
}

pub fn danger() -> egui::Color32 {
    palette().danger
}

pub fn toggle_inactive() -> egui::Color32 {
    palette().toggle_inactive
}

pub fn toggle_inactive_hover() -> egui::Color32 {
    palette().toggle_inactive_hover
}

pub fn toggle_knob() -> egui::Color32 {
    palette().toggle_knob
}

pub fn toggle_label() -> egui::Color32 {
    palette().toggle_label
}

pub fn section_surface() -> egui::Color32 {
    palette().section_surface
}

pub fn section_border() -> egui::Color32 {
    palette().section_border
}

/// The empty part of a gauge or a bar.
pub fn track() -> egui::Color32 {
    palette().track
}

pub fn menu_hover() -> egui::Color32 {
    palette().menu_hover
}

pub fn menu_text() -> egui::Color32 {
    palette().menu_text
}

/// The palette in use is light.
pub fn is_light() -> bool {
    palette().menu_text != egui::Color32::WHITE
}
