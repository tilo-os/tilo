//! Files named by path (a launch and the images beside it), through
//! `tilo-files`. None of them has a path on the
//! web, where these report the file system as unsupported.

use std::path::{Path, PathBuf};

use tilo_files::{Dir, FsError, VPath, blocking};

fn at(path: &Path) -> Result<(Dir, VPath), FsError> {
    tilo_files::file_at(path)
}

/// `path` with every symbolic link resolved.
pub(crate) fn canonical(path: &Path) -> Option<PathBuf> {
    tilo_files::canonicalize(path).ok()
}

/// The file, or `None` when it is larger than `limit` bytes.
pub(crate) fn read_bounded(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, FsError> {
    let (dir, name) = at(path)?;
    blocking::read_bounded(&dir, &name, usize::try_from(limit).unwrap_or(usize::MAX))
}
