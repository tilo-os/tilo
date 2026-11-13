//! Slots that are an overflow menu alone, in the title's row.
use super::{Rect, TabBar};
use crate::widget::window_controls::{CONTROL_GAP, CONTROL_PX};
use crate::window_toolbar::WindowToolbar;

impl TabBar {
    pub(super) fn active_slots(&self) -> Option<&WindowToolbar> {
        self.active_toolbar().filter(|toolbar| !toolbar.menu_only())
    }

    fn menu_only(&self) -> bool {
        self.active_toolbar().is_some_and(WindowToolbar::menu_only)
    }

    pub(super) fn tab_row_limit(&self, row: Rect) -> f64 {
        let limit = self.trailing_limit(row);
        if self.menu_only() {
            limit - CONTROL_PX - CONTROL_GAP
        } else {
            limit
        }
    }

    pub(super) fn menu_button_row(&self, tab_row: Rect) -> Option<Rect> {
        if !self.menu_only() {
            return None;
        }
        let x1 = self.trailing_limit(tab_row);
        let x0 = (x1 - CONTROL_PX).max(tab_row.x0);
        Some(Rect::new(x0, tab_row.y0, x1.max(x0), tab_row.y1))
    }
}

#[cfg(test)]
#[path = "tabs_title_menu_tests.rs"]
mod tests;
