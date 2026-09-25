//! Existing-only transaction admission. This component alone does not qualify ingress.
use crate::Store;
use std::path::Path;

#[derive(Debug)]
pub(crate) enum AccessError {
    Busy,
    Denied(anyhow::Error),
}

#[cfg(unix)]
pub(crate) fn try_open(root: &Path, machine: &str) -> Result<Store, AccessError> {
    open_existing(root, machine, false)
}

#[cfg(unix)]
pub(crate) fn open(root: &Path, machine: &str) -> Result<Store, AccessError> {
    open_existing(root, machine, true)
}

#[cfg(unix)]
fn open_existing(root: &Path, machine: &str, wait: bool) -> Result<Store, AccessError> {
    let directory_file =
        std::fs::File::open(root).map_err(|error| AccessError::Denied(error.into()))?;
    open_existing_file(root, machine, directory_file, wait)
}

#[cfg(unix)]
pub(crate) fn try_open_held(
    root: &Path,
    machine: &str,
    directory: &std::fs::File,
) -> Result<Store, AccessError> {
    let file = directory
        .try_clone()
        .map_err(|error| AccessError::Denied(error.into()))?;
    open_existing_file(root, machine, file, false)
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

/// A validated decoded Store state kept between exclusive turns, together
/// with open handles on the exact aggregate files it was decoded from
/// (`storage::Aggregate`). Holding the handles keeps those inodes alive, so
/// equality under the next turn's lock proves the persisted aggregate is
/// unchanged. The retained diff base lets such a turn still journal.
#[cfg(unix)]
pub(crate) struct RetainedState {
    aggregate: crate::storage::Aggregate,
    state: crate::State,
    tip: Option<crate::storage::Tip>,
}
#[cfg(unix)]
impl RetainedState {
    pub(crate) fn generation(&self) -> u64 {
        self.state.generation
    }
    /// Under the caller's held Store lock: whether `current` observes the
    /// exact aggregate this state was decoded from.
    pub(crate) fn same_aggregate(&self, current: &crate::storage::Aggregate) -> bool {
        self.aggregate.same(current)
    }
    /// A copy of the validated decode (and its diff base) for another Store handle.
    pub(crate) fn state_copy(&self) -> (crate::State, Option<crate::storage::Tip>) {
        (self.state.clone(), self.tip.clone())
    }
    /// Bytes retained besides the decode (the diff base), for charging.
    pub(crate) fn base_len(&self) -> usize {
        self.tip.as_ref().map_or(0, crate::storage::Tip::len)
    }
    /// Cheap unlocked hint: the directory still names the retained files.
    /// The next turn re-proves this under the Store lock.
    pub(crate) fn matches(&self, directory: &std::fs::File) -> bool {
        self.aggregate.matches_unlocked(directory)
    }
}
/// Handle of the aggregate an opened Store was decoded from.
#[cfg(unix)]
pub(crate) struct OpenedAggregate {
    aggregate: crate::storage::Aggregate,
    generation: u64,
    pub(crate) reused: bool,
}
#[cfg(unix)]
impl OpenedAggregate {
    pub(crate) fn length(&self) -> u64 {
        self.aggregate.length()
    }
    /// Retain the Store's state for the next turn only when this turn neither
    /// committed nor latched a failure: the retained value is then exactly the
    /// validated decode of the still-open aggregate handles. An observation
    /// with a torn or stale journal tail is never retained.
    pub(crate) fn retain(self, store: Store) -> Option<RetainedState> {
        if store.failed_commit.get().is_some()
            || store.state.generation != self.generation
            || !self.aggregate.clean()
        {
            return None;
        }
        Some(RetainedState {
            aggregate: self.aggregate,
            tip: store.tip.into_inner(),
            state: store.state,
        })
    }
}

/// `try_open`, reusing a retained decode when the locked aggregate is the
/// exact same files. Any mismatch falls back to the full read, durability
/// reconciliation and validation of `try_open`.
#[cfg(unix)]
pub(crate) fn try_open_retained(
    root: &Path,
    machine: &str,
    retained: &mut Option<RetainedState>,
    admit_decode: &mut dyn FnMut() -> bool,
) -> Result<(Store, OpenedAggregate), AccessError> {
    use crate::storage::{
        Aggregate,
        transaction::{ExistingStoreLock, LockFailure},
    };
    use atomic_file::StorageDirectory;
    let deny = |error| AccessError::Denied(anyhow::Error::from(error));
    let directory_file = std::fs::File::open(root).map_err(deny)?;
    let locked =
        ExistingStoreLock::acquire(root, directory_file, false).map_err(|error| match error {
            LockFailure::Busy => AccessError::Busy,
            LockFailure::Unavailable(error) => AccessError::Denied(error),
        })?;
    let directory =
        StorageDirectory::from_file(locked.directory().try_clone().map_err(deny)?).map_err(deny)?;
    let mut aggregate = Aggregate::open(&directory).map_err(deny)?;
    if retained
        .as_ref()
        .is_some_and(|candidate| candidate.aggregate.same(&aggregate))
    {
        let retained = retained.take().expect("checked retained state");
        let (directory_file, lock) = locked.into_parts();
        let generation = retained.state.generation;
        let store = Store {
            policy_time: None,
            root: root.into(),
            directory: directory_file,
            state: retained.state,
            _lock: crate::HeldLock::new(lock),
            failed_commit: std::cell::Cell::new(None),
            tip: std::cell::RefCell::new(retained.tip),
        };
        return Ok((
            store,
            OpenedAggregate {
                aggregate: retained.aggregate,
                generation,
                reused: true,
            },
        ));
    }
    // A stale retained decode is released before the full decode, which must
    // first be admitted by the caller's budget.
    *retained = None;
    if !admit_decode() {
        return Err(AccessError::Busy);
    }
    let persisted = aggregate
        .read_reconciled(&directory)
        .map_err(AccessError::Denied)?;
    let (directory_file, lock) = locked.into_parts();
    let store = Store::load_locked(
        root,
        machine,
        directory_file,
        lock,
        Some(persisted),
        crate::StoreOpenMode::ExistingDelivery,
    )
    .map_err(AccessError::Denied)?;
    let generation = store.state.generation;
    Ok((
        store,
        OpenedAggregate {
            aggregate,
            generation,
            reused: false,
        },
    ))
}

/// Blocking `open`, reusing a copy of a validated decode when `reuse`
/// (called under the acquired Store lock with the observed aggregate)
/// returns one for the identical files. Returns whether the copy was used.
#[cfg(unix)]
pub(crate) fn open_reusing(
    root: &Path,
    machine: &str,
    reuse: impl FnOnce(
        &crate::storage::Aggregate,
    ) -> Option<(crate::State, Option<crate::storage::Tip>)>,
) -> Result<(Store, bool), AccessError> {
    use crate::storage::{
        Aggregate,
        transaction::{ExistingStoreLock, LockFailure},
    };
    use atomic_file::StorageDirectory;
    let deny = |error| AccessError::Denied(anyhow::Error::from(error));
    let directory_file = std::fs::File::open(root).map_err(deny)?;
    let began = std::time::Instant::now();
    let locked =
        ExistingStoreLock::acquire(root, directory_file, true).map_err(|error| match error {
            LockFailure::Busy => AccessError::Busy,
            LockFailure::Unavailable(error) => AccessError::Denied(error),
        })?;
    qos_action_broker::agent_timing::slow_store("store_lock_wait", began, &[]);
    let directory =
        StorageDirectory::from_file(locked.directory().try_clone().map_err(deny)?).map_err(deny)?;
    let mut aggregate = Aggregate::open(&directory).map_err(deny)?;
    let began = std::time::Instant::now();
    let reused = reuse(&aggregate);
    qos_action_broker::agent_timing::slow_store(
        "store_reuse_copy",
        began,
        &[("reused", u64::from(reused.is_some()))],
    );
    if let Some((state, tip)) = reused {
        let (directory_file, lock) = locked.into_parts();
        return Ok((
            Store {
                policy_time: None,
                root: root.into(),
                directory: directory_file,
                state,
                _lock: crate::HeldLock::new(lock),
                failed_commit: std::cell::Cell::new(None),
                tip: std::cell::RefCell::new(tip),
            },
            true,
        ));
    }
    let persisted = aggregate
        .read_reconciled(&directory)
        .map_err(AccessError::Denied)?;
    let (directory_file, lock) = locked.into_parts();
    Store::load_locked(
        root,
        machine,
        directory_file,
        lock,
        Some(persisted),
        crate::StoreOpenMode::ExistingDelivery,
    )
    .map(|store| (store, false))
    .map_err(AccessError::Denied)
}

#[cfg(not(unix))]
pub(crate) fn try_open(_root: &Path, _machine: &str) -> Result<Store, AccessError> {
    Err(AccessError::Denied(anyhow::anyhow!(
        "qualified Unix Store required"
    )))
}
#[cfg(not(unix))]
pub(crate) fn open(root: &Path, machine: &str) -> Result<Store, AccessError> {
    try_open(root, machine)
}

#[cfg(test)]
mod tests;

#[cfg(not(unix))]
pub(crate) fn try_open_held(
    root: &Path,
    machine: &str,
    _directory: &std::fs::File,
) -> Result<Store, AccessError> {
    try_open(root, machine)
}
