//! What the desk says about the apps that stopped answering, and what the person chose to do about
//! them.
//! The compositor pings each app it shows and tells the host which windows went quiet; the host
//! names them here, by the compositor's id for each window
//! ([`DesktopHandle::set_unresponsive_windows`]). From then on those tabs read as stopped answering
//! — in the published desk reading every view reads, and in the card drawn over the window
//! ([`super::hung`]). The person's answer comes back the same way:
//! [`DesktopHandle::take_liveness_choices`] hands it to the host that owns the compositor, whether
//! it was pressed on the card, asked by the desk's own verbs, or sent by a script.

use std::collections::BTreeSet;

use tilo_desktop_model::TabId;

use super::DesktopHandle;

/// What the person chose on a window that says its app stopped responding: keep waiting for it, or
/// quit it now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LivenessChoice {
    /// Keep waiting: the app gets another period to answer.
    Wait,
    /// Force quit: the app ends, and its window with it.
    ForceQuit,
}

impl DesktopHandle {
    /// The windows whose app stopped answering, by the compositor's id for each: the host names
    /// them as the compositor judges, and from then on their tabs say so and offer to wait for the
    /// app or quit it. Returns whether that changed anything.
    pub fn set_unresponsive_windows(&self, windows: Vec<u64>) -> bool {
        let mut shared = self.shared.borrow_mut();
        let windows: BTreeSet<u64> = windows.into_iter().collect();
        if shared.unresponsive == windows {
            return false;
        }
        shared.unresponsive = windows;
        shared.unresponsive_revision = shared.unresponsive_revision.wrapping_add(1);
        shared.wake.wake();
        true
    }

    /// Whether the app of the window the compositor knows as `window` stopped answering, as the
    /// host last said.
    #[must_use]
    pub fn is_unresponsive(&self, window: u64) -> bool {
        self.shared.borrow().unresponsive.contains(&window)
    }

    /// Changes whenever the set of windows whose app stopped answering may
    /// ([`DesktopHandle::set_unresponsive_windows`]).
    #[must_use]
    pub fn unresponsive_revision(&self) -> u64 {
        self.shared.borrow().unresponsive_revision
    }

    /// The tabs whose app stopped answering, as the host last said: the windows the desk offers to
    /// wait for or quit.
    #[must_use]
    pub fn unresponsive_tabs(&self) -> Vec<TabId> {
        let shared = self.shared.borrow();
        if shared.unresponsive.is_empty() {
            return Vec::new();
        }
        shared
            .tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter(|tab| {
                matches!(
                    &tab.tile,
                    tilo_desktop_model::TileContent::Foreign { surface_id, .. }
                        if shared.unresponsive.contains(surface_id)
                )
            })
            .map(|tab| tab.id)
            .collect()
    }

    /// What the person chose on the cards of windows that stopped answering since the last call:
    /// the host keeps waiting for its app, or quits it.
    pub fn take_liveness_choices(&self) -> Vec<(TabId, LivenessChoice)> {
        std::mem::take(&mut self.shared.borrow_mut().liveness)
    }

    /// The person chose `choice` on the window of `tab` , as the desk's own verbs do: the host
    /// carries it out with the cards' choices.
    pub fn liveness_choice(&self, tab: TabId, choice: LivenessChoice) {
        let mut shared = self.shared.borrow_mut();
        shared.liveness.push((tab, choice));
        shared.wake.wake();
    }

    /// The compositor's window id for the window showing `tab` , when this device runs its app:
    /// what the compositor pings and what it ends.
    #[must_use]
    pub fn window_of_tab(&self, tab: TabId) -> Option<u64> {
        self.shared.borrow().window_of_tab(tab)
    }
}

impl super::Shared {
    /// The compositor's window id for the window showing `tab` , when a window of an app this
    /// device runs fills it.
    pub(crate) fn window_of_tab(&self, tab: TabId) -> Option<u64> {
        self.tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|held| held.id == tab)
            .and_then(|held| match &held.tile {
                tilo_desktop_model::TileContent::Foreign { surface_id, .. } => Some(*surface_id),
                _ => None,
            })
    }
}
