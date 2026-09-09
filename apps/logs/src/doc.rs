//! Document hygiene for the Logs viewer.
//!
//! Journal records are host observations, and the viewer's query, selected
//! row, scroll position, popups, and focus are each personal, transient view
//! state. None belongs in the collaborative Automerge document: replicating
//! them lets one person unexpectedly change another person's investigation.
//!
//! Older builds did persist a handful of those keys. [`LogsDoc`] removes that
//! legacy state when it encounters it, but never seeds replacement state.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ReadDoc};

const LEGACY_VIEW_KEYS: [&str; 6] = [
    "min_priority",
    "unit_filter",
    "boot_filter",
    "search",
    "selected",
    "selected_entry",
];

/// Stateless migration helpers over the shared document.
pub struct LogsDoc;

impl LogsDoc {
    /// Remove transient state written by pre-local-view versions of Logs.
    ///
    /// This is deliberately the only Logs document write. It is idempotent,
    /// preserves unrelated collaborative data at the supplied root, and makes
    /// old replicas converge on the same empty Logs namespace.
    pub fn prune_legacy_view_state(doc: &mut AutoCommit, root: &ObjId) {
        for key in LEGACY_VIEW_KEYS {
            if matches!(doc.get(root, key), Ok(Some(_))) {
                let _ = doc.delete(root, key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ROOT, transaction::Transactable};

    #[test]
    fn migration_removes_only_legacy_transient_keys() {
        let mut doc = AutoCommit::new();
        let _ = doc.put(&ROOT, "search", "old query");
        let _ = doc.put(&ROOT, "selected_entry", "logs.entry.old");
        let _ = doc.put(&ROOT, "unrelated", "keep me");

        LogsDoc::prune_legacy_view_state(&mut doc, &ROOT);

        assert!(doc.get(&ROOT, "search").unwrap().is_none());
        assert!(doc.get(&ROOT, "selected_entry").unwrap().is_none());
        assert!(doc.get(&ROOT, "unrelated").unwrap().is_some());
    }

    #[test]
    fn fresh_logs_viewer_does_not_materialize_document_state() {
        let mut doc = AutoCommit::new();
        LogsDoc::prune_legacy_view_state(&mut doc, &ROOT);
        assert!(doc.keys(&ROOT).next().is_none());
    }
}
