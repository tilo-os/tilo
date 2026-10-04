//! What Files does to the filesystem, as a typed effect vocabulary.
//!
//! Creating, renaming, trashing and moving files change the filesystem, not
//! the Files document: the filesystem (and, for the Tilo workspace, its own
//! projection) keeps their history. Every caller — a menu, a drag, the
//! palette, an agent, a script — reaches the same handler
//! ([`crate::FileExplorerWidget`]'s `ResourceSurface::effect`) with one of
//! these values. An entry is named by the id of its published row
//! (`files.entry:…`), or by its name in the folder shown.

// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource;
use ui::tilo_resource::Effect;

/// The filesystem work of the folder a Files window shows.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "files.effects")]
pub enum FilesEffect {
    /// New folder.
    ///
    /// Creates a folder in the folder shown.
    NewFolder {
        #[param("Name of the new folder")]
        name: String,
    },
    /// Rename.
    Rename {
        #[param("The entry: its row id or its name in the folder shown")]
        entry: String,
        #[param("The new name")]
        name: String,
    },
    /// Move to Trash.
    #[effect(row, destructive)]
    Trash {
        #[param("The entry: its row id or its name in the folder shown")]
        entry: String,
    },
    /// Undo move to Trash.
    ///
    /// Puts back what this window last moved to Trash.
    UndoTrash,
    /// Empty Trash.
    ///
    /// Deletes everything in Trash for good, from the Trash folder.
    #[effect(destructive)]
    EmptyTrash,
    /// Move entries into a folder.
    ///
    /// Moves entries of the folder shown into another folder, keeping their
    /// names, and answers what moved.
    #[effect(returns)]
    Move {
        #[param("Entries of the folder shown: names or absolute paths")]
        entries: Vec<String>,
        #[param("Absolute path of the destination folder")]
        folder: String,
        #[param("Create the destination folder when it does not exist (the default)")]
        create: Option<bool>,
    },
}
