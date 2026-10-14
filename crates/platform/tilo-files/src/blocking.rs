//! [`Dir`]'s operations, synchronously: an app loading and saving its own
//! files from an event handler, a CLI, a `Drop` that must persist. Each call
//! finishes on the calling thread.
//!
//! On the web they work on memory trees and on the app locations the host
//! mirrored ([`web::mirror`](crate::web::mirror)): an app's `data`, `config`
//! and `state`. Changes there are written through to the browser's storage
//! in order, after the call returns. Anywhere else, and for locks, they
//! return [`FsError::Unsupported`] with
//! [`Feature::Blocking`](crate::Feature::Blocking): the page's thread must
//! not wait.

use crate::{Dir, Entry, FsError, Lock, LockMode, Meta, VPath};

/// [`Dir::read`], synchronously.
///
/// # Errors
///
/// As [`Dir::read`], and [`FsError::Unsupported`] on the web.
pub fn read(dir: &Dir, path: &VPath) -> Result<Vec<u8>, FsError> {
    dir.local()?.read(path)
}

/// The first `max` bytes of the file (all of it when shorter): a media
/// header, without reading a large file whole. Only the prefix is read from
/// disk.
///
/// # Errors
///
/// As [`Dir::read`], and [`FsError::Unsupported`] on the web off the mirror.
pub fn read_prefix(dir: &Dir, path: &VPath, max: usize) -> Result<Vec<u8>, FsError> {
    dir.local()?.read_prefix(path, max)
}

/// The whole file when it holds at most `max` bytes, `None` when it holds
/// more: a user's file opened under a size limit. At most `max + 1` bytes
/// are read, so a file that grows while it is read is still refused without
/// loading it.
///
/// # Errors
///
/// As [`read_prefix`].
pub fn read_bounded(dir: &Dir, path: &VPath, max: usize) -> Result<Option<Vec<u8>>, FsError> {
    let bytes = read_prefix(dir, path, max.saturating_add(1))?;
    Ok((bytes.len() <= max).then_some(bytes))
}

/// Add `bytes` to the end of the file, creating it and its folders when
/// missing: a progressive download a reader follows, a log. Not atomic: a
/// crash may leave part of `bytes` written.
///
/// # Errors
///
/// [`FsError::IsADirectory`], the backend's failure, and
/// [`FsError::Unsupported`] on the web off the mirror.
pub fn append(dir: &Dir, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    crate::below_root(path)?;
    dir.local()?.append(path, bytes)
}

/// Replace the file's contents in place, creating it and its folders when
/// missing: a cache entry the app can fetch again. Not crash-safe: a crash
/// may leave the file partly written, where [`write_atomic`] leaves the old
/// or the new contents.
///
/// # Errors
///
/// [`FsError::IsADirectory`], the backend's failure, and
/// [`FsError::Unsupported`] on the web off the mirror.
pub fn write(dir: &Dir, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    crate::below_root(path)?;
    dir.local()?.write(path, bytes)
}

/// [`Dir::write_atomic`], synchronously.
///
/// # Errors
///
/// As [`Dir::write_atomic`], and [`FsError::Unsupported`] on the web.
pub fn write_atomic(dir: &Dir, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    crate::below_root(path)?;
    dir.local()?.write_atomic(path, bytes)
}

/// [`Dir::save_document`], synchronously.
///
/// # Errors
///
/// As [`Dir::save_document`], and [`FsError::Unsupported`] on the web off the
/// mirror.
pub fn save_document(dir: &Dir, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    crate::below_root(path)?;
    dir.local()?.save_document(path, bytes)
}

/// [`Dir::write_private`], synchronously.
///
/// # Errors
///
/// As [`Dir::write_private`], and [`FsError::Unsupported`] on the web off the
/// mirror.
pub fn write_private(dir: &Dir, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    crate::below_root(path)?;
    dir.local()?.write_private(path, bytes)
}

/// [`Dir::create_new`], synchronously.
///
/// # Errors
///
/// As [`Dir::create_new`], and [`FsError::Unsupported`] on the web.
pub fn create_new(dir: &Dir, path: &VPath, bytes: &[u8]) -> Result<bool, FsError> {
    crate::below_root(path)?;
    dir.local()?.create_new(path, bytes)
}

/// [`Dir::rename_noreplace`], synchronously.
///
/// # Errors
///
/// As [`Dir::rename_noreplace`], and [`FsError::Unsupported`] on the web.
pub fn rename_noreplace(dir: &Dir, from: &VPath, to: &VPath) -> Result<(), FsError> {
    crate::check_rename(from, to)?;
    dir.local()?.rename_noreplace(from, to)
}

/// [`Dir::remove`], synchronously.
///
/// # Errors
///
/// As [`Dir::remove`], and [`FsError::Unsupported`] on the web.
pub fn remove(dir: &Dir, path: &VPath) -> Result<(), FsError> {
    crate::below_root(path)?;
    dir.local()?.remove(path)
}

/// Remove a file, or a directory and everything below it: a cache the app
/// discards. A symbolic link is removed itself; what it points to is kept.
///
/// # Errors
///
/// [`FsError::NotFound`], [`FsError::InvalidPath`] for the root, the
/// backend's failure, and [`FsError::Unsupported`] on the web off the mirror.
pub fn remove_all(dir: &Dir, path: &VPath) -> Result<(), FsError> {
    crate::below_root(path)?;
    dir.local()?.remove_all(path)
}

/// [`Dir::create_dir_all`], synchronously.
///
/// # Errors
///
/// As [`Dir::create_dir_all`], and [`FsError::Unsupported`] on the web.
pub fn create_dir_all(dir: &Dir, path: &VPath) -> Result<(), FsError> {
    dir.local()?.create_dir_all(path)
}

/// [`Dir::list`], synchronously.
///
/// # Errors
///
/// As [`Dir::list`], and [`FsError::Unsupported`] on the web.
pub fn list(dir: &Dir, path: &VPath) -> Result<Vec<Entry>, FsError> {
    dir.local()?.list(path)
}

/// [`Dir::metadata`], synchronously.
///
/// # Errors
///
/// As [`Dir::metadata`], and [`FsError::Unsupported`] on the web.
pub fn metadata(dir: &Dir, path: &VPath) -> Result<Meta, FsError> {
    dir.local()?.metadata(path)
}

/// [`Dir::lock`], waiting on this thread.
///
/// # Errors
///
/// As [`Dir::lock`], and [`FsError::Unsupported`] on the web.
pub fn lock(dir: &Dir, path: &VPath, mode: LockMode) -> Result<Lock, FsError> {
    crate::below_root(path)?;
    let held = dir.local()?.lock_blocking(path, mode)?;
    Ok(Lock { mode, _held: held })
}

/// Takes the lock only if nobody holds a conflicting one; `None` when the
/// lock is busy.
///
/// # Errors
///
/// As [`Dir::lock`], and [`FsError::Unsupported`] on the web.
pub fn try_lock(dir: &Dir, path: &VPath, mode: LockMode) -> Result<Option<Lock>, FsError> {
    crate::below_root(path)?;
    let held = dir.local()?.try_lock(path, mode)?;
    Ok(held.map(|held| Lock { mode, _held: held }))
}
