//! Desktop keyboard shortcuts on a live key press: which configured
//! [`ShortcutAction`] it is, on every host. The native shell and the browser
//! desktop resolve presses through this one table and key translation, so a
//! chord means the same everywhere and a rebind reaches both.

use std::collections::BTreeSet;

use ui::shortcut_table::{KeyChord, ShortcutAction, ShortcutConfig};
use winit::event::Modifiers;
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};

/// Match a persisted shortcut against a live winit key event.
///
/// The native compositor uses this for session-owned bindings too. Keeping
/// that path on the same key-name translation as the shortcut editor avoids
/// claiming a configured named key before a client sees it and then failing
/// to perform its action.
pub fn shortcut_chord_matches(logical_key: &Key, modifiers: Modifiers, chord: &KeyChord) -> bool {
    let state = modifiers.state();
    let key_matches = shortcut_key_name(logical_key).is_some_and(|key| {
        chord.key.eq_ignore_ascii_case(&key)
            || state.shift_key()
                && unshifted_shortcut_key(&key)
                    .is_some_and(|base| chord.key.eq_ignore_ascii_case(base))
    });
    key_matches
        && chord.modifiers.contains("Super") == state.super_key()
        && chord.modifiers.contains("Ctrl") == state.control_key()
        && chord.modifiers.contains("Alt") == state.alt_key()
        && chord.modifiers.contains("Shift") == state.shift_key()
}

/// Winit reports the logical character produced by Shift, while shortcut
/// configuration stores the physical punctuation printed on the key.
fn unshifted_shortcut_key(key: &str) -> Option<&'static str> {
    Some(match key {
        "?" => "/",
        ":" => ";",
        "\"" => "'",
        "<" => ",",
        ">" => ".",
        "_" => "-",
        "+" => "=",
        "{" => "[",
        "}" => "]",
        "|" => "\\",
        "~" => "`",
        "!" => "1",
        "@" => "2",
        "#" => "3",
        "$" => "4",
        "%" => "5",
        "^" => "6",
        "&" => "7",
        "*" => "8",
        "(" => "9",
        ")" => "0",
        _ => return None,
    })
}

/// The sole configurable shortcut resolver. The input pre-router owns a press
/// exactly when this returns an action; the dispatcher performs that result.
pub fn configured_shortcut_action(
    shortcuts: &ShortcutConfig,
    logical_key: &Key,
    modifiers: Modifiers,
) -> Option<ShortcutAction> {
    shortcuts
        .bindings
        .iter()
        .find_map(|(action, chord)| {
            shortcut_chord_matches(logical_key, modifiers, chord).then_some(*action)
        })
        .or_else(|| is_bare_f11(logical_key, modifiers).then_some(ShortcutAction::ToggleFullscreen))
}

/// F11 is every desktop's full-screen key; it toggles the same real
/// fullscreen as the configurable chord, unless the person bound it to
/// something else.
fn is_bare_f11(logical_key: &Key, modifiers: Modifiers) -> bool {
    *logical_key == Key::Named(NamedKey::F11) && modifiers.state().is_empty()
}

fn shortcut_key_name(logical_key: &Key) -> Option<String> {
    let name = match logical_key {
        Key::Character(key) if key.as_str() == " " => "space".to_string(),
        Key::Character(key) if key.chars().count() == 1 => key.to_ascii_lowercase(),
        Key::Named(named) => match named {
            NamedKey::ArrowLeft => "left",
            NamedKey::ArrowRight => "right",
            NamedKey::ArrowUp => "up",
            NamedKey::ArrowDown => "down",
            NamedKey::Tab => "tab",
            NamedKey::Enter => "enter",
            NamedKey::Space => "space",
            NamedKey::Home => "home",
            NamedKey::End => "end",
            NamedKey::PageUp => "pageup",
            NamedKey::PageDown => "pagedown",
            NamedKey::Backspace => "backspace",
            NamedKey::Delete => "delete",
            NamedKey::Insert => "insert",
            NamedKey::PrintScreen => "printscreen",
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
        .to_string(),
        _ => return None,
    };
    Some(name)
}

pub fn shortcut_chord_from_event(logical_key: &Key, modifiers: Modifiers) -> Option<KeyChord> {
    let key = shortcut_key_name(logical_key)?;
    let state = modifiers.state();
    let mut names = BTreeSet::new();
    if state.super_key() {
        names.insert("Super".to_string());
    }
    if state.control_key() {
        names.insert("Ctrl".to_string());
    }
    if state.alt_key() {
        names.insert("Alt".to_string());
    }
    if state.shift_key() {
        names.insert("Shift".to_string());
    }
    Some(KeyChord {
        modifiers: names,
        key,
    })
}

/// The shortcut a key press in a browser page asks for, from the same table
/// the native shell reads.
///
/// A browser passes Super through only sometimes, so the page also takes a
/// stand-in for it that no browser or system binds: Alt+Shift, or
/// Ctrl+Option on a Mac (Option+Shift selects by words there). The stand-in
/// reads as Super; any other modifier held with it stays, so Ctrl with the
/// stand-in is Ctrl+Super. Because the stand-in's own keys change what a
/// letter or digit types (Shift+1 types `!`, Option+O `ø`), those keys are
/// read by their place on the keyboard. Super+Tab, the overview, is the
/// system's in most browsers, so O stands in for its Tab.
pub fn page_shortcut_action(
    shortcuts: &ShortcutConfig,
    logical_key: &Key,
    physical_key: PhysicalKey,
    modifiers: Modifiers,
    mac: bool,
) -> Option<ShortcutAction> {
    let (key, modifiers) = page_chord(logical_key, physical_key, modifiers, mac)?;
    configured_shortcut_action(shortcuts, &key, modifiers).or_else(|| {
        (key == Key::Character("o".into()))
            .then(|| configured_shortcut_action(shortcuts, &Key::Named(NamedKey::Tab), modifiers))
            .flatten()
    })
}

/// The key and modifiers [`page_shortcut_action`] looks up, or `None` when
/// neither Super nor the stand-in is held.
pub fn page_chord(
    logical_key: &Key,
    physical_key: PhysicalKey,
    modifiers: Modifiers,
    mac: bool,
) -> Option<(Key, Modifiers)> {
    let mut state = modifiers.state();
    if state.super_key() {
        return Some((logical_key.clone(), modifiers));
    }
    let stand_in = if mac {
        ModifiersState::CONTROL | ModifiersState::ALT
    } else {
        ModifiersState::ALT | ModifiersState::SHIFT
    };
    if !state.contains(stand_in) {
        return None;
    }
    state.remove(stand_in);
    state.insert(ModifiersState::SUPER);
    let place = match physical_key {
        PhysicalKey::Code(code) => {
            let name = format!("{code:?}");
            name.strip_prefix("Key")
                .or_else(|| name.strip_prefix("Digit"))
                .filter(|place| place.len() == 1)
                .map(str::to_lowercase)
        }
        PhysicalKey::Unidentified(_) => None,
    };
    let key = place.map_or_else(|| logical_key.clone(), |place| Key::Character(place.into()));
    Some((key, state.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{KeyCode, NativeKeyCode};

    fn press(
        logical: &Key,
        code: Option<KeyCode>,
        state: ModifiersState,
        mac: bool,
    ) -> Option<ShortcutAction> {
        let physical = code.map_or(
            PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
            PhysicalKey::Code,
        );
        page_shortcut_action(
            &ShortcutConfig::default(),
            logical,
            physical,
            state.into(),
            mac,
        )
    }

    fn native(logical: &Key, state: ModifiersState) -> Option<ShortcutAction> {
        configured_shortcut_action(&ShortcutConfig::default(), logical, state.into())
    }

    /// The page and the native shell read one table: Super+Up maximizes and
    /// Ctrl+Super+Up goes to the strip above in both, Super+3 shows the
    /// third strip, whether Super itself or the page's stand-in is held.
    #[test]
    fn the_page_resolves_chords_as_the_native_shell_does() {
        let sup = ModifiersState::SUPER;
        let alt_shift = ModifiersState::ALT | ModifiersState::SHIFT;
        let ctrl_opt = ModifiersState::CONTROL | ModifiersState::ALT;
        let up = Key::Named(NamedKey::ArrowUp);
        let cases = [
            (up.clone(), Some(KeyCode::ArrowUp), ModifiersState::empty()),
            (up, Some(KeyCode::ArrowUp), ModifiersState::CONTROL),
            (
                Key::Named(NamedKey::ArrowDown),
                Some(KeyCode::ArrowDown),
                ModifiersState::empty(),
            ),
            (
                Key::Named(NamedKey::ArrowLeft),
                Some(KeyCode::ArrowLeft),
                ModifiersState::empty(),
            ),
            (
                Key::Named(NamedKey::PageDown),
                Some(KeyCode::PageDown),
                ModifiersState::empty(),
            ),
            (
                Key::Character("3".into()),
                Some(KeyCode::Digit3),
                ModifiersState::empty(),
            ),
            (
                Key::Character("3".into()),
                Some(KeyCode::Digit3),
                ModifiersState::CONTROL,
            ),
        ];
        for (key, code, extra) in cases {
            let want = native(&key, sup | extra);
            assert!(want.is_some(), "{key:?} with {extra:?} is a native chord");
            assert_eq!(press(&key, code, sup | extra, false), want, "Super {key:?}");
            // The stand-in's Shift turns 3 into #, which its place still reads.
            let shifted = match &key {
                Key::Character(_) => Key::Character("#".into()),
                other => other.clone(),
            };
            assert_eq!(
                press(&shifted, code, alt_shift | extra, false),
                want,
                "Alt+Shift {key:?} with {extra:?}"
            );
            if extra.is_empty() {
                assert_eq!(
                    press(&key, code, ctrl_opt, true),
                    want,
                    "Ctrl+Option {key:?}"
                );
            }
        }
        assert_eq!(
            native(&Key::Named(NamedKey::ArrowUp), sup),
            Some(ShortcutAction::MaximizeWindow)
        );
        assert_eq!(
            native(
                &Key::Named(NamedKey::ArrowUp),
                sup | ModifiersState::CONTROL
            ),
            Some(ShortcutAction::PreviousStrip)
        );
        assert_eq!(
            native(&Key::Character("3".into()), sup).and_then(ShortcutAction::focus_strip_index),
            Some(2)
        );
    }

    /// O stands in for Tab, so the overview opens where the browser keeps
    /// Super+Tab; without Super or the stand-in nothing is a chord.
    #[test]
    fn o_opens_the_overview_and_plain_keys_are_no_chord() {
        assert_eq!(
            press(
                &Key::Character("\u{f8}".into()),
                Some(KeyCode::KeyO),
                ModifiersState::CONTROL | ModifiersState::ALT,
                true,
            ),
            Some(ShortcutAction::WorkspaceOverview)
        );
        for state in [
            ModifiersState::empty(),
            ModifiersState::ALT,
            ModifiersState::SHIFT,
            ModifiersState::CONTROL | ModifiersState::ALT,
        ] {
            assert_eq!(
                press(
                    &Key::Named(NamedKey::ArrowUp),
                    Some(KeyCode::ArrowUp),
                    state,
                    false
                ),
                None,
                "{state:?}"
            );
        }
    }
}
