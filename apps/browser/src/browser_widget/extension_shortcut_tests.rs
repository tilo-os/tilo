//! An extension's shortcut never takes one the Browser answers itself
//! (`chrome.commands`, `extension_commands.rs`).

use super::*;

fn key(named: WNamedKey) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        WKey::Named(named),
        None,
        ElementState::Pressed,
    ))
}

#[test]
fn an_extension_is_never_given_a_shortcut_the_browser_answers() {
    use crate::extension_commands::{Shortcut, ShortcutKey, reserved};
    let mac = cfg!(target_os = "macos");
    let mut widget = BrowserWidget::new_headless();
    let command = ui::platform::PlatformConventions::default().command_modifier();
    for (letter, shift) in [
        ("f", false),
        ("f", true),
        ("g", false),
        ("g", true),
        ("d", false),
        ("r", false),
        ("r", true),
        ("0", false),
    ] {
        let mut event = key(WNamedKey::Enter);
        if let WindowEvent::KeyboardInput { event, .. } = &mut event {
            event.logical_key = WKey::Character(letter.into());
        }
        let held = if shift {
            command | winit::keyboard::ModifiersState::SHIFT
        } else {
            command
        };
        assert!(
            widget.handle_browser_shortcut(&event, held.into()),
            "the Browser answers {letter}"
        );
        let character = letter.chars().next().unwrap();
        let shortcut = Shortcut {
            primary: true,
            control: false,
            alt: false,
            shift,
            key: ShortcutKey::Character(character),
        };
        assert!(reserved(shortcut, mac), "{letter} with shift {shift}");
    }
    if !mac {
        for arrow in [WNamedKey::ArrowLeft, WNamedKey::ArrowRight] {
            assert!(
                widget.handle_browser_shortcut(
                    &key(arrow),
                    winit::keyboard::ModifiersState::ALT.into()
                )
            );
        }
        let back = Shortcut::parse("Alt+Left", false).unwrap();
        assert!(reserved(back, false));
    }
    // A key the Browser does not answer is free to give.
    let free = Shortcut::parse("Ctrl+Shift+Y", mac).unwrap();
    assert!(!reserved(free, mac));
}
