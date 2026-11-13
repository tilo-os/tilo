//! Windows that are a person's own, whatever desk they open on.
//!
//! Some apps show a person's own things rather than a workspace's: Settings
//! holds their appearance, their devices and access, their keys and billing,
//! kept in their own records whichever workspace the window opens in. A window
//! of such an app belongs to the person who opened it. It stands on their own
//! desk, and a workspace's shared desktop never holds it: nobody else in the
//! workspace sees it, opens it, or finds it on their desk later.
//!
//! The rule holds on both sides of a shared desktop: what a device writes into
//! a workspace's layout leaves these windows out, and what it reads from one
//! never shows them, so a window an older build placed there disappears for
//! everyone and the next write takes it out of the record.

use crate::layout::{Tab, TileContent};
use crate::ownership::SharedDesktop;
use crate::tree::DesktopTree;

/// The apps whose windows are a person's own: their profile, devices and
/// access, keys and billing are pages of Settings.
pub const PERSONAL_APPS: &[&str] = &["so.tilo.settings"];

/// Whether a window of `app_id` is a person's own.
#[must_use]
pub fn is_personal_app(app_id: &str) -> bool {
    PERSONAL_APPS
        .iter()
        .any(|personal| personal.eq_ignore_ascii_case(app_id))
}

/// Whether `tile` shows an app whose windows are a person's own.
#[must_use]
pub fn is_personal_tile(tile: &TileContent) -> bool {
    crate::apps::canonical_app_id(tile).is_some_and(|app| is_personal_app(app.as_str()))
}

impl Tab {
    /// Whether this window is the person's own who opened it ([`self`](crate::personal)):
    /// it is never part of a workspace's shared desktop.
    #[must_use]
    pub fn is_personal(&self) -> bool {
        is_personal_tile(&self.tile)
    }
}

/// The ids of every personal window `tree` holds.
#[must_use]
pub fn personal_tabs(tree: &DesktopTree) -> Vec<crate::TabId> {
    tree.tabs()
        .filter(|tab| tab.is_personal())
        .map(|tab| tab.id)
        .collect()
}

/// `tree` without its personal windows: a group left empty goes with them.
pub fn leave_out_of_tree(tree: &mut DesktopTree) {
    for tab in personal_tabs(tree) {
        tree.remove_tab(tab);
    }
}

/// `desktop` as a workspace shares it: without its personal windows, nor the
/// record, maker or agent run noted for them.
pub fn leave_out(desktop: &mut SharedDesktop) {
    let personal = personal_tabs(&desktop.tree);
    if personal.is_empty() {
        return;
    }
    for tab in &personal {
        desktop.tree.remove_tab(*tab);
    }
    desktop.records.retain(|(tab, _)| !personal.contains(tab));
    desktop.authors.retain(|id, _| !personal.contains(id));
    desktop.opened_by.retain(|tab, _| !personal.contains(tab));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Strip, WindowGroup};
    use crate::{StripId, WindowWidth, Workspace, WorkspaceSlot};

    fn window(id: u64, app: &str) -> Tab {
        Tab {
            id,
            title: app.into(),
            tile: TileContent::LocalApp {
                app_id: app.into(),
                label: app.into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }
    }

    fn desktop() -> SharedDesktop {
        let mut workspace = Workspace::new(WorkspaceSlot(1), "Lounge");
        let mut strip = Strip::new(StripId(10), "Main");
        for (group, tab) in [
            (100, window(1, "so.tilo.chat")),
            (101, window(2, "so.tilo.settings")),
        ] {
            strip.window_groups.push(WindowGroup {
                id: group,
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            });
        }
        workspace.strips.push(strip);
        SharedDesktop {
            tree: DesktopTree {
                active: workspace.slot,
                workspaces: vec![workspace],
                ..DesktopTree::default()
            },
            records: vec![(1, String::new()), (2, "settings-of-ada".into())],
            authors: [(1, "ada".to_owned()), (2, "ada".to_owned())].into(),
            opened_by: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn settings_is_personal_and_chat_is_not() {
        assert!(is_personal_app("so.tilo.settings"));
        assert!(!is_personal_app("so.tilo.chat"));
    }

    #[test]
    fn a_shared_desktop_leaves_out_a_persons_own_window_and_what_it_noted() {
        let mut shared = desktop();
        leave_out(&mut shared);
        let left: Vec<u64> = shared.tree.tabs().map(|tab| tab.id).collect();
        assert_eq!(left, vec![1], "the chat stays, the settings window goes");
        assert_eq!(
            shared.tree.workspaces[0].strips[0].window_groups.len(),
            1,
            "its group goes with it"
        );
        assert_eq!(shared.records, vec![(1, String::new())]);
        assert!(!shared.authors.contains_key(&2));
    }
}
