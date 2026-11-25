//! A window shared with its workspace, as the window's menu offers it.
//!
//! Sharing a window with its workspace shows its live picture in the tab that stands for it on
//! the person's other devices and to the workspace's members. Only a window whose contents live in
//! its process here needs it: a Tilo app's window opens on every device on the record it shows, so
//! the session reads no share for it and its menu offers none. The share is the host session's
//! (its window-share resource): the shell keeps nothing of it. What a window's menu says is what
//! the session reads of the share when the menu is asked for ([`WorkspaceShares`]), and choosing
//! the row leaves the ask for the session ([`DesktopHandle::take_workspace_share_asks`]), which
//! invokes the share's own action with the tab: the action that checks the workspace's
//! governance is the only way a window is shared.
//!
//! The row stands only where the share can work: a window this device maps
//! ([`tilo_desktop_model::window_share::mapped_window`]) in a tab it minted, in a workspace the
//! session takes part in, of an app whose window the others cannot open themselves. Another device's window, the picture of one, and a shell whose host
//! shares nothing offer no such row.

use std::rc::Rc;

use tilo_desktop_model::window_share::{MappedWindow, mapped_window};
use tilo_desktop_model::{TabId, peer_salt_of};

use super::DesktopHandle;

/// What the host's session reads of one window's share with the workspace it stands in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceShare {
    /// The session shares the window, or is starting to.
    pub shared: bool,
    /// How many devices of the workspace asked to see it.
    pub viewers: usize,
    /// Whether anything of the window goes out to those devices now, as the share's reading says
    /// (`streaming`). While nothing does they wait for it, and the row says so: it counts nobody
    /// as watching who sees nothing.
    pub streaming: bool,
}

/// The host session's reading of its window shares: what it says now of the window this device
/// maps in the tab. `None` when the session shares nothing of it: it takes no part in the
/// window's workspace, or the window opens on every device of it anyway.
pub type WorkspaceShares = Rc<dyn Fn(TabId, MappedWindow<'_>) -> Option<WorkspaceShare>>;

impl DesktopHandle {
    /// Read the windows' shares with their workspaces from `shares`, the host session's: each
    /// window this device can share offers the share in its menu, as `shares` says it stands when
    /// the menu is asked for. A shell nobody gave a reading offers no such row.
    pub fn reads_workspace_shares(&self, shares: WorkspaceShares) {
        self.shared.borrow_mut().workspace_shares = Some(shares);
    }

    /// The windows whose menu asked for their share with the workspace since the last call, in
    /// the order they asked: the tab, whether to share it (`true`) or stop, and the compositor's
    /// serial of the person's press that chose the row, when one did. The host invokes its window
    /// share with each, as that press.
    pub fn take_workspace_share_asks(&self) -> Vec<(TabId, bool, Option<u32>)> {
        std::mem::take(&mut self.shared.borrow_mut().workspace_share_asks)
    }

    /// What the session reads of sharing the window `tab` shows with its workspace. `None` for a
    /// window that cannot be shared from here: one this device does not map, a tab another
    /// device minted, a workspace the session takes no part in, a window every device opens, or a
    /// shell with no reading.
    pub fn workspace_share(&self, tab: TabId) -> Option<WorkspaceShare> {
        let shared = self.shared.borrow();
        let reading = shared.workspace_shares.clone()?;
        // A tab another device minted is that device's window to share, whatever it shows here.
        if peer_salt_of(tab) != shared.minter.salt() {
            return None;
        }
        let window = mapped_window(&shared.tree.workspaces, tab)?;
        reading(tab, window)
    }

    /// Leave the ask to share the window `tab` shows with its workspace, or to stop, for the
    /// host session.
    pub fn ask_workspace_share(&self, tab: TabId, share: bool) {
        let mut shared = self.shared.borrow_mut();
        shared
            .workspace_share_asks
            .push((tab, share, crate::surface::handled_press()));
        shared.wake.wake();
    }
}
