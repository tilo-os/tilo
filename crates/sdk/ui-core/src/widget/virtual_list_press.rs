//! What a click on a row of a [`VirtualList`] does: it chooses the row, opens it, or opens its
//! menu.

use super::{ListModel, VirtualList};

use vello::kurbo::Point;
use winit::keyboard::{Key, NamedKey};

impl<M: ListModel> VirtualList<M> {
    /// A completed click on the row of `key`: select it by the modifiers, and
    /// activate it when it is the second click on the same row in time (or
    /// rename it, where the list renames on a double click).
    pub(super) fn click_row(&mut self, key: M::Key, modifiers: winit::event::Modifiers) {
        let state = modifiers.state();
        let toggle = state.control_key() || state.super_key();
        let extend = state.shift_key();
        self.apply_selection(key.clone(), toggle, extend);
        self.focus_row(key.clone());
        if toggle || extend {
            self.clicks.record(key);
        } else if self.clicks.register(key.clone()) {
            if self.rename_on_double_click && self.begin_rename(&key) {
                return;
            }
            self.activate_key(key);
        }
    }

    /// Open `key` as Enter or a second click does: through the published view
    /// action when the host bound one, else the activation callback. A list whose
    /// rows open nothing chooses the row instead, as its click does.
    pub(super) fn activate_key(&mut self, key: M::Key) {
        if self.opens.is_none() && self.on_activate.is_none() {
            self.apply_selection(key, false, false);
            return;
        }
        // Opening the row is one act: the selection it moves onto the way is not
        // reported as a choice of its own.
        self.choose_quietly(key.clone());
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

    /// Whether `key`, pressed with `modifiers`, opens the focused row's menu: the
    /// Menu key, or Shift+F10 on it, as it does on a tree's rows.
    pub(super) fn take_menu_key(
        &mut self,
        key: &Key,
        modifiers: winit::keyboard::ModifiersState,
    ) -> bool {
        let menu = *key == Key::Named(NamedKey::ContextMenu)
            || (modifiers.shift_key() && *key == Key::Named(NamedKey::F10));
        menu && self.open_menu_from_keyboard()
    }

    /// Open the focused row's context menu: the one the keyboard is on, at the
    /// middle of its row.
    pub(super) fn open_menu_from_keyboard(&mut self) -> bool {
        if self.on_context.is_none() {
            return false;
        }
        let Some(key) = self.focused_row_key() else {
            return false;
        };
        let Some(index) = self.model.index_of(&key) else {
            return false;
        };
        let point = self
            .row_box(index)
            .map_or(self.rect.center(), |row| row.center());
        self.open_context(index, key, point);
        true
    }
}
