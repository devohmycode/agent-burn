//! A compact on/off switch. Adapted from Taskbar Hub's
//! `src/ui/components/toggle.rs` (Apache-2.0, see NOTICE), with a keyboard
//! focus ring: Tab reaches it, Space or Enter flips it.

use eframe::egui;

use crate::ui::{
    theme::{
        accent, focus_stroke, toggle_inactive, toggle_inactive_hover, toggle_knob, toggle_label,
    },
    tokens::{CONTROL_HEIGHT, TOGGLE_WIDTH},
};

pub struct Toggle<'a> {
    value: &'a mut bool,
    enabled_label: &'a str,
    disabled_label: &'a str,
}

impl<'a> Toggle<'a> {
    pub fn new(value: &'a mut bool) -> Self {
        Self {
            value,
            enabled_label: "On",
            disabled_label: "Off",
        }
    }

    pub fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let (rect, mut response) = ui.allocate_exact_size(
            egui::vec2(TOGGLE_WIDTH, CONTROL_HEIGHT),
            egui::Sense::click(),
        );
        if response.clicked() {
            *self.value = !*self.value;
            response.mark_changed();
        }
        let value = *self.value;
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, value, "")
        });
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        let switch_rect = egui::Rect::from_center_size(
            egui::pos2(rect.right() - 20.0, rect.center().y),
            egui::vec2(40.0, 22.0),
        );
        let track = if value {
            accent()
        } else if response.hovered() {
            toggle_inactive_hover()
        } else {
            toggle_inactive()
        };
        let painter = ui.painter();
        painter.rect_filled(switch_rect, 11.0, track);
        if response.has_focus() {
            painter.rect_stroke(
                switch_rect.expand(3.0),
                14.0,
                focus_stroke(),
                egui::StrokeKind::Outside,
            );
        }
        let knob_x = if value {
            switch_rect.right() - 11.0
        } else {
            switch_rect.left() + 11.0
        };
        painter.circle_filled(
            egui::pos2(knob_x, switch_rect.center().y),
            8.0,
            toggle_knob(),
        );
        painter.text(
            egui::pos2(switch_rect.left() - 8.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            if value {
                self.enabled_label
            } else {
                self.disabled_label
            },
            egui::FontId::proportional(14.0),
            toggle_label(),
        );
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_reserves_one_fixed_height_row() {
        let context = egui::Context::default();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            let mut value = false;
            let top = ui.cursor().top();
            Toggle::new(&mut value).show(ui);
            let consumed_height = ui.cursor().top() - top - ui.spacing().item_spacing.y;

            assert_eq!(consumed_height, CONTROL_HEIGHT);
        });
        output.textures_delta.clear();
    }
}
