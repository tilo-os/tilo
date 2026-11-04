//! What a transfer does to the filesystem around its copies: removals,
//! durability syncs and the Trash.

use std::fs::{self, File};
use std::io;
use std::path::Path;

/// Take `path` away: a folder with everything in it, a file, or a link.
///
/// # Errors
/// Returns what the filesystem says; a path that is not there is nothing to
/// take away.
pub fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
        }
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Move `path` to the person's Trash, saying so when the Trash cannot take
/// it.
pub fn trash(path: &Path) {
    if fs::symlink_metadata(path).is_err() {
        return;
    }
    if let Err(error) = crate::trash::trash(path) {
        tracing::warn!(%error, path = %path.display(), "an item could not be moved to the Trash");
    }
}

/// Flush `path`'s directory entry, so a rename or a fresh name survives a
/// power cut.
///
/// # Errors
/// Returns what the filesystem says.
pub fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// Flush every write this filesystem has cached: one group commit for a batch
/// of copies, rather than one per file.
///
/// # Errors
/// Returns what the filesystem says.
pub(crate) fn sync_filesystem(path: &Path) -> io::Result<()> {
    let file = File::open(path)?;
    #[cfg(unix)]
    {
        rustix::fs::syncfs(&file).map_err(io::Error::from)
    }
    #[cfg(not(unix))]
    {
        file.sync_all()
    }
}

/// Flush the folder `path` is in.
///
/// # Errors
/// Returns what the filesystem says.
pub fn sync_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => sync_directory(parent),
        _ => Ok(()),
    }
}

/// Run a transfer step whose failure leaves the transfer's own outcome intact
/// — a durability sync, a cleanup — and say so in the journal with the path it
/// was about: what it lost shows up only after a crash or a restart.
pub fn best_effort<T>(what: &str, path: &Path, step: impl FnOnce() -> io::Result<T>) {
    if let Err(error) = step() {
        tracing::warn!(%error, path = %path.display(), what, "a transfer step did not finish");
    }
}

/// [`best_effort`] for a step that already failed with `error`.
pub fn best_effort_by(what: &str, path: &Path, error: impl std::fmt::Display) {
    tracing::warn!(%error, path = %path.display(), what, "a transfer step did not finish");
}
