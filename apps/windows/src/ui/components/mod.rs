pub mod bars;
pub mod fluent;
pub mod gauge;
pub mod quota_chart;
pub mod text_field;
pub mod toggle;

#[cfg(test)]
pub mod keyboard {
    //! Keyboard input fed straight to egui, without touching the system.

    use eframe::egui;

    pub fn key(key: egui::Key, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// One frame's input: the previous key released, then `next` pressed.
    pub fn frame(previous: Option<egui::Key>, next: Option<egui::Key>) -> egui::RawInput {
        let mut events = Vec::new();
        if let Some(previous) = previous {
            events.push(key(previous, false));
        }
        if let Some(next) = next {
            events.push(key(next, true));
        }
        egui::RawInput {
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1100.0, 900.0),
            )),
            ..Default::default()
        }
    }

    /// The keys pressed one per frame, after a first frame without input.
    pub fn sequence(keys: &[egui::Key]) -> Vec<egui::RawInput> {
        let mut frames = vec![frame(None, None)];
        let mut previous = None;
        for key in keys {
            frames.push(frame(previous, Some(*key)));
            previous = Some(*key);
        }
        frames.push(frame(previous, None));
        frames
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Key};

    use super::{fluent::navigation_item, keyboard::sequence, toggle::Toggle};

    #[test]
    fn tab_reaches_each_control_and_space_flips_the_toggle() {
        let context = egui::Context::default();
        let mut value = false;
        let mut navigated = false;
        let mut focus = Vec::new();
        for input in sequence(&[Key::Tab, Key::Tab, Key::Space]) {
            let mut output = context.run_ui(input, |ui| {
                let item = navigation_item(ui, false, "Quotas");
                navigated |= item.clicked();
                let toggle = Toggle::new(&mut value).show(ui);
                focus.push((item.has_focus(), toggle.has_focus()));
            });
            output.textures_delta.clear();
        }

        assert!(focus.contains(&(true, false)), "{focus:?}");
        assert!(focus.contains(&(false, true)), "{focus:?}");
        assert!(value);
        assert!(!navigated);
    }

    #[test]
    fn enter_opens_the_focused_navigation_item() {
        let context = egui::Context::default();
        let mut navigated = false;
        for input in sequence(&[Key::Tab, Key::Enter]) {
            let mut output = context.run_ui(input, |ui| {
                navigated |= navigation_item(ui, false, "Quotas").clicked();
            });
            output.textures_delta.clear();
        }

        assert!(navigated);
    }
}
