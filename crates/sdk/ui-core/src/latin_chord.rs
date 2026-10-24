//! Shortcuts on non-Latin keyboard sources.
//!
//! A person typing Russian, Greek, Hebrew or Arabic still copies with Ctrl+C:
//! the chord names the key, not the letter the active source puts on it. The
//! platform reports the letter (`с`, `ψ`, `ב`), so every handler that matches
//! `Character("c")` would miss it. Under Ctrl, Super or Alt, a key whose
//! source types a letter outside the Latin script is renamed here, once, to
//! the Latin character a US keyboard prints at that position; the text it
//! carries is left as typed. Latin letters with diacritics (German `ö`,
//! French `é`) stay as they are: they are shortcuts of their own on the
//! layouts that have them.

use winit::event::WindowEvent;
use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey, SmolStr};

/// Rename a modifier chord typed on a non-Latin source to its Latin key.
/// Returns `None` when `event` needs no renaming.
#[must_use]
pub fn renamed(event: &WindowEvent, modifiers: ModifiersState) -> Option<WindowEvent> {
    let WindowEvent::KeyboardInput { event: key, .. } = event else {
        return None;
    };
    if !(modifiers.control_key() || modifiers.super_key() || modifiers.alt_key()) {
        return None;
    }
    let Key::Character(typed) = &key.logical_key else {
        return None;
    };
    if !typed.chars().any(is_non_latin_letter) {
        return None;
    }
    let PhysicalKey::Code(code) = key.physical_key else {
        return None;
    };
    let latin = us_character(code)?;
    let latin = if modifiers.shift_key() {
        latin.to_ascii_uppercase()
    } else {
        latin
    };
    let mut event = event.clone();
    if let WindowEvent::KeyboardInput { event: key, .. } = &mut event {
        key.logical_key = Key::Character(SmolStr::new(latin.encode_utf8(&mut [0; 4])));
    }
    Some(event)
}

/// A letter outside the Latin script: Basic Latin through Latin Extended-B,
/// IPA and Latin Extended Additional are Latin; every other alphabetic
/// character is not.
#[must_use]
pub fn is_non_latin_letter(ch: char) -> bool {
    ch.is_alphabetic() && !matches!(ch, '\0'..='\u{2AF}' | '\u{1E00}'..='\u{1EFF}')
}

/// The unshifted character a US keyboard prints at `code`.
fn us_character(code: KeyCode) -> Option<char> {
    Some(match code {
        KeyCode::KeyA => 'a',
        KeyCode::KeyB => 'b',
        KeyCode::KeyC => 'c',
        KeyCode::KeyD => 'd',
        KeyCode::KeyE => 'e',
        KeyCode::KeyF => 'f',
        KeyCode::KeyG => 'g',
        KeyCode::KeyH => 'h',
        KeyCode::KeyI => 'i',
        KeyCode::KeyJ => 'j',
        KeyCode::KeyK => 'k',
        KeyCode::KeyL => 'l',
        KeyCode::KeyM => 'm',
        KeyCode::KeyN => 'n',
        KeyCode::KeyO => 'o',
        KeyCode::KeyP => 'p',
        KeyCode::KeyQ => 'q',
        KeyCode::KeyR => 'r',
        KeyCode::KeyS => 's',
        KeyCode::KeyT => 't',
        KeyCode::KeyU => 'u',
        KeyCode::KeyV => 'v',
        KeyCode::KeyW => 'w',
        KeyCode::KeyX => 'x',
        KeyCode::KeyY => 'y',
        KeyCode::KeyZ => 'z',
        KeyCode::Digit0 => '0',
        KeyCode::Digit1 => '1',
        KeyCode::Digit2 => '2',
        KeyCode::Digit3 => '3',
        KeyCode::Digit4 => '4',
        KeyCode::Digit5 => '5',
        KeyCode::Digit6 => '6',
        KeyCode::Digit7 => '7',
        KeyCode::Digit8 => '8',
        KeyCode::Digit9 => '9',
        KeyCode::Backquote => '`',
        KeyCode::Minus => '-',
        KeyCode::Equal => '=',
        KeyCode::BracketLeft => '[',
        KeyCode::BracketRight => ']',
        KeyCode::Backslash => '\\',
        KeyCode::Semicolon => ';',
        KeyCode::Quote => '\'',
        KeyCode::Comma => ',',
        KeyCode::Period => '.',
        KeyCode::Slash => '/',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::ElementState;

    fn press(typed: &str, code: KeyCode) -> WindowEvent {
        let mut event = crate::synthetic_input::key_event(
            Key::Character(typed.into()),
            Some(typed.into()),
            ElementState::Pressed,
        );
        event.physical_key = PhysicalKey::Code(code);
        crate::synthetic_input::keyboard_input(event)
    }

    fn logical(event: &WindowEvent) -> &Key {
        let WindowEvent::KeyboardInput { event, .. } = event else {
            panic!("a key event")
        };
        &event.logical_key
    }

    #[test]
    fn a_chord_on_a_cyrillic_greek_or_hebrew_letter_names_its_latin_key() {
        for (typed, code, latin) in [
            ("с", KeyCode::KeyC, "c"),
            ("м", KeyCode::KeyV, "v"),
            ("ψ", KeyCode::KeyC, "c"),
            ("ז", KeyCode::KeyZ, "z"),
            ("х", KeyCode::BracketLeft, "["),
        ] {
            let renamed = renamed(&press(typed, code), ModifiersState::CONTROL)
                .unwrap_or_else(|| panic!("Ctrl+{typed} is renamed"));
            assert_eq!(logical(&renamed), &Key::Character(latin.into()));
            let WindowEvent::KeyboardInput { event, .. } = &renamed else {
                unreachable!()
            };
            assert_eq!(
                event.text.as_deref(),
                Some(typed),
                "the text stays as typed"
            );
        }
        let redo = renamed(
            &press("Я", KeyCode::KeyZ),
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        )
        .expect("Ctrl+Shift+Я is renamed");
        assert_eq!(logical(&redo), &Key::Character("Z".into()));
    }

    #[test]
    fn typing_latin_letters_and_unmodified_keys_are_left_alone() {
        assert!(renamed(&press("с", KeyCode::KeyC), ModifiersState::empty()).is_none());
        assert!(renamed(&press("с", KeyCode::KeyC), ModifiersState::SHIFT).is_none());
        assert!(renamed(&press("c", KeyCode::KeyC), ModifiersState::CONTROL).is_none());
        assert!(renamed(&press("ö", KeyCode::Semicolon), ModifiersState::CONTROL).is_none());
        assert!(renamed(&press("é", KeyCode::Digit2), ModifiersState::SUPER).is_none());
        assert!(renamed(&press("ß", KeyCode::Minus), ModifiersState::CONTROL).is_none());
    }
}
