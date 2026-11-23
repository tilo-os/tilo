//! Which windows have drawn what they show: the tab each window shows presented its first frame.
//! A host publishes it beside each window's own state, so a caller waits for a window to be drawn
//! rather than for a while.

use std::collections::BTreeSet;

use tilo_desktop_model::TabId;

use super::DesktopHandle;

impl DesktopHandle {
    /// The tabs whose window shows them drawn, as of the last update pass.
    #[must_use]
    pub fn drawn(&self) -> BTreeSet<TabId> {
        self.shared.borrow().drawn.clone()
    }
}
