use winit::event::{ElementState, KeyEvent, RawKeyEvent};
use winit::keyboard::{Key, KeyCode, KeyLocation, PhysicalKey, SmolStr};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

#[test]
fn synthetic_key_event_preserves_public_fields_and_modifier_supplements() {
    let event = KeyEvent::synthetic(
        RawKeyEvent {
            physical_key: PhysicalKey::Code(KeyCode::KeyQ),
            state: ElementState::Pressed,
        },
        Key::Character(SmolStr::new("Q")),
        Some(SmolStr::new("œ")),
        KeyLocation::Numpad,
        true,
        Some(SmolStr::new("\u{11}")),
        Key::Character(SmolStr::new("q")),
    );

    assert_eq!(event.physical_key, PhysicalKey::Code(KeyCode::KeyQ));
    assert_eq!(event.logical_key, Key::Character(SmolStr::new("Q")));
    assert_eq!(event.text.as_deref(), Some("œ"));
    assert_eq!(event.location, KeyLocation::Numpad);
    assert_eq!(event.state, ElementState::Pressed);
    assert!(event.repeat);
    assert_eq!(event.text_with_all_modifiers(), Some("\u{11}"));
    assert_eq!(
        event.key_without_modifiers(),
        Key::Character(SmolStr::new("q"))
    );
}
