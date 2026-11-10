//! The documents this reader's workspace holds, as its empty state offers
//! them, and opening one in place of the record the window shows: a reviewer
//! opens the paper an agent filled in a hosted workspace without a file
//! chooser, on that paper's own markup record.

use vello::kurbo::{Point, Rect};

use super::PdfApp;

/// A document this reader's workspace holds, as its empty state offers it: a
/// reviewer opens the `application.pdf` an agent filled in without opening a
/// file chooser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceDocument {
    /// What opens it: the launch argument that shows the document in this
    /// window.
    pub target: String,
    /// What it is called.
    pub name: String,
    /// How long it is, under the name.
    pub caption: String,
}

impl PdfApp {
    /// The documents this reader's workspace holds, as the empty state
    /// offers them: a press on one opens it here, on its own markup record.
    pub(crate) fn attach_workspace_documents(&mut self, documents: Vec<WorkspaceDocument>) {
        if self.workspace_documents != documents {
            self.dirty = true;
        }
        self.workspace_documents = documents;
    }

    /// The workspace's documents the empty state offers, in the order it
    /// shows them: each opens in this window by its target.
    pub fn workspace_documents(&self) -> &[WorkspaceDocument] {
        &self.workspace_documents
    }

    /// The document a press or an assistive action at `point` opens, as the
    /// last draw pass placed it: what the empty state lists, in the order it
    /// shows them.
    pub(crate) fn workspace_document_at(&self, point: Point) -> Option<&WorkspaceDocument> {
        let rows = self.empty_document_rows.borrow();
        let index = rows.iter().position(|row| row.contains(point))?;
        self.workspace_documents.get(index)
    }

    /// The rows the empty state's documents took, in the order it lists them.
    pub(crate) fn workspace_document_rows(&self) -> Vec<Rect> {
        self.empty_document_rows.borrow().clone()
    }

    /// Open the workspace's document `target` (the identity a row names) in
    /// this window, in place of the record it shows: the host rebuilds the
    /// window on that document's own markup record. Whether a host can.
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
                || "That document".to_owned(),
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
