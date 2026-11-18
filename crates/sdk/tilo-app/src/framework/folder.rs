//! The folder a window of [`ResourceApp::FOLDER_HISTORY`](super::ResourceApp::FOLDER_HISTORY)
//! lists in its History: the workspace's tree, the one the window's rows read.
//!
//! The runner opens it with the window's storage and fills the window's History source with
//! it, so History reads the same records the rows do, named with the same directory, from
//! the window's launch on. Nothing is copied and the view keeps no second handle for History.
//!
//! A tree is kept in chunks, each a record of its own ([`fs_protocol::tree`]): its own record
//! holds the root's identity and lists the chunks, and each chunk holds the nodes whose record
//! ids name it. History reads every chunk's changes, opening a chunk as the tree lists it, and
//! reads them again only when one of the records moves. Undo and Restore of a row are
//! History's own verbs on the chunk's record, which syncs to every device holding the tree as
//! any change of it does: the device whose disk holds the file writes the content back.

use std::rc::Rc;

use fs_protocol::files::{FileDocumentId, TreeChunk};
use fs_protocol::tree::{
    FILES_APP, FilesTree, Tree, TreeChunks, chunk_document_id, document_id, listed_chunks,
};
use ui_core::automerge::AutoCommit;
use ui_core::history_mode::folder::{FolderHistorySource, FolderVersions};
use ui_core::history_source::folder::{FolderChange, FolderChunks};
use ui_core::history_source::verbs::HistoryEdit;

use super::People;
use super::settings::ViewStorage;
use super::workspace::{WorkspaceResource, WorkspaceState};

/// The workspace's tree, as History reads its changes: its own record and its chunks.
struct WorkspaceFolder {
    /// The records every app of the workspace opens, which each chunk is opened from.
    workspace: Rc<WorkspaceState>,
    /// The tree's own record (chunk 0): the root's identity and the chunks it lists.
    tree: WorkspaceResource<FilesTree>,
    people: People,
}

impl WorkspaceFolder {
    /// The record of chunk `chunk`: the tree's own for 0.
    fn record(&self, chunk: TreeChunk) -> Option<WorkspaceResource<FilesTree>> {
        if chunk == 0 {
            return Some(self.tree.clone());
        }
        self.workspace
            .open_at::<FilesTree>(FILES_APP, &chunk_document_id(chunk))
    }

    /// The chunks the tree's own record lists.
    fn listed(&self) -> Vec<TreeChunk> {
        self.tree.read(listed_chunks).unwrap_or_default()
    }
}

impl FolderVersions for WorkspaceFolder {
    fn folder_changes(&self, file: Option<FileDocumentId>) -> Vec<FolderChange> {
        let authors = self.people.registry();
        ui_core::history_source::folder::changes(self, &authors, file)
    }

    /// Decided as a call that may take back anything the tree takes: undo and restore write
    /// whatever the change they act on wrote.
    fn apply(&self, chunk: TreeChunk, edit: HistoryEdit, node: Option<&str>) -> Result<(), String> {
        let record = self
            .record(chunk)
            .ok_or("the folder's record is not open here")?;
        if !fs_protocol::file_action::verbs::ALL
            .iter()
            .all(|verb| record.may(verb))
        {
            return Err("you may not change the workspace's files".to_owned());
        }
        record
            .apply_history(edit, node)
            .map_err(|error| error.to_string())
    }

    /// The revision of every record the folder is, combined: History reads the changes again
    /// when any chunk moves, and only then.
    fn folder_revision(&self) -> u64 {
        let mut revision = self.tree.revision();
        for chunk in self.listed() {
            if let Some(record) = self.record(chunk) {
                revision = revision
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    .wrapping_add(record.revision());
            }
        }
        revision
    }
}

impl FolderChunks for WorkspaceFolder {
    fn tree(&self) -> Option<Tree> {
        let mut chunks = TreeChunks::default();
        self.tree.read(|doc| chunks.read_index(doc))?;
        for chunk in chunks.listed() {
            if let Some(record) = self.record(chunk) {
                record.read(|doc| chunks.read_chunk(chunk, doc));
            }
        }
        Some(chunks.tree())
    }

    fn read_chunk<T>(&self, chunk: TreeChunk, read: impl FnOnce(&AutoCommit) -> T) -> Option<T> {
        self.record(chunk)?.read(read)
    }
}

/// Fill `source` with the workspace's tree `storage` opens, its people named by `people`.
/// A window with no workspace to open it in lists nothing.
pub(crate) fn show(source: &FolderHistorySource, storage: &ViewStorage, people: &People) {
    let Some(workspace) = storage.workspace_state() else {
        return;
    };
    if let Some(tree) = workspace.open_at::<FilesTree>(FILES_APP, &document_id()) {
        source.show(Rc::new(WorkspaceFolder {
            workspace,
            tree,
            people: people.clone(),
        }));
    }
}
