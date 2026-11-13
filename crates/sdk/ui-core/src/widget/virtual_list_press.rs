//! What a click on a row of a [`VirtualList`] does: it chooses the row, opens it, or opens its
//! menu.

use super::Choosing;
use super::{ListModel, VirtualList};

use vello::kurbo::Point;
use winit::keyboard::{Key, NamedKey};

impl<M: ListModel> VirtualList<M> {
    pub(super) fn click_row(&mut self, key: M::Key, modifiers: winit::event::Modifiers) {
        let state = modifiers.state();
        let toggle = state.control_key() || state.super_key();
        let extend = state.shift_key();
        let opens = self
            .model
            .index_of(&key)
            .is_some_and(|index| self.opens_item(index));
        if opens && !toggle && !extend {
            self.focus_row(key.clone());
            self.activate_key(key);
            return;
        }
        if self.open_on_press && !toggle && !extend {
            // Opening the row is one act: the selection it moves onto the way is not reported
            // as a choice of its own.
            self.focus_row(key.clone());
            self.activate_key(key);
            return;
        }
        self.apply_selection(key.clone(), toggle, extend, Choosing::ByHand);
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

    pub(super) fn activate_key(&mut self, key: M::Key) {
        // A row that opens where it stands opens there, whether or not anything else listens:
        // the list holds the item open, the row grows into its document, and the host hears
        // nothing it must show itself.
        if self
            .model
            .index_of(&key)
            .is_some_and(|index| self.opens_here(index))
        {
            self.choose_quietly(key.clone());
            self.open_where_it_stands(key);
            return;
        }
        if self.opens.is_none() && self.on_activate.is_none() {
            self.apply_selection(key, false, false, Choosing::ByHand);
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

    pub(super) fn take_menu_key(
        &mut self,
        key: &Key,
        modifiers: winit::keyboard::ModifiersState,
    ) -> bool {
        let menu = *key == Key::Named(NamedKey::ContextMenu)
            || (modifiers.shift_key() && *key == Key::Named(NamedKey::F10));
        menu && self.open_menu_from_keyboard()
    }

    pub(super) fn open_menu_from_keyboard(&mut self) -> bool {
        if !self.menus.offers_row_menu() {
            return false;
        }
        let Some(key) = self.focused_row_key() else {
            let inset = f64::from(crate::tokens::Spacing::BASE.s6);
            return self
                .open_context_menu(None, Point::new(self.rect.x0 + inset, self.rect.y0 + inset));
        };
        let Some(index) = self.model.index_of(&key) else {
            return false;
        };
        let point = self
            .row_box(index)
            .map_or(self.rect.center(), |row| row.center());
        self.open_row_menu(key, point)
    }
}
