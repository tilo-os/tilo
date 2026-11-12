//! The shares as a window's menu reads them, and what the menu asks of them.
//!
//! A window's menu offers to share the window with its workspace, or to stop. The desk keeps
//! nothing of the share: what the row says is what the shares' resource reads when the menu is
//! asked for ([`WindowShares::reading`]), and choosing the row invokes the resource's own action
//! with the tab, through the door every caller uses. The menu is one more caller of
//! `session:window-share`, so a share started from it is checked, announced and ended exactly as
//! one a script or an agent starts.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_desktop_model::TabId;
use tilo_desktop_surface::{DesktopHandle, WorkspaceShare, WorkspaceShares};

use super::{SHARES_RESOURCE, WindowShares};
use crate::keys::Invocation;
use crate::sync::Client;

impl WindowShares {
    /// What the resource's reading says of sharing the window of `tab` : shared while it is among
    /// the windows shared or waiting to be, and viewed by as many devices as the reading names
    /// viewers of it.
    fn share_of(&self, tab: TabId) -> WorkspaceShare {
        let reading = self.reading();
        let tab = tab.to_string();
        let of_tab = |rows: &str| {
            reading[rows]
                .as_array()
                .into_iter()
                .flatten()
                .find(|row| row["tab"].as_str() == Some(tab.as_str()))
        };
        match of_tab("sharing") {
            Some(shared) => WorkspaceShare {
                shared: true,
                viewers: shared["viewers"].as_array().map_or(0, Vec::len),
            },
            None => WorkspaceShare {
                shared: of_tab("starting").is_some(),
                viewers: 0,
            },
        }
    }
}

/// What the desk's window menus read of `shares` : how the resource's reading says each window's
/// share stands when its menu is asked for. A session that takes no part in a workspace (`sync`
/// holds no topic of it) shares nothing with it, so the windows there offer no share.
pub(crate) fn reading_for_menus(
    shares: &Rc<RefCell<WindowShares>>,
    sync: Option<&Rc<Client>>,
) -> WorkspaceShares {
    let shares = Rc::clone(shares);
    let sync = sync.cloned();
    Rc::new(move |tab, workspace| {
        sync.as_ref()?.workspace_topic(workspace)?;
        // A menu is never asked for while the shares take their pass; were it, the window would
        // offer no share for that one reading rather than a wrong one.
        let shares = shares.try_borrow().ok()?;
        Some(shares.share_of(tab))
    })
}

/// What the windows' menus asked of the shares since the last pass, as the resource's published
/// actions: `share` with the tab, or `stop` with it.
pub(crate) fn follow_asks(desk: &DesktopHandle) -> Vec<Invocation> {
    desk.take_workspace_share_asks()
        .into_iter()
        .map(|(tab, share)| {
            Invocation::new(SHARES_RESOURCE, if share { "share" } else { "stop" })
                .with("tab", tab.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::Starting;
    use super::*;

    const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";

    /// A session with no collaboration plane takes part in no workspace: it reads no share for a
    /// window's menu, so no window offers one.
    #[test]
    fn a_session_in_no_workspace_reads_no_share_for_a_menu() {
        let shares = Rc::new(RefCell::new(WindowShares::default()));
        let reading = reading_for_menus(&shares, None);
        assert_eq!(reading(7, WORKSPACE), None);
    }

    /// A window whose share is still starting reads as shared, with nobody watching yet: its menu
    /// offers the stop, which takes the wait back. A window the reading does not name is not
    /// shared.
    #[test]
    fn a_window_whose_share_is_starting_reads_as_shared() {
        let mut shares = WindowShares::default();
        assert_eq!(shares.share_of(7), WorkspaceShare::default());
        shares.starting.push(Starting {
            window: "window".to_owned(),
            tab: 7,
            app_id: "so.tilo.notes".to_owned(),
            workspace: WORKSPACE.to_owned(),
            waiting_for: String::new(),
            controlled: BTreeSet::new(),
        });
        assert_eq!(
            shares.share_of(7),
            WorkspaceShare {
                shared: true,
                viewers: 0,
            }
        );
        assert_eq!(shares.share_of(8), WorkspaceShare::default());
    }
}
