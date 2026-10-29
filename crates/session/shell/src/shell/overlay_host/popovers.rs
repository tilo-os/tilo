//! Every menu and flyout the shell shows over the windows, as one set: the
//! stacked popovers (the tab menu, the power menu), the top bar's (the
//! workspace menu, the profile card), the screenshot receipt's "Send…"
//! menu, and each window bar's own (its overflow and action menus, the
//! location's choices and suggestions, the reactions' emoji picker).
//!
//! What follows from one being open is decided here once, for all of them:
//! it holds the seat, so Escape and the press that closes it reach the
//! shell rather than the window under it ([`ShellOverlayHost::popover_open`]);
//! Escape closes the topmost ([`ShellOverlayHost::dismiss_topmost_popover`]);
//! and when the desktop's arrangement changes under them (the canvas shown
//! or put away) every one closes ([`ShellOverlayHost::dismiss_popovers`]).
//! A new kind of menu joins by being added to these three, not by handling
//! Escape itself.

use ui::widget::GroupRow;

impl super::ShellOverlayHost {
    /// Whether any menu or flyout is open over the windows.
    pub(crate) fn popover_open(&self) -> bool {
        self.layers.popover_open()
            || self.bar_popovers.any_open()
            || self.capture_studio.send_menu_open()
            || self.window_bar_popup_open()
    }

    /// Escape: close the topmost open layer, else the top bar's popover,
    /// else a window bar's menu. Whether one closed.
    pub(super) fn dismiss_topmost_popover(&mut self) -> bool {
        if self.layers.dismiss_topmost() {
            return true;
        }
        if self.bar_popovers.any_open() {
            self.close_bar_popovers_for(None);
            self.bump_chrome();
            return true;
        }
        if self.close_window_bar_popups() {
            self.bump_chrome();
            return true;
        }
        false
    }

    /// Close every menu and flyout over the windows: what they hang from is
    /// about to stand somewhere else. Whether any was open.
    pub(super) fn dismiss_popovers(&mut self) -> bool {
        let mut closed = self.layers.dismiss_popovers();
        if self.bar_popovers.any_open() {
            self.close_bar_popovers_for(None);
            closed = true;
        }
        closed |= self.close_window_bar_popups();
        if closed {
            self.bump_chrome();
        }
        closed
    }

    /// Whether a window's bar has a menu or picker open over its window.
    pub(crate) fn window_bar_popup_open(&self) -> bool {
        self.group_row_ptrs.values().any(|&ptr| {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            row.groups
                .iter()
                .any(ui::widget::WindowGroup::bar_popup_open)
        })
    }

    /// Close every window bar's menus and pickers. Whether any was open.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "the window bars are written through `group_row_ptrs`, which is sound only under `&mut self`"
    )]
    fn close_window_bar_popups(&mut self) -> bool {
        let mut closed = false;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`; `&mut self` makes
            // the visit exclusive.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                closed |= group.close_bar_popups();
            }
        }
        closed
    }
}
