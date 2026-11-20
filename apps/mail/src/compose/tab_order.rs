//! Tab and Shift+Tab in the composer: they move through the headers to the
//! message instead of leaving the window.

use winit::event::{ElementState, WindowEvent};

use super::{ComposeWindow, Field};

impl ComposeWindow {
    /// Whether Tab is the composer's while it has the keyboard in a header,
    /// to move on through the headers rather than out of the window.
    pub(crate) fn takes_tab(&self) -> bool {
        self.open
            && self.has_keys
            && matches!(
                self.focus,
                Field::To | Field::Cc | Field::Bcc | Field::Subject
            )
    }

    /// Whether `event` presses Tab, and if so whether it goes back
    /// (Shift+Tab).
    pub(super) fn tab_press(
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> Option<bool> {
        matches!(
            event,
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
                    state: ElementState::Pressed,
                    ..
                },
                ..
            }
        )
        .then(|| modifiers.state().shift_key())
    }

    /// The field Tab (or, `back`, Shift+Tab) moves to from `from`: To, Cc
    /// and Bcc when they are shown, Subject, then the message.
    pub(super) fn header_after(&self, from: Field, back: bool) -> Field {
        let mut order = vec![Field::To];
        if self.show_cc_bcc {
            order.extend([Field::Cc, Field::Bcc]);
        }
        order.extend([Field::Subject, Field::Body]);
        let at = order.iter().position(|field| *field == from).unwrap_or(0);
        let to = if back { at.saturating_sub(1) } else { at + 1 };
        order
            .get(to)
            .or_else(|| order.last())
            .copied()
            .unwrap_or(from)
    }
}
