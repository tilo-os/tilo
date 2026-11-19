//! The calendar's colours as the window draws them: the palette place an item
//! picked, resolved through the theme the OS controls.

use calendar_model::model::{Palette, Priority};
use ui::theme::Theme;
use ui::tokens::colors::PresenceHue;
use vello::peniko::Color;

/// A colour an item picked, resolved on the window's theme.
pub(crate) trait Themed {
    fn color(self, theme: &Theme) -> Color;
}

impl Themed for Priority {
    fn color(self, theme: &Theme) -> Color {
        let c = &theme.tokens.colors;
        match self {
            Priority::None => c.text_dim,
            Priority::Low => c.semantic.info,
            Priority::Medium => c.semantic.warning,
            Priority::High => c.semantic.danger,
        }
    }
}

impl Themed for Palette {
    fn color(self, theme: &Theme) -> Color {
        let c = &theme.tokens.colors;
        let hue = match self {
            Palette::Indigo => PresenceHue::Blue,
            Palette::Emerald => PresenceHue::Green,
            Palette::Amber => PresenceHue::Amber,
            Palette::Red => PresenceHue::Red,
            Palette::Blue => PresenceHue::Sky,
            Palette::Violet => PresenceHue::Violet,
            Palette::Slate => return c.text_tertiary,
        };
        c.presence.hue(hue)
    }
}
