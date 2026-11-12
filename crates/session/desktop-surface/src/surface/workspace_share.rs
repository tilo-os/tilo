//! A window shared with its workspace, as the window's menu offers it.
//!
//! Sharing a window with its workspace shows its live picture in the tab that stands for it on
//! the person's other devices and to the workspace's members. The share is the host session's
//! (its window-share resource): the desk keeps nothing of it. What a window's menu says is what
//! the session reads of the share when the menu is asked for ([`WorkspaceShares`]), and choosing
//! the row leaves the ask for the session ([`DesktopHandle::take_workspace_share_asks`]), which
//! invokes the share's own action with the tab: the action that checks the workspace's
//! governance is the only way a window is shared.
//!
//! The row stands only where the share can work: a window this device maps
//! ([`tilo_desktop_model::window_share::mapped_window`]) in a tab it minted, in a workspace the
//! session takes part in. Another device's window, the picture of one, and a desk whose host
//! shares nothing offer no such row.

use std::rc::Rc;

use tilo_desktop_model::window_share::mapped_window;
use tilo_desktop_model::{TabId, peer_salt_of};

use super::DesktopHandle;

/// What the host's session reads of one window's share with the workspace it stands in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceShare {
    /// The session shares the window, or is starting to.
    pub shared: bool,
    /// How many devices of the workspace view it now.
    pub viewers: usize,
}

/// The host session's reading of its window shares: what it says now of the window in the tab,
/// which stands in the workspace named by its id. `None` when the session shares nothing with
/// that workspace, because it takes no part in it.
pub type WorkspaceShares = Rc<dyn Fn(TabId, &str) -> Option<WorkspaceShare>>;

impl DesktopHandle {
    /// Read the windows' shares with their workspaces from `shares`, the host session's: each
    /// window this device can share offers the share in its menu, as `shares` says it stands when
    /// the menu is asked for. A desk nobody gave a reading offers no such row.
    pub fn reads_workspace_shares(&self, shares: WorkspaceShares) {
        self.shared.borrow_mut().workspace_shares = Some(shares);
    }

    /// The windows whose menu asked for their share with the workspace since the last call, in
    /// the order they asked: the tab, and whether to share it (`true`) or stop. The host invokes
    /// its window share with each.
    pub fn take_workspace_share_asks(&self) -> Vec<(TabId, bool)> {
        std::mem::take(&mut self.shared.borrow_mut().workspace_share_asks)
    }

    /// What the session reads of sharing the window `tab` shows with its workspace. `None` for a
    /// window that cannot be shared from here: one this device does not map, a tab another
    /// device minted, a workspace the session takes no part in, or a desk with no reading.
    pub(super) fn workspace_share(&self, tab: TabId) -> Option<WorkspaceShare> {
        let (reading, workspace) = {
            let shared = self.shared.borrow();
            let reading = shared.workspace_shares.clone()?;
            // A tab another device minted is that device's window to share, whatever it shows
            // here.
            if peer_salt_of(tab) != shared.minter.salt() {
                return None;
            }
            let window = mapped_window(&shared.tree.workspaces, tab)?;
            (reading, window.workspace.to_owned())
        };
        reading(tab, &workspace)
    }

    /// Leave the ask to share the window `tab` shows with its workspace, or to stop, for the
    /// host session.
    pub(super) fn ask_workspace_share(&self, tab: TabId, share: bool) {
        let mut shared = self.shared.borrow_mut();
        shared.workspace_share_asks.push((tab, share));
        shared.wake.wake();
    }
}
