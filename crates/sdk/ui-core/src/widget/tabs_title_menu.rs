//! Slots that are an overflow menu alone, in the title's row.
//!
//! The desktop puts items of its own in every window's overflow menu
//! ("Share to channel…", [`crate::window_slots::WindowSlots::set_host_items`]).
//! A window whose app declared no bar then carries those items as its only
//! slots ([`WindowToolbar::menu_only`]): they add no row under the tabs and
//! do not take the title's place. The "More actions" button stands at the
//! end of the tabs' row, before the window's controls, and the tabs and the
//! title keep the rest of the row.

use super::{Rect, TabBar};
use crate::widget::window_controls::{CONTROL_GAP, CONTROL_PX};
use crate::window_toolbar::WindowToolbar;

impl TabBar {
    /// The active tab's slots where they are more than an overflow menu:
    /// slots that take the title's row, or a row of their own under the
    /// tabs.
    pub(super) fn active_slots(&self) -> Option<&WindowToolbar> {
        self.active_toolbar().filter(|toolbar| !toolbar.menu_only())
    }

    /// Whether the active tab's slots are an overflow menu alone.
    fn menu_only(&self) -> bool {
        self.active_toolbar().is_some_and(WindowToolbar::menu_only)
    }

    /// Where what stands in the tabs' row `row` ends: before the window's
    /// controls, and before the menu button of slots that are a menu alone.
    pub(super) fn tab_row_limit(&self, row: Rect) -> f64 {
        let limit = self.controls.content_limit(row);
        if self.menu_only() {
            limit - CONTROL_PX - CONTROL_GAP
        } else {
            limit
        }
    }

    /// Where slots that are a menu alone stand: the menu button's room at
    /// the end of the tabs' row `tab_row`. `None` for any other slots.
    pub(super) fn menu_button_row(&self, tab_row: Rect) -> Option<Rect> {
        if !self.menu_only() {
            return None;
        }
        let x1 = self.controls.content_limit(tab_row);
        let x0 = (x1 - CONTROL_PX).max(tab_row.x0);
        Some(Rect::new(x0, tab_row.y0, x1.max(x0), tab_row.y1))
    }
}

#[cfg(test)]
#[path = "tabs_title_menu_tests.rs"]
mod tests;
