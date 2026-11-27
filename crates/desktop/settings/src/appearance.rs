//! Typed keys of the `appearance` section that more than one writer sets:
//! the Settings pane, the shell's appearance actions and the palette.
//!
//! The schedule and the theme documents keep their own schemas
//! ([`crate::day_schedule`], `ui::theme_layers`); these are the plain leaves.

use crate::Key;
use crate::day_schedule::{MODE_KEY, SECTION};

pub use settings_model::appearance::{
    DEVICE_SECTION, TEXT_SCALE_MAX, TEXT_SCALE_MIN, TRANSLUCENCY_MAX, TRANSLUCENCY_MIN,
    preferred_language,
};

pub const MODE: Key<Option<String>> = Key::new(SECTION, MODE_KEY, || None);

pub const HIGH_CONTRAST: Key<bool> = Key::new(SECTION, "high_contrast", || false);

/// The desktop's wallpaper: the id of a picture the image ships
/// (`tilo-wallpapers`' catalog), or `"custom"` for the person's own picture,
/// which the compositor keeps per device. Unset shows the default picture.
pub const WALLPAPER: Key<Option<String>> = Key::new(SECTION, "wallpaper", || None);

/// The interface language ([`settings_model::appearance::LANGUAGE`]).
pub const LANGUAGE: Key<Option<String>> =
    Key::new(SECTION, settings_model::appearance::LANGUAGE, || None);

/// The interface face the person chose, by any family name a theme's type
/// role may name (`Default`, `Hyperlegible`, `Noto Sans`). Unset follows the
/// theme's own face.
pub const UI_FACE: Key<Option<String>> = Key::new(SECTION, "ui_face", || None);

/// The accent the person chose, by any name the presence palette gives a hue
/// (`violet`, `teal`). Unset follows the theme's own accent. A high-contrast
/// theme keeps its colourless accent whatever this says.
pub const ACCENT: Key<Option<String>> = Key::new(SECTION, "accent", || None);

/// How much of the theme's translucency the person keeps: `1` as the theme
/// sets it, `0` floating chrome opaque. Unset follows the theme. The
/// accessibility switch that makes every panel opaque (`reduce_transparency`)
/// still wins over this.
pub const TRANSLUCENCY: Key<Option<f32>> = Key::new(SECTION, "translucency", || None);
