//! Native text view handoff through the platform's existing continuation route.
use super::CodeApp;
use crate::doc::EditorDoc;
use automerge::{AutoCommit, ObjId};
use ui::collaboration::ContinuationMetadata;
use ui::text_position::TextPosition;
use ui::widget::{Continuable, Edits};

impl Continuable for CodeApp {
    fn continuation_metadata(
        &self,
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<ContinuationMetadata> {
        let node = self.active_file_node()?;
        let position = self
            .editor
            .continuation_position(doc, &self.buffer_key(node))?;
        Some(ContinuationMetadata {
            schema_version: 1,
            bytes: serde_json::to_vec(&position).ok()?,
        })
    }

    fn restore_continuation_metadata(
        &mut self,
        metadata: &ContinuationMetadata,
        doc: &mut Edits<'_>,
        _path: &ObjId,
    ) -> bool {
        if metadata.schema_version != 1 {
            return false;
        }
        let Ok(position) = serde_json::from_slice::<TextPosition>(&metadata.bytes) else {
            return false;
        };
        if !position.valid() || EditorDoc::file(doc.doc(), &position.resource_id).is_none() {
            return false;
        }
        self.pending_open = Some(position.resource_id.clone());
        self.pending_continuation = Some(position);
        true
    }
}

impl CodeApp {
    pub(super) fn restore_pending_continuation(&mut self, doc: &AutoCommit) {
        let Some(position) = self.pending_continuation.as_ref() else {
            return;
        };
        if self
            .active_file_node()
            .is_some_and(|node| self.buffer_key(node) == position.resource_id)
            && self.editor.restore_continuation_position(doc, position)
        {
            self.pending_continuation = None;
        }
    }
}
