//! Typed keys of the `appearance` section that more than one writer sets:
//! the Settings pane, the shell's appearance actions and the palette.
//!
//! The schedule and the theme documents keep their own schemas
//! ([`crate::day_schedule`], `ui::theme_layers`); these are the plain leaves.

use crate::Key;
use crate::day_schedule::{MODE_KEY, SECTION};

/// This device's appearance: the defaults its OS image ships
/// (`ui::theme_layers::DISTRIBUTION_DEFAULTS_KEY`).
pub const DEVICE_SECTION: &str = "appearance_device";

/// `"dark"`, `"light"` or [`crate::day_schedule::AUTOMATIC_MODE`]. Unset
/// leaves the polarity to the distribution's theme.
pub const MODE: Key<Option<String>> = Key::new(SECTION, MODE_KEY, || None);

/// High contrast in the current mode's polarity. Choosing a mode leaves it.
pub const HIGH_CONTRAST: Key<bool> = Key::new(SECTION, "high_contrast", || false);

/// The band of type scales the theme applies (`appearance.text_scale`); a
/// stored scale outside it is clamped.
pub const TEXT_SCALE_MIN: f32 = 0.5;
pub const TEXT_SCALE_MAX: f32 = 4.0;

/// The desktop's wallpaper: the id of a picture the image ships
/// (`tilo-wallpapers`' catalog), or `"custom"` for the person's own picture,
/// which the compositor keeps per device. Unset shows the default picture.
pub const WALLPAPER: Key<Option<String>> = Key::new(SECTION, "wallpaper", || None);

/// The interface language as a BCP 47 tag (`de-DE`). Unset follows the
/// system locale.
pub const LANGUAGE: Key<Option<String>> = Key::new(SECTION, "language", || None);

/// The interface face the person chose, by any family name a theme's type
/// role may name (`Default`, `Hyperlegible`, `Noto Sans`). Unset follows the
/// theme's own face.
pub const UI_FACE: Key<Option<String>> = Key::new(SECTION, "ui_face", || None);

/// How much of the theme's translucency the person keeps: `1` as the theme
/// sets it, `0` floating chrome opaque. Unset follows the theme. The
/// accessibility switch that makes every panel opaque (`reduce_transparency`)
/// still wins over this.
pub const TRANSLUCENCY: Key<Option<f32>> = Key::new(SECTION, "translucency", || None);

/// The band a stored translucency is clamped to.
pub const TRANSLUCENCY_MIN: f32 = 0.0;
/// See [`TRANSLUCENCY_MIN`].
pub const TRANSLUCENCY_MAX: f32 = 1.0;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, Value, json};

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    #[test]
    fn appearance_keys_read_the_leaves_the_theme_reads() {
        let document = object(json!({
            "appearance": {
                "mode": "dark",
                "high_contrast": true,
                "language": "de-DE",
                "wallpaper": "custom",
                "ui_face": "Hyperlegible",
                "translucency": 0.4,
            },
        }));
        assert_eq!(MODE.from_document(&document).as_deref(), Some("dark"));
        assert!(HIGH_CONTRAST.from_document(&document));
        assert_eq!(LANGUAGE.from_document(&document).as_deref(), Some("de-DE"));
        assert_eq!(
            WALLPAPER.from_document(&document).as_deref(),
            Some("custom")
        );
        assert_eq!(
            UI_FACE.from_document(&document).as_deref(),
            Some("Hyperlegible")
        );
        assert_eq!(TRANSLUCENCY.from_document(&document), Some(0.4));
        let empty = Map::new();
        assert_eq!(MODE.from_document(&empty), None);
        assert!(!HIGH_CONTRAST.from_document(&empty));
        assert_eq!(WALLPAPER.from_document(&empty), None);
        assert_eq!(UI_FACE.from_document(&empty), None);
        assert_eq!(TRANSLUCENCY.from_document(&empty), None);
    }
}
