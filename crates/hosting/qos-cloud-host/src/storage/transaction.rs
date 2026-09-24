//! Original Store lock and publication mechanics. No decoded owner or readiness.
use crate::StoreCommitFailure;
use anyhow::Result;
use qos_action_broker::peer_actions::PolicyTime;
use std::{
    cell::Cell,
    fs::File,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub(crate) enum LockFailure {
    Busy,
    Unavailable(anyhow::Error),
}
#[derive(Debug)]
pub(crate) enum TypedLockFailure {
    Busy,
    Io(std::io::Error),
}

pub(crate) struct ExistingStoreLock {
    root: PathBuf,
    directory: File,
    lock: File,
}

impl ExistingStoreLock {
    pub(crate) fn acquire(root: &Path, directory: File, wait: bool) -> Result<Self, LockFailure> {
        Self::acquire_typed(root, directory, wait).map_err(|error| match error {
            TypedLockFailure::Busy => LockFailure::Busy,
            TypedLockFailure::Io(error) => LockFailure::Unavailable(error.into()),
        })
    }
    pub(crate) fn acquire_typed(
        root: &Path,
        directory: File,
        wait: bool,
    ) -> std::result::Result<Self, TypedLockFailure> {
        use atomic_file::{Privacy, StorageDirectory};
        use std::{ffi::OsStr, fs::TryLockError};
        let deny = TypedLockFailure::Io;
        let admitted =
            StorageDirectory::from_file(directory.try_clone().map_err(deny)?).map_err(deny)?;
        admitted
            .validate_privacy(Privacy::OwnerOnly)
            .map_err(deny)?;
        // The sentinel is opened relative to this exact directory. No create,
        // namespace repair or ambient path lookup is permitted here.
        let lock = admitted
            .open_read(OsStr::new("state.lock"), Privacy::Inherit)
            .map_err(deny)?;
        if wait {
            lock.lock().map_err(deny)?;
        } else {
            match lock.try_lock() {
                Ok(()) => (),
                Err(TryLockError::WouldBlock) => return Err(TypedLockFailure::Busy),
                Err(TryLockError::Error(error)) => return Err(deny(error)),
            }
        }
        Ok(Self {
            root: root.into(),
            directory,
            lock,
        })
    }

    pub(crate) fn directory(&self) -> &File {
        &self.directory
    }
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn into_parts(self) -> (File, File) {
        (self.directory, self.lock)
    }
}

#[derive(Debug)]
pub(crate) enum PublishFailure {
    Latched(StoreCommitFailure),
    Clock(qos_action_broker::peer_actions::PolicyClockError),
    Io {
        failure: StoreCommitFailure,
        error: std::io::Error,
    },
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "only tests call the untyped publisher; production uses the typed variants"
    )
)]
pub(crate) fn publish_bytes(
    directory: &File,
    root: &Path,
    failed: &Cell<Option<StoreCommitFailure>>,
    time: Option<&PolicyTime>,
    generation: u64,
    bytes: &[u8],
) -> Result<()> {
    publish_bytes_typed(directory, root, failed, time, generation, bytes)
        .map_err(PublishFailure::into_error)
}
impl PublishFailure {
    pub(crate) fn into_error(self) -> anyhow::Error {
        match self {
            PublishFailure::Latched(failure) => crate::commit_error(
                anyhow::anyhow!("failed cloud transaction requires reopen"),
                failure,
            ),
            PublishFailure::Clock(error) => error.into(),
            PublishFailure::Io { failure, error } => crate::commit_error(error.into(), failure),
        }
    }
}

pub(crate) fn publish_bytes_typed(
    directory: &File,
    root: &Path,
    failed: &Cell<Option<StoreCommitFailure>>,
    time: Option<&PolicyTime>,
    generation: u64,
    bytes: &[u8],
) -> std::result::Result<(), PublishFailure> {
    // No known diff base: the aggregate is checkpointed (any journal retired).
    publish_bytes_with_tip(directory, root, failed, time, generation, bytes, &mut None)
}

/// `publish_bytes_typed` from the last persisted state `tip` of this lock
/// hold, which lets a small change append to the journal. `tip` follows the
/// write: it describes `bytes` afterwards, or is cleared on failure.
pub(crate) fn publish_bytes_with_tip(
    directory: &File,
    root: &Path,
    failed: &Cell<Option<StoreCommitFailure>>,
    time: Option<&PolicyTime>,
    generation: u64,
    bytes: &[u8],
    tip: &mut Option<super::Tip>,
) -> std::result::Result<(), PublishFailure> {
    if let Some(failure) = failed.get() {
        return Err(PublishFailure::Latched(failure));
    }
    // The selected owner samples its retained origin immediately before the
    // original publication. No wall-clock fallback can revive a retired origin.
    if let Some(time) = time {
        time.now_ns().map_err(PublishFailure::Clock)?;
    }
    if let Err(super::WriteError { state, error }) = super::write(directory, root, tip, bytes) {
        let outcome = match state {
            atomic_file::PublicationState::NotPublished => crate::StoreSaveOutcome::NotPublished,
            atomic_file::PublicationState::PublishedDurabilityUnknown => {
                crate::StoreSaveOutcome::PublishedDurabilityUnknown
            }
            atomic_file::PublicationState::PublicationUnknown => {
                crate::StoreSaveOutcome::PublicationUnknown
            }
        };
        let failure = StoreCommitFailure {
            generation,
            outcome,
        };
        failed.set(Some(failure));
        return Err(PublishFailure::Io { failure, error });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
