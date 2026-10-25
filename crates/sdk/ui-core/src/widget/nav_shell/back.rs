//! Back: the inspector's sheet first, then the app's page stack, then a
//! narrow window's push stack. The window bar's Back, Escape and the
//! system's back gesture all take the same first step: a sheet the person
//! raised is lowered or put away before anything navigates.

use super::{Column, NavShell, stack_index};
use crate::gesture::{NavAvailability, NavDirection};
use crate::widget::{AppModel, ModelPath, Widget};

impl NavShell {
    /// Go back one step: lower or put away the inspector's sheet
    /// ([`Self::sheet_back`]), else go back one screen, the app's page
    /// stack first ([`Self::with_back`]), then a narrow window's push
    /// stack. Returns whether it did.
    pub fn back(&mut self) -> bool {
        if self.sheet_back() {
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

    pub(super) fn pop_push_stack(&mut self) -> bool {
        if self.geometry.back_column.is_none() {
            return false;
        }
        let stack = self.geometry.stack.clone();
        let front = stack_index(&stack, self.front_pane());
        let Some(previous) = front.checked_sub(1).and_then(|i| stack.get(i).copied()) else {
            return false;
        };
        self.show(previous);
        true
    }

    /// The system's back gesture: the sheet's step first, then what the
    /// shell frames.
    pub(super) fn nav_gesture(
        &mut self,
        dir: NavDirection,
        state: &AppModel,
        path: &ModelPath,
    ) -> bool {
        if dir == NavDirection::Back && self.sheet_back() {
            return true;
        }
        crate::widget::defaults::on_nav_gesture(self.children_mut(), dir, state, path)
    }

    pub(super) fn nav_gesture_availability(&self) -> NavAvailability {
        let own = NavAvailability {
            back: self.sheet_shown(),
            forward: false,
        };
        own.or(crate::widget::defaults::nav_availability(self.children()))
    }
}
