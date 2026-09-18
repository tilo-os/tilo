//! Shared document-history ownership for native and embedded application hosts.

use super::App;
use crate::history_source::{AuthorDirectory, HistoryComparison};
use crate::standalone_history::{HistoryReadContext, HistoryTarget, StandaloneHistory};
use crate::widget::{HistoryEntry, HistoryEntryId};

/// A freshly authorized history projection. A snapshot is a value, not a grant
/// to read again or restore a document; every subsequent operation revalidates.
#[derive(Clone, Debug)]
pub struct DocumentHistorySnapshot {
    /// The most recent committed entries, bounded by `DEFAULT_MAX_ENTRIES`.
    pub entries: Vec<HistoryEntry>,
    pub selection: Vec<HistoryEntryId>,
    pub comparison: Option<HistoryComparison>,
}

impl App {
    /// Capture the focused component's document, or the explicit legacy app
    /// document when focus has no component owner. Moving focus into a history
    /// panel never changes this captured destination.
    ///
    /// # Errors
    /// Returns an error when that document is closed or cannot currently be read.
    pub fn open_document_history(&mut self) -> Result<(), String> {
        self.document_history = None;
        let mut binding = self
            .focused_node
            .and_then(|node| self.bindings.binding_for(node))
            .or_else(|| self.root_widget.callback_binding());
        if binding.is_none() {
            for node in self.node_map.keys() {
                let Some(candidate) = self.bindings.binding_for(*node) else {
                    continue;
                };
                if binding
                    .as_ref()
                    .is_some_and(|current| current.lifetime_id != candidate.lifetime_id)
                {
                    return Err("Choose a document before opening History".into());
                }
                binding = Some(candidate);
            }
        }
        if binding.is_none()
            && self
                .records
                .as_ref()
                .is_some_and(|records| records.active().is_none())
        {
            return Err("Choose a document before opening History".into());
        }
        let target = binding.map_or_else(
            || HistoryTarget::Legacy {
                app_id: crate::collaboration::local_app_id(),
                record: self
                    .records
                    .as_ref()
                    .and_then(|records| records.active())
                    .cloned(),
                document_identity: self.state.document_identity(),
            },
            |binding| HistoryTarget::Bound(Box::new(binding)),
        );
        let context = HistoryReadContext {
            document: &self.state.doc,
            active_record: self.records.as_ref().and_then(|records| records.active()),
            document_identity: self.state.document_identity(),
            permissions: &self.permission_ctx,
        };
        self.document_history = Some(
            StandaloneHistory::open(target, &context, AuthorDirectory::new())
                .map_err(|error| format!("History is unavailable: {error:?}"))?,
        );
        Ok(())
    }

    /// Read the currently open history through its captured document authority.
    /// Revoked and stale sessions purge their cached rows before returning an error.
    ///
    /// # Errors
    /// Returns an error if the captured document is stale or no longer readable.
    pub fn document_history_snapshot(&mut self) -> Result<Option<DocumentHistorySnapshot>, String> {
        let context = HistoryReadContext {
            document: &self.state.doc,
            active_record: self.records.as_ref().and_then(|records| records.active()),
            document_identity: self.state.document_identity(),
            permissions: &self.permission_ctx,
        };
        let Some(history) = self.document_history.as_mut() else {
            return Ok(None);
        };
        history
            .refresh(&context)
            .map_err(|error| format!("History is unavailable: {error:?}"))?;
        Ok(Some(DocumentHistorySnapshot {
            entries: history.rows().to_vec(),
            selection: history.selection().to_vec(),
            comparison: history.comparison().cloned(),
        }))
    }

    /// Select committed entries and materialize their exact before/after states.
    /// An empty selection clears the comparison. This never restores or mutates
    /// the live document, including any pending typing action.
    ///
    /// # Errors
    /// Returns an error for a stale session, denied read or foreign entry.
    pub fn select_document_history(&mut self, entries: &[HistoryEntryId]) -> Result<(), String> {
        let context = HistoryReadContext {
            document: &self.state.doc,
            active_record: self.records.as_ref().and_then(|records| records.active()),
            document_identity: self.state.document_identity(),
            permissions: &self.permission_ctx,
        };
        let history = self
            .document_history
            .as_mut()
            .ok_or("History is not open")?;
        history
            .select(&context, entries)
            .map_err(|error| format!("History selection is unavailable: {error:?}"))?;
        if !entries.is_empty() {
            history
                .compare_selection(&context)
                .map_err(|error| format!("History comparison is unavailable: {error:?}"))?;
        }
        Ok(())
    }

    /// Discard the private history view and its captured document authority.
    pub fn close_document_history(&mut self) {
        self.document_history = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ROOT, transaction::Transactable};

    #[test]
    fn legacy_history_observes_committed_state_without_closing_pending_edits() {
        let mut app = App::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(crate::widget::Text::new("Document")),
        );
        app.state.doc.put(ROOT, "title", "before").unwrap();
        app.state.commit_action(None, None);
        app.state.doc.put(ROOT, "title", "typing").unwrap();
        let pending = app.state.doc.pending_ops();
        app.open_document_history().unwrap();
        let snapshot = app.document_history_snapshot().unwrap().unwrap();
        assert_eq!(snapshot.entries.len(), 1);
        app.select_document_history(&[snapshot.entries[0].id])
            .unwrap();
        assert!(
            app.document_history_snapshot()
                .unwrap()
                .unwrap()
                .comparison
                .is_some()
        );
        assert_eq!(app.state.doc.pending_ops(), pending);
        app.close_document_history();
        assert!(app.document_history_snapshot().unwrap().is_none());
    }

    #[test]
    fn replacing_legacy_document_invalidates_even_with_no_record_set() {
        let mut app = App::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(crate::widget::Text::new("Document")),
        );
        app.state
            .doc
            .put(ROOT, "private", "first document")
            .unwrap();
        app.state.commit_action(None, None);
        app.open_document_history().unwrap();
        let old = app.document_history_snapshot().unwrap().unwrap().entries[0].id;
        assert!(app.state.adopt_doc(automerge::AutoCommit::new()));
        assert!(app.document_history_snapshot().is_err());
        assert!(app.select_document_history(&[old]).is_err());
        assert!(app.document_history.as_ref().unwrap().rows().is_empty());
    }
}
