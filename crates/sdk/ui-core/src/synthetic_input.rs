//! Deterministic keyboard inputs shared by native scenarios and headless driving.

use serde::{Deserialize, Serialize};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

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

/// Build a deterministic `WindowEvent::KeyboardInput` for both input drivers.
pub(crate) fn key_event(key: &Key, state: ElementState) -> WindowEvent {
    let text = match (key, state) {
        (Key::Character(value), ElementState::Pressed) => Some(value.clone()),
        _ => None,
    };
    let event = winit::event::KeyEvent::synthetic(
        winit::event::RawKeyEvent {
            physical_key: winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            ),
            state,
        },
        key.clone(),
        text.clone(),
        winit::keyboard::KeyLocation::Standard,
        false,
        text,
        key.clone(),
    );
    WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event,
        is_synthetic: false,
    }
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
                } = key_event(&key, state)
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
                    key_event(&key, ElementState::Pressed)
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
