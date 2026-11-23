//! The palette's two answers beside a question it puts while a History review keeps its request: go
//! ahead, or leave it.

use super::ActionPalette;
use crate::widget::{Button, Row};

/// Go ahead, then the way out: the keyboard's parts after the field (0) are these (1, 2).
pub(super) fn controls(command: &std::rc::Rc<std::cell::Cell<Option<bool>>>, submit: &str) -> Row {
    let go = command.clone();
    let cancel = command.clone();
    Row::new()
        .push(Box::new(
            Button::new("palette-submit", submit).on_open(move || go.set(Some(true))),
        ))
        .push(Box::new(
            Button::new("palette-cancel", "Cancel").on_open(move || cancel.set(Some(false))),
        ))
}

impl ActionPalette {
    /// The answers back to Confirm and Cancel.
    pub(super) fn reset_controls(&mut self) {
        self.controls = controls(&self.command, "Confirm");
    }
}
