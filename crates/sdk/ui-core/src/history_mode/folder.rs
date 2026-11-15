//! A folder's versions as History reads them.
//!
//! A window whose History lists a folder's changes holds the tree record already: it is
//! what its rows are. It provides this, so History reads the same record rather than a
//! second copy, and names the people and agents its versions record with the same
//! directory its rows use.

use std::rc::Rc;

use crate::HistoryEntry;

/// What History needs of the folder it lists: the tree record's changes as rows, and a
/// number that says when to read them again.
pub trait FolderVersions {
    /// The folder's changes, oldest first: one row per file and writer
    /// ([`crate::history_source::folder`]). Empty when the record cannot be read now.
    fn folder_entries(&self) -> Vec<HistoryEntry>;

    /// A number that changes when the folder's versions do: the tree record's revision,
    /// so History reads the changes again only when there are new ones.
    fn folder_revision(&self) -> u64;
}

/// A window that holds the folder its History lists: it answers the tree record it lists
/// as [`FolderVersions`]. Provided as a capability, so the runner that installs the
/// window's History reads the same record the window's rows are.
pub trait FolderHistoryWindow {
    fn folder_history(&self) -> Rc<dyn FolderVersions>;
}

/// A folder's History source, as a registration holds it: the window's own record, read
/// through the window ([`FolderVersions`]).
#[derive(Clone)]
pub struct FolderHistorySource(Rc<dyn FolderVersions>);

impl FolderHistorySource {
    #[must_use]
    pub fn new(versions: Rc<dyn FolderVersions>) -> Self {
        Self(versions)
    }

    /// The folder's changes now.
    #[must_use]
    pub fn entries(&self) -> Vec<HistoryEntry> {
        self.0.folder_entries()
    }

    /// A number that changes when the folder's versions do.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.0.folder_revision()
    }

    /// Whether two sources read the same window's folder: what a runner checks when it
    /// hands a History to a window it did not build.
    #[must_use]
    pub fn same_owner(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}
