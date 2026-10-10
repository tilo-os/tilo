//! Launch leases on payload trees.
//!
//! An update retires the payload tree it replaced, and a collection removes
//! trees no row names. A launch that already chose a tree but whose sandbox
//! has not mounted it yet is invisible to the mount table
//! ([`crate::mounts`]), so it holds `<root>/leases/<package id>` shared
//! ([`hold_payload`]) from before it checks the tree until its sandbox
//! exits. `flock(2)` drops the hold with the launcher however it ends.
//!
//! Whoever removes a tree first takes its lease exclusively without
//! waiting: a held tree stays, as an orphan the next collection after the
//! launch finds free. Otherwise the remover keeps the lease across moving
//! the tree out of `pkgs/` and deleting it, so a launch that arrives
//! meanwhile waits and then finds the tree whole or gone, and removes the
//! lease file with the tree.
//!
//! Leases live beside `pkgs/`, never inside a payload tree, whose files are
//! the signed collection's.

use std::io;
use std::path::Path;

use advisory_lock::{FileLock, LockMode, TryLockError};
use tilo_app_package::PackageId;

use crate::layout::{Layout, default_user_root};

/// Hold the payload tree at `payload_root` for a launch, when it is a tree
/// of the user's store; trees elsewhere (the image's store) are never
/// removed and need no lease. Keep the guard until the sandbox that mounts
/// the tree has exited, and check the tree is still there after taking it.
///
/// # Errors
///
/// Returns the error from creating or locking the lease file.
pub fn hold_payload(payload_root: &Path) -> io::Result<Option<FileLock>> {
    match default_user_root() {
        Some(root) => hold_payload_in(&Layout::new(root), payload_root),
        None => Ok(None),
    }
}

/// [`hold_payload`] for the store at `layout`.
///
/// # Errors
///
/// As [`hold_payload`].
pub fn hold_payload_in(layout: &Layout, payload_root: &Path) -> io::Result<Option<FileLock>> {
    let Some(id) = layout.materialised_id(payload_root) else {
        return Ok(None);
    };
    let path = lease_file(layout, id)?;
    FileLock::open_current(&path, LockMode::Shared).map(Some)
}

/// `id`'s lease, exclusively, when no launch holds it.
pub(crate) fn take(layout: &Layout, id: PackageId) -> io::Result<Option<FileLock>> {
    match FileLock::try_open_current(&lease_file(layout, id)?, LockMode::Exclusive) {
        Ok(lock) => Ok(Some(lock)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// Whether a launch holds `id`, creating nothing.
pub(crate) fn busy(layout: &Layout, id: PackageId) -> io::Result<bool> {
    let path = layout.lease(id);
    if !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
        return Ok(false);
    }
    match FileLock::try_open(&path, LockMode::Exclusive) {
        Ok(_) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// Remove `id`'s lease file, which `taken` holds exclusively; a launch
/// waiting on it takes the next file at the name.
pub(crate) fn forget(layout: &Layout, id: PackageId, taken: FileLock) -> io::Result<()> {
    match std::fs::remove_file(layout.lease(id)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    drop(taken);
    Ok(())
}

/// `<root>/leases/<hex>`, its directory created and never a link.
fn lease_file(layout: &Layout, id: PackageId) -> io::Result<std::path::PathBuf> {
    let path = layout.lease(id);
    if let Some(dir) = path.parent()
        && !std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir())
    {
        match std::fs::remove_file(dir) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        std::fs::create_dir_all(dir)?;
    }
    Ok(path)
}
