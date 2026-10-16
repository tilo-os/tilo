//! Captures, clips and chosen files by native path, through `tilo-files`.

use std::io;
use std::path::Path;

use tilo_files::blocking;

fn at(path: &Path) -> io::Result<(tilo_files::Dir, tilo_files::VPath)> {
    tilo_files::file_at(path).map_err(io::Error::other)
}

/// A regular file, whole, when it is at most `limit` bytes.
pub(crate) fn read_bounded(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let (dir, name) = at(path).ok()?;
    blocking::read_bounded(&dir, &name, limit).ok()?
}

/// Replace (or create) the file atomically, creating its folders.
pub(crate) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (dir, name) = at(path)?;
    blocking::write_atomic(&dir, &name, bytes).map_err(io::Error::other)
}

/// Delete the file; a missing file is not an error worth reporting.
pub(crate) fn remove(path: &Path) {
    if let Ok((dir, name)) = at(path) {
        let _ = blocking::remove(&dir, &name);
    }
}

/// The file's size and modification time.
pub(crate) fn metadata(path: &Path) -> Option<tilo_files::Meta> {
    let (dir, name) = at(path).ok()?;
    blocking::metadata(&dir, &name).ok()
}
