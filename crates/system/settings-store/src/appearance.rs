//! Typed keys of the `appearance` section that more than one writer sets:
//! the Settings pane, the shell's appearance actions and the palette.
//!
//! The schedule and the theme documents keep their own schemas
//! ([`crate::day_schedule`], `ui::theme_layers`); these are the plain leaves.

use crate::Key;
use crate::day_schedule::{MODE_KEY, SECTION};

/// `"dark"`, `"light"` or [`crate::day_schedule::AUTOMATIC_MODE`]. Unset
/// leaves the polarity to the distribution's theme.
pub const MODE: Key<Option<String>> = Key::new(SECTION, MODE_KEY, || None);

/// High contrast in the current mode's polarity. Choosing a mode leaves it.
pub const HIGH_CONTRAST: Key<bool> = Key::new(SECTION, "high_contrast", || false);

/// The interface language as a BCP 47 tag (`de-DE`). Unset follows the
/// system locale.
pub const LANGUAGE: Key<Option<String>> = Key::new(SECTION, "language", || None);

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
            "appearance": {"mode": "dark", "high_contrast": true, "language": "de-DE"},
        }));
        assert_eq!(MODE.from_document(&document).as_deref(), Some("dark"));
        assert!(HIGH_CONTRAST.from_document(&document));
        assert_eq!(LANGUAGE.from_document(&document).as_deref(), Some("de-DE"));
        let empty = Map::new();
        assert_eq!(MODE.from_document(&empty), None);
        assert!(!HIGH_CONTRAST.from_document(&empty));
    }
}
