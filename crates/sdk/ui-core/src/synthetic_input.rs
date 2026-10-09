//! Deterministic window input shared by native scenarios, headless driving,
//! test control and tests: what they deliver in place of a device.

use serde::{Deserialize, Serialize};
use winit::event::{DeviceId, ElementState, KeyEvent, MouseButton, RawKeyEvent, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NamedKey, NativeKeyCode, PhysicalKey, SmolStr};

/// Supported named keys for deterministic native input scenarios.
///
/// Character input is supplied separately, so unknown key names are rejected
/// rather than silently becoming typed text.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeKey {
    Enter,
    Tab,
    Escape,
    Space,
    Backspace,
    Delete,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
}

impl From<NativeKey> for Key {
    fn from(key: NativeKey) -> Self {
        Self::Named(match key {
            NativeKey::Enter => NamedKey::Enter,
            NativeKey::Tab => NamedKey::Tab,
            NativeKey::Escape => NamedKey::Escape,
            NativeKey::Space => NamedKey::Space,
            NativeKey::Backspace => NamedKey::Backspace,
            NativeKey::Delete => NamedKey::Delete,
            NativeKey::ArrowUp => NamedKey::ArrowUp,
            NativeKey::ArrowDown => NamedKey::ArrowDown,
            NativeKey::ArrowLeft => NamedKey::ArrowLeft,
            NativeKey::ArrowRight => NamedKey::ArrowRight,
            NativeKey::Home => NamedKey::Home,
            NativeKey::End => NamedKey::End,
        })
    }
}

/// `key` going down or up as keyboard input, carrying its text on press.
#[must_use]
pub fn key_input(key: &Key, state: ElementState) -> WindowEvent {
    let text = match (key, state) {
        (Key::Character(value), ElementState::Pressed) => Some(value.clone()),
        _ => None,
    };
    keyboard_input(key_event(key.clone(), text, state))
}

/// A key event for `logical_key` from no particular physical key: standard
/// location, not a repeat. Adjust the public fields for anything else.
#[must_use]
pub fn key_event(logical_key: Key, text: Option<SmolStr>, state: ElementState) -> KeyEvent {
    KeyEvent::synthetic(
        RawKeyEvent {
            physical_key: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
            state,
        },
        logical_key.clone(),
        text.clone(),
        KeyLocation::Standard,
        false,
        text,
        logical_key,
    )
}

/// `event` delivered to the window as real keyboard input.
#[must_use]
pub fn keyboard_input(event: KeyEvent) -> WindowEvent {
    WindowEvent::KeyboardInput {
        device_id: DeviceId::dummy(),
        event,
        is_synthetic: false,
    }
}

#[must_use]
pub fn cursor_moved(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(x, y),
    }
}

#[must_use]
pub fn mouse_input(state: ElementState, button: MouseButton) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button,
    }
}

/// The left button going down.
#[must_use]
pub fn mouse_press() -> WindowEvent {
    mouse_input(ElementState::Pressed, MouseButton::Left)
}

/// The left button coming up.
#[must_use]
pub fn mouse_release() -> WindowEvent {
    mouse_input(ElementState::Released, MouseButton::Left)
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::{ElementState, WindowEvent};
    use winit::keyboard::{Key, NamedKey};

    #[test]
    fn synthetic_keys_decode_strictly_and_emit_text_only_on_press() {
        // Case `character_text_is_emitted_only_on_press`.
        {
            let key = Key::Character("é".into());
            for (state, expected_text) in [
                (ElementState::Pressed, Some("é")),
                (ElementState::Released, None),
            ] {
                let WindowEvent::KeyboardInput {
                    event,
                    is_synthetic,
                    ..
                } = key_input(&key, state)
                else {
                    panic!("expected keyboard input");
                };
                assert_eq!(event.text.as_deref(), expected_text);
                assert_eq!(event.logical_key, key);
                assert_eq!(event.state, state);
                assert!(!event.repeat);
                assert!(!is_synthetic);
            }
        }

        // Case `named_keys_decode_strictly_and_map_to_keyboard_events`.
        {
            for (name, named) in [
                ("enter", NamedKey::Enter),
                ("tab", NamedKey::Tab),
                ("escape", NamedKey::Escape),
                ("space", NamedKey::Space),
                ("backspace", NamedKey::Backspace),
                ("delete", NamedKey::Delete),
                ("arrow_up", NamedKey::ArrowUp),
                ("arrow_down", NamedKey::ArrowDown),
                ("arrow_left", NamedKey::ArrowLeft),
                ("arrow_right", NamedKey::ArrowRight),
                ("home", NamedKey::Home),
                ("end", NamedKey::End),
            ] {
                let decoded: NativeKey = serde_json::from_value(serde_json::json!(name)).unwrap();
                assert_eq!(serde_json::to_value(decoded).unwrap(), name);
                let key: Key = decoded.into();
                assert_eq!(key, Key::Named(named));
                let WindowEvent::KeyboardInput { event, .. } =
                    key_input(&key, ElementState::Pressed)
                else {
                    panic!("expected keyboard input");
                };
                assert_eq!(event.logical_key, key);
                assert_eq!(event.text, None);
            }
            for rejected in ["Enter", "return", "ArrowUp", "up", "a", "", "f1"] {
                assert!(serde_json::from_value::<NativeKey>(serde_json::json!(rejected)).is_err());
            }
        }
    }
}
