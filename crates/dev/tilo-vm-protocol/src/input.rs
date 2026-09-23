//! Input for headful slots: what `tilo-vm input` asks for, and how key
//! names and text map to Linux evdev key codes (US layout).
//!
//! The codes are the kernel's `input-event-codes.h`. They travel through a
//! per-seat uinput device, so the compositor sees them through the real
//! libinput path, exactly like a keyboard and a pointer plugged into that seat.

use serde::{Deserialize, Serialize};

/// One step of `tilo-vm input`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputAction {
    /// Press and release each combo in order, e.g. `ctrl+alt+t`, `Return`.
    Key { combos: Vec<String> },
    /// Type text as key strokes on a US layout.
    Type { text: String },
    /// Move the pointer to output pixel coordinates.
    Move { x: u32, y: u32 },
    /// Click a button, optionally after moving to `at`.
    Click {
        button: Button,
        #[serde(default)]
        at: Option<(u32, u32)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Right,
    Middle,
}

impl Button {
    pub fn code(self) -> u16 {
        match self {
            Self::Left => BTN_LEFT,
            Self::Right => BTN_RIGHT,
            Self::Middle => BTN_MIDDLE,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "left" | "1" => Some(Self::Left),
            "right" | "3" => Some(Self::Right),
            "middle" | "2" => Some(Self::Middle),
            _ => None,
        }
    }
}

pub const KEY_LEFTSHIFT: u16 = 42;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
/// The highest key code the virtual keyboard advertises.
pub const KEY_MAX_USED: u16 = 248;

/// Named keys, lowercase. Letters, digits and punctuation go through
/// [`char_stroke`] instead.
const NAMED: &[(&str, u16)] = &[
    ("esc", 1),
    ("escape", 1),
    ("minus", 12),
    ("equal", 13),
    ("backspace", 14),
    ("tab", 15),
    ("leftbrace", 26),
    ("bracketleft", 26),
    ("rightbrace", 27),
    ("bracketright", 27),
    ("enter", 28),
    ("return", 28),
    ("ctrl", 29),
    ("control", 29),
    ("leftctrl", 29),
    ("semicolon", 39),
    ("apostrophe", 40),
    ("grave", 41),
    ("shift", 42),
    ("leftshift", 42),
    ("backslash", 43),
    ("comma", 51),
    ("dot", 52),
    ("period", 52),
    ("slash", 53),
    ("rightshift", 54),
    ("alt", 56),
    ("leftalt", 56),
    ("space", 57),
    ("capslock", 58),
    ("f1", 59),
    ("f2", 60),
    ("f3", 61),
    ("f4", 62),
    ("f5", 63),
    ("f6", 64),
    ("f7", 65),
    ("f8", 66),
    ("f9", 67),
    ("f10", 68),
    ("f11", 87),
    ("f12", 88),
    ("rightctrl", 97),
    ("altgr", 100),
    ("rightalt", 100),
    ("home", 102),
    ("up", 103),
    ("pageup", 104),
    ("pgup", 104),
    ("left", 105),
    ("right", 106),
    ("end", 107),
    ("down", 108),
    ("pagedown", 109),
    ("pgdn", 109),
    ("insert", 110),
    ("delete", 111),
    ("del", 111),
    ("super", 125),
    ("meta", 125),
    ("logo", 125),
    ("win", 125),
    ("leftmeta", 125),
    ("rightmeta", 126),
    ("menu", 127),
    ("compose", 127),
];

const LETTERS: [u16; 26] = [
    30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45,
    21, 44,
];

/// Punctuation on a US layout: (unshifted, shifted, code).
const PUNCTUATION: &[(char, char, u16)] = &[
    ('1', '!', 2),
    ('2', '@', 3),
    ('3', '#', 4),
    ('4', '$', 5),
    ('5', '%', 6),
    ('6', '^', 7),
    ('7', '&', 8),
    ('8', '*', 9),
    ('9', '(', 10),
    ('0', ')', 11),
    ('-', '_', 12),
    ('=', '+', 13),
    ('[', '{', 26),
    (']', '}', 27),
    (';', ':', 39),
    ('\'', '"', 40),
    ('`', '~', 41),
    ('\\', '|', 43),
    (',', '<', 51),
    ('.', '>', 52),
    ('/', '?', 53),
];

/// One key stroke: the key and whether shift is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stroke {
    pub code: u16,
    pub shift: bool,
}

/// The stroke that types `c` on a US layout, or `None` for characters a
/// US keyboard cannot type directly.
pub fn char_stroke(c: char) -> Option<Stroke> {
    let plain = |code| Some(Stroke { code, shift: false });
    match c {
        'a'..='z' => plain(letter(c)?),
        'A'..='Z' => Some(Stroke {
            code: letter(c.to_ascii_lowercase())?,
            shift: true,
        }),
        ' ' => plain(57),
        '\n' => plain(28),
        '\t' => plain(15),
        _ => PUNCTUATION.iter().find_map(|(lower, upper, code)| {
            if c == *lower {
                plain(*code)
            } else if c == *upper {
                Some(Stroke {
                    code: *code,
                    shift: true,
                })
            } else {
                None
            }
        }),
    }
}

fn letter(c: char) -> Option<u16> {
    let index = u32::from(c).checked_sub(u32::from('a'))?;
    LETTERS.get(usize::try_from(index).ok()?).copied()
}

/// The strokes that type `text`.
///
/// # Errors
/// The first character a US layout cannot type.
pub fn text_strokes(text: &str) -> Result<Vec<Stroke>, char> {
    text.chars().map(|c| char_stroke(c).ok_or(c)).collect()
}

/// Key codes of a combo such as `ctrl+alt+t`, `super`, `Return` or
/// `shift+/`: pressed in order, released in reverse.
///
/// # Errors
/// A part that names no key.
pub fn combo_codes(combo: &str) -> Result<Vec<u16>, String> {
    if combo.is_empty() {
        return Err(combo.to_owned());
    }
    // A trailing `+` is the plus key itself (`ctrl++`).
    let (head, plus) = match combo.strip_suffix("++") {
        Some(head) => (head, true),
        None => (combo, false),
    };
    let mut codes = Vec::new();
    let parts: Vec<&str> = if head.is_empty() {
        Vec::new()
    } else {
        head.split('+').collect()
    };
    for part in parts {
        codes.extend(key_codes(part).ok_or_else(|| part.to_owned())?);
    }
    if plus {
        codes.extend([KEY_LEFTSHIFT, 13]);
    }
    if codes.is_empty() {
        return Err(combo.to_owned());
    }
    Ok(codes)
}

/// One named key or character; a shifted character yields shift first.
fn key_codes(name: &str) -> Option<Vec<u16>> {
    let lower = name.to_ascii_lowercase();
    if let Some((_, code)) = NAMED.iter().find(|(n, _)| *n == lower) {
        return Some(vec![*code]);
    }
    let mut chars = name.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    // In a combo a letter names its key: `ctrl+T` is ctrl+t.
    let stroke = char_stroke(if c.is_ascii_alphabetic() {
        c.to_ascii_lowercase()
    } else {
        c
    })?;
    Some(if stroke.shift {
        vec![KEY_LEFTSHIFT, stroke.code]
    } else {
        vec![stroke.code]
    })
}

/// Validate actions before they travel: every combo and character must map.
///
/// # Errors
/// A sentence naming the first action that cannot be sent.
pub fn validate(actions: &[InputAction]) -> Result<(), String> {
    for action in actions {
        match action {
            InputAction::Key { combos } => {
                if combos.is_empty() {
                    return Err("no key given".into());
                }
                for combo in combos {
                    combo_codes(combo)
                        .map_err(|part| format!("'{part}' in '{combo}' names no key"))?;
                }
            }
            InputAction::Type { text } => {
                text_strokes(text)
                    .map_err(|c| format!("'{c}' cannot be typed on a US keyboard layout"))?;
            }
            InputAction::Move { .. } | InputAction::Click { .. } => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combos_map_to_evdev_codes() {
        assert_eq!(combo_codes("ctrl+alt+t").unwrap(), [29, 56, 20]);
        assert_eq!(combo_codes("Return").unwrap(), [28]);
        assert_eq!(combo_codes("super").unwrap(), [125]);
        assert_eq!(combo_codes("ctrl+T").unwrap(), [29, 20]);
        assert_eq!(combo_codes("shift+/").unwrap(), [42, 53]);
        assert_eq!(combo_codes("?").unwrap(), [42, 53]);
        assert_eq!(combo_codes("ctrl++").unwrap(), [29, 42, 13]);
        assert_eq!(combo_codes("F12").unwrap(), [88]);
        assert_eq!(combo_codes("ctrl+nokey"), Err("nokey".into()));
        assert!(combo_codes("").is_err());
    }

    #[test]
    fn text_types_on_a_us_layout() {
        let strokes = text_strokes("Hi!\n").unwrap();
        assert_eq!(
            strokes,
            [
                Stroke {
                    code: 35,
                    shift: true
                },
                Stroke {
                    code: 23,
                    shift: false
                },
                Stroke {
                    code: 2,
                    shift: true
                },
                Stroke {
                    code: 28,
                    shift: false
                },
            ]
        );
        assert_eq!(text_strokes("zé"), Err('é'));
        for c in ('a'..='z')
            .chain('A'..='Z')
            .chain("0123456789 -=[];'`\\,./!@#$%^&*()_+{}:\"~|<>?".chars())
        {
            let stroke = char_stroke(c).unwrap();
            assert!(stroke.code > 0 && stroke.code <= KEY_MAX_USED, "{c}");
        }
    }

    #[test]
    fn validation_names_the_problem() {
        assert!(
            validate(&[InputAction::Key {
                combos: vec!["ctrl+c".into()]
            }])
            .is_ok()
        );
        let error = validate(&[InputAction::Type {
            text: "naïve".into(),
        }])
        .unwrap_err();
        assert!(error.contains('ï'), "{error}");
        assert!(validate(&[InputAction::Key { combos: vec![] }]).is_err());
    }

    #[test]
    fn actions_round_trip_as_tagged_json() {
        let action = InputAction::Click {
            button: Button::Right,
            at: Some((10, 20)),
        };
        let text = serde_json::to_string(&action).unwrap();
        assert_eq!(text, r#"{"kind":"click","button":"right","at":[10,20]}"#);
        assert_eq!(serde_json::from_str::<InputAction>(&text).unwrap(), action);
    }
}
