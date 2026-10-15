//! One color per agent, wherever it appears. Each agent owns a fixed slot of
//! a categorical palette validated for color-vision deficiencies on both
//! dashboard surfaces (light `#fbfbfb`, dark `#2b2b2b`); agents past the eighth
//! share the neutral "Other" color. A color never follows an agent's rank.

use eframe::egui::Color32;

/// Slot order: adjacent slots are the validated neighbors in a stack.
const AGENTS: [&str; 8] = [
    "codex", "claude", "cursor", "opencode", "droid", "gemini", "copilot", "amp",
];
const LIGHT: [u32; 8] = [
    0x2a78d6, 0xeb6834, 0x1baf7a, 0xeda100, 0xe87ba4, 0x008300, 0x4a3aa7, 0xe34948,
];
const DARK: [u32; 8] = [
    0x3987e5, 0xd95926, 0x199e70, 0xc98500, 0xd55181, 0x008300, 0x9085e9, 0xe66767,
];
const OTHER_LIGHT: u32 = 0x8f8e89;
const OTHER_DARK: u32 = 0x8a8984;

/// The slot of `agent`, `None` for "Other".
pub fn slot(agent: &str) -> Option<usize> {
    AGENTS.iter().position(|known| *known == agent)
}

/// Stack and legend order: slots first, "Other" last.
pub fn order(agent: &str) -> usize {
    slot(agent).unwrap_or(AGENTS.len())
}

pub fn color(slot: Option<usize>, light: bool) -> Color32 {
    let rgb = match (slot, light) {
        (Some(slot), true) => LIGHT[slot],
        (Some(slot), false) => DARK[slot],
        (None, true) => OTHER_LIGHT,
        (None, false) => OTHER_DARK,
    };
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// How the dashboard names an agent.
pub fn name(agent: &str) -> String {
    match agent {
        "claude" => "Claude".into(),
        "codex" => "Codex".into(),
        "cursor" => "Cursor".into(),
        "opencode" => "OpenCode".into(),
        "openclaw" => "OpenClaw".into(),
        "droid" => "Droid".into(),
        "gemini" => "Gemini".into(),
        "copilot" => "Copilot".into(),
        "amp" => "Amp".into(),
        "qwen" => "Qwen".into(),
        "kimi" => "Kimi".into(),
        "kilo" => "Kilo".into(),
        "goose" => "Goose".into(),
        "hermes" => "Hermes".into(),
        "codebuff" => "Codebuff".into(),
        "pi" => "Pi".into(),
        other => {
            let mut chars = other.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_keeps_its_color_whatever_else_is_shown() {
        assert_eq!(slot("claude"), Some(1));
        assert_eq!(
            color(slot("claude"), true),
            Color32::from_rgb(0xeb, 0x68, 0x34)
        );
        assert_eq!(slot("hermes"), None);
        assert!(order("hermes") > order("amp"));
    }
}
