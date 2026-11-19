//! The files a window shows: the folder of the workspace's tree it shows, so a
//! window comes back to where the person left it.

use tilo_resource::{self, Atom, Effect, Model, SearchEntry, resource};

/// The window's own resource: the folder of the workspace's tree it shows, so a
/// window comes back to where the person left it.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct FilesWindow {
    /// The node of the tree the window shows; empty for the workspace's root.
    pub folder: Atom,
}

/// What a window of the workspace's files does beyond showing a folder: the ways to put
/// something new into the folder it shows.
///
/// The folder and the files land through the tree's own verbs (`new-folder`, `new-file`,
/// `write`), as the person at the window, in the chunk of the tree each node goes to.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "files.window", app = "so.tilo.file-explorer")]
pub enum FilesWindowEffect {
    /// New folder
    ///
    /// Make a folder called `name` in the folder the window shows; with no name, show the
    /// row it is named in, at the top of that folder.
    #[effect(kind = "undoable", icon = "folder-plus")]
    NewFolder {
        #[param("Its name; none to name it in the window")]
        name: Option<String>,
    },

    /// Upload files
    ///
    /// Choose files on this device with its file chooser and put them into the folder the
    /// window shows.
    #[effect(job, attended, kind = "undoable", icon = "upload")]
    Upload,
}

#[resource(
    kind = "files.workspace",
    label = "files-workspace-window",
    effects = FilesWindowEffect,
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
