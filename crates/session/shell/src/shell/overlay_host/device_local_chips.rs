//! A window of a host or unconfined program never reaches a room
//! with anyone else in it ([`crate::shell::state::is_unconfined`],
//! [`crate::shell::state::is_one_persons`]). In a workspace shared with
//! others its bar says so: a quiet "Only on this device" chip beside its
//! pill ([`ui::widget::TabBar::device_local_chip`]).

use std::collections::HashSet;

use ui::widget::GroupRow;

use super::ShellOverlayHost;
use crate::shell::state::{TabId, WorkspaceTree, is_one_persons, is_unconfined};

/// The tabs whose bar shows the chip, as of one tree generation.
#[derive(Default)]
pub(crate) struct DeviceLocal {
    generation: Option<u64>,
    tabs: HashSet<TabId>,
}

/// The tabs of `tree` whose bar shows the chip: each window of a host or
/// unconfined program in a workspace shared with others (its room has
/// members besides its owner).
pub(crate) fn device_local_tabs(tree: &WorkspaceTree) -> HashSet<TabId> {
    tree.desktop
        .workspaces
        .iter()
        .filter(|workspace| !is_one_persons(workspace))
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter(|tab| is_unconfined(tab, &tree.package_launches))
        .map(|tab| tab.id)
        .collect()
}

impl ShellOverlayHost {
    /// Every window bar marks the tabs that stay on this device. Runs every
    /// frame: the tabs are read again only once the tree changed, and a
    /// group whose marks did not change does nothing. Whether a bar changed.
    pub(super) fn sync_device_local_chips(&mut self) -> bool {
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        {
            let chrome = chrome.borrow();
            let generation = chrome.tree_handle().generation();
            if self.device_local.generation != Some(generation) {
                self.device_local.tabs = device_local_tabs(&chrome.tree());
                self.device_local.generation = Some(generation);
            }
        }
        let local = &self.device_local.tabs;
        let mut changed = false;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                let ids: Vec<u64> = group.tabs().iter().map(|tab| tab.id).collect();
                for id in ids {
                    changed |= group.set_tab_device_local(id, local.contains(&id));
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Tab, TileContent};

    // USER REQUIREMENT (2026-10-20): "windows should stay private by default"
    /// Only an unconfined window in a workspace shared with others shows the
    /// chip: not a Tilo app launched as its package, and nothing in a
    /// workspace of the person's alone.
    #[test]
    fn only_unconfined_windows_in_a_shared_workspace_show_the_chip() {
        let mut tree = WorkspaceTree::bootstrap_demo();
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
        tree.desktop.workspaces[0].policy.members.clear();
        tree.desktop.workspaces[0].policy.owner = Some("cd".repeat(32));
        assert!(
            device_local_tabs(&tree).is_empty(),
            "a workspace of the person's alone"
        );

        tree.desktop.workspaces[0]
            .policy
            .members
            .insert("ab".repeat(32), tilo_action_policy::Role::Editor);
        assert_eq!(device_local_tabs(&tree), HashSet::from([terminal]));
    }
}
