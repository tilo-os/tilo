//! A replication read is not a desktop mutation or a copy of room history.
use super::{RoomId, WorkspaceTree, WorkspaceTreeReplica};
use automerge::{ChangeHash, ReadDoc, transaction::Transactable};
use tilo_lock::MutexRecover as _;

impl WorkspaceTreeReplica {
    pub(super) fn requested_room_heads(&self, room: RoomId) -> Option<Vec<ChangeHash>> {
        let tree = self.tree.read();
        if tree.requires_native_owner(room) {
            tracing::debug!(%room, "not requesting room replication: the room's native owner replicates it");
            return None;
        }
        let Some(doc) = tree.room_docs.get(&room) else {
            return Some(Vec::new());
        };
        // A head set identifies the exact committed CRDT state, not merely
        // the tree's UI generation. An open transaction or unapplied changes
        // can change validation without changing those heads: never reuse
        // validation in either case.
        let cacheable = doc.pending_ops() == 0 && ReadDoc::get_missing_deps(doc, &[]).is_empty();
        let heads = cacheable.then(|| doc.committed_heads());
        if let Some(heads) = &heads
            && self.sync_heads.lock_recover().get(&room) == Some(heads)
        {
            return Some(heads.clone());
        }
        let checked = WorkspaceTree::snapshot_room_heads(doc);
        drop(tree);
        if cacheable && let Some(checked) = &checked {
            self.sync_heads.lock_recover().insert(room, checked.clone());
        }
        Some(checked.unwrap_or_default())
    }
}
