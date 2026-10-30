//! The chord a global shortcut is bound to: modifiers and one physical
//! key, so a keyboard layout switch never changes what a grant takes.
//!
//! Apps propose triggers in the portal's spelling (`Ctrl+Shift+K`); the
//! canonical form (`CTRL+SHIFT+KEYK`) is what bindings are compared by,
//! and the key's evdev code is what the session matches a press against.

use serde::Serialize;
use settings_store::hardware::{KeyChord, ShortcutConfig};

/// The modifiers held with a key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
#[allow(clippy::struct_excessive_bools, reason = "one flag per modifier key")]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
}

/// A canonical chord.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Trigger {
    /// `CTRL+SHIFT+KEYK`: modifiers in a fixed order, then the key.
    pub canonical: String,
    /// The key's evdev code.
    pub keycode: u32,
    #[serde(flatten)]
    pub modifiers: Modifiers,
}

/// The canonical keys and their evdev codes (`linux/input-event-codes.h`).
const KEYS: &[(&str, u32)] = &[
    ("ESCAPE", 1),
    ("DIGIT1", 2),
    ("DIGIT2", 3),
    ("DIGIT3", 4),
    ("DIGIT4", 5),
    ("DIGIT5", 6),
    ("DIGIT6", 7),
    ("DIGIT7", 8),
    ("DIGIT8", 9),
    ("DIGIT9", 10),
    ("DIGIT0", 11),
    ("BACKSPACE", 14),
    ("TAB", 15),
    ("KEYQ", 16),
    ("KEYW", 17),
    ("KEYE", 18),
    ("KEYR", 19),
    ("KEYT", 20),
    ("KEYY", 21),
    ("KEYU", 22),
    ("KEYI", 23),
    ("KEYO", 24),
    ("KEYP", 25),
    ("ENTER", 28),
    ("KEYA", 30),
    ("KEYS", 31),
    ("KEYD", 32),
    ("KEYF", 33),
    ("KEYG", 34),
    ("KEYH", 35),
    ("KEYJ", 36),
    ("KEYK", 37),
    ("KEYL", 38),
    ("KEYZ", 44),
    ("KEYX", 45),
    ("KEYC", 46),
    ("KEYV", 47),
    ("KEYB", 48),
    ("KEYN", 49),
    ("KEYM", 50),
    ("SPACE", 57),
    ("F1", 59),
    ("F2", 60),
    ("F3", 61),
    ("F4", 62),
    ("F5", 63),
    ("F6", 64),
    ("F7", 65),
    ("F8", 66),
    ("F9", 67),
    ("F10", 68),
    ("F11", 87),
    ("F12", 88),
    ("HOME", 102),
    ("ARROWUP", 103),
    ("PAGEUP", 104),
    ("ARROWLEFT", 105),
    ("ARROWRIGHT", 106),
    ("END", 107),
    ("ARROWDOWN", 108),
    ("PAGEDOWN", 109),
    ("INSERT", 110),
    ("DELETE", 111),
];

/// The canonical name of a key as an app or a person spells it.
fn canonical_key(value: &str) -> Option<&'static str> {
    let value = value.trim().to_ascii_uppercase();
    let spelled = match value.as_str() {
        "ESC" => "ESCAPE".to_owned(),
        "RETURN" => "ENTER".to_owned(),
        "UP" => "ARROWUP".to_owned(),
        "DOWN" => "ARROWDOWN".to_owned(),
        "LEFT" => "ARROWLEFT".to_owned(),
        "RIGHT" => "ARROWRIGHT".to_owned(),
        key if key.len() == 1 && key.bytes().all(|byte| byte.is_ascii_alphabetic()) => {
            format!("KEY{key}")
        }
        key if key.len() == 1 && key.bytes().all(|byte| byte.is_ascii_digit()) => {
            format!("DIGIT{key}")
        }
        key => key.to_owned(),
    };
    KEYS.iter()
        .find(|(name, _)| *name == spelled)
        .map(|(name, _)| *name)
}

impl Trigger {
    /// The chord of `modifiers` and `key`; `None` for a key without a
    /// canonical name or no modifier at all, so an app can never take
    /// ordinary typing.
    #[must_use]
    pub fn of(modifiers: Modifiers, key: &str) -> Option<Self> {
        let Modifiers {
            ctrl,
            alt,
            shift,
            logo,
        } = modifiers;
        let key = canonical_key(key)?;
        let keycode = KEYS.iter().find(|(name, _)| *name == key)?.1;
        let names: Vec<&str> = [
            (ctrl, "CTRL"),
            (alt, "ALT"),
            (shift, "SHIFT"),
            (logo, "SUPER"),
        ]
        .iter()
        .filter(|(held, _)| *held)
        .map(|(_, name)| *name)
        .collect();
        if names.is_empty() {
            return None;
        }
        Some(Self {
            canonical: format!("{}+{key}", names.join("+")),
            keycode,
            modifiers,
        })
    }

    /// A trigger in the portal's spelling (`Ctrl+Shift+K`, `LOGO+space`).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let mut modifiers = Modifiers::default();
        let mut key = None;
        for piece in value.split('+') {
            let piece = piece.trim().to_ascii_uppercase();
            let modifier = match piece.as_str() {
                "CTRL" | "CONTROL" => &mut modifiers.ctrl,
                "ALT" => &mut modifiers.alt,
                "SHIFT" => &mut modifiers.shift,
                "SUPER" | "META" | "WIN" | "LOGO" => &mut modifiers.logo,
                "" => return None,
                _ => {
                    if key.replace(piece).is_some() {
                        return None;
                    }
                    continue;
                }
            };
            if std::mem::replace(modifier, true) {
                return None;
            }
        }
        Self::of(modifiers, &key?)
    }

    /// How the trigger reads in a portal reply: `Ctrl + Shift + K`.
    #[must_use]
    pub fn display(&self) -> String {
        self.canonical
            .split('+')
            .map(|piece| match piece {
                "CTRL" => "Ctrl".to_owned(),
                "ALT" => "Alt".to_owned(),
                "SHIFT" => "Shift".to_owned(),
                "SUPER" => "Super".to_owned(),
                "ARROWUP" => "Up".to_owned(),
                "ARROWDOWN" => "Down".to_owned(),
                "ARROWLEFT" => "Left".to_owned(),
                "ARROWRIGHT" => "Right".to_owned(),
                key => key
                    .strip_prefix("KEY")
                    .or_else(|| key.strip_prefix("DIGIT"))
                    .map_or_else(|| key_name(key), str::to_owned),
            })
            .collect::<Vec<_>>()
            .join(" + ")
    }

    /// Whether a press of `keycode` with these modifiers is this chord.
    #[must_use]
    pub fn matches(&self, keycode: u32, modifiers: Modifiers) -> bool {
        self.keycode == keycode && self.modifiers == modifiers
    }
}

/// A named key as it reads: `F5`, `Space`.
fn key_name(key: &str) -> String {
    if key.len() > 1 && key.starts_with('F') {
        return key.to_owned();
    }
    let mut name = key.to_ascii_lowercase();
    if let Some(first) = name.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    name
}

/// Chords the session's conventions keep from every app, beyond its
/// shortcut table: the X server kill chord, in-app search, the window menu
/// and the tab chords every app answers itself.
const SESSION_RESERVED: &[&str] = &[
    "CTRL+ALT+BACKSPACE",
    "CTRL+KEYK",
    "ALT+SPACE",
    "CTRL+TAB",
    "CTRL+SHIFT+TAB",
    "CTRL+KEYT",
    "CTRL+SHIFT+KEYT",
    "CTRL+KEYW",
    "ALT+TAB",
    "ALT+SHIFT+TAB",
    "ALT+F4",
];

/// The chord of an entry of the desktop's shortcut table.
fn table_trigger(chord: &KeyChord) -> Option<Trigger> {
    Trigger::of(
        Modifiers {
            ctrl: chord.modifiers.contains("Ctrl"),
            alt: chord.modifiers.contains("Alt"),
            shift: chord.modifiers.contains("Shift"),
            logo: chord.modifiers.contains("Super"),
        },
        &chord.key,
    )
}

/// Whether `trigger` is the session's own: the desktop's shortcuts win
/// over every grant.
#[must_use]
pub fn reserved(trigger: &Trigger, table: &ShortcutConfig) -> bool {
    SESSION_RESERVED.contains(&trigger.canonical.as_str())
        || table
            .bindings
            .values()
            .filter_map(table_trigger)
            .any(|taken| taken.canonical == trigger.canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_are_canonical_physical_and_need_a_modifier() {
        let trigger = Trigger::parse("Shift + Ctrl + k").expect("a chord");
        assert_eq!(trigger.canonical, "CTRL+SHIFT+KEYK");
        assert_eq!(trigger.keycode, 37);
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let ctrl_shift = Modifiers {
            shift: true,
            ..ctrl
        };
        assert!(trigger.matches(37, ctrl_shift));
        assert!(!trigger.matches(37, ctrl));
        assert_eq!(trigger.display(), "Ctrl + Shift + K");
        assert_eq!(Trigger::parse(&trigger.display()), Some(trigger));
        assert!(Trigger::parse("k").is_none());
        assert!(Trigger::parse("Ctrl+Ctrl+K").is_none());
        assert!(Trigger::parse("Ctrl+K+J").is_none());
        assert!(Trigger::parse("Ctrl+PrintScreen").is_none());
        assert_eq!(
            Trigger::parse("LOGO+space").map(|t| t.display()).as_deref(),
            Some("Super + Space")
        );
        assert_eq!(
            Trigger::parse("Alt+F12").map(|t| t.display()).as_deref(),
            Some("Alt + F12")
        );
    }

    #[test]
    fn the_desktops_chords_and_conventions_are_reserved() {
        let table = ShortcutConfig::default();
        assert!(reserved(&Trigger::parse("Ctrl+K").expect("chord"), &table));
        assert!(!reserved(
            &Trigger::parse("Ctrl+Shift+Alt+F9").expect("chord"),
            &table
        ));
        let taken = table
            .bindings
            .values()
            .find_map(table_trigger)
            .expect("the default table binds chords");
        assert!(reserved(&taken, &table));
    }
}
