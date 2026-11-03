//! One spelling for a key: the token a shortcut declares (`Ctrl+Enter`), the
//! token a live press answers to (`enter`), and the key a test drives
//! (`Enter`).
//!
//! A shortcut is written by a person and matched against a press, which winit
//! reports as a named key or a character. Both sides read their key through
//! [`canonical`], so `Esc`, `escape` and the Escape key are one press, and so
//! are `Space` and the space bar.

use winit::keyboard::{Key, NamedKey};

/// The key `spec` names, as a driver or a typed shortcut writes it: `Enter`,
/// `Tab`, `Escape`, the arrows, `F1`–`F12`, `Home`, `End`, `PageUp`,
/// `PageDown`, `Delete`, `Space` and the rest; one character is that
/// character typed (`" "` is the space bar's character, `"Space"` the key).
#[must_use]
pub fn parse(spec: &str) -> Key {
    if spec.chars().count() == 1 {
        return Key::Character(spec.into());
    }
    match canonical(spec).as_str() {
        "enter" => Key::Named(NamedKey::Enter),
        "tab" => Key::Named(NamedKey::Tab),
        "escape" => Key::Named(NamedKey::Escape),
        "space" => Key::Named(NamedKey::Space),
        "backspace" => Key::Named(NamedKey::Backspace),
        "delete" => Key::Named(NamedKey::Delete),
        "insert" => Key::Named(NamedKey::Insert),
        "up" => Key::Named(NamedKey::ArrowUp),
        "down" => Key::Named(NamedKey::ArrowDown),
        "left" => Key::Named(NamedKey::ArrowLeft),
        "right" => Key::Named(NamedKey::ArrowRight),
        "home" => Key::Named(NamedKey::Home),
        "end" => Key::Named(NamedKey::End),
        "pageup" => Key::Named(NamedKey::PageUp),
        "pagedown" => Key::Named(NamedKey::PageDown),
        "f1" => Key::Named(NamedKey::F1),
        "f2" => Key::Named(NamedKey::F2),
        "f3" => Key::Named(NamedKey::F3),
        "f4" => Key::Named(NamedKey::F4),
        "f5" => Key::Named(NamedKey::F5),
        "f6" => Key::Named(NamedKey::F6),
        "f7" => Key::Named(NamedKey::F7),
        "f8" => Key::Named(NamedKey::F8),
        "f9" => Key::Named(NamedKey::F9),
        "f10" => Key::Named(NamedKey::F10),
        "f11" => Key::Named(NamedKey::F11),
        "f12" => Key::Named(NamedKey::F12),
        _ => Key::Character(spec.into()),
    }
}

/// The token `key` answers to, or `None` for a key no shortcut can name (a
/// modifier pressed on its own, a media key).
#[must_use]
pub fn token(key: &Key) -> Option<String> {
    let name = match key {
        Key::Character(text) if text.as_str() == " " => "space".to_owned(),
        Key::Character(text) if text.chars().count() == 1 => text.to_lowercase(),
        Key::Named(named) => match named {
            NamedKey::Enter => "enter",
            NamedKey::Tab => "tab",
            NamedKey::Escape => "escape",
            NamedKey::Space => "space",
            NamedKey::Backspace => "backspace",
            NamedKey::Delete => "delete",
            NamedKey::Insert => "insert",
            NamedKey::ArrowUp => "up",
            NamedKey::ArrowDown => "down",
            NamedKey::ArrowLeft => "left",
            NamedKey::ArrowRight => "right",
            NamedKey::Home => "home",
            NamedKey::End => "end",
            NamedKey::PageUp => "pageup",
            NamedKey::PageDown => "pagedown",
            NamedKey::F1 => "f1",
            NamedKey::F2 => "f2",
            NamedKey::F3 => "f3",
            NamedKey::F4 => "f4",
            NamedKey::F5 => "f5",
            NamedKey::F6 => "f6",
            NamedKey::F7 => "f7",
            NamedKey::F8 => "f8",
            NamedKey::F9 => "f9",
            NamedKey::F10 => "f10",
            NamedKey::F11 => "f11",
            NamedKey::F12 => "f12",
            _ => return None,
        }
        .to_owned(),
        _ => return None,
    };
    Some(name)
}

/// `key` as one token: its [`token`] when it is a live key, else the name a
/// shortcut writes, in the same spelling. `Esc` and `Escape`, `Space` and
/// `" "`, `Up` and `ArrowUp`, `E` and `e` are each one token.
///
/// The names are the toolkit's one table ([`tilo_ui_graph::keys`]), which the
/// compiler checks an app's declared shortcut with too, so a key an app may
/// write is a key a press answers to.
#[must_use]
pub fn canonical(key: &str) -> String {
    tilo_ui_graph::keys::key(key).unwrap_or_else(|| key.trim().to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_spelling_names_the_key_a_shortcut_writes_and_the_one_a_press_reports() {
        for (declared, pressed) in [
            ("Enter", NamedKey::Enter),
            ("esc", NamedKey::Escape),
            ("Space", NamedKey::Space),
            ("ArrowUp", NamedKey::ArrowUp),
            ("PgDn", NamedKey::PageDown),
            ("F5", NamedKey::F5),
        ] {
            assert_eq!(parse(declared), Key::Named(pressed), "{declared}");
            assert_eq!(
                token(&Key::Named(pressed)),
                Some(canonical(declared)),
                "{declared}"
            );
        }
        assert_eq!(parse("e"), Key::Character("e".into()));
        assert_eq!(parse(" "), Key::Character(" ".into()), "a typed space");
        assert_eq!(token(&parse("E")), Some("e".to_owned()));
        assert_eq!(token(&parse(" ")), Some("space".to_owned()));
        assert_eq!(canonical("Return"), "enter");
        assert_eq!(token(&parse("Shift")), None, "a modifier is no shortcut");
    }
}
