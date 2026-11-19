//! The resource behind every Files window of a workspace in a page: what the window offers
//! on the folder it shows. Which folder that is belongs to the window, not to the resource:
//! each person, and each of their windows, goes into folders on its own.

use tilo_resource::{self, Effect, Model, SearchEntry, resource};

/// The resource every Files window of a workspace in a page opens. It holds nothing: the
/// folder a window shows is that window's own view, never something it writes here, so
/// one person going into a folder moves nobody else's window.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct FilesWindow {}

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

    /// New file
    ///
    /// Make an empty text file called `name` in the folder the window shows, and open it;
    /// with no name, show the row it is named in, at the top of that folder.
    #[effect(kind = "undoable", icon = "file-plus")]
    NewFile {
        #[param("Its name; none to name it in the window")]
        name: Option<String>,
    },

    /// Enclosing folder
    ///
    /// Show the folder that holds the one the window shows. Only this window moves.
    #[effect(kind = "read", icon = "arrow-up")]
    Up,

    /// Download
    ///
    /// Save the file at `node`, or the folder at `node` as one zip, onto this device.
    #[effect(kind = "read", icon = "download")]
    Download {
        #[param("The file or folder, by its node in the workspace's tree")]
        node: String,
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
    /// What the resource holds: nothing, as the folder a window shows is its own.
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
