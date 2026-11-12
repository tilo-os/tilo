//! Back: the inspector's sheet first, then the app's page stack, then a narrow window's push
//! stack, where a phone's places screen returns to the screen its toggle covered. The window
//! bar's Back, Escape and the system's back gesture all take these steps, so a sheet the person
//! raised is lowered or put away before anything navigates, and a sidebar floating over the
//! window is put away before a screen changes.

use super::{Column, NavPane, NavShell, SidebarPlacement, stack_index};
use crate::gesture::{NavAvailability, NavDirection};
use crate::widget::{AppModel, ModelPath, Widget};

impl NavShell {
    /// Go back one step: lower or put away the inspector's sheet
    /// (`Self::sheet_back`), put away a sidebar floating over the window, else go back one
    /// screen, the app's page stack first ([`Self::with_back`]), then a narrow window's push
    /// stack. Returns whether it did.
    pub fn back(&mut self) -> bool {
        if self.sheet_back() || self.drawer_back() {
            return true;
        }
        if self.inputs.borrow().content_back
            && self.geometry.back_column == Some(Column::Content)
            && let Some(back) = self.content_back.as_mut()
        {
            (back.go_back)();
            self.refresh_content_back();
            return true;
        }
        self.pop_push_stack()
    }

    /// Put away a sidebar floating over the window (a drawer), with the step landing at once as a
    /// key's does. Returns whether one was open.
    fn drawer_back(&mut self) -> bool {
        if self.geometry.sidebar != SidebarPlacement::Drawer || !self.is_sidebar_open() {
            return false;
        }
        self.toggle_by_key(Column::Sidebar);
        true
    }

    pub(super) fn pop_push_stack(&mut self) -> bool {
        if self.geometry.back_column.is_none() {
            return false;
        }
        if self.front_pane() == NavPane::Sidebar {
            // The places stand in front of the screen the window opens on: Back goes forward to the
            // screen the toggle covered, so the list the person came from is one step away.
            self.show(self.screen_behind_sidebar());
            return true;
        }
        let stack = self.geometry.stack.clone();
        let front = stack_index(&stack, self.front_pane());
        let Some(previous) = front.checked_sub(1).and_then(|i| stack.get(i).copied()) else {
            return false;
        };
        self.show(previous);
        true
    }

    /// The system's back gesture: the sheet's and the floating sidebar's steps first, then what
    /// the app's views hold of their own history, then the shell's steps — the same ones Escape
    /// and the bar's Back take.
    pub(super) fn nav_gesture(
        &mut self,
        dir: NavDirection,
        state: &AppModel,
        path: &ModelPath,
    ) -> bool {
        if dir == NavDirection::Back && (self.sheet_back() || self.drawer_back()) {
            return true;
        }
        if crate::widget::defaults::on_nav_gesture(self.children_mut(), dir, state, path) {
            return true;
        }
        dir == NavDirection::Back && self.back()
    }

    /// Whether a sidebar floats over the window (a drawer) and the person has it raised.
    fn drawer_open(&self) -> bool {
        self.geometry.sidebar == SidebarPlacement::Drawer && self.is_sidebar_open()
    }

    pub(super) fn nav_gesture_availability(&self) -> NavAvailability {
        let own = NavAvailability {
            back: self.can_go_back() || self.drawer_open(),
            forward: false,
        };
        own.or(crate::widget::defaults::nav_availability(self.children()))
    }
}
