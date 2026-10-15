//! The quota history chart: the remaining share as recorded, held from one
//! reading to the next and dashed across a gap without readings, the window
//! resets, the even pace and the forecast when there is one. Nothing is
//! drawn between readings that the readings do not say.

use eframe::egui::{self, Color32, Pos2, Stroke};

use crate::{
    dashboard::clock,
    forecast::{Forecast, Reset},
    quota::QuotaReading,
    ui::theme::{danger, menu_text, muted, section_border},
};

const HEIGHT: f32 = 170.0;
const AXIS_WIDTH: f32 = 40.0;
const LABEL_HEIGHT: f32 = 18.0;

/// A straight piece of the recorded line, from and to `(time, remaining)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    pub from: (i64, f64),
    pub to: (i64, f64),
    /// Across a gap without readings: the value is the last one known.
    pub gap: bool,
}

/// The recorded line between `from_ms` and `to_ms`: each reading's remaining
/// share holds until the next reading, then steps to it. The last reading
/// before `from_ms` starts the line at `from_ms` when it belongs to the same
/// window as the next one. A stretch longer than `gap_ms`, or across a
/// window change, has no reading saying what happened: it is a gap.
pub fn recorded_pieces(
    readings: &[QuotaReading],
    from_ms: i64,
    to_ms: i64,
    gap_ms: i64,
) -> Vec<Piece> {
    let start = match readings
        .iter()
        .rposition(|reading| reading.observed_at_ms <= from_ms)
    {
        Some(index)
            if readings
                .get(index + 1)
                .is_some_and(|next| !next.same_window(&readings[index])) =>
        {
            index + 1
        }
        Some(index) => index,
        None => 0,
    };
    let shown: Vec<&QuotaReading> = readings[start..]
        .iter()
        .take_while(|reading| reading.observed_at_ms <= to_ms)
        .collect();
    let mut pieces = Vec::new();
    for pair in shown.windows(2) {
        let (before, after) = (pair[0], pair[1]);
        let (t0, t1) = (before.observed_at_ms.max(from_ms), after.observed_at_ms);
        let (r0, r1) = (
            100.0 - before.window.used_percent,
            100.0 - after.window.used_percent,
        );
        pieces.push(Piece {
            from: (t0, r0),
            to: (t1, r0),
            gap: t1 - before.observed_at_ms > gap_ms || !after.same_window(before),
        });
        if (r1 - r0).abs() > f64::EPSILON {
            pieces.push(Piece {
                from: (t1, r0),
                to: (t1, r1),
                gap: false,
            });
        }
    }
    pieces
}

pub struct Chart<'a> {
    /// Consistent readings, oldest first.
    pub readings: &'a [QuotaReading],
    pub resets: &'a [Reset],
    pub from_ms: i64,
    pub to_ms: i64,
    /// The window to draw the even pace across: start and reset.
    pub pace: Option<(i64, i64)>,
    pub forecast: Option<&'a Forecast>,
    pub now_ms: i64,
    pub color: Color32,
    /// Readings further apart than this are a gap.
    pub gap_ms: i64,
}

pub fn show(ui: &mut egui::Ui, chart: &Chart) {
    let width = ui.available_width().max(200.0);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, HEIGHT + LABEL_HEIGHT),
        egui::Sense::hover(),
    );
    let plot = egui::Rect::from_min_max(
        egui::pos2(rect.left() + AXIS_WIDTH, rect.top() + 6.0),
        egui::pos2(rect.right() - 8.0, rect.top() + HEIGHT),
    );
    let span = (chart.to_ms - chart.from_ms).max(1) as f64;
    let x = |time: i64| plot.left() + ((time - chart.from_ms) as f64 / span) as f32 * plot.width();
    let y = |remaining: f64| {
        plot.bottom() - (remaining.clamp(0.0, 100.0) / 100.0) as f32 * plot.height()
    };
    let point = |(time, remaining): (i64, f64)| egui::pos2(x(time), y(remaining));
    let painter = ui.painter_at(rect);
    let small = egui::FontId::proportional(11.0);

    for share in [0.0, 50.0, 100.0] {
        let level = y(share);
        painter.line_segment(
            [
                egui::pos2(plot.left(), level),
                egui::pos2(plot.right(), level),
            ],
            Stroke::new(if share == 0.0 { 1.0 } else { 0.5 }, section_border()),
        );
        painter.text(
            egui::pos2(plot.left() - 6.0, level),
            egui::Align2::RIGHT_CENTER,
            format!("{share:.0}%"),
            small.clone(),
            muted(),
        );
    }
    for (time, align) in [
        (chart.from_ms, egui::Align2::LEFT_TOP),
        (chart.to_ms, egui::Align2::RIGHT_TOP),
    ] {
        if let Some(at) = clock::from_millis(time) {
            painter.text(
                egui::pos2(x(time), plot.bottom() + 4.0),
                align,
                clock::local_date_time(at),
                small.clone(),
                muted(),
            );
        }
    }

    // The lock-out: after the forecast runs out, before the reset.
    if let Some(forecast) = chart.forecast
        && let Some(runs_out) = forecast.runs_out_ms
    {
        let band = egui::Rect::from_min_max(
            egui::pos2(x(runs_out).max(plot.left()), plot.top()),
            egui::pos2(x(forecast.reset_ms).min(plot.right()), plot.bottom()),
        );
        if band.width() > 0.0 {
            painter.rect_filled(band, 0.0, danger().gamma_multiply(0.12));
            painter.text(
                band.center_top() + egui::vec2(0.0, 4.0),
                egui::Align2::CENTER_TOP,
                "Runs out",
                small.clone(),
                danger(),
            );
        }
    }
    if let Some((start, reset)) = chart.pace {
        dashed(
            &painter,
            point((start, 100.0)),
            point((reset, 0.0)),
            muted(),
            1.0,
        );
    }
    for reset in chart.resets {
        if (chart.from_ms..=chart.to_ms).contains(&reset.at_ms) {
            let at = x(reset.at_ms);
            dashed(
                &painter,
                egui::pos2(at, plot.top()),
                egui::pos2(at, plot.bottom()),
                muted(),
                1.0,
            );
            painter.text(
                egui::pos2(at + 3.0, plot.top()),
                egui::Align2::LEFT_TOP,
                if reset.scheduled {
                    "reset"
                } else {
                    "early reset"
                },
                small.clone(),
                muted(),
            );
        }
    }
    if (chart.from_ms..=chart.to_ms).contains(&chart.now_ms) {
        let at = x(chart.now_ms);
        painter.line_segment(
            [egui::pos2(at, plot.top()), egui::pos2(at, plot.bottom())],
            Stroke::new(1.0, menu_text().gamma_multiply(0.35)),
        );
    }
    for piece in recorded_pieces(chart.readings, chart.from_ms, chart.to_ms, chart.gap_ms) {
        if piece.gap {
            dashed(
                &painter,
                point(piece.from),
                point(piece.to),
                chart.color.gamma_multiply(0.6),
                1.5,
            );
        } else {
            painter.line_segment(
                [point(piece.from), point(piece.to)],
                Stroke::new(2.0, chart.color),
            );
        }
    }
    if let Some(forecast) = chart.forecast {
        let latest = &forecast.latest;
        let from = (latest.observed_at_ms, 100.0 - latest.window.used_percent);
        let to = match forecast.runs_out_ms {
            Some(runs_out) => (runs_out, 0.0),
            None => (forecast.reset_ms, forecast.remaining_at_reset()),
        };
        let color = if forecast.runs_out_ms.is_some() {
            danger()
        } else {
            chart.color
        };
        dashed(&painter, point(from), point(to), color, 2.0);
        painter.circle_stroke(point(to), 3.5, Stroke::new(1.5, color));
    }
    // The last reading, marked.
    if let Some(last) = chart
        .readings
        .iter()
        .rev()
        .find(|reading| (chart.from_ms..=chart.to_ms).contains(&reading.observed_at_ms))
    {
        painter.circle_filled(
            point((last.observed_at_ms, 100.0 - last.window.used_percent)),
            3.0,
            chart.color,
        );
    }

    if let Some(pointer) = response
        .hover_pos()
        .filter(|pointer| plot.contains(*pointer))
    {
        let time =
            chart.from_ms + (((pointer.x - plot.left()) / plot.width()) as f64 * span) as i64;
        let nearest = chart
            .readings
            .iter()
            .filter(|reading| (chart.from_ms..=chart.to_ms).contains(&reading.observed_at_ms))
            .min_by_key(|reading| (reading.observed_at_ms - time).abs());
        if let Some(reading) = nearest {
            let at = point((reading.observed_at_ms, 100.0 - reading.window.used_percent));
            painter.circle_stroke(at, 4.5, Stroke::new(1.5, menu_text()));
            let text = format!(
                "{}\n{:.0}% left ({:.0}% used)",
                clock::local_date_time(reading.observed_at()),
                100.0 - reading.window.used_percent,
                reading.window.used_percent
            );
            response.clone().on_hover_ui_at_pointer(|ui| {
                ui.label(text);
            });
        }
    }
}

fn dashed(painter: &egui::Painter, from: Pos2, to: Pos2, color: Color32, width: f32) {
    painter.add(egui::Shape::dashed_line(
        &[from, to],
        Stroke::new(width, color),
        4.0,
        4.0,
    ));
}

/// A swatch and a name, for the legend under a chart.
pub fn legend(ui: &mut egui::Ui, color: Color32, forecast: bool) {
    ui.horizontal_wrapped(|ui| {
        let swatch = |ui: &mut egui::Ui, color: Color32, dashed_line: bool, text: &str| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 10.0), egui::Sense::hover());
            let (from, to) = (rect.left_center(), rect.right_center());
            if dashed_line {
                dashed(ui.painter(), from, to, color, 1.5);
            } else {
                ui.painter()
                    .line_segment([from, to], Stroke::new(2.0, color));
            }
            ui.label(egui::RichText::new(text).color(muted()).size(12.0));
            ui.add_space(6.0);
        };
        swatch(ui, color, false, "Remaining, as recorded");
        swatch(ui, color.gamma_multiply(0.6), true, "No reading");
        swatch(ui, muted(), true, "Even pace");
        if forecast {
            swatch(ui, color, true, "Forecast");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::QuotaWindow;

    const MINUTE: i64 = 60_000;

    /// A reading `minute` minutes in, of a week that started 3.5 days before.
    fn reading(minute: i64, used: f64) -> QuotaReading {
        QuotaReading {
            series: "codex".into(),
            observed_at_ms: minute * MINUTE,
            window: QuotaWindow {
                window_minutes: 10_080.0,
                used_percent: used,
                elapsed_percent: (minute + 5_040) as f64 / 10_080.0 * 100.0,
                api_equivalent_spent: 0.0,
            },
        }
    }

    #[test]
    fn readings_hold_then_step_and_a_gap_is_marked() {
        let readings = [reading(0, 10.0), reading(5, 12.0), reading(65, 20.0)];

        let pieces = recorded_pieces(&readings, 0, 100 * MINUTE, 30 * MINUTE);

        assert_eq!(
            pieces,
            [
                Piece {
                    from: (0, 90.0),
                    to: (5 * MINUTE, 90.0),
                    gap: false
                },
                Piece {
                    from: (5 * MINUTE, 90.0),
                    to: (5 * MINUTE, 88.0),
                    gap: false
                },
                Piece {
                    from: (5 * MINUTE, 88.0),
                    to: (65 * MINUTE, 88.0),
                    gap: true
                },
                Piece {
                    from: (65 * MINUTE, 88.0),
                    to: (65 * MINUTE, 80.0),
                    gap: false
                },
            ]
        );
    }

    #[test]
    fn a_window_change_is_not_bridged_by_the_old_value() {
        let mut new_window = reading(30, 1.0);
        new_window.window.elapsed_percent = 0.1;
        let readings = [reading(0, 80.0), reading(20, 82.0), new_window];

        // From the new window's start, the old window's value is not carried.
        let pieces = recorded_pieces(&readings, 25 * MINUTE, 60 * MINUTE, 30 * MINUTE);
        assert!(pieces.is_empty(), "{pieces:?}");
        // Across the change, the hold is a gap.
        let pieces = recorded_pieces(&readings, 0, 60 * MINUTE, 30 * MINUTE);
        assert!(pieces[2].gap && pieces[2].to.0 == 30 * MINUTE, "{pieces:?}");
    }

    #[test]
    fn the_line_starts_from_the_last_reading_before_the_range_and_stops_at_the_last_one() {
        let readings = [
            reading(0, 10.0),
            reading(50, 30.0),
            reading(70, 40.0),
            reading(200, 60.0),
        ];

        let pieces = recorded_pieces(&readings, 60 * MINUTE, 120 * MINUTE, 30 * MINUTE);

        // 50 starts the line at 60; nothing after 70 inside the range.
        assert_eq!(pieces.first().unwrap().from, (60 * MINUTE, 70.0));
        assert_eq!(pieces.last().unwrap().to, (70 * MINUTE, 60.0));
        assert!(recorded_pieces(&readings[..1], 0, MINUTE, MINUTE).is_empty());
    }
}
