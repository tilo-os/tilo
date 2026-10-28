//! Curated keyboard-layout dataset for the installer's layout picker.
//!
//! Each [`Layout`] carries two things:
//!
//!   * The **apply** data — the xkb layout/variant the installed system
//!     should use (`xkb_layout`, `xkb_variant`) plus a human
//!     `display_name` shown in the picker `Dropdown`.
//!   * The **visual** data — the glyphs each physical key produces under
//!     this layout, so [`crate::keyboard_view::KeyboardView`] can draw a
//!     live preview of exactly what the selected layout types.
//!
//! The visual rows model the four main blocks of an ANSI/ISO keyboard, in
//! US physical-key order:
//!
//!   * row 0 — number row: `1 2 3 4 5 6 7 8 9 0 - =`            (12 keys)
//!   * row 1 — top letter row: `q w e r t y u i o p [ ]`        (12 keys)
//!   * row 2 — home row: `a s d f g h j k l ; '`                (11 keys)
//!   * row 3 — bottom row: `z x c v b n m , . /`                (10 keys)
//!
//! For every layout the entries are the *output* glyphs at those physical
//! positions (so AZERTY's top row reads `a z e r t y …`, QWERTZ swaps
//! y/z and adds `ü ö ä`, Dvorak reorders entirely, …). `number_shift`
//! carries the shifted glyphs for the number row, where they aren't simply
//! the uppercase of the base (e.g. `1`→`!`, AZERTY `&`→`1`); the preview
//! shows those in a corner of each number key.
//!
//! The dataset is static Rust data — no I/O — so the picker and the
//! integrity tests are deterministic.

/// Expected key counts for the four visual rows, in order.
pub const ROW_LENS: [usize; 4] = [12, 12, 11, 10];

/// A single keyboard layout: apply metadata + visual key glyphs.
pub struct Layout {
    /// Unique, stable id (e.g. `"us"`, `"de"`, `"dvorak"`).
    pub id: &'static str,
    /// xkb layout passed to the installed system (e.g. `"us"`, `"de"`).
    pub xkb_layout: &'static str,
    /// Optional xkb variant (e.g. Dvorak = layout `"us"` variant
    /// `"dvorak"`); `None` for the plain layout.
    pub xkb_variant: Option<&'static str>,
    /// Human-readable name shown in the picker (and in Review).
    pub display_name: &'static str,
    /// Base-level glyphs for the four main rows, in US physical order.
    /// Lengths must match [`ROW_LENS`].
    pub rows: [&'static [&'static str]; 4],
    /// Shifted glyphs for the number row (row 0); length 12.
    pub number_shift: &'static [&'static str],
}

impl Layout {
    /// The xkb variant or the empty string (never `None`) — convenient
    /// for argv plumbing where empty means "no variant".
    pub fn variant_or_empty(&self) -> &'static str {
        self.xkb_variant.unwrap_or("")
    }
}

/// The display name of the default layout (English / US).
pub const DEFAULT_DISPLAY_NAME: &str = "English (US)";

/// The curated set, in display order. English (US) is first / default.
pub static LAYOUTS: &[Layout] = &[
    Layout {
        id: "us",
        xkb_layout: "us",
        xkb_variant: None,
        display_name: "English (US)",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-", "="],
            &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "[", "]"],
            &["a", "s", "d", "f", "g", "h", "j", "k", "l", ";", "'"],
            &["z", "x", "c", "v", "b", "n", "m", ",", ".", "/"],
        ],
        number_shift: &["!", "@", "#", "$", "%", "^", "&", "*", "(", ")", "_", "+"],
    },
    Layout {
        id: "gb",
        xkb_layout: "gb",
        xkb_variant: None,
        display_name: "English (UK)",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-", "="],
            &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "[", "]"],
            &["a", "s", "d", "f", "g", "h", "j", "k", "l", ";", "'"],
            &["z", "x", "c", "v", "b", "n", "m", ",", ".", "/"],
        ],
        number_shift: &["!", "\"", "£", "$", "%", "^", "&", "*", "(", ")", "_", "+"],
    },
    Layout {
        id: "de",
        xkb_layout: "de",
        xkb_variant: None,
        display_name: "German (QWERTZ)",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "ß", "´"],
            &["q", "w", "e", "r", "t", "z", "u", "i", "o", "p", "ü", "+"],
            &["a", "s", "d", "f", "g", "h", "j", "k", "l", "ö", "ä"],
            &["y", "x", "c", "v", "b", "n", "m", ",", ".", "-"],
        ],
        number_shift: &["!", "\"", "§", "$", "%", "&", "/", "(", ")", "=", "?", "`"],
    },
    Layout {
        id: "fr",
        xkb_layout: "fr",
        xkb_variant: None,
        display_name: "French (AZERTY)",
        rows: [
            &["&", "é", "\"", "'", "(", "-", "è", "_", "ç", "à", ")", "="],
            &["a", "z", "e", "r", "t", "y", "u", "i", "o", "p", "^", "$"],
            &["q", "s", "d", "f", "g", "h", "j", "k", "l", "m", "ù"],
            &["w", "x", "c", "v", "b", "n", ",", ";", ":", "!"],
        ],
        number_shift: &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "°", "+"],
    },
    Layout {
        id: "es",
        xkb_layout: "es",
        xkb_variant: None,
        display_name: "Spanish",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "'", "¡"],
            &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "`", "+"],
            &["a", "s", "d", "f", "g", "h", "j", "k", "l", "ñ", "´"],
            &["z", "x", "c", "v", "b", "n", "m", ",", ".", "-"],
        ],
        number_shift: &["!", "\"", "·", "$", "%", "&", "/", "(", ")", "=", "?", "¿"],
    },
    Layout {
        id: "it",
        xkb_layout: "it",
        xkb_variant: None,
        display_name: "Italian",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "'", "ì"],
            &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "è", "+"],
            &["a", "s", "d", "f", "g", "h", "j", "k", "l", "ò", "à"],
            &["z", "x", "c", "v", "b", "n", "m", ",", ".", "-"],
        ],
        number_shift: &["!", "\"", "£", "$", "%", "&", "/", "(", ")", "=", "?", "^"],
    },
    Layout {
        id: "pt",
        xkb_layout: "pt",
        xkb_variant: None,
        display_name: "Portuguese",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "'", "«"],
            &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "+", "´"],
            &["a", "s", "d", "f", "g", "h", "j", "k", "l", "ç", "º"],
            &["z", "x", "c", "v", "b", "n", "m", ",", ".", "-"],
        ],
        number_shift: &["!", "\"", "#", "$", "%", "&", "/", "(", ")", "=", "?", "»"],
    },
    Layout {
        id: "br",
        xkb_layout: "br",
        xkb_variant: None,
        display_name: "Portuguese (Brazil)",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-", "="],
            &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "´", "["],
            &["a", "s", "d", "f", "g", "h", "j", "k", "l", "ç", "~"],
            &["z", "x", "c", "v", "b", "n", "m", ",", ".", ";"],
        ],
        number_shift: &["!", "@", "#", "$", "%", "¨", "&", "*", "(", ")", "_", "+"],
    },
    Layout {
        id: "ru",
        xkb_layout: "ru",
        xkb_variant: None,
        display_name: "Russian (ЙЦУКЕН)",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-", "="],
            &["й", "ц", "у", "к", "е", "н", "г", "ш", "щ", "з", "х", "ъ"],
            &["ф", "ы", "в", "а", "п", "р", "о", "л", "д", "ж", "э"],
            &["я", "ч", "с", "м", "и", "т", "ь", "б", "ю", "."],
        ],
        number_shift: &["!", "\"", "№", ";", "%", ":", "?", "*", "(", ")", "_", "+"],
    },
    Layout {
        id: "se",
        xkb_layout: "se",
        xkb_variant: None,
        display_name: "Swedish",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "+", "´"],
            &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "å", "¨"],
            &["a", "s", "d", "f", "g", "h", "j", "k", "l", "ö", "ä"],
            &["z", "x", "c", "v", "b", "n", "m", ",", ".", "-"],
        ],
        number_shift: &["!", "\"", "#", "$", "%", "&", "/", "(", ")", "=", "?", "`"],
    },
    Layout {
        id: "no",
        xkb_layout: "no",
        xkb_variant: None,
        display_name: "Norwegian",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "+", "\\"],
            &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "å", "¨"],
            &["a", "s", "d", "f", "g", "h", "j", "k", "l", "ø", "æ"],
            &["z", "x", "c", "v", "b", "n", "m", ",", ".", "-"],
        ],
        number_shift: &["!", "\"", "#", "¤", "%", "&", "/", "(", ")", "=", "?", "`"],
    },
    Layout {
        id: "dk",
        xkb_layout: "dk",
        xkb_variant: None,
        display_name: "Danish",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "+", "´"],
            &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "å", "¨"],
            &["a", "s", "d", "f", "g", "h", "j", "k", "l", "æ", "ø"],
            &["z", "x", "c", "v", "b", "n", "m", ",", ".", "-"],
        ],
        number_shift: &["!", "\"", "#", "¤", "%", "&", "/", "(", ")", "=", "?", "`"],
    },
    Layout {
        id: "ch",
        xkb_layout: "ch",
        xkb_variant: None,
        display_name: "Swiss German",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "'", "^"],
            &["q", "w", "e", "r", "t", "z", "u", "i", "o", "p", "ü", "¨"],
            &["a", "s", "d", "f", "g", "h", "j", "k", "l", "ö", "ä"],
            &["y", "x", "c", "v", "b", "n", "m", ",", ".", "-"],
        ],
        number_shift: &["+", "\"", "*", "ç", "%", "&", "/", "(", ")", "=", "?", "`"],
    },
    Layout {
        id: "pl",
        xkb_layout: "pl",
        xkb_variant: None,
        display_name: "Polish",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-", "="],
            &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "[", "]"],
            &["a", "s", "d", "f", "g", "h", "j", "k", "l", ";", "'"],
            &["z", "x", "c", "v", "b", "n", "m", ",", ".", "/"],
        ],
        number_shift: &["!", "@", "#", "$", "%", "^", "&", "*", "(", ")", "_", "+"],
    },
    Layout {
        id: "dvorak",
        xkb_layout: "us",
        xkb_variant: Some("dvorak"),
        display_name: "Dvorak",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "[", "]"],
            &["'", ",", ".", "p", "y", "f", "g", "c", "r", "l", "/", "="],
            &["a", "o", "e", "u", "i", "d", "h", "t", "n", "s", "-"],
            &[";", "q", "j", "k", "x", "b", "m", "w", "v", "z"],
        ],
        number_shift: &["!", "@", "#", "$", "%", "^", "&", "*", "(", ")", "{", "}"],
    },
    Layout {
        id: "colemak",
        xkb_layout: "us",
        xkb_variant: Some("colemak"),
        display_name: "Colemak",
        rows: [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-", "="],
            &["q", "w", "f", "p", "g", "j", "l", "u", "y", ";", "[", "]"],
            &["a", "r", "s", "t", "d", "h", "n", "e", "i", "o", "'"],
            &["z", "x", "c", "v", "b", "k", "m", ",", ".", "/"],
        ],
        number_shift: &["!", "@", "#", "$", "%", "^", "&", "*", "(", ")", "_", "+"],
    },
];

/// All display names, in picker order — fed to the layout `Dropdown`.
pub fn display_names() -> Vec<String> {
    LAYOUTS.iter().map(|l| l.display_name.to_string()).collect()
}

/// Look up a layout by its `display_name` (what the dropdown stores).
pub fn by_display_name(name: &str) -> Option<&'static Layout> {
    let name = name.trim();
    LAYOUTS.iter().find(|l| l.display_name == name)
}

/// Absolute fallback for [`default_layout`] if `LAYOUTS` were ever empty —
/// which it never is (`layout_dataset_is_complete_unique_and_well_formed` below asserts ~15-25
/// curated entries) — so the lookup can return a plain value instead of
/// indexing into or unwrapping data a future edit could in principle shrink
/// to zero.
static EMPTY_LAYOUT: Layout = Layout {
    id: "us",
    xkb_layout: "us",
    xkb_variant: None,
    display_name: DEFAULT_DISPLAY_NAME,
    rows: [&[], &[], &[], &[]],
    number_shift: &[],
};

/// The default layout (English / US). Always present.
pub fn default_layout() -> &'static Layout {
    by_display_name(DEFAULT_DISPLAY_NAME)
        .or_else(|| LAYOUTS.first())
        .unwrap_or(&EMPTY_LAYOUT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn layout_dataset_is_complete_unique_and_well_formed() {
        {
            assert!(
                LAYOUTS.len() >= 15,
                "curated set should have ~15-25 layouts, got {}",
                LAYOUTS.len()
            );
        }
        {
            let ids: Vec<_> = LAYOUTS.iter().map(|l| l.id).collect();
            assert_eq!(ids, install_profile_model::KEYBOARDS);
        }
        {
            for l in LAYOUTS {
                for (r, expected) in ROW_LENS.iter().enumerate() {
                    assert_eq!(
                        l.rows[r].len(),
                        *expected,
                        "{} row {} has {} keys, expected {}",
                        l.id,
                        r,
                        l.rows[r].len(),
                        expected
                    );
                }
                assert_eq!(
                    l.number_shift.len(),
                    ROW_LENS[0],
                    "{} number_shift has {} entries, expected {}",
                    l.id,
                    l.number_shift.len(),
                    ROW_LENS[0]
                );
            }
        }
        {
            let mut ids = HashSet::new();
            let mut names = HashSet::new();
            for l in LAYOUTS {
                assert!(ids.insert(l.id), "duplicate id {}", l.id);
                assert!(
                    names.insert(l.display_name),
                    "duplicate name {}",
                    l.display_name
                );
            }
        }
        {
            for l in LAYOUTS {
                assert!(!l.xkb_layout.is_empty(), "{} has empty xkb_layout", l.id);
                assert!(
                    !l.display_name.is_empty(),
                    "{} has empty display_name",
                    l.id
                );
                if let Some(v) = l.xkb_variant {
                    assert!(!v.is_empty(), "{} has empty (Some) xkb_variant", l.id);
                }
            }
        }
        {
            // The brief requires at least these.
            for id in ["us", "de", "fr", "gb", "es", "dvorak", "colemak"] {
                assert!(
                    LAYOUTS.iter().any(|l| l.id == id),
                    "required layout '{id}' missing"
                );
            }
        }
    }

    #[test]
    fn layout_lookup_default_and_variants_resolve_to_real_xkb_layouts() {
        {
            let d = default_layout();
            assert_eq!(d.id, "us");
            assert_eq!(d.xkb_layout, "us");
            assert!(d.xkb_variant.is_none());
            assert_eq!(d.display_name, DEFAULT_DISPLAY_NAME);
            assert_eq!(d.rows[1][0], "q", "US top row starts with q");
        }
        {
            for l in LAYOUTS {
                let got = by_display_name(l.display_name).expect("found");
                assert_eq!(got.id, l.id);
            }
            assert!(by_display_name("Klingon").is_none());
        }
        {
            // Dvorak / Colemak are us-variants, not their own xkb layout.
            let dv = by_display_name("Dvorak").unwrap();
            assert_eq!(dv.xkb_layout, "us");
            assert_eq!(dv.xkb_variant, Some("dvorak"));
            assert_eq!(dv.variant_or_empty(), "dvorak");
            let us = by_display_name("English (US)").unwrap();
            assert_eq!(us.variant_or_empty(), "");
        }
        {
            // Sanity: the visual data encodes the famous remaps.
            let de = by_display_name("German (QWERTZ)").unwrap();
            assert_eq!(
                de.rows[1][5], "z",
                "QWERTZ swaps y/z (position 6 of top row)"
            );
            assert_eq!(de.rows[3][0], "y", "QWERTZ bottom row starts with y");
            let fr = by_display_name("French (AZERTY)").unwrap();
            assert_eq!(fr.rows[1][0], "a", "AZERTY top row starts with a");
            assert_eq!(fr.rows[1][1], "z", "AZERTY second key is z");
            let dv = by_display_name("Dvorak").unwrap();
            assert_eq!(dv.rows[2][0], "a");
            assert_eq!(dv.rows[2][1], "o", "Dvorak home row: a o e u i …");
        }
    }
}
