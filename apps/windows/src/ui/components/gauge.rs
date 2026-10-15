//! A quota meter. It only fills for a known reading: an unknown share draws
//! an empty track with "Unknown", a reached limit says so in words, and an
//! old reading is drawn muted. Color never carries the state alone.

use eframe::egui;

use crate::ui::theme::{accent, caution, danger, menu_text, muted, track};

/// Share at which a meter turns to the caution color.
pub const NEAR_LIMIT: f64 = 80.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GaugeState {
    Unknown,
    Normal,
    NearLimit,
    Exhausted,
}

impl GaugeState {
    pub fn of(used_percent: Option<f64>) -> Self {
        match used_percent {
            None => Self::Unknown,
            Some(used) if !used.is_finite() => Self::Unknown,
            Some(used) if used >= 100.0 => Self::Exhausted,
            Some(used) if used >= NEAR_LIMIT => Self::NearLimit,
            Some(_) => Self::Normal,
        }
    }

    /// Words that carry the state without color.
    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::Unknown => Some("Unknown"),
            Self::Normal => None,
            Self::NearLimit => Some("Near limit"),
            Self::Exhausted => Some("Limit reached"),
        }
    }
}

/// Draws the meter bar: `used_percent` filled, and a tick at
/// `elapsed_percent`, the share of the window already past.
pub fn bar(
    ui: &mut egui::Ui,
    used_percent: Option<f64>,
    elapsed_percent: Option<f64>,
    stale: bool,
) -> egui::Response {
    let height = 8.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height + 6.0),
        egui::Sense::hover(),
    );
    let bar = egui::Rect::from_min_size(
        egui::pos2(rect.left(), rect.center().y - height / 2.0),
        egui::vec2(rect.width(), height),
    );
    let painter = ui.painter();
    painter.rect_filled(bar, height / 2.0, track());
    let state = GaugeState::of(used_percent);
    if let Some(used) = used_percent.filter(|_| state != GaugeState::Unknown) {
        let fill = if stale {
            muted()
        } else {
            match state {
                GaugeState::Exhausted => danger(),
                GaugeState::NearLimit => caution(),
                _ => accent(),
            }
        };
        let width = bar.width() * (used.clamp(0.0, 100.0) / 100.0) as f32;
        if width > 0.0 {
            let filled = egui::Rect::from_min_size(bar.min, egui::vec2(width.max(height), height));
            painter.rect_filled(filled, height / 2.0, fill);
        }
    }
    if let Some(elapsed) = elapsed_percent.filter(|elapsed| elapsed.is_finite()) {
        let x = bar.left() + bar.width() * (elapsed.clamp(0.0, 100.0) / 100.0) as f32;
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(2.0, menu_text()),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_readings_fill_the_meter() {
        assert_eq!(GaugeState::of(None), GaugeState::Unknown);
        assert_eq!(GaugeState::of(Some(f64::NAN)), GaugeState::Unknown);
        assert_eq!(GaugeState::of(Some(0.0)), GaugeState::Normal);
        assert_eq!(GaugeState::of(Some(79.9)), GaugeState::Normal);
        assert_eq!(GaugeState::of(Some(80.0)), GaugeState::NearLimit);
        assert_eq!(GaugeState::of(Some(100.0)), GaugeState::Exhausted);
        assert_eq!(GaugeState::of(Some(130.0)), GaugeState::Exhausted);
        assert_eq!(GaugeState::Exhausted.label(), Some("Limit reached"));
    }
}
