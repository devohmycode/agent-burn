//! Daily stacked bars: one column per day, one segment per series. A day
//! without a recorded row keeps its place on the axis and draws nothing.

use eframe::egui;

use crate::ui::theme::{menu_text, muted, section_border, track};

pub struct Series {
    pub name: String,
    pub color: egui::Color32,
}

pub struct Segment {
    /// Index in the series list.
    pub series: usize,
    pub value: f64,
}

pub struct Column {
    /// Short label, e.g. `Oct 13`.
    pub label: String,
    /// Tooltip title, e.g. `2026-10-13`.
    pub title: String,
    /// Bottom to top, in series order.
    pub segments: Vec<Segment>,
    /// The engine reported this day.
    pub recorded: bool,
}

const HEIGHT: f32 = 220.0;
const AXIS_WIDTH: f32 = 64.0;
const LABEL_HEIGHT: f32 = 20.0;
const GAP: f32 = 2.0;

/// The smallest 1, 2, 2.5 or 5 × 10ⁿ at or above `value`.
pub fn nice_ceiling(value: f64) -> f64 {
    if !(value.is_finite() && value > 0.0) {
        return 1.0;
    }
    let magnitude = 10f64.powf(value.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|step| step * magnitude)
        .find(|candidate| *candidate >= value * (1.0 - 1e-9))
        .unwrap_or(10.0 * magnitude)
}

pub fn legend(ui: &mut egui::Ui, series: &[Series]) {
    if series.len() < 2 {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for entry in series {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, entry.color);
            ui.label(egui::RichText::new(&entry.name).color(menu_text()));
            ui.add_space(8.0);
        }
    });
}

pub fn stacked_bars(
    ui: &mut egui::Ui,
    id: egui::Id,
    series: &[Series],
    columns: &[Column],
    value_text: impl Fn(f64) -> String,
) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, HEIGHT), egui::Sense::hover());
    let plot = egui::Rect::from_min_max(
        egui::pos2(rect.left() + AXIS_WIDTH, rect.top() + 8.0),
        egui::pos2(rect.right(), rect.bottom() - LABEL_HEIGHT),
    );
    let highest = columns
        .iter()
        .map(|column| {
            column
                .segments
                .iter()
                .map(|segment| segment.value)
                .sum::<f64>()
        })
        .fold(0.0, f64::max);
    let ceiling = nice_ceiling(highest);
    let painter = ui.painter_at(rect);
    let text_color = muted();
    let small = egui::FontId::proportional(11.0);

    for step in 0..=2 {
        let value = ceiling * f64::from(step) / 2.0;
        let y = plot.bottom() - plot.height() * (step as f32 / 2.0);
        let stroke = if step == 0 {
            egui::Stroke::new(1.0, section_border())
        } else {
            egui::Stroke::new(1.0, track())
        };
        painter.line_segment(
            [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
            stroke,
        );
        painter.text(
            egui::pos2(plot.left() - 8.0, y),
            egui::Align2::RIGHT_CENTER,
            value_text(value),
            small.clone(),
            text_color,
        );
    }
    if columns.is_empty() {
        return;
    }

    let step = plot.width() / columns.len() as f32;
    let bar_width = (step * 0.6).clamp(2.0, 18.0);
    let label_every = (70.0 / step).ceil().max(1.0) as usize;
    for (index, column) in columns.iter().enumerate() {
        let center = plot.left() + step * (index as f32 + 0.5);
        let mut bottom = plot.bottom();
        let drawn: Vec<&Segment> = column
            .segments
            .iter()
            .filter(|segment| segment.value > 0.0)
            .collect();
        for (position, segment) in drawn.iter().enumerate() {
            let height = plot.height() * (segment.value / ceiling) as f32;
            let top = bottom - height;
            let is_top = position + 1 == drawn.len();
            // A surface gap separates stacked segments.
            let visible_bottom = if position == 0 { bottom } else { bottom - GAP };
            if visible_bottom - top >= 0.5 {
                let radius = if is_top {
                    (bar_width / 2.0).min(4.0) as u8
                } else {
                    0
                };
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(center - bar_width / 2.0, top),
                        egui::pos2(center + bar_width / 2.0, visible_bottom),
                    ),
                    egui::CornerRadius {
                        nw: radius,
                        ne: radius,
                        sw: 0,
                        se: 0,
                    },
                    series[segment.series].color,
                );
            }
            bottom = top;
        }
        let last = columns.len() - 1;
        if index == last || (index % label_every == 0 && last - index >= label_every) {
            painter.text(
                egui::pos2(center, plot.bottom() + 4.0),
                egui::Align2::CENTER_TOP,
                &column.label,
                small.clone(),
                text_color,
            );
        }
        let hit = egui::Rect::from_min_max(
            egui::pos2(center - step / 2.0, plot.top()),
            egui::pos2(center + step / 2.0, plot.bottom()),
        );
        ui.interact(hit, id.with(index), egui::Sense::hover())
            .on_hover_ui(|ui| tooltip(ui, series, column, &value_text));
    }
}

fn tooltip(
    ui: &mut egui::Ui,
    series: &[Series],
    column: &Column,
    value_text: &impl Fn(f64) -> String,
) {
    ui.label(egui::RichText::new(&column.title).strong());
    if !column.recorded {
        ui.label("No usage recorded");
        return;
    }
    let total: f64 = column.segments.iter().map(|segment| segment.value).sum();
    ui.label(format!("Total: {}", value_text(total)));
    for segment in column.segments.iter().rev() {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 2.0, series[segment.series].color);
            ui.label(format!(
                "{}: {}",
                series[segment.series].name,
                value_text(segment.value)
            ));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_axis_ends_on_a_round_value_at_or_above_the_highest_bar() {
        assert_eq!(nice_ceiling(0.0), 1.0);
        assert_eq!(nice_ceiling(0.37), 0.5);
        assert_eq!(nice_ceiling(1.0), 1.0);
        assert_eq!(nice_ceiling(2.2), 2.5);
        assert_eq!(nice_ceiling(812.0), 1000.0);
        assert_eq!(nice_ceiling(1340.0), 2000.0);
    }
}
