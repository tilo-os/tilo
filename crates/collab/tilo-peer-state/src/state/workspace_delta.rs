//! The workspace changes a member's device makes itself: their heads, what it
//! publishes past them, and whether a change has come back committed.

use super::StatePeer;
use automerge::ReadDoc;

impl StatePeer {
    /// The heads of the committed workspace document, `None` without one. Equal
    /// heads at two moments mean nothing was committed to the workspace between
    /// them.
    pub fn workspace_heads(&self) -> Option<Vec<automerge::ChangeHash>> {
        Some(self.core.workspace_doc.as_deref()?.committed_heads())
    }

    /// The workspace document's changes past `heads`, encoded, in causal order:
    /// what a member's device publishes of a change it made itself.
    #[must_use]
    pub fn workspace_changes_since(&self, heads: &[automerge::ChangeHash]) -> Vec<Vec<u8>> {
        let Some(workspace) = self.core.workspace_doc.as_deref() else {
            return Vec::new();
        };
        workspace
            .clone()
            .get_changes(heads)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect()
    }

    /// Whether the workspace document holds the change `hash`.
    #[must_use]
    pub fn workspace_holds_change(&self, hash: &automerge::ChangeHash) -> bool {
        self.core
            .workspace_doc
            .as_deref()
            .is_some_and(|workspace| workspace.get_change_by_hash(hash).is_some())
    }
}
