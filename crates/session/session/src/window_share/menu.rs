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

use serde_json::Value;
use tilo_desktop_model::TabId;
use tilo_desktop_surface::{DesktopHandle, WorkspaceShare, WorkspaceShares};

use super::{SHARES_RESOURCE, WindowShares};
use crate::keys::Invocation;
use crate::sync::Client;

impl WindowShares {
    /// What the resource's reading says of sharing the window of `tab` : shared while it is among
    /// the windows shared or waiting to be, asked for by as many devices as the reading names
    /// viewers of it, and going out to them while the reading says so (`streaming`).
    fn share_of(&self, tab: TabId) -> WorkspaceShare {
        let reading = self.reading();
        let tab = tab.to_string();
        let of_tab = |rows: &str| {
            reading
                .get(rows)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|row| row.get("tab").and_then(Value::as_str) == Some(tab.as_str()))
        };
        match of_tab("sharing") {
            Some(shared) => WorkspaceShare {
                shared: true,
                viewers: shared
                    .get("viewers")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len),
                streaming: shared
                    .get("streaming")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
            None => WorkspaceShare {
                shared: of_tab("starting").is_some(),
                viewers: 0,
                streaming: false,
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
        sync.as_ref()?.workspace_of(workspace)?;
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

    use super::super::{Asked, Paged, Starting};
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
                streaming: false,
            }
        );
        assert_eq!(shares.share_of(8), WorkspaceShare::default());
    }

    /// A device that asked to see a window is not one that sees it. While nothing of the window
    /// goes out, the bar is told the device waits and the menu counts it as waiting; once the
    /// shares note that the window goes out, both say so. The reading, the menu and the bar give
    /// one answer, the reading's.
    #[test]
    fn a_device_that_asked_for_a_window_nothing_went_out_of_is_not_said_to_see_it() {
        const TAB: TabId = 7;
        const VIEWER: u64 = 0x9ab6;
        let mut shares = WindowShares::default();
        shares.pages.insert(
            TAB,
            Paged {
                tab: TAB,
                window: "window".to_owned(),
                app_id: "so.tilo.browser".to_owned(),
                workspace: WORKSPACE.to_owned(),
                ws: tilo_desktop_model::WorkspaceId::parse(WORKSPACE).expect("a workspace id"),
                width: 800,
                height: 600,
                announced: std::collections::BTreeMap::new(),
            },
        );
        shares.asked.insert(
            (TAB, VIEWER),
            Asked {
                device: iroh::SecretKey::from_bytes(&[5; 32]).public(),
                at: std::time::Instant::now(),
                decode: Vec::new(),
                name: "Ada".to_owned(),
            },
        );
        // What the reading, the menu and the bar each say of whether the window goes out.
        let said = |shares: &WindowShares| {
            let reading = shares.reading()["sharing"][0]["streaming"].as_bool();
            let told: Vec<(u64, bool)> = shares
                .exposures()
                .iter()
                .map(|viewer| (viewer.peer, viewer.streaming))
                .collect();
            (reading, shares.share_of(TAB), told)
        };
        let asked = |streaming| WorkspaceShare {
            shared: true,
            viewers: 1,
            streaming,
        };

        assert_eq!(
            said(&shares),
            (Some(false), asked(false), vec![(VIEWER, false)]),
            "nothing of the window went out: the device that asked for it waits"
        );
        shares.streaming.insert(TAB);
        assert_eq!(
            said(&shares),
            (Some(true), asked(true), vec![(VIEWER, true)]),
            "the window goes out: the device watches it"
        );
    }
}
