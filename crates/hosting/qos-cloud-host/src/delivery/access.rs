//! Existing-only transaction admission. This component alone does not qualify ingress.
use crate::{Store, StoreGuard};
use std::path::Path;

#[derive(Debug)]
pub(crate) enum AccessError {
    Busy,
    Denied(anyhow::Error),
}

#[cfg(unix)]
pub(crate) fn try_open(root: &Path, machine: &str) -> Result<StoreGuard, AccessError> {
    open_existing(root, machine, false)
}

#[cfg(unix)]
pub(crate) fn open(root: &Path, machine: &str) -> Result<StoreGuard, AccessError> {
    open_existing(root, machine, true)
}

/// The long-lived Store when this process owns `root`, otherwise the
/// existing delivery Store opened from disk for this turn alone.
#[cfg(unix)]
fn open_existing(root: &Path, machine: &str, wait: bool) -> Result<StoreGuard, AccessError> {
    if let Some(lent) = crate::owner_cell::acquire(root, machine, wait) {
        return delivery_selected(lent?);
    }
    let directory_file =
        std::fs::File::open(root).map_err(|error| AccessError::Denied(error.into()))?;
    open_existing_file(root, machine, directory_file, wait).map(StoreGuard::owned)
}

/// The long-lived Store, admitted only once delivery storage is selected.
#[cfg(unix)]
fn delivery_selected(store: StoreGuard) -> Result<StoreGuard, AccessError> {
    if store.state.version != crate::delivery::VERSION {
        return Err(AccessError::Denied(anyhow::anyhow!(
            "existing delivery Store selection required"
        )));
    }
    Ok(store)
}

#[cfg(unix)]
pub(crate) fn try_open_held(
    root: &Path,
    machine: &str,
    directory: &std::fs::File,
) -> Result<StoreGuard, AccessError> {
    if let Some(lent) = crate::owner_cell::acquire(root, machine, false) {
        return delivery_selected(lent?);
    }
    let file = directory
        .try_clone()
        .map_err(|error| AccessError::Denied(error.into()))?;
    open_existing_file(root, machine, file, false).map(StoreGuard::owned)
}

#[cfg(unix)]
fn open_existing_file(
    root: &Path,
    machine: &str,
    directory_file: std::fs::File,
    wait: bool,
) -> Result<Store, AccessError> {
    use crate::storage::transaction::{ExistingStoreLock, LockFailure};
    use atomic_file::StorageDirectory;
    let deny = |error| AccessError::Denied(anyhow::Error::from(error));
    let locked =
        ExistingStoreLock::acquire(root, directory_file, wait).map_err(|error| match error {
            LockFailure::Busy => AccessError::Busy,
            LockFailure::Unavailable(error) => AccessError::Denied(error),
        })?;
    let directory =
        StorageDirectory::from_file(locked.directory().try_clone().map_err(deny)?).map_err(deny)?;
    let persisted =
        crate::storage::read_reconciled_existing(&directory).map_err(AccessError::Denied)?;
    let (directory_file, lock) = locked.into_parts();
    Store::load_locked(
        root,
        machine,
        directory_file,
        lock,
        Some(persisted),
        crate::StoreOpenMode::ExistingDelivery,
    )
    .map_err(AccessError::Denied)
}

#[cfg(not(unix))]
pub(crate) fn try_open(_root: &Path, _machine: &str) -> Result<StoreGuard, AccessError> {
    Err(AccessError::Denied(anyhow::anyhow!(
        "qualified Unix Store required"
    )))
}
#[cfg(not(unix))]
pub(crate) fn open(root: &Path, machine: &str) -> Result<StoreGuard, AccessError> {
    try_open(root, machine)
}

#[cfg(test)]
mod tests;

#[cfg(not(unix))]
pub(crate) fn try_open_held(
    root: &Path,
    machine: &str,
    _directory: &std::fs::File,
) -> Result<StoreGuard, AccessError> {
    try_open(root, machine)
}
