//! The editor's text sync against the current authoritative document.
//!
//! The search projection may already have read the active buffer in this
//! sync. Handing that immutable observation to the editor avoids walking the
//! same CRDT text twice while keeping the document authoritative.

use super::{AppModel, EditorView, ModelPath, ObjId};

#[cfg(test)]
#[path = "editor_sync_text_tests.rs"]
mod sync_text_tests;

pub(crate) struct SyncText<'doc> {
    pub(crate) document: &'doc automerge::AutoCommit,
    pub(crate) object: ObjId,
    pub(crate) text: String,
}

impl EditorView {
    #[cfg(test)]
    pub(crate) fn sync_text_reads(&self) -> usize {
        self.sync_text_reads
    }

    pub(crate) fn sync_observed(
        &mut self,
        state: &AppModel,
        path: &ModelPath,
        observed: Option<&SyncText<'_>>,
    ) {
        use automerge::transaction::Transactable;
        self.collect_highlights();
        let (state, _) = ui::doc::document_ref(state, path);
        // Across syncs only committed heads identify content. Within this sync,
        // an immutable borrow also identifies pending content without caching
        // a pending-operation count or reusing an earlier observation.
        let heads = (state.pending_ops() == 0).then(|| state.committed_heads());
        if heads.is_some() && self.text_heads == heads {
            return;
        }
        if let Some(t) = self.text_obj.clone() {
            use automerge::ReadDoc;
            let text = if let Some(read) =
                observed.filter(|read| std::ptr::eq(read.document, state) && read.object == t)
            {
                std::borrow::Cow::Borrowed(read.text.as_str())
            } else {
                #[cfg(test)]
                {
                    self.sync_text_reads = self.sync_text_reads.saturating_add(1);
                }
                std::borrow::Cow::Owned(state.text(&t).unwrap_or_default())
            };
            if text.as_ref() != self.cached_text {
                self.rebuild(&text);
            }
        }
        self.text_heads = heads;
    }
}
