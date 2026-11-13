//! Re-reading a file when it changes, instead of on a timer. The parent directory
//! is watched, not the file: a producer that renames a new file over the old one,
//! deletes it or creates it again replaces the inode a watch would bind to.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tilo_files::native::{ChangeWatch, watch_changes};
use tokio::sync::watch;

pub struct FileChanges {
    _watch: ChangeWatch,
    changes: watch::Receiver<()>,
    /// The first [`Self::next`] returns at once, so a change between the caller's
    /// first read and the watch going up is not missed.
    primed: bool,
}

impl std::fmt::Debug for FileChanges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileChanges").finish_non_exhaustive()
    }
}

impl FileChanges {
    pub fn for_file(path: &Path) -> std::io::Result<Self> {
        let name = path
            .file_name()
            .map(OsString::from)
            .ok_or_else(|| std::io::Error::other(format!("{} names no file", path.display())))?;
        let directory = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let (publish, changes) = watch::channel(());
        let watch = watch_changes(
            &directory,
            move |entry| entry.file_name() == Some(name.as_os_str()),
            move || {
                publish.send_replace(());
            },
        )?;
        Ok(Self {
            _watch: watch,
            changes,
            primed: true,
        })
    }

    /// Wait until the file may have changed; if the watch ends, this never
    /// returns again.
    pub async fn next(&mut self) {
        if std::mem::take(&mut self.primed) {
            return;
        }
        if self.changes.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}
