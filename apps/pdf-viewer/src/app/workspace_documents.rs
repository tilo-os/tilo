//! The documents this reader's workspace holds, as its empty state offers
//! them, and opening one in place of the record the window shows: a reviewer
//! opens the paper an agent filled in a hosted workspace without a file
//! chooser, on that paper's own markup record.

use vello::kurbo::{Point, Rect};

use super::PdfApp;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceDocument {
    pub target: String,
    pub name: String,
    pub caption: String,
}

impl PdfApp {
    pub(crate) fn attach_workspace_documents(&mut self, documents: Vec<WorkspaceDocument>) {
        if self.workspace_documents != documents {
            self.dirty = true;
        }
        self.workspace_documents = documents;
    }

    pub fn workspace_documents(&self) -> &[WorkspaceDocument] {
        &self.workspace_documents
    }

    pub(crate) fn workspace_document_at(&self, point: Point) -> Option<&WorkspaceDocument> {
        let rows = self.empty_document_rows.borrow();
        let index = rows.iter().position(|row| row.contains(point))?;
        self.workspace_documents.get(index)
    }

    pub(crate) fn workspace_document_rows(&self) -> Vec<Rect> {
        self.empty_document_rows.borrow().clone()
    }

    pub(crate) fn open_workspace_document(&mut self, target: &str) -> bool {
        let Some(open_here) = self.open_here.clone() else {
            return false;
        };
        let outcome = open_here.open(&[target.to_owned()]);
        self.pending_document_open = Some((target.to_owned(), outcome));
        self.status_note.show_progress(crate::t("pdf-note-opening"));
        self.dirty = true;
        true
    }

    /// Answer the open of a workspace document this reader asked for: shown
    /// (the host replaces this window), or refused, which the person is told.
    pub(super) fn follow_document_open(&mut self) {
        let Some(answer) = self
            .pending_document_open
            .as_ref()
            .and_then(|(_, outcome)| outcome.get())
        else {
            return;
        };
        let Some((target, _)) = self.pending_document_open.take() else {
            return;
        };
        self.dirty = true;
        let Err(reason) = answer else {
            return;
        };
        let name = self
            .workspace_documents
            .iter()
            .find(|document| document.target == target)
            .map_or_else(
                || crate::t("pdf-empty-the-document"),
                |document| document.name.clone(),
            );
        // The list stays: what the host refused may be the paper's own state,
        // not the choice, so the person picks again rather than being left on
        // an error surface.
        tracing::info!(%target, %reason, "a workspace document did not open");
        self.status_note.show(crate::t_args(
            "pdf-note-document-not-opened",
            &[("name", name.into())],
        ));
    }
}
