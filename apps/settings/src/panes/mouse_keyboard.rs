//! Mouse & Keyboard pane — pointer behaviour and keyboard layout.
//!
//! The toggles and sliders carry CRDT keys so the surface is interactive;
//! the pointer-speed and key-repeat values would drive the compositor's
//! libinput configuration in a full deployment. That binding isn't routed
//! through the shared `os-actions` dispatcher yet, so changes here update
//! the CRDT (and would be picked up by the compositor) but the panel
//! documents the seam rather than shelling out directly.

use ui::widget::{Column, Dropdown, Slider, Toggle, Widget};

use crate::widgets::{card, caption, rows, setting_row, setting_row_described};

pub const POINTER_SPEED_KEY: &str = "input.pointer_speed";
pub const NATURAL_SCROLL_KEY: &str = "input.natural_scroll";
pub const TAP_CLICK_KEY: &str = "input.tap_to_click";
pub const KEY_REPEAT_KEY: &str = "input.key_repeat";
pub const LAYOUT_KEY: &str = "input.keyboard_layout";

pub fn build() -> Box<dyn Widget> {
    let mouse = card(
        "Pointer",
        rows(vec![
            setting_row(
                "Pointer speed",
                Box::new(
                    Slider::new("settings.input.pointer_speed", POINTER_SPEED_KEY)
                        .with_id("input.pointer_speed"),
                ),
            ),
            setting_row_described(
                "Natural scrolling",
                "Content tracks finger movement",
                Box::new(
                    Toggle::new("settings.input.natural_scroll", NATURAL_SCROLL_KEY)
                        .with_id("input.natural_scroll"),
                ),
            ),
            setting_row_described(
                "Tap to click",
                "Tap the touchpad to register a click",
                Box::new(
                    Toggle::new("settings.input.tap_click", TAP_CLICK_KEY)
                        .with_id("input.tap_click"),
                ),
            ),
        ]),
    );

    let layouts = vec![
        "English (US)".to_string(),
        "English (UK)".to_string(),
        "German".to_string(),
        "French".to_string(),
        "Dutch".to_string(),
        "Spanish".to_string(),
    ];
    let keyboard = card(
        "Keyboard",
        rows(vec![
            setting_row(
                "Layout",
                Box::new(
                    Dropdown::new(layouts, LAYOUT_KEY)
                        .with_id("input.layout")
                        .with_placeholder("English (US)"),
                ),
            ),
            setting_row(
                "Key repeat speed",
                Box::new(
                    Slider::new("settings.input.key_repeat", KEY_REPEAT_KEY)
                        .with_id("input.key_repeat"),
                ),
            ),
            caption(
                "Layout and repeat rate apply to the compositor's input \
                 stack on the next session.",
            ),
        ]),
    );

    Box::new(
        Column::new()
            .with_intrinsic_height()
            .push(mouse)
            .push(keyboard),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    #[test]
    fn builds_layout() {
        let mut pane = build();
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }
}
