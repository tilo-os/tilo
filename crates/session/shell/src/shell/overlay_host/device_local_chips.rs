//! A window of a host or unconfined program never reaches a room
//! ([`crate::shell::state::is_unconfined`]). In a public workspace, where
//! strangers share the room, its bar says so: a quiet "Only on this device"
//! chip beside its pill ([`ui::widget::TabBar::device_local_chip`]).

use std::collections::HashSet;

use ui::widget::GroupRow;

use super::ShellOverlayHost;
use crate::shell::state::{TabId, WorkspaceTree, is_unconfined};

/// The tabs of `tree` whose bar shows the chip: each window of a host or
/// unconfined program in a workspace whose room is public.
pub(crate) fn device_local_tabs(tree: &WorkspaceTree) -> HashSet<TabId> {
    tree.desktop
        .workspaces
        .iter()
        .filter(|workspace| tree.room_is_public(workspace.room))
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter(|tab| is_unconfined(tab, &tree.package_launches))
        .map(|tab| tab.id)
        .collect()
}

impl ShellOverlayHost {
    /// Every window bar marks the tabs that stay on this device. Runs every
    /// frame; a group whose marks did not change does nothing.
    pub(super) fn sync_device_local_chips(&mut self) {
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        let local = {
            let chrome = chrome.borrow();
            let tree = chrome.tree();
            device_local_tabs(&tree)
        };
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                let ids: Vec<u64> = group.tabs().iter().map(|tab| tab.id).collect();
                for id in ids {
                    group.set_tab_device_local(id, local.contains(&id));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Tab, TileContent};

    // USER REQUIREMENT (2026-10-20): "In a public workspace its window bar shows a quiet 'Only on this device' chip, using the design-system Chip."
    /// Only an unconfined window in a public workspace shows the chip: not
    /// a Tilo app launched as its package, and nothing in a private
    /// workspace.
    #[test]
    fn only_unconfined_windows_in_a_public_workspace_show_the_chip() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.desktop.workspaces[0].room;
        let (terminal, notes) = {
            let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
            let terminal = group.tabs[0].id;
            group.tabs[0].tile = TileContent::Foreign {
                app_id: "Alacritty".into(),
                surface_id: 1,
            };
            let notes = terminal.wrapping_add(1_000);
            group.tabs.push(Tab {
                id: notes,
                title: "Groceries".into(),
                tile: TileContent::Foreign {
                    app_id: "so.tilo.notes".into(),
                    surface_id: 2,
                },
            });
            (terminal, notes)
        };
        tree.package_launches.insert(notes, "so.tilo.notes".into());
        assert!(device_local_tabs(&tree).is_empty(), "a private workspace");

        let mut doc = automerge::AutoCommit::new();
        automerge::transaction::Transactable::put(
            &mut doc,
            automerge::ROOT,
            tilo_desktop_model::projection::ROOM_PUBLIC_KEY,
            true,
        )
        .unwrap();
        tree.room_docs.insert(room, doc);
        assert_eq!(device_local_tabs(&tree), HashSet::from([terminal]));
    }
}
