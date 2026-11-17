//! Which windows have drawn what they show: the tab each window shows presented its first frame.
//! A host publishes it beside each window's own state, so a caller waits for a window to be drawn
//! rather than for a while.

use std::collections::BTreeSet;

use tilo_desktop_model::TabId;

use super::{DesktopHandle, DesktopSurface};

impl DesktopHandle {
    /// The tabs whose window shows them drawn, as of the last update pass.
    #[must_use]
    pub fn drawn(&self) -> BTreeSet<TabId> {
        self.shared.borrow().drawn.clone()
    }
}

impl DesktopSurface {
    /// Note which windows' shown tabs have presented, answering whether that changed.
    pub(super) fn sync_drawn(&self) -> bool {
        let drawn: BTreeSet<TabId> = self
            .rows
            .values()
            .flat_map(|row| row.groups.iter())
            .filter(|group| !group.is_closing())
            .filter(|group| group.active_host().is_some_and(ui::Widget::presented))
            .map(ui::widget::WindowGroup::active_tab_id)
            .collect();
        let mut shared = self.shared.borrow_mut();
        if shared.drawn == drawn {
            return false;
        }
        shared.drawn = drawn;
        true
    }
}
