//! What an edit was made against: a read-only stamp of a document's owner,
//! frontier and open action.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash};
#[cfg(feature = "native")]
use taffy::NodeId;

/// A read-only invalidation stamp. It contains no document text and confers no
/// write authority; an unavailable stamp must invalidate deferred insertion.
#[derive(Clone, PartialEq, Eq)]
pub struct DocumentEditStamp {
    // Exact owner lifetime and undo/redo revision, never callback admission.
    owner: Option<(uuid::Uuid, u64)>,
    frontier: Vec<ChangeHash>,
    pending: usize,
}
impl DocumentEditStamp {
    /// Bind an embedded field's private equality check to its record and node.
    #[cfg(feature = "native")]
    pub(crate) fn bind_fingerprint(
        &self,
        node: NodeId,
        field: [u8; 32],
        key: &[u8; 32],
    ) -> [u8; 32] {
        let mut hash = blake3::Hasher::new_keyed(key);
        hash.update(b"tilo.ime.embedded-edit.v1");
        hash.update(&u64::from(node).to_le_bytes());
        if let Some((owner, history_revision)) = self.owner {
            hash.update(owner.as_bytes());
            hash.update(&history_revision.to_le_bytes());
        }
        hash.update(&self.pending.to_le_bytes());
        for head in &self.frontier {
            hash.update(&head.0);
        }
        hash.update(&field);
        *hash.finalize().as_bytes()
    }

    pub(crate) fn for_owned_document(
        document: &AutoCommit,
        lifetime: uuid::Uuid,
        history_revision: u64,
    ) -> Self {
        Self {
            owner: Some((lifetime, history_revision)),
            frontier: document.base_heads(),
            pending: document.pending_ops(),
        }
    }

    /// Snapshot an unbound host document without committing its open action.
    pub fn for_document(document: &AutoCommit) -> Self {
        Self {
            owner: None,
            frontier: document.base_heads(),
            pending: document.pending_ops(),
        }
    }
}
