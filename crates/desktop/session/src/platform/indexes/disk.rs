//! The index this device keeps of what the person's files hold. It is read from the disk and
//! kept for as long as the session runs.

use std::path::PathBuf;
use std::sync::Arc;

use tilo_desktop_search::Held;
use tilo_desktop_search::files_index::FilesIndex;
use tilo_desktop_search::providers::files::{FileNames, Files};

use crate::platform::indexes::{Index, Indexes};

/// This device's index of the person's home folder: its names and what the files hold.
pub struct SessionIndexes {
    files: Option<(PathBuf, FileNames, Option<Arc<FilesIndex>>)>,
}

impl SessionIndexes {
    /// The index this device keeps of the person's files.
    #[must_use]
    pub fn new(files: Option<(PathBuf, FileNames, Option<Arc<FilesIndex>>)>) -> Self {
        Self { files }
    }
}

impl Indexes for SessionIndexes {
    fn of(&self, _person: &str) -> Arc<dyn Index> {
        Arc::new(SessionIndex {
            files: self.files.clone(),
        })
    }
}

/// One session's reading of this device's index.
struct SessionIndex {
    files: Option<(PathBuf, FileNames, Option<Arc<FilesIndex>>)>,
}

impl Index for SessionIndex {
    fn files(&self) -> Option<Held> {
        let (home, names, index) = self.files.as_ref()?;
        Some(Held::new(Files::new(
            home.clone(),
            names.clone(),
            index.clone(),
        )))
    }
}
