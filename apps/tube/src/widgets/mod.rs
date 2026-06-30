//! App-local widgets + small composition helpers.
//!
//! `ui` owns the canonical toolkit; these are Tube-specific primitives we
//! either can't express with the stock widgets (a SponsorBlock seek bar, a
//! gradient thumbnail, vector icons) or want clickable in our own
//! [`ActionBus`] dialect (`Flex`, `Switch`). The free helpers at the bottom
//! cut the boilerplate of wrapping `ui::Text` and building spacers.

pub mod controls;
pub mod flex;
pub mod seek_bar;
pub mod thumbnail;

pub use controls::{Icon, IconKind, Switch};
pub use flex::{Align, Axis, Flex, IntoElement, Justify, Len, Surface};
pub use seek_bar::SeekBar;
pub use thumbnail::Thumbnail;

use ui::theme::Theme;
use ui::widget::Text;
use vello::peniko::Color;

/// A line of text at `size` in the theme's primary colour.
pub fn text(s: impl AsRef<str>, size: f32) -> Text {
    Text::new(s.as_ref()).with_size(size)
}

/// A line of text in an explicit colour.
pub fn text_c(s: impl AsRef<str>, size: f32, color: Color) -> Text {
    Text::new(s.as_ref()).with_size(size).with_color(color)
}

/// Dim secondary text (channel names, metadata).
pub fn dim(theme: &Theme, s: impl AsRef<str>, size: f32) -> Text {
    text_c(s, size, theme.text_dim)
}

/// A fixed-size empty box, useful as a gap/spacer.
pub fn gap(w: f32, h: f32) -> Flex {
    Flex::row().fixed(w, h)
}

/// A flexible spacer that eats remaining space on the main axis.
pub fn spring() -> Flex {
    Flex::row().grow(1.0)
}

/// A 1px horizontal rule in the theme's border colour.
pub fn divider(theme: &Theme) -> Flex {
    Flex::row().fill_w().h(Len::Px(1.0)).surface(Surface {
        fill: Some(theme.border_standard),
        radius: 0.0,
        border: None,
    })
}

/// Card surface treatment used by panels across the app.
pub fn card_surface(theme: &Theme) -> Surface {
    Surface {
        fill: Some(theme.tokens.colors.surfaces.raised),
        radius: theme.tokens.radii.lg as f64,
        border: Some((1.0, theme.border_standard)),
    }
}

/// A subtle "chip"/pill surface.
pub fn chip_surface(theme: &Theme, active: bool) -> Surface {
    if active {
        Surface {
            fill: Some(theme.accent_primary),
            radius: theme.tokens.radii.full as f64,
            border: None,
        }
    } else {
        Surface {
            fill: Some(theme.tokens.colors.neutral.stops[7]),
            radius: theme.tokens.radii.full as f64,
            border: None,
        }
    }
}
