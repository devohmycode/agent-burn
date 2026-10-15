//! A single-line text field of the shared control height. Adapted from
//! Taskbar Hub's `src/ui/components/text_field.rs` (Apache-2.0, see NOTICE).

use eframe::egui;

use crate::ui::tokens::CONTROL_HEIGHT;

pub struct SinglelineField<'a> {
    widget: egui::TextEdit<'a>,
    desired_width: f32,
}

pub fn singleline(text: &mut dyn egui::TextBuffer) -> SinglelineField<'_> {
    SinglelineField {
        widget: egui::TextEdit::singleline(text)
            .desired_width(f32::INFINITY)
            .margin(egui::Margin::symmetric(8, 6)),
        desired_width: f32::INFINITY,
    }
}

impl SinglelineField<'_> {
    #[must_use]
    pub fn desired_width(mut self, width: f32) -> Self {
        self.desired_width = width;
        self.widget = self.widget.desired_width(width);
        self
    }

    #[must_use]
    pub fn hint_text(mut self, hint_text: impl Into<egui::WidgetText>) -> Self {
        self.widget = self.widget.hint_text(hint_text);
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> egui::text_edit::TextEditOutput {
        let width = self.desired_width.min(ui.available_width()).max(1.0);
        let layout = egui::Layout::centered_and_justified(ui.layout().main_dir());
        ui.allocate_ui_with_layout(egui::vec2(width, CONTROL_HEIGHT), layout, |ui| {
            self.widget.show(ui)
        })
        .inner
    }
}

impl egui::Widget for SinglelineField<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        self.show(ui).response.response
    }
}
