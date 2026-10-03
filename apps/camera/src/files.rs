//! Captures, clips and chosen files by native path, through `tilo-files`.

use std::io;
use std::path::Path;

use tilo_files::{Kind, blocking};

fn at(path: &Path) -> io::Result<(tilo_files::Dir, tilo_files::VPath)> {
    tilo_files::file_at(path).map_err(io::Error::other)
}

/// A regular file, whole, when it is at most `limit` bytes.
pub(crate) fn read_bounded(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let (dir, name) = at(path).ok()?;
    let meta = blocking::metadata(&dir, &name).ok()?;
    if meta.kind != Kind::File || meta.len > limit as u64 {
        return None;
    }
    let bytes = blocking::read(&dir, &name).ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

/// Replace (or create) the file atomically, creating its folders.
pub(crate) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (dir, name) = at(path)?;
    blocking::write_atomic(&dir, &name, bytes).map_err(io::Error::other)
}

/// Save a document the user chose a destination for, keeping an existing
/// file's link and mode.
pub(crate) fn save(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (dir, name) = at(path)?;
    blocking::save_document(&dir, &name, bytes).map_err(io::Error::other)
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
