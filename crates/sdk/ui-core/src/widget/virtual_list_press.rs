//! What a click on a row of a [`VirtualList`] does: it chooses the row, opens it, or opens its
//! menu.

use super::{ListModel, VirtualList};

use vello::kurbo::Point;

impl<M: ListModel> VirtualList<M> {
    /// A completed click on the row of `key`: select it by the modifiers, and
    /// activate it when it is the second click on the same row in time.
    pub(super) fn click_row(&mut self, key: M::Key, modifiers: winit::event::Modifiers) {
        let state = modifiers.state();
        let toggle = state.control_key() || state.super_key();
        let extend = state.shift_key();
        self.apply_selection(key.clone(), toggle, extend);
        self.focus_row(key.clone());
        if toggle || extend {
            self.clicks.record(key);
        } else if self.clicks.register(key.clone()) {
            self.activate_key(key);
        }
    }

    /// Open `key` as Enter or a second click does: through the published view
    /// action when the host bound one, else the activation callback.
    pub(super) fn activate_key(&mut self, key: M::Key) {
        if let Some((_, handler)) = &mut self.opens {
            handler(key);
        } else if let Some(callback) = &mut self.on_activate {
            callback(key);
        }
    }

    /// Open the row's context menu: select it unless it already is, then
    /// hand the host the key, the point and the row's rect.
    pub(super) fn open_context(&mut self, index: usize, key: M::Key, point: Point) {
        if !self.selection.is_selected(&key) {
            self.apply_selection(key.clone(), false, false);
        }
        self.focus_row(key.clone());
        // The row's own box, below the band of a heading it starts a group under, or the
        // list's when the row is not placed.
        let rect = self.row_box(index).unwrap_or(self.rect);
        if let Some(callback) = &mut self.on_context {
            callback(key, point, rect);
        }
    }
}
