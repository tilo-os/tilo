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
            schema_version: ui::collaboration::semantic_schema::TEXT_POSITION_V1,
            bytes: serde_json::to_vec(&position).ok()?,
        })
    }

    fn restore_continuation_metadata(
        &mut self,
        metadata: &ContinuationMetadata,
        doc: &mut Edits<'_>,
        _path: &ObjId,
    ) -> bool {
        if metadata.schema_version != ui::collaboration::semantic_schema::TEXT_POSITION_V1 {
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

    fn restore_live_continuation_metadata(
        &mut self,
        metadata: &ContinuationMetadata,
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> bool {
        if metadata.schema_version != ui::collaboration::semantic_schema::TEXT_POSITION_V1 {
            return false;
        }
        let Ok(position) = serde_json::from_slice::<TextPosition>(&metadata.bytes) else {
            return false;
        };
        if !position.valid()
            || EditorDoc::file(doc, &position.resource_id)
                .is_none_or(|file| file.deleted || file.kind != "file")
            || EditorDoc::buffer_text_key(doc, &position.resource_id)
                .and_then(|text| position.resolve(doc, &text))
                .is_none()
        {
            return false;
        }
        if self.active_file_node().is_none() {
            // Project only the already admitted resource metadata. Opening this
            // view never reads disk, seeds text, or submits an app action.
            let mut files = EditorDoc::files(doc);
            files.sort_by_key(|file| file.path.matches('/').count());
            self.project_files(files);
            let Some(node) = self
                .session
                .project
                .node_for_resource(&position.resource_id)
            else {
                return false;
            };
            if !self.session.open_file(node) {
                return false;
            }
            self.bound_node = None;
            self.ensure_buffers(doc);
        }
        self.active_file_node()
            .is_some_and(|node| self.buffer_key(node) == position.resource_id)
            && self.editor.restore_continuation_position(doc, &position)
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
