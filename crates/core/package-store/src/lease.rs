//! Launch leases on payload trees.

use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "store")]
use advisory_lock::TryLockError;
use advisory_lock::{FileLock, LockMode};
use tilo_app_package::PackageId;

use crate::layout::{Layout, default_user_root};
use crate::registry::Registry;

/// Hold the payload tree at `payload_root` for a launch, when it is a tree
/// of the user's store; trees elsewhere (the image's store) are never
/// removed and need no lease. Keep the guard until the sandbox that mounts
/// the tree has exited, and check the tree is still there after taking it.
pub fn hold_payload(payload_root: &Path) -> io::Result<Option<FileLock>> {
    match default_user_root() {
        Some(root) => hold_payload_in(&Layout::new(root), payload_root),
        None => Ok(None),
    }
}

pub fn hold_payload_in(layout: &Layout, payload_root: &Path) -> io::Result<Option<FileLock>> {
    let Some(id) = layout.materialised_id(payload_root) else {
        return Ok(None);
    };
    let path = lease_file(layout, id)?;
    FileLock::open_current(&path, LockMode::Shared).map(Some)
}

/// Record that the launch holding `lease` used its tree now: the app it
/// opened is closing, or still running.
///
/// # Errors
///
/// When the lease file's time cannot be set.
pub fn record_use(lease: &FileLock) -> io::Result<()> {
    lease.file().set_modified(SystemTime::now())
}

/// Record that the launch of `app_id`, which held `lease` on the tree at
/// `payload_root`, ends now: its app is closing.
///
/// The use goes on the held tree, and also on the tree `app_id`'s row names
/// now when an update replaced the held one while the app was open: the
/// replaced tree's lease leaves with that tree, and the update carried over
/// only the use before it, so the close would otherwise be lost.
///
/// # Errors
///
/// When a lease file's time cannot be set or the registry cannot be read.
pub fn record_close(lease: &FileLock, app_id: &str, payload_root: &Path) -> io::Result<()> {
    record_use(lease)?;
    match default_user_root() {
        Some(root) => record_use_of_replacement(&Layout::new(root), app_id, payload_root),
        None => Ok(()),
    }
}

/// When `app_id`'s row in the store at `layout` names a tree other than
/// `ran_from`, record a use of that tree.
fn record_use_of_replacement(layout: &Layout, app_id: &str, ran_from: &Path) -> io::Result<()> {
    let registry = Registry::load(&layout.registry()).map_err(io::Error::other)?;
    let Some(row) = registry.find(app_id) else {
        return Ok(());
    };
    if row.payload_root == ran_from {
        return Ok(());
    }
    match hold_payload_in(layout, &row.payload_root)? {
        Some(held) => record_use(&held),
        None => Ok(()),
    }
}

/// When a launch last used `id`'s tree, in Unix seconds; `None` when no
/// launch ever held it.
#[must_use]
pub fn last_use(layout: &Layout, id: PackageId) -> Option<u64> {
    let meta = std::fs::symlink_metadata(layout.lease(id)).ok()?;
    if !meta.is_file() {
        return None;
    }
    let since = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(since.as_secs())
}

/// `id`'s lease, exclusively, when no launch holds it.
#[cfg(feature = "store")]
pub(crate) fn take(layout: &Layout, id: PackageId) -> io::Result<Option<FileLock>> {
    match FileLock::try_open_current(&lease_file(layout, id)?, LockMode::Exclusive) {
        Ok(lock) => Ok(Some(lock)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(feature = "store")]
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
#[cfg(feature = "store")]
pub(crate) fn forget(layout: &Layout, id: PackageId, taken: FileLock) -> io::Result<()> {
    match std::fs::remove_file(layout.lease(id)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    drop(taken);
    Ok(())
}

/// `<root>/leases/<hex>`, its directory created and never a link.
///
/// Launches take leases at the same time (an agent and its companions start
/// together), so the directory may appear between the look and the removal
/// of whatever was there instead: a directory found then is the one wanted,
/// not a failure.
fn lease_file(layout: &Layout, id: PackageId) -> io::Result<std::path::PathBuf> {
    let path = layout.lease(id);
    if let Some(dir) = path.parent()
        && !std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir())
    {
        match std::fs::remove_file(dir) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::IsADirectory
                ) => {}
            Err(error) => return Err(error),
        }
        std::fs::create_dir_all(dir)?;
        if !std::fs::symlink_metadata(dir)?.is_dir() {
            return Err(io::Error::other(format!(
                "{} is not a directory of leases",
                dir.display()
            )));
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launches_taking_first_leases_at_once_all_hold() {
        const LAUNCHES: usize = 16;
        for _ in 0..8 {
            let root = tempfile::tempdir().expect("store root");
            let layout = Layout::new(root.path().to_path_buf());
            let payloads: Vec<_> = (0..LAUNCHES)
                .map(|n| {
                    let payload = layout.packages().join(format!("{n:064x}"));
                    std::fs::create_dir_all(&payload).expect("payload tree");
                    payload
                })
                .collect();
            let start = std::sync::Barrier::new(LAUNCHES);
            std::thread::scope(|scope| {
                let held: Vec<_> = payloads
                    .iter()
                    .map(|payload| {
                        let (layout, start) = (&layout, &start);
                        scope.spawn(move || {
                            start.wait();
                            hold_payload_in(layout, payload)
                        })
                    })
                    .collect();
                for launch in held {
                    let lease = launch.join().expect("launch thread");
                    assert!(
                        lease.expect("the lease is held").is_some(),
                        "a store tree is leased"
                    );
                }
            });
        }
    }
}
