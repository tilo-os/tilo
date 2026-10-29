//! The room changes a member's device makes itself: their heads, what it
//! publishes past them, and whether a change has come back committed.

use super::StatePeer;
use automerge::ReadDoc;

impl StatePeer {
    /// The heads of the committed room document, `None` without one. Equal
    /// heads at two moments mean nothing was committed to the room between
    /// them.
    pub fn room_heads(&self) -> Option<Vec<automerge::ChangeHash>> {
        Some(self.core.room_doc.as_deref()?.committed_heads())
    }

    /// The room document's changes past `heads`, encoded, in causal order:
    /// what a member's device publishes of a change it made itself.
    #[must_use]
    pub fn room_changes_since(&self, heads: &[automerge::ChangeHash]) -> Vec<Vec<u8>> {
        let Some(room) = self.core.room_doc.as_deref() else {
            return Vec::new();
        };
        room.clone()
            .get_changes(heads)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect()
    }

    /// Whether the room document holds the change `hash`.
    #[must_use]
    pub fn room_holds_change(&self, hash: &automerge::ChangeHash) -> bool {
        self.core
            .room_doc
            .as_deref()
            .is_some_and(|room| room.get_change_by_hash(hash).is_some())
    }
}
