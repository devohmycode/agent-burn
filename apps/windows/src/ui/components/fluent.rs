//! Windows 11 (Fluent) building blocks of the dashboard: page title,
//! sections of cards, setting rows and navigation items. Adapted from Taskbar
//! Hub's `src/ui/components/fluent.rs` (Apache-2.0, see NOTICE); icons are
//! left out and every interactive part takes the keyboard focus.

use eframe::egui;

use crate::ui::theme::{
    SEMIBOLD, accent, focus_stroke, menu_hover, menu_text, muted, section_border, section_surface,
    selected_menu_fill,
};

pub const NAVIGATION_ITEM_HEIGHT: f32 = 36.0;
const CARD_RADIUS: u8 = 6;
const CARD_GAP: f32 = 4.0;
/// Room kept on the right of a setting row for its controls.
pub const CONTROL_WIDTH: f32 = 340.0;

pub fn semibold(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(SEMIBOLD.into()))
}

pub fn page_title(ui: &mut egui::Ui, title: &str) {
    ui.add_space(18.0);
    ui.horizontal(|ui| {
        ui.add_space(28.0);
        ui.label(
            egui::RichText::new(title)
                .font(semibold(28.0))
                .color(menu_text()),
        );
    });
    ui.add_space(14.0);
}

/// A titled group of cards.
pub fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new(title)
            .font(semibold(14.0))
            .color(menu_text()),
    );
    ui.add_space(4.0);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = CARD_GAP;
        body(ui);
    });
    ui.add_space(12.0);
}

/// A card holding free content.
pub fn card(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(section_surface())
        .stroke(egui::Stroke::new(1.0, section_border()))
        .corner_radius(CARD_RADIUS)
        .inner_margin(egui::Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
}

/// A setting row: a title, a detail line and the controls, right-aligned.
pub fn row(ui: &mut egui::Ui, title: &str, detail: &str, control: impl FnOnce(&mut egui::Ui)) {
    card(ui, |ui| {
        ui.set_min_height(40.0);
        ui.horizontal(|ui| {
            let total = ui.available_width();
            let controls = CONTROL_WIDTH.min(total * 0.55);
            let text_width = total - controls - ui.spacing().item_spacing.x;
            ui.allocate_ui_with_layout(
                egui::vec2(text_width.max(40.0), 40.0),
                egui::Layout::top_down(egui::Align::Min).with_main_justify(true),
                |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.add(
                        egui::Label::new(egui::RichText::new(title).size(14.0).color(menu_text()))
                            .truncate(),
                    );
                    if !detail.is_empty() {
                        ui.add(
                            egui::Label::new(egui::RichText::new(detail).size(12.0).color(muted()))
                                .wrap(),
                        );
                    }
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
        });
    });
}

/// A navigation item; the selected one carries the accent pill.
pub fn navigation_item(ui: &mut egui::Ui, selected: bool, title: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), NAVIGATION_ITEM_HEIGHT),
        egui::Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, title)
    });
    let painter = ui.painter();
    let fill = if selected {
        selected_menu_fill()
    } else if response.hovered() {
        menu_hover()
    } else {
        egui::Color32::TRANSPARENT
    };
    painter.rect_filled(rect, 4.0, fill);
    if response.has_focus() {
        painter.rect_stroke(
            rect.shrink(1.0),
            4.0,
            focus_stroke(),
            egui::StrokeKind::Inside,
        );
    }
    if selected {
        let pill = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 1.5, rect.center().y),
            egui::vec2(3.0, 16.0),
        );
        painter.rect_filled(pill, 1.5, accent());
    }
    painter
        .with_clip_rect(rect.shrink2(egui::vec2(8.0, 0.0)))
        .text(
            egui::pos2(rect.left() + 16.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            title,
            egui::FontId::proportional(14.0),
            menu_text(),
        );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A card with a drop-down list; returns true when the choice changed.
pub fn choice_row<T: PartialEq + Clone>(
    ui: &mut egui::Ui,
    title: &str,
    detail: &str,
    value: &mut T,
    choices: &[(T, &str)],
) -> bool {
    let mut changed = false;
    let selected = choices
        .iter()
        .find(|(choice, _)| choice == value)
        .map_or("", |(_, label)| *label);
    row(ui, title, detail, |ui| {
        egui::ComboBox::from_id_salt(title)
            .width(220.0)
            .selected_text(selected)
            .show_ui(ui, |ui| {
                for (choice, label) in choices {
                    changed |= ui.selectable_value(value, choice.clone(), *label).changed();
                }
            });
    });
    changed
}
