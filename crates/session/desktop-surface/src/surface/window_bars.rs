//! The slots the apps this desktop runs put in their windows' bars.
//!
//! Every app here runs in the host's process, each on the window its tile
//! is shown in ([`ui::hosted_window::HostedWindow`], given as the tile is
//! built): the desktop shows what each declared in its tab's bar and hands
//! it what the person did there, through the toolkit's
//! [`ui::window_slots::WindowSlots`], as the session's shell does.

use super::{DesktopSurface, route_of};
use crate::navigation::Admitted;
use ui::hosted_window::HostedWindow;

impl DesktopSurface {
    /// Show every window's declared slots in its bar, hand each app what
    /// the person did with them, and close the tabs whose app asked to.
    /// Returns whether the desktop shows something new or an app has
    /// something to hear.
    pub(super) fn sync_window_bars(&mut self) -> bool {
        let mut windows: Vec<(u64, HostedWindow)> = Vec::new();
        for group in self.rows.values_mut().flat_map(|row| row.groups.iter_mut()) {
            let ids: Vec<u64> = group.tabs().iter().map(|tab| tab.id).collect();
            for id in ids {
                if let Some(window) = group
                    .host_for_tab_mut(id)
                    .and_then(|host| host.hosted_window())
                {
                    windows.push((id, window.clone()));
                }
            }
        }
        let sync = self.window_slots.sync_hosted(
            &windows,
            self.rows.values_mut().flat_map(|row| row.groups.iter_mut()),
        );
        if !sync.closing.is_empty() {
            let mut shared = self.shared.borrow_mut();
            for tab in sync.closing {
                if let Some(ids) = route_of(&shared.tree, tab) {
                    shared.queue.push(Admitted::Close(ids));
                }
            }
        }
        sync.changed
    }
}

#[cfg(test)]
#[path = "window_bars_tests.rs"]
mod tests;
