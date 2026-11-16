//! The folder a window of [`ResourceApp::FOLDER_HISTORY`](super::ResourceApp::FOLDER_HISTORY)
//! lists in its History: the workspace's tree record, the one the window's rows read.
//!
//! The runner opens it with the window's storage and fills the window's History source with
//! it, so History reads the same record the rows do, named with the same directory, from
//! the window's launch on. Nothing is copied and the view keeps no second handle for History.

use fs_protocol::tree::{FILES_APP, FilesTree, document_id};
use ui_core::history_mode::folder::{FolderHistorySource, FolderVersions};

use super::People;
use super::workspace::WorkspaceRecord;

/// The workspace's tree, as History reads its changes.
struct WorkspaceFolder {
    tree: WorkspaceRecord<FilesTree>,
    people: People,
}

impl FolderVersions for WorkspaceFolder {
    fn folder_entries(&self) -> Vec<ui_core::HistoryEntry> {
        let authors = self.people.registry();
        self.tree
            .read(|doc| ui_core::history_source::folder::entries(doc, &authors))
            .unwrap_or_default()
    }

    fn folder_revision(&self) -> u64 {
        self.tree.revision()
    }
}

/// Fill `source` with the workspace's tree `storage` opens, its people named by `people`.
/// A window with no workspace to open it in lists nothing.
pub(crate) fn show(
    source: &FolderHistorySource,
    storage: &super::settings::ViewStorage,
    people: &People,
) {
    if let Some(tree) = storage.shared_record::<FilesTree>(FILES_APP, &document_id()) {
        source.show(std::rc::Rc::new(WorkspaceFolder {
            tree,
            people: people.clone(),
        }));
    }
}
