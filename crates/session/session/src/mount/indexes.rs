//! The indexes this device keeps for search: what its apps published of their content, and what
//! the person's files hold. They are read from the disk and kept for as long as the session runs.

use std::path::PathBuf;
use std::sync::Arc;

use tilo_desktop_search::Held;
use tilo_desktop_search::content_index::AppContentIndex;
use tilo_desktop_search::files_index::FilesIndex;
use tilo_desktop_search::indexed::IndexedContent;
use tilo_desktop_search::providers::files::{FileNames, Files};
use ui::floating_surface::Recipient;

use crate::app_names::app_name;
use crate::platform::indexes::{Index, Indexes};

/// This device's indexes: what apps publish of their content, and the person's home folder with
/// its names and what the files hold.
pub struct SessionIndexes {
    content: Arc<AppContentIndex>,
    files: Option<(PathBuf, FileNames, Option<Arc<FilesIndex>>)>,
}

impl SessionIndexes {
    /// The indexes this device keeps: what apps hold, published under `content`, and the person's
    /// files.
    #[must_use]
    pub fn new(
        content: Arc<AppContentIndex>,
        files: Option<(PathBuf, FileNames, Option<Arc<FilesIndex>>)>,
    ) -> Self {
        Self { content, files }
    }
}

impl Indexes for SessionIndexes {
    fn of(&self, person: &str) -> Arc<dyn Index> {
        Arc::new(SessionIndex {
            // Rows about what an app holds say who wrote them: the person at this session.
            content: Arc::new(IndexedContent::new(
                Arc::clone(&self.content),
                app_name,
                person,
            )),
            files: self.files.clone(),
        })
    }
}

/// One session's reading of this device's indexes.
struct SessionIndex {
    content: Arc<IndexedContent>,
    files: Option<(PathBuf, FileNames, Option<Arc<FilesIndex>>)>,
}

impl Index for SessionIndex {
    fn content(&self) -> Held {
        Held::from(Arc::clone(&self.content))
    }

    fn files(&self) -> Option<Held> {
        let (home, names, index) = self.files.as_ref()?;
        Some(Held::new(Files::new(
            home.clone(),
            names.clone(),
            index.clone(),
        )))
    }

    fn recipients(&self, query: &str) -> Vec<Recipient> {
        self.content.recipients(query)
    }
}
