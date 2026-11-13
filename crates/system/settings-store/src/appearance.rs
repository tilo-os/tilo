//! Typed keys of the `appearance` section that more than one writer sets:
//! the Settings pane, the shell's appearance actions and the palette.
//!
//! The schedule and the theme documents keep their own schemas
//! ([`crate::day_schedule`], `ui::theme_layers`); these are the plain leaves.

use crate::Key;
use crate::day_schedule::{MODE_KEY, SECTION};

pub const DEVICE_SECTION: &str = "appearance_device";

pub const MODE: Key<Option<String>> = Key::new(SECTION, MODE_KEY, || None);

pub const HIGH_CONTRAST: Key<bool> = Key::new(SECTION, "high_contrast", || false);

pub const TEXT_SCALE_MIN: f32 = 0.5;
pub const TEXT_SCALE_MAX: f32 = 4.0;

/// The desktop's wallpaper: the id of a picture the image ships
/// (`tilo-wallpapers`' catalog), or `"custom"` for the person's own picture,
/// which the compositor keeps per device. Unset shows the default picture.
pub const WALLPAPER: Key<Option<String>> = Key::new(SECTION, "wallpaper", || None);

/// The interface language, named as its language pack is (`de`; see
/// [`tilo_paths::language_of`]). Unset follows the device's locale.
pub const LANGUAGE: Key<Option<String>> = Key::new(SECTION, "language", || None);

/// The interface language: the one the person chose ([`LANGUAGE`]), else the
/// locale the device runs in (`LC_ALL`, `LC_MESSAGES`, `LANGUAGE`, `LANG`, read
/// as POSIX).
///
/// Every surface resolves its language — and the language of the dates it
/// formats — from this one answer, so a person who chose English on a German
/// machine reads English throughout: the system locale is only the default,
/// never a second source.
#[must_use]
pub fn preferred_language() -> Option<String> {
    let chosen = LANGUAGE.get();
    let system = tilo_process::environment::environment().locale.clone();
    preferred_language_from(chosen.as_deref(), system.as_deref())
}

/// [`preferred_language`] over an explicit choice and system locale. A value
/// that names no language (`C`, `POSIX`, malformed) falls back to the next
/// source; `None` means neither named one.
#[must_use]
pub fn preferred_language_from(chosen: Option<&str>, system: Option<&str>) -> Option<String> {
    [chosen, system].into_iter().flatten().find_map(|value| {
        tilo_process::environment::parse_posix_locale(value).map(|locale| locale.to_string())
    })
}

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

pub const TRANSLUCENCY_MIN: f32 = 0.0;
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
                "accent": "violet",
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
        assert_eq!(ACCENT.from_document(&document).as_deref(), Some("violet"));
        assert_eq!(TRANSLUCENCY.from_document(&document), Some(0.4));
        let empty = Map::new();
        assert_eq!(MODE.from_document(&empty), None);
        assert!(!HIGH_CONTRAST.from_document(&empty));
        assert_eq!(WALLPAPER.from_document(&empty), None);
        assert_eq!(UI_FACE.from_document(&empty), None);
        assert_eq!(ACCENT.from_document(&empty), None);
        assert_eq!(TRANSLUCENCY.from_document(&empty), None);
    }

    #[test]
    fn the_chosen_language_wins_and_the_system_locale_is_only_the_default() {
        assert_eq!(
            preferred_language_from(Some("en-US"), Some("de_DE.UTF-8")).as_deref(),
            Some("en-US")
        );
        assert_eq!(
            preferred_language_from(Some("de_DE.UTF-8"), None).as_deref(),
            Some("de-DE")
        );
        assert_eq!(
            preferred_language_from(None, Some("de_DE.UTF-8")).as_deref(),
            Some("de-DE"),
            "with no choice the device's locale is the default"
        );
        assert_eq!(
            preferred_language_from(Some("C"), Some("fr_FR.UTF-8")).as_deref(),
            Some("fr-FR"),
            "an explicit C names no language, so the default decides"
        );
        assert_eq!(preferred_language_from(Some("C"), Some("C.UTF-8")), None);
        assert_eq!(preferred_language_from(Some("not a locale!"), None), None);
        assert_eq!(preferred_language_from(None, None), None);
    }
}
