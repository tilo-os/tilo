//! What a click on a row of a [`VirtualList`] does: it chooses the row, opens it, or opens its
//! menu.

use super::Choosing;
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
        // A row open into its item opens with a press, as a disclosure does: one gesture
        // shows what the row holds, without a second press to activate it. Where the list is
        // too narrow to hold the item under the row, the press opens it in front instead.
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

    /// Open `key` as Enter or a second click does: through the published view
    /// action when the host bound one, else the activation callback. A list whose
    /// rows open nothing chooses the row instead, as its click does.
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
        // Narrower than a compact window's width the item opens in front of the list, through
        // the host that shows it, as the code below does for a row that holds no document.
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

    /// Open the focused row's menu: the one the keyboard is on, at the middle of its row; or,
    /// with no row focused, the list's own menu, where its rows start.
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
