//! Files named by path (a chooser, a drop, a legacy notebook folder, a
//! capture fixture), through `tilo-files`. None of them has a path on the
//! web, where these report the file system as unsupported.

use std::path::{Path, PathBuf};

use tilo_files::{Dir, FsError, Kind, Location, Meta, VPath, blocking};

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

pub(crate) fn read_to_string(path: &Path) -> Result<String, FsError> {
    String::from_utf8(read(path)?).map_err(|_| FsError::Io("the file is not UTF-8 text".into()))
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

pub(crate) fn metadata(path: &Path) -> Result<Meta, FsError> {
    let (dir, name) = at(path)?;
    blocking::metadata(&dir, &name)
}

pub(crate) fn is_file(meta: &Meta) -> bool {
    meta.kind == Kind::File
}

/// Replaces the file so a crash leaves the old or the new contents.
pub(crate) fn write(path: &Path, bytes: &[u8]) -> Result<(), FsError> {
    let (dir, name) = at(path)?;
    blocking::write_atomic(&dir, &name, bytes)
}

/// Removes a file or an empty directory.
pub(crate) fn remove(path: &Path) -> Result<(), FsError> {
    let (dir, name) = at(path)?;
    blocking::remove(&dir, &name)
}

pub(crate) fn create_dir_all(path: &Path) -> Result<(), FsError> {
    blocking::create_dir_all(&directory(path)?, &VPath::root())
}

/// `error` for callers that report `std::io` errors.
pub(crate) fn io(error: FsError) -> std::io::Error {
    use std::io::ErrorKind;
    let kind = match error {
        FsError::NotFound => ErrorKind::NotFound,
        FsError::AlreadyExists => ErrorKind::AlreadyExists,
        FsError::NotADirectory => ErrorKind::NotADirectory,
        FsError::IsADirectory => ErrorKind::IsADirectory,
        FsError::DirectoryNotEmpty => ErrorKind::DirectoryNotEmpty,
        FsError::PermissionDenied => ErrorKind::PermissionDenied,
        FsError::Unsupported { .. } => ErrorKind::Unsupported,
        _ => ErrorKind::Other,
    };
    std::io::Error::new(kind, error)
}
