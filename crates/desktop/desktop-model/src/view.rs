//! One device's view onto its desktop: which workspace and strip it is
//! looking at, and what it has been told about each workspace's records. None
//! of this is shared — a colleague changing tabs never moves this view.

use std::collections::{BTreeMap, HashSet};

use crate::ids::WorkspaceId;
use crate::tree::Layout;
use crate::workspace::Workspace;

impl Layout {
    /// The salt replication treats as this tree's own — the id range this
    /// tree mints in, writes and sweeps: [`Self::salt`].
    pub const fn local_salt(&self) -> u16 {
        self.salt
    }

    /// The id of every workspace this device is in, deduplicated, in workspace
    /// order. A workspace whose id is still the unminted sentinel is skipped.
    pub fn workspace_ids(&self) -> Vec<WorkspaceId> {
        let mut seen = HashSet::new();
        self.workspaces
            .iter()
            .map(Workspace::id)
            .filter(|r| !r.is_nil() && seen.insert(*r))
            .collect()
    }

    /// Tell the tree which records `workspace`'s index holds. Returns whether
    /// this was news, so the caller can skip re-asking when nothing moved.
    pub fn set_workspace_records(
        &mut self,
        identity: WorkspaceId,
        held: BTreeMap<String, String>,
    ) -> bool {
        match self.workspace_records_held.get(&identity) {
            Some(existing) if *existing == held => false,
            _ => {
                self.workspace_records_held.insert(identity, held);
                true
            }
        }
    }

    pub fn set_active(&mut self, id: WorkspaceId) {
        if self.workspaces.iter().any(|w| w.id() == id) {
            self.active = id;
        }
    }

    /// Show the workspace above the active one. `false` at the top.
    pub fn switch_up(&mut self) -> bool {
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.id() == self.active)
            .unwrap_or(0);
        let Some(prev) = idx.checked_sub(1).and_then(|i| self.workspaces.get(i)) else {
            return false;
        };
        self.active = prev.id();
        true
    }

    /// Show the workspace below the active one. `false` at the bottom.
    pub fn switch_down(&mut self) -> bool {
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.id() == self.active)
            .unwrap_or(0);
        let Some(next) = self.workspaces.get(idx.saturating_add(1)) else {
            return false;
        };
        self.active = next.id();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    tilo_test_env::scenario! {
        fn a_layouts_local_view;

        phase switching_walks_workspaces_and_clamps_at_the_ends;
        {
            let mut tree = Layout {
                workspaces: vec![
                    Workspace::new(WorkspaceId::from_u128(1), "A"),
                    Workspace::new(WorkspaceId::from_u128(2), "B"),
                ],
                active: WorkspaceId::from_u128(1),
                ..Layout::default()
            };
            assert!(!tree.switch_up());
            assert!(tree.switch_down());
            assert_eq!(tree.active, WorkspaceId::from_u128(2));
            assert!(!tree.switch_down());
            tree.set_active(WorkspaceId::from_u128(9));
            assert_eq!(tree.active, WorkspaceId::from_u128(2));
            assert_eq!(tree.workspace_ids().len(), 2);
        }

        phase workspace_records_report_only_news;
        {
            let mut tree = Layout::default();
            let identity = WorkspaceId::new();
            let held = BTreeMap::from([("r".to_owned(), "doc".to_owned())]);
            assert!(tree.set_workspace_records(identity, held.clone()));
            assert!(!tree.set_workspace_records(identity, held));
            assert_eq!(tree.local_salt(), 0);
            tree.salt = 3;
            assert_eq!(tree.local_salt(), 3);
        }
    }
}
