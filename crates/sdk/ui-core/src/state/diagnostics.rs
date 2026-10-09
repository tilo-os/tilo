//! Stable, serializable projections for state inspection and structured logs.
//!
//! These values contain graph metadata, never application scalar contents, so
//! operators can diagnose convergence without leaking document text.

use automerge::ChangeHash;
use serde::Serialize;

use super::{DocumentFrontier, State};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DocumentDiagnostics {
    pub actor: String,
    pub heads: Vec<String>,
    pub changes: usize,
    pub operations: usize,
    pub undo_depth: usize,
    pub redo_depth: usize,
    pub history_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FrontierDiagnostics {
    pub heads: Vec<String>,
}

fn hashes(values: &[ChangeHash]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

impl State {
    /// Metadata-only snapshot suitable for support bundles and JSON tooling.
    /// Reading it commits no transaction and advances no serialization cursor.
    pub fn diagnostics(&mut self) -> DocumentDiagnostics {
        let heads = self.doc.get_heads();
        let changes = self.doc.get_changes(&[]);
        DocumentDiagnostics {
            actor: self.doc.get_actor().to_hex_string(),
            heads: hashes(&heads),
            changes: changes.len(),
            operations: changes
                .iter()
                .fold(0usize, |total, change| total.saturating_add(change.len())),
            undo_depth: self.undo.undo_depth(&self.doc),
            redo_depth: self.undo.redo_depth(&self.doc),
            history_revision: self.undo.revision(),
        }
    }

    #[must_use]
    pub fn outbound_diagnostics(&self) -> FrontierDiagnostics {
        self.outbound.diagnostics()
    }
}

impl DocumentFrontier {
    #[must_use]
    pub fn diagnostics(&self) -> FrontierDiagnostics {
        FrontierDiagnostics {
            heads: hashes(self.heads()),
        }
    }
}

#[cfg(test)]
mod tests {
    use automerge::transaction::Transactable;

    use super::*;

    #[test]
    fn diagnostics_are_metadata_only_and_do_not_consume_a_delta() {
        let mut state = State::new();
        let mut frontier = DocumentFrontier::at_document(&mut state.doc);
        state
            .doc
            .put(automerge::ROOT, "secret", "not projected")
            .unwrap();
        state.commit_action(None, None);

        let diagnostics = state.diagnostics();
        let json = serde_json::to_string(&diagnostics).unwrap();
        assert_eq!(diagnostics.changes, 1);
        assert!(diagnostics.operations >= 1);
        assert!(!json.contains("secret"));
        assert!(!json.contains("not projected"));
        assert!(!frontier.take_delta(&mut state.doc).is_empty());
    }
}
