//! [`Dir`]'s operations, synchronously: loading and saving from an event handler, a
//! CLI, a `Drop` that must persist. Each call finishes on the calling thread. On the
//! web they work on memory trees and on the app locations the host mirrored
//! (`web::mirror`), whose changes are written through to browser storage after the call
//! returns; anywhere else, and for locks, they return [`FsError::Unsupported`] with
//! [`Feature::Blocking`](crate::Feature::Blocking), because the page's thread must not
//! wait.

use crate::{Dir, Entry, FsError, Lock, LockMode, Meta, VPath};

pub fn read(dir: &Dir, path: &VPath) -> Result<Vec<u8>, FsError> {
    dir.local()?.read(path)
}

/// The first `max` bytes of the file (all of it when shorter), for a media header: only the prefix is
/// read from disk.
pub fn read_prefix(dir: &Dir, path: &VPath, max: usize) -> Result<Vec<u8>, FsError> {
    dir.local()?.read_prefix(path, max)
}

/// The whole file when it holds at most `max` bytes, `None` when more: a user's file opened under a
/// size limit. At most `max + 1` bytes are read, so a file that grows while it is read is still refused
/// without loading it.
pub fn read_bounded(dir: &Dir, path: &VPath, max: usize) -> Result<Option<Vec<u8>>, FsError> {
    let bytes = read_prefix(dir, path, max.saturating_add(1))?;
    Ok((bytes.len() <= max).then_some(bytes))
}

/// Adds `bytes` to the end, creating the file and its folders when missing: a progressive download a
/// reader follows, a log. Not atomic: a crash may leave part of `bytes` written.
pub fn append(dir: &Dir, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    crate::below_root(path)?;
    dir.local()?.append(path, bytes)
}

/// Replaces the contents in place, creating the file and its folders when missing: a cache entry the
/// app can fetch again. Not crash-safe, where [`write_atomic`] leaves the old or the new contents.
pub fn write(dir: &Dir, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    crate::below_root(path)?;
    dir.local()?.write(path, bytes)
}

pub fn write_atomic(dir: &Dir, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    crate::below_root(path)?;
    dir.local()?.write_atomic(path, bytes)
}

pub fn save_document(dir: &Dir, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    crate::below_root(path)?;
    dir.local()?.save_document(path, bytes)
}

pub fn write_private(dir: &Dir, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    crate::below_root(path)?;
    dir.local()?.write_private(path, bytes)
}

pub fn create_new(dir: &Dir, path: &VPath, bytes: &[u8]) -> Result<bool, FsError> {
    crate::below_root(path)?;
    dir.local()?.create_new(path, bytes)
}

pub fn rename_noreplace(dir: &Dir, from: &VPath, to: &VPath) -> Result<(), FsError> {
    crate::check_rename(from, to)?;
    dir.local()?.rename_noreplace(from, to)
}

pub fn remove(dir: &Dir, path: &VPath) -> Result<(), FsError> {
    crate::below_root(path)?;
    dir.local()?.remove(path)
}

/// A file, or a directory and everything below it: a cache the app discards. A symbolic link is removed
/// itself; what it points to is kept.
pub fn remove_all(dir: &Dir, path: &VPath) -> Result<(), FsError> {
    crate::below_root(path)?;
    dir.local()?.remove_all(path)
}

pub fn create_dir_all(dir: &Dir, path: &VPath) -> Result<(), FsError> {
    dir.local()?.create_dir_all(path)
}

pub fn list(dir: &Dir, path: &VPath) -> Result<Vec<Entry>, FsError> {
    dir.local()?.list(path)
}

pub fn metadata(dir: &Dir, path: &VPath) -> Result<Meta, FsError> {
    dir.local()?.metadata(path)
}

pub fn lock(dir: &Dir, path: &VPath, mode: LockMode) -> Result<Lock, FsError> {
    crate::below_root(path)?;
    let held = dir.local()?.lock_blocking(path, mode)?;
    Ok(Lock { mode, _held: held })
}

/// Takes the lock only if nobody holds a conflicting one; `None` when it is busy.
pub fn try_lock(dir: &Dir, path: &VPath, mode: LockMode) -> Result<Option<Lock>, FsError> {
    crate::below_root(path)?;
    let held = dir.local()?.try_lock(path, mode)?;
    Ok(held.map(|held| Lock { mode, _held: held }))
}
