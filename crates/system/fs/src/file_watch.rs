//! Re-reading a file when it changes, instead of on a timer.
//!
//! The capability list a mount enforces is re-read while the mount runs, so
//! a revocation takes effect without a remount. [`FileChanges`] waits for the
//! host's file events (`inotify` on Linux, `FSEvents` on macOS) through
//! [`tilo_files::native::watch_changes`]: a change is seen in milliseconds,
//! and an unchanged file costs nothing.
//!
//! The parent directory is watched, not the file: a producer that renames
//! a new file over the old one, deletes it or creates it again replaces
//! the inode a file watch would be bound to.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tilo_files::native::{ChangeWatch, watch_changes};
use tokio::sync::watch;

/// Wakes on each change of one file.
pub struct FileChanges {
    _watch: ChangeWatch,
    changes: watch::Receiver<()>,
    /// The first [`Self::next`] returns at once: a change between the
    /// caller's first read and the watch going up must not be missed.
    primed: bool,
}

impl std::fmt::Debug for FileChanges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileChanges").finish_non_exhaustive()
    }
}

impl FileChanges {
    /// Watch `path` through its parent directory.
    ///
    /// # Errors
    ///
    /// The path has no file name, or its directory cannot be watched.
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

    /// Wait until the file may have changed. If the watch ends (its
    /// directory went away) this never returns again.
    pub async fn next(&mut self) {
        if std::mem::take(&mut self.primed) {
            return;
        }
        if self.changes.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}
