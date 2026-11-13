//! The spelling of a key and of a chord: what an app declares (`key: "Ctrl+Enter"`), what the
//! person presses, and what a menu or a palette shows.
//!
//! The compiler checks a declaration with it and the toolkit matches a press with it, so one table
//! says what a key is called: `Esc` and the Escape key are one key, on any keyboard layout.

/// The token of the key `name` names: `enter`, `escape`, `space`, `tab`, the arrows,
/// `pageup`/`pagedown`, `home`/`end`, `delete`/`insert`, `f1`–`f12`, or one character in lowercase
/// (`e`, `/`, `ä`). `None` when no key goes by that name.
#[must_use]
pub fn key(name: &str) -> Option<String> {
    if name == " " {
        return Some("space".to_owned());
    }
    let name = name.trim();
    if name.chars().count() == 1 {
        return Some(name.to_lowercase());
    }
    // A key painted the way a keyboard spells it (`Page Up`) names the same key as the token a press
    // reports (`pageup`).
    let canonical = match name.to_ascii_lowercase().replace([' ', '_'], "").as_str() {
        "return" | "enter" => "enter",
        "esc" | "escape" => "escape",
        "spacebar" | "space" => "space",
        "tab" => "tab",
        "backspace" | "back" => "backspace",
        "del" | "delete" => "delete",
        "ins" | "insert" => "insert",
        "arrowup" | "up" => "up",
        "arrowdown" | "down" => "down",
        "arrowleft" | "left" => "left",
        "arrowright" | "right" => "right",
        "home" => "home",
        "end" => "end",
        "pgup" | "pageup" => "pageup",
        "pgdn" | "pgdown" | "pagedown" => "pagedown",
        "f1" => "f1",
        "f2" => "f2",
        "f3" => "f3",
        "f4" => "f4",
        "f5" => "f5",
        "f6" => "f6",
        "f7" => "f7",
        "f8" => "f8",
        "f9" => "f9",
        "f10" => "f10",
        "f11" => "f11",
        "f12" => "f12",
        _ => return None,
    };
    Some(canonical.to_owned())
}

/// The keys a chord can name, for a message that teaches the spelling.
pub const KEY_NAMES: &str = "`Enter`, `Esc`, `Space`, `Tab`, `Backspace`, `Delete`, `Home`, `End`, `PageUp`, `PageDown`, the arrows, `F1`–`F12`, or one character";

/// Which modifier `name` is, the way a chord writes it.
fn modifier(name: &str) -> Option<Modifier> {
    match name.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "cmd" | "command" | "⌘" | "⌃" => Some(Modifier::Command),
        "shift" | "⇧" => Some(Modifier::Shift),
        "alt" | "option" | "⌥" => Some(Modifier::Alt),
        "super" | "meta" | "win" | "logo" => Some(Modifier::System),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Modifier {
    /// The platform's command modifier: Command on macOS, Control elsewhere.
    Command,
    Shift,
    Alt,
    /// The desktop's own modifier (Super).
    System,
}

/// A key with the modifiers held with it, as a shortcut is written: `Ctrl+Enter`, `Ctrl+Shift+K`,
/// `Alt+Up`, `F2`, `e`.
// A key press reports each modifier as a flag of its own, so a chord holds four of them.
#[allow(
    clippy::struct_excessive_bools,
    reason = "a press reports the four modifiers as independent flags; a chord mirrors them"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    /// Held: the platform's command modifier (Command on macOS, Control elsewhere).
    pub command: bool,
    /// Held: the desktop's own modifier (Super).
    pub system: bool,
    pub shift: bool,
    pub alt: bool,
    /// The key's [`key`] token.
    pub key: String,
}

impl Chord {
    /// The chord `text` writes: modifiers, each with `+` after it, and then one key.
    ///
    /// # Errors
    ///
    /// What is not a chord: no key, a part that is neither a modifier nor a key, a modifier given
    /// twice, a key before a modifier, or an empty part.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
        if text.is_empty() || parts.iter().any(|part| part.is_empty()) {
            return Err(format!(
                "`{text}` is no shortcut: it is modifiers and one key, such as `Ctrl+Enter`"
            ));
        }
        let mut key_seen: Option<&str> = None;
        for part in &parts {
            match (modifier(part), key_seen) {
                (None, None) => key_seen = Some(*part),
                (None, Some(_)) => {
                    return Err(format!(
                        "`{text}` names more than one key: it is modifiers and one key, such as `Ctrl+Enter`"
                    ));
                }
                (Some(_), Some(key)) => {
                    return Err(format!(
                        "`{text}` holds one key: `{key}` comes before `{part}`, which is a modifier"
                    ));
                }
                (Some(_), None) => {}
            }
        }
        let Some(part) = parts.pop() else {
            return Err(format!("`{text}` names no key: {KEY_NAMES}"));
        };
        if modifier(part).is_some() {
            return Err(format!(
                "`{part}` is a modifier, not a key: `{part}+Enter` holds it with a key"
            ));
        }
        let Some(key) = key(part) else {
            return Err(format!("`{part}` is no key: {KEY_NAMES}"));
        };
        let mut chord = Self {
            command: false,
            system: false,
            shift: false,
            alt: false,
            key,
        };
        for part in parts {
            match modifier(part) {
                Some(Modifier::Command) if !chord.command => chord.command = true,
                Some(Modifier::System) if !chord.system => chord.system = true,
                Some(Modifier::Shift) if !chord.shift => chord.shift = true,
                Some(Modifier::Alt) if !chord.alt => chord.alt = true,
                Some(_) => return Err(format!("`{part}` is given twice in `{text}`")),
                None => {
                    return Err(format!(
                        "`{part}` is no modifier: `Ctrl`, `Shift`, `Alt` or `Super`"
                    ));
                }
            }
        }
        Ok(chord)
    }

    /// The chord as declared and shown: the modifiers in one order, then the key, each as the
    /// keyboard in use spells it (`Ctrl+Shift+enter`, or a Mac reader's `⌘⇧↩`).
    #[must_use]
    pub fn text(&self) -> String {
        let mut parts = Vec::with_capacity(5);
        for (held, name) in [
            (self.system, "Super"),
            (self.command, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
        ] {
            if held {
                parts.push(name.to_owned());
            }
        }
        parts.push(self.key.clone());
        parts.join("+")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every way of writing a key names it, and nothing else does.
    #[test]
    fn one_table_names_the_keys_a_shortcut_can_use() {
        assert_eq!(key("Enter").as_deref(), Some("enter"));
        assert_eq!(key("Return").as_deref(), Some("enter"));
        assert_eq!(key("Esc").as_deref(), Some("escape"));
        assert_eq!(key("PgDn").as_deref(), Some("pagedown"));
        assert_eq!(key("F5").as_deref(), Some("f5"));
        assert_eq!(key("E").as_deref(), Some("e"));
        assert_eq!(key(" ").as_deref(), Some("space"));
        assert_eq!(key("Page Up").as_deref(), Some("pageup"));
        assert_eq!(key("Shift"), None);
        assert_eq!(key("Entr"), None);
    }

    /// A chord reads the modifiers in any order, and says what a wrong one misses.
    #[test]
    fn a_chord_is_modifiers_and_one_key() {
        let chord = Chord::parse("Shift+cTrL+Enter").expect("a chord");
        assert!(chord.command && chord.shift && !chord.alt && !chord.system);
        assert_eq!(chord.key, "enter");
        assert_eq!(chord.text(), "Ctrl+Shift+enter");
        assert_eq!(Chord::parse("e").expect("a key on its own").text(), "e");
        assert_eq!(
            Chord::parse("Super+Space").expect("a desktop chord").text(),
            "Super+space"
        );
        assert_eq!(
            Chord::parse("Ctrl+Enter").unwrap(),
            Chord::parse("Control+Return").unwrap()
        );
        for (wrong, said) in [
            ("", "no shortcut"),
            ("Ctrl", "is a modifier, not a key"),
            ("Ctrl++Enter", "no shortcut"),
            ("Ctrl+Entr", "is no key"),
            ("Ctrl+Shift+Shift+K", "given twice"),
            ("Ctrl+K+Shift", "comes before"),
            ("Ctrl+K+Enter", "more than one key"),
        ] {
            let error = Chord::parse(wrong).expect_err(wrong);
            assert!(error.contains(said), "`{wrong}`: {error}");
        }
    }
}
