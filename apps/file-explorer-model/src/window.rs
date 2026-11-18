//! The files a window shows: the folder of the workspace's tree it shows, so a
//! window comes back to where the person left it.

use tilo_resource::{self, Atom, Model, SearchEntry, resource};

/// The window's own resource: the folder of the workspace's tree it shows, so a
/// window comes back to where the person left it.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct FilesWindow {
    /// The node of the tree the window shows; empty for the workspace's root.
    pub folder: Atom,
}

#[resource(
    kind = "files.workspace",
    label = "files-workspace-window",
    search = FilesWindow::search,
    read_content = FilesWindow::read_document
)]
impl FilesWindow {}

impl FilesWindow {
    /// What the resource holds: the folder the window shows.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }

    /// Nothing: a workspace's files are found by name and by what their text
    /// holds in the tree's own search source, not as rows of this resource.
    fn search(_: &Self) -> Vec<SearchEntry> {
        Vec::new()
    }
}
