//! Draws the widget: one row per quota window of the chosen provider, at
//! the row positions of Taskbar Hub's widgets (Apache-2.0, see NOTICE): a
//! label, a segmented gauge of what is left and the text `62% · 2h`. The
//! text always says what the color says.

use super::{
    canvas::{Canvas, Rect},
    color::Rgba,
    text::{TextAlign, TextStyle},
    view::{RowState, RowView, WidgetView},
};
use crate::ui::series;

/// Logical size of the widget; the Windows 11 taskbar is 48 high.
pub const WIDTH: f64 = 134.0;
pub const HEIGHT: f64 = 46.0;
const LABEL_X: f64 = 6.0;
const LABEL_WIDTH: f64 = 18.0;
const GAUGE_X: f64 = 26.0;
const GAUGE_WIDTH: f64 = 39.0;
const GAUGE_HEIGHT: f64 = 9.0;
const SEGMENTS: u32 = 4;
const TEXT_X: f64 = 69.0;
pub const TEXT_WIDTH: f64 = WIDTH - TEXT_X - 2.0;
const TEXT_HEIGHT: f64 = 13.0;
pub const TEXT_SIZE: f64 = 12.0;
/// Left share at or under which a window is drawn in the warning color,
/// then in the alert color.
const LOW: f64 = 25.0;
const VERY_LOW: f64 = 10.0;
const CARD_RADIUS: f64 = 6.0;

/// Top of the text box of a row, among `count` rows (one or two).
fn text_y(index: usize, count: usize) -> f64 {
    match (count, index) {
        (1, _) => 17.0,
        (_, 0) => 9.0,
        _ => 25.0,
    }
}

/// Colors on a dark or a light taskbar.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub card: Rgba,
    pub outline: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub track: Rgba,
    pub warning: Rgba,
    pub alert: Rgba,
}

impl Palette {
    pub fn new(dark_taskbar: bool) -> Self {
        if dark_taskbar {
            Self {
                card: Rgba::hex(0x202020),
                outline: Rgba::hex(0x3A3A3A),
                text: Rgba::hex(0xFFFFFF),
                muted: Rgba::hex(0xA0A0A0),
                track: Rgba::hex(0x444444),
                warning: Rgba::hex(0xFBBF24),
                alert: Rgba::hex(0xF87171),
            }
        } else {
            Self {
                card: Rgba::hex(0xF3F3F3),
                outline: Rgba::hex(0xD0D0D0),
                text: Rgba::hex(0x1A1A1A),
                muted: Rgba::hex(0x5F5F5F),
                track: Rgba::hex(0xAAAAAA),
                warning: Rgba::hex(0xB45309),
                alert: Rgba::hex(0xB91C1C),
            }
        }
    }
}

/// The provider's color in the dashboard palette.
fn provider_color(view: &WidgetView, dark_taskbar: bool) -> Rgba {
    let color = series::color(
        series::slot(&view.provider.label().to_lowercase()),
        !dark_taskbar,
    );
    Rgba::rgb(color.r(), color.g(), color.b())
}

/// The widget at `scale` device pixels per logical pixel, `now_ms` deciding
/// the time left. A floating widget sits on a card of the taskbar's colors,
/// readable over any window.
pub fn draw(
    view: &WidgetView,
    dark_taskbar: bool,
    scale: f64,
    now_ms: i64,
    floating: bool,
) -> Canvas {
    let palette = Palette::new(dark_taskbar);
    let fill = provider_color(view, dark_taskbar);
    let mut canvas = Canvas::new(WIDTH, HEIGHT, scale);
    if floating {
        let card = Rect::new(0.0, 0.0, WIDTH, HEIGHT);
        canvas.fill_rounded(card, palette.card.with_alpha(245), CARD_RADIUS);
        canvas.stroke_rounded(card, palette.outline, CARD_RADIUS, 1.0);
    }
    let count = view.rows.len().clamp(1, 2);
    for (index, row) in view.rows.iter().take(2).enumerate() {
        let y = text_y(index, count);
        draw_row(&mut canvas, row, y, fill, &palette, now_ms);
    }
    canvas
}

fn draw_row(
    canvas: &mut Canvas,
    row: &RowView,
    y: f64,
    fill: Rgba,
    palette: &Palette,
    now_ms: i64,
) {
    let mut label = TextStyle::segoe(TEXT_SIZE, 400, palette.muted);
    label.align = TextAlign::Left;
    canvas.draw_text(
        Rect::new(LABEL_X, y, LABEL_WIDTH, TEXT_HEIGHT),
        row.label,
        &label,
    );

    let gauge = Rect::new(GAUGE_X, y + 2.0, GAUGE_WIDTH, GAUGE_HEIGHT);
    canvas.fill_rounded(gauge, palette.track, 1.5);
    let (color, text_color) = match row.remaining() {
        Some(left) if left <= VERY_LOW => (palette.alert, palette.alert),
        Some(left) if left <= LOW => (palette.warning, palette.warning),
        _ => (fill, palette.text),
    };
    if let Some(left) = row.remaining() {
        canvas.progress(gauge, color, 1.5, left / 100.0, SEGMENTS, 1.0);
    }
    let text_color = match &row.state {
        RowState::Read { stale: true, .. } | RowState::Reset { .. } | RowState::Unknown => {
            if row.failure.is_some() && matches!(row.state, RowState::Unknown) {
                palette.alert
            } else {
                palette.muted
            }
        }
        RowState::Read { .. } => text_color,
    };
    canvas.draw_text(
        Rect::new(TEXT_X, y, TEXT_WIDTH, TEXT_HEIGHT),
        &row.text(now_ms),
        &TextStyle::segoe(TEXT_SIZE, 500, text_color),
    );
}

#[cfg(test)]
pub mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{
        quota::{CLAUDE, CLAUDE_SESSION, QuotaHistory, QuotaReading, QuotaWindow},
        settings::Provider,
        widget::{color::is_premultiplied, text::measure_text, view},
    };

    pub const NOW: i64 = 1_791_979_200_000;

    pub fn claude(session_used: f64, week_used: f64) -> WidgetView {
        let reading = |series: &str, used: f64, minutes: f64, elapsed: f64| QuotaReading {
            series: series.into(),
            observed_at_ms: NOW - 60_000,
            window: QuotaWindow {
                window_minutes: minutes,
                used_percent: used,
                elapsed_percent: elapsed,
                api_equivalent_spent: 0.0,
            },
        };
        let mut readings = BTreeMap::new();
        readings.insert(
            CLAUDE_SESSION.to_string(),
            vec![reading(CLAUDE_SESSION, session_used, 300.0, 30.0)],
        );
        readings.insert(
            CLAUDE.to_string(),
            vec![reading(CLAUDE, week_used, 10_080.0, 55.0)],
        );
        view::view(
            &QuotaHistory {
                readings,
                failures: BTreeMap::new(),
            },
            Provider::Claude,
            NOW,
        )
    }

    #[test]
    fn every_scale_draws_valid_premultiplied_pixels() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for dark in [true, false] {
                let canvas = draw(&claude(40.0, 95.0), dark, scale, NOW, false);
                assert_eq!(canvas.width(), (WIDTH * scale).round() as u32);
                assert!(canvas.pixels().iter().all(|pixel| is_premultiplied(*pixel)));
                assert!(canvas.pixels().iter().any(|pixel| *pixel >> 24 == 0xFF));
            }
        }
    }

    #[test]
    fn a_floating_widget_draws_an_opaque_card() {
        let canvas = draw(&claude(40.0, 25.0), false, 1.25, NOW, true);
        let pixels = canvas.pixels();
        let middle = pixels[(canvas.height() / 2 * canvas.width() + 3) as usize];
        assert!(middle >> 24 >= 240, "{middle:08x}");
        assert!(pixels.iter().all(|pixel| is_premultiplied(*pixel)));
    }

    #[test]
    fn the_longest_texts_fit_without_being_cut() {
        let style = TextStyle::segoe(TEXT_SIZE, 500, Rgba::rgb(0, 0, 0));
        for text in ["100% · 59m", "100% · 23h", "100% · 6d", "reset", "--", "!"] {
            let width = measure_text(text, &style);
            assert!(width <= TEXT_WIDTH, "{text}: {width} > {TEXT_WIDTH}");
        }
        let label = TextStyle::segoe(TEXT_SIZE, 400, Rgba::rgb(0, 0, 0));
        for text in ["5h", "7d", "$"] {
            assert!(measure_text(text, &label) <= LABEL_WIDTH, "{text}");
        }
    }

    #[test]
    fn a_low_quota_takes_the_alert_color_and_says_its_value() {
        let view = claude(95.0, 20.0);
        assert_eq!(view.rows[0].text(NOW), "5% · 3h");
        let canvas = draw(&view, true, 1.0, NOW, false);
        let palette = Palette::new(true);
        let alert = 0xFF00_0000
            | (u32::from(palette.alert.r) << 16)
            | (u32::from(palette.alert.g) << 8)
            | u32::from(palette.alert.b);
        // The first segment of the session gauge is filled in the alert color.
        let (x, y) = (GAUGE_X as u32, (text_y(0, 2) + 2.0 + 4.0) as u32);
        assert_eq!(canvas.pixels()[(y * canvas.width() + x) as usize], alert);
    }
}
