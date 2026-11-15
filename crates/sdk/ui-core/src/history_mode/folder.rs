//! A folder's versions as History reads them.
//!
//! A window whose History lists a folder's changes shows the workspace's tree record. The
//! runner that opens the window holds that record and fills the window's source with it
//! once its storage is open, so History reads the same record the window's rows read rather
//! than a second copy, and names the people and agents its versions record with the same
//! directory the rows use. The source is the window's from its launch on: a view built
//! again, or built late because its record was still arriving, reads through the same one.

use std::cell::RefCell;
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

/// A folder's History source, as a registration holds it: the window's own slot, which
/// its runner fills with the tree record once the window's storage is open
/// ([`Self::show`]). Until then it lists nothing.
#[derive(Clone, Default)]
pub struct FolderHistorySource(Rc<RefCell<Option<Rc<dyn FolderVersions>>>>);

impl FolderHistorySource {
    /// The folder `versions` reads, from now on.
    pub fn show(&self, versions: Rc<dyn FolderVersions>) {
        *self.0.borrow_mut() = Some(versions);
    }

    fn versions(&self) -> Option<Rc<dyn FolderVersions>> {
        self.0.borrow().clone()
    }

    /// The folder's changes now.
    #[must_use]
    pub fn entries(&self) -> Vec<HistoryEntry> {
        self.versions()
            .map(|versions| versions.folder_entries())
            .unwrap_or_default()
    }

    /// A number that changes when the folder's versions do. The slot being filled counts as
    /// a change: History reads the folder once its record is there.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.versions()
            .map_or(0, |versions| versions.folder_revision().saturating_add(1))
    }

    /// Whether two sources are the same window's: what a runner checks when it hands a
    /// History to a window it did not build.
    #[must_use]
    pub fn same_owner(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}
