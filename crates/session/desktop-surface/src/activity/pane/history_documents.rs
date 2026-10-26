//! Which admitted document a history row belongs to, and the writes that
//! address it: restore, undo and naming touch only the document the row was
//! read from.

use super::super::index::{HistoryOrigin, WorkspaceHistoryId};
use super::super::{ActivityTree, HistoryRoom};
use super::RoomActivityPane;
use automerge::{AutoCommit, ChangeHash};
use std::collections::BTreeMap;
use ui::widget::HistoryEntryId;

impl RoomActivityPane {
    /// The address of a displayed row this pane read; `None` when it read no
    /// such row, so a write naming it must be refused rather than guessed.
    pub(super) fn known_origin_of(&self, id: &HistoryEntryId) -> Option<WorkspaceHistoryId> {
        self.projected_origins
            .get(id)
            .cloned()
            .or_else(|| self.derived_origins.get(id).cloned())
    }

    /// The admitted document a displayed row came from: the address every
    /// read, compare and write of that row resolves through. A row this pane
    /// did not read keeps the single-document host's native addressing.
    pub(super) fn origin_of(&self, id: &HistoryEntryId) -> WorkspaceHistoryId {
        self.known_origin_of(id).unwrap_or(WorkspaceHistoryId {
            origin: HistoryOrigin::DesktopTree,
            change: *id,
        })
    }

    /// The document a write naming row `id` may touch: only the document
    /// this pane read the row from. A row it did not read is refused on a
    /// multi-room host and keeps native addressing on a single-document one.
    pub(super) fn write_room_of(
        &self,
        tree: &impl ActivityTree,
        id: &HistoryEntryId,
    ) -> Option<HistoryRoom> {
        if let Some(origin) = self.known_origin_of(id) {
            return match origin.origin {
                HistoryOrigin::DesktopTree => Some(None),
                HistoryOrigin::Room(room) => Some(Some(room)),
                HistoryOrigin::Record { .. } | HistoryOrigin::AppRecord { .. } => None,
            };
        }
        (tree.history_rooms() == [None]).then_some(None)
    }

    /// The document a row's change lives in, as this pane asks the tree for
    /// it: `None` is a host whose one document holds every workspace.
    pub(crate) fn history_room_of(&self, id: &HistoryEntryId) -> HistoryRoom {
        match self.origin_of(id).origin {
            HistoryOrigin::Room(room) => Some(room),
            _ => None,
        }
    }

    pub(super) fn rows_revision(&self) -> u64 {
        self.history_logs
            .values()
            .fold(0, |sum, log| sum.wrapping_add(log.rows_revision()))
    }

    /// Apply `write` to the admitted document `room` names and commit it, so
    /// the change reaches peers through the same sync as the changes it
    /// addresses. A room a host admits through its owner, or one whose
    /// document this tree does not hold, is not writable here: nothing is
    /// written and an unavailable room is never replaced by a projection.
    pub(super) fn write_history_document<T>(
        tree: &mut impl ActivityTree,
        room: HistoryRoom,
        write: impl FnOnce(&mut AutoCommit) -> (bool, T),
    ) -> Option<(bool, T)> {
        if !tree.history_room_writable(room) {
            return None;
        }
        let doc = tree.history_document_mut(room)?;
        let (wrote, value) = write(doc);
        if wrote && room.is_some() {
            let _ = tilo_resource::history::commit_stamped(doc);
        }
        Some((wrote, value))
    }

    /// Restore a raw change of a host whose one document holds every
    /// workspace, when the pane has not itself listed it yet.
    pub(super) fn revert_native(
        &mut self,
        tree: &mut impl ActivityTree,
        id: &HistoryEntryId,
    ) -> bool {
        let Some((wrote, ())) = Self::write_history_document(tree, None, |doc| {
            let Some(target) = ui::heads_for_entry(doc, id) else {
                return (false, ());
            };
            let current = doc.get_heads();
            if current == target {
                return (false, ());
            }
            ui::state::write_revert_patches(doc, &current, &target);
            (true, ())
        }) else {
            return false;
        };
        if !wrote {
            return false;
        }
        tree.commit_activity_change();
        let _ = tree.resync_activity_cache();
        self.last_heads.clear();
        self.refresh(tree);
        true
    }
}

/// The document whose history the active workspace's room is: the pane's
/// default write target and the document its own rows come from. A host whose
/// one document holds every workspace names none.
pub(super) fn active_history_room(tree: &impl ActivityTree, rooms: &[HistoryRoom]) -> HistoryRoom {
    if rooms.len() == 1 && rooms[0].is_none() {
        return None;
    }
    tree.active_workspace()
        .map(tilo_desktop_model::Workspace::room)
        .and_then(|room| rooms.iter().copied().find(|known| *known == Some(room)))
        .or_else(|| rooms.first().copied())
        .flatten()
}

/// The settled heads of each admitted document the tree holds.
pub(crate) fn history_heads(tree: &impl ActivityTree) -> BTreeMap<HistoryRoom, Vec<ChangeHash>> {
    tree.history_rooms()
        .into_iter()
        .filter_map(|room| {
            tree.history_document(room)
                .map(|doc| (room, ui::history_source::committed_heads(doc)))
        })
        .collect()
}
