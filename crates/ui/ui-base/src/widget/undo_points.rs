//! The ephemeral state (caret, selection) a widget saves with the undo points its owner's
//! actions make, and restores when one is undone.

/// A widget that restores its ephemeral state (caret, selection) around the undo points
/// its owner's actions make.
pub trait UndoPoints: super::Widget {
    /// Ephemeral state (caret, selection) saved with an undo point.
    fn get_state(&self) -> Option<String> {
        None
    }

    /// Restore ephemeral state saved by [`Self::get_state`].
    fn set_state(&mut self, _state: &str) {}
}

/// `widget`'s ephemeral state.
pub fn get_state(widget: &(impl super::Widget + ?Sized)) -> Option<String> {
    super::capability::capability::<dyn UndoPoints>(widget)?.get_state()
}

/// Restore `widget`'s ephemeral state.
pub fn set_state(widget: &mut (impl super::Widget + ?Sized), state: &str) {
    if let Some(points) = super::capability::capability_mut::<dyn UndoPoints>(widget) {
        points.set_state(state);
    }
}
