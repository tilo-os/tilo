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

    /// The generation of the workspace's history this copy holds: zero for one never founded
    /// again.
    #[must_use]
    pub fn workspace_generation(&self) -> u64 {
        self.core
            .workspace_doc
            .as_deref()
            .map_or(0, crate::workspace::base::generation)
    }

    /// The changes `device` signed into this copy of the workspace as its own admission, in
    /// every generation of its history, with what they build on, encoded in causal order: what
    /// a later admission of the device builds on, so it never signs a second change under a
    /// number it signed one under ([`crate::own_admission`]). Empty where it signed none here.
    #[must_use]
    pub fn own_admissions(&self, device: &[u8; 32]) -> Vec<Vec<u8>> {
        let Some(workspace) = self.core.workspace_doc.as_deref() else {
            return Vec::new();
        };
        let generation = crate::workspace::base::generation(workspace);
        let actors: Vec<_> = (0..=generation)
            .map(|generation| crate::workspace_device_actor_at(device, generation))
            .collect();
        let mut document = workspace.clone();
        let changes = document.get_changes(&[]);
        let mut wanted: std::collections::HashSet<automerge::ChangeHash> = changes
            .iter()
            .filter(|change| actors.contains(change.actor_id()))
            .map(automerge::Change::hash)
            .collect();
        if wanted.is_empty() {
            return Vec::new();
        }
        // Causal order puts every change after what it builds on: walking it backwards takes
        // in each change's dependencies before they are passed.
        for change in changes.iter().rev() {
            if wanted.contains(&change.hash()) {
                wanted.extend(change.deps().iter().copied());
            }
        }
        changes
            .iter()
            .filter(|change| wanted.contains(&change.hash()))
            .map(|change| change.raw_bytes().to_vec())
            .collect()
    }

    /// Whether the workspace document holds the change `hash`.
    #[must_use]
    pub fn workspace_holds_change(&self, hash: &automerge::ChangeHash) -> bool {
        self.core
            .workspace_doc
            .as_deref()
            .is_some_and(|workspace| workspace.has_change(hash))
    }

    /// Whether `candidate`, the workspace document an input made, holds a
    /// change waiting for parents no change held before waited for: it came
    /// ahead of what it builds on, or what it builds on never came. Nothing
    /// here can apply it, so its sender is asked for the state it holds.
    pub(super) fn newly_waiting(&self, candidate: &automerge::AutoCommit) -> bool {
        let waited: std::collections::BTreeSet<automerge::ChangeHash> = self
            .core
            .workspace_doc
            .as_deref()
            .map(|doc| doc.get_missing_deps(&[]).into_iter().collect())
            .unwrap_or_default();
        candidate
            .get_missing_deps(&[])
            .iter()
            .any(|missing| !waited.contains(missing))
    }
}
