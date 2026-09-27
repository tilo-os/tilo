//! One device's view onto its desktop: which workspace and strip it is
//! looking at, and what it has been told about each room's records. None
//! of this is shared — a colleague changing tabs never moves this view.

use std::collections::{BTreeMap, HashSet};

use crate::ids::{RoomId, WorkspaceId};
use crate::tree::DesktopTree;

impl DesktopTree {
    /// The salt replication treats as this tree's own — the id range this
    /// tree may write and sweep: [`Self::peer_salt_override`] when set,
    /// otherwise the host's own `host_salt`.
    pub fn local_salt_or(&self, host_salt: u16) -> u16 {
        self.peer_salt_override.unwrap_or(host_salt)
    }

    /// Every room this device is in, deduplicated, in workspace order. A
    /// workspace whose room is still the unminted sentinel is skipped.
    pub fn rooms(&self) -> Vec<RoomId> {
        let mut seen = HashSet::new();
        self.workspaces
            .iter()
            .map(|w| w.room)
            .filter(|r| !r.is_nil() && seen.insert(*r))
            .collect()
    }

    /// The durable room identity of `workspace_id` — what turns a local
    /// handle into the value the wire speaks.
    pub fn room_of(&self, workspace_id: WorkspaceId) -> Option<RoomId> {
        self.workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .map(|w| w.room)
    }

    /// Tell the tree which records `room`'s index holds. Returns whether
    /// this was news, so the caller can skip re-asking when nothing moved.
    pub fn set_room_records(&mut self, room: RoomId, held: BTreeMap<String, String>) -> bool {
        match self.room_records_held.get(&room) {
            Some(existing) if *existing == held => false,
            _ => {
                self.room_records_held.insert(room, held);
                true
            }
        }
    }

    /// Take the references peers have added since this was last called.
    /// Drained rather than read so a queue that only grows isn't a leak.
    pub fn take_inbound_room_records(&mut self) -> Vec<(RoomId, String, String)> {
        std::mem::take(&mut self.room_records_inbound)
    }

    /// Show workspace `id`, if the tree has it.
    pub fn set_active(&mut self, id: WorkspaceId) {
        if self.workspaces.iter().any(|w| w.id == id) {
            self.active = id;
        }
    }

    /// Show the workspace above the active one. `false` at the top.
    pub fn switch_up(&mut self) -> bool {
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.id == self.active)
            .unwrap_or(0);
        let Some(prev) = idx.checked_sub(1).and_then(|i| self.workspaces.get(i)) else {
            return false;
        };
        self.active = prev.id;
        true
    }

    /// Show the workspace below the active one. `false` at the bottom.
    pub fn switch_down(&mut self) -> bool {
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.id == self.active)
            .unwrap_or(0);
        let Some(next) = self.workspaces.get(idx.saturating_add(1)) else {
            return false;
        };
        self.active = next.id;
        true
    }

    /// Move the active workspace's focused strip one step in `dir`, clamping
    /// at both ends. Returns the resulting index, or `None` when the active
    /// workspace has no strips.
    pub fn move_active_strip(&mut self, dir: i32) -> Option<usize> {
        let workspace = self
            .workspaces
            .iter()
            .find(|workspace| workspace.id == self.active)?;
        let last = workspace.strips.len().checked_sub(1)?;
        let current = self
            .active_strip
            .get(&self.active)
            .and_then(|id| workspace.strips.iter().position(|strip| strip.id == *id))
            .unwrap_or(0)
            .min(last);
        let next = match dir.signum() {
            -1 => current.saturating_sub(1),
            1 => current.saturating_add(1).min(last),
            _ => current,
        };
        let strip = workspace.strips.get(next)?.id;
        self.active_strip.insert(self.active, strip);
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    #[test]
    fn switching_walks_workspaces_and_clamps_at_the_ends() {
        let mut tree = DesktopTree {
            workspaces: vec![
                Workspace::new(WorkspaceId(1), "A"),
                Workspace::new(WorkspaceId(2), "B"),
            ],
            active: WorkspaceId(1),
            ..DesktopTree::default()
        };
        assert!(!tree.switch_up());
        assert!(tree.switch_down());
        assert_eq!(tree.active, WorkspaceId(2));
        assert!(!tree.switch_down());
        tree.set_active(WorkspaceId(9));
        assert_eq!(tree.active, WorkspaceId(2));
        assert_eq!(tree.rooms().len(), 2);
        assert_eq!(tree.room_of(WorkspaceId(1)), Some(tree.workspaces[0].room));
    }

    #[test]
    fn room_records_report_only_news() {
        let mut tree = DesktopTree::default();
        let room = RoomId::new();
        let held = BTreeMap::from([("r".to_owned(), "doc".to_owned())]);
        assert!(tree.set_room_records(room, held.clone()));
        assert!(!tree.set_room_records(room, held));
        assert_eq!(tree.local_salt_or(7), 7);
        tree.peer_salt_override = Some(3);
        assert_eq!(tree.local_salt_or(7), 3);
    }
}
