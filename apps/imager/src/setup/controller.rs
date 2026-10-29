//! A folder chosen for a copy into Tilo: what the first-login and later
//! folder imports review. It grants no file authority; the copy worker
//! revalidates every path before doing work.

use std::path::PathBuf;

use super::model::FolderMode;
use imager_backend::folders::FolderScan;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderDestination {
    /// The import resolves and revalidates the actual Tilo-managed target.
    Unresolved,
    Resolved(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderChoice {
    pub scan: FolderScan,
    pub destination: FolderDestination,
    pub mode: FolderMode,
    pub mode_overridden: bool,
}
