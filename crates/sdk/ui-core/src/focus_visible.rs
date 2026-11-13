//! Focus-visible: the focus ring shows for keyboard focus only (`docs/design-language.md`, "Colour
//! roles"). A click or tap that focuses a control must never ring it; a Tab, an arrow or any other key
//! brings the ring back.

use std::cell::Cell;

use winit::event::{ElementState, WindowEvent};

thread_local! {
    static POINTER_LAST: Cell<bool> = const { Cell::new(false) };
}

/// Note `event`: a key press makes focus visible, a pointer press or a touch hides it.
pub fn note(event: &WindowEvent) {
    let pointer = match event {
        WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => false,
        WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        }
        | WindowEvent::Touch(_) => true,
        _ => return,
    };
    POINTER_LAST.with(|last| last.set(pointer));
}

/// Whether a focus ring should show now: the last focusing input was the keyboard (or there has been
/// none yet).
#[must_use]
pub fn shown() -> bool {
    !POINTER_LAST.with(Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_hides_the_ring_and_a_key_brings_it_back() {
        assert!(shown(), "nothing has happened yet: keyboard");
        note(&crate::synthetic_input::mouse_press());
        assert!(!shown(), "a click never shows the ring");
        note(&crate::synthetic_input::cursor_moved(4.0, 4.0));
        assert!(!shown(), "moving the pointer changes nothing");
        note(&crate::synthetic_input::keyboard_input(
            crate::synthetic_input::key_event(
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
                None,
                ElementState::Pressed,
            ),
        ));
        assert!(shown(), "the keyboard brings the ring back");
    }
}
