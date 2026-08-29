//! App-local widgets + small composition helpers.
//!
//! `ui` owns the canonical toolkit; these are Tube-specific primitives we
//! either can't express with the stock widgets (a `SponsorBlock` seek bar, a
//! gradient thumbnail, vector icons) or want clickable in our own
//! [`ActionBus`] dialect (`Flex`, `Switch`). The free helpers at the bottom
//! cut the boilerplate of wrapping `ui::Text` and building spacers.

pub mod channel_avatar;
pub mod controls;
pub mod flex;
pub mod image_box;
pub mod player_host;
pub mod seek_bar;
pub mod thumbnail;
pub mod video_surface;

pub use channel_avatar::ChannelAvatar;
pub use controls::{Icon, IconKind, Switch};
pub use flex::{Align, Axis, Flex, IntoElement, Justify, Len, Surface};
pub use image_box::ImageBox;
pub use player_host::{PlayerHost, SharedPlayer};
pub use seek_bar::SeekBar;
pub use thumbnail::Thumbnail;
pub use video_surface::VideoSurface;

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
        radius: f64::from(theme.tokens.radii.lg),
        border: Some((1.0, theme.border_standard)),
    }
}

/// A subtle "chip"/pill surface.
pub fn chip_surface(theme: &Theme, active: bool) -> Surface {
    if active {
        Surface {
            fill: Some(theme.accent_primary),
            radius: f64::from(theme.tokens.radii.full),
            border: None,
        }
    } else {
        subtle_surface(theme)
    }
}

/// The colour of a *resting* subtle surface — icon buttons, inactive chips,
/// the small veneer panels that float on the base background.
///
/// The neutral ramp runs darkest→lightest, so the high stops (`[6]`..`[8]`,
/// slate-400..slate-200) are near-*white*. Using them as a fill on the dark
/// base painted a bright box that no dark-theme foreground could read on top of
/// (white-on-slate-200 ≈ 1.2:1). `[2]` is a genuine one-step-up surface in both
/// themes (slate-800 in dark, light-grey in light), keeping `text_primary` and
/// `text_dim` readable. Centralised here so every veneer stays theme-correct.
pub fn subtle_fill(theme: &Theme) -> Color {
    theme.tokens.colors.neutral.stops[2]
}

/// The hover/active step of a subtle surface — one ramp step toward the
/// foreground from [`subtle_fill`], so a hover is visible while text stays
/// readable.
pub fn hover_fill(theme: &Theme) -> Color {
    theme.tokens.colors.neutral.stops[3]
}

/// A resting subtle [`Surface`] with a full pill radius.
pub fn subtle_surface(theme: &Theme) -> Surface {
    Surface {
        fill: Some(subtle_fill(theme)),
        radius: f64::from(theme.tokens.radii.full),
        border: None,
    }
}

/// A hover [`Surface`] with a full pill radius.
pub fn hover_pill_surface(theme: &Theme) -> Surface {
    Surface {
        fill: Some(hover_fill(theme)),
        radius: f64::from(theme.tokens.radii.full),
        border: None,
    }
}
