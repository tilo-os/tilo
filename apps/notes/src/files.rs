//! Files named by path (a chooser, a drop, a launch), through
//! `tilo-files`. None of them has a path on the
//! web, where these report the file system as unsupported.

use std::path::{Path, PathBuf};

use tilo_files::{Dir, FsError, Location, VPath, blocking};

fn at(path: &Path) -> Result<(Dir, VPath), FsError> {
    tilo_files::file_at(path)
}

fn directory(path: &Path) -> Result<Dir, FsError> {
    Dir::open(Location::Native(path.to_path_buf()))
}

/// `path` with every symbolic link resolved.
pub(crate) fn canonical(path: &Path) -> Option<PathBuf> {
    tilo_files::canonicalize(path).ok()
}

pub(crate) fn read(path: &Path) -> Result<Vec<u8>, FsError> {
    let (dir, name) = at(path)?;
    blocking::read(&dir, &name)
}

/// The file, or `None` when it is larger than `limit` bytes.
pub(crate) fn read_bounded(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, FsError> {
    let (dir, name) = at(path)?;
    if blocking::metadata(&dir, &name)?.len > limit {
        return Ok(None);
    }
    let bytes = blocking::read(&dir, &name)?;
    Ok((u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= limit).then_some(bytes))
}

pub(crate) fn create_dir_all(path: &Path) -> Result<(), FsError> {
    blocking::create_dir_all(&directory(path)?, &VPath::root())
}
