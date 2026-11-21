//! Setup's crash journal: which step of the ceremony the person reached, kept
//! as a document of the store in the identity directory, so a restart
//! resumes where they were and never skips a step they had not finished.

use std::path::PathBuf;
use std::rc::Rc;

use durable_store::{DurableStore, StorageKey, WriteOp};

/// The journal's reserved name in the document store beside the identity.
pub const PROGRESS_KEY: &str = "so.tilo.onboarding-progress";
const PROGRESS_VERSION: u32 = 1;
const PROGRESS_MAX_BYTES: u64 = 4096;

/// Secret-free crash journal for the small interval between identity
/// acquisition and explicit first-use completion. It is a document of the
/// store kept in the identity directory, so it follows the identity's
/// per-user lifecycle and trust boundary.
#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum PersistedStage {
    AcquiringIdentity,
    /// The identity exists and setup must still offer a portable backup.
    IdentityReady,
    /// A portable backup was verified, but setup has not reached its hand-off.
    IdentityReadyBackedUp,
    /// The person handled the backup choice and reached the final hand-off.
    ReadyToEnter,
    /// The hand-off was reached after a portable backup was written and
    /// verified. Only this fact is journaled, never the backup's path.
    ReadyToEnterBackedUp,
    TourNavigate,
    TourOrganise,
    TourCollaborate,
    TourStart,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct PersistedProgress {
    version: u32,
    stage: PersistedStage,
}

pub(super) fn progress_key() -> StorageKey {
    StorageKey::app(PROGRESS_KEY)
}

/// The document store the journal is kept in: the identity directory's own.
pub(super) fn journal_store(identity_dir: &std::path::Path) -> Rc<dyn DurableStore> {
    Rc::new(app_state::shared_store(identity_dir))
}

fn store_error(error: &durable_store::StoreError) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

/// The journal's writer: the store it is kept in, leased from the first
/// change written through it.
pub(super) struct Journal {
    pub(super) store: Rc<dyn DurableStore>,
    /// The identity directory, made private before the first change is written.
    pub(super) identity_dir: Option<PathBuf>,
    pub(super) lease: Option<durable_store::Lease>,
}

impl Journal {
    pub(super) fn at(identity_dir: &std::path::Path) -> Self {
        Self {
            store: journal_store(identity_dir),
            identity_dir: Some(identity_dir.to_path_buf()),
            lease: None,
        }
    }

    /// Accept `change` without waiting for the disk: the store commits it on
    /// its own thread, after every change accepted before it.
    pub(super) fn write(
        &mut self,
        change: JournalChange,
    ) -> std::io::Result<durable_store::Revision> {
        if self.lease.is_none() {
            if let Some(identity_dir) = &self.identity_dir {
                crate::firstboot::prepare_identity_directory(identity_dir)?;
            }
            self.lease = Some(
                self.store
                    .lease(&progress_key())
                    .map_err(|error| store_error(&error))?,
            );
        }
        let bytes = match change {
            JournalChange::Store(stage) => serde_json::to_vec_pretty(&PersistedProgress {
                version: PROGRESS_VERSION,
                stage,
            })
            .map_err(|error| std::io::Error::other(error.to_string()))?,
            // An empty journal is no journal.
            JournalChange::Clear => Vec::new(),
        };
        self.store
            .write(&progress_key(), WriteOp::Replace(bytes), None)
            .map_err(|error| store_error(&error))
    }

    /// What became of the change that returned `revision`: `None` while it
    /// is still being written.
    pub(super) fn outcome(&self, revision: durable_store::Revision) -> Option<Result<(), String>> {
        match self.store.durability(&progress_key(), revision) {
            durable_store::Durability::Pending => None,
            durable_store::Durability::Durable => Some(Ok(())),
            durable_store::Durability::Failed(error) => Some(Err(error)),
        }
    }

    /// Wait until every change accepted is on disk.
    /// Wakes `wake` once what was written has settled, kept or refused.
    pub(super) fn wake_when_settled(&self, wake: durable_store::ReadyWake) {
        let _ = self.store.wake_when_settled(&progress_key(), wake);
    }

    pub(super) fn flush(&self) -> std::io::Result<()> {
        self.store
            .flush(Some(&progress_key()))
            .map_err(|error| store_error(&error))
    }
}

/// A change to the crash journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum JournalChange {
    Store(PersistedStage),
    Clear,
}

/// Whether startup must retain trusted first-use chrome even though an
/// identity already exists. A journal that cannot be read counts as
/// pending: malformed recovery state must never become a shortcut around
/// completion.
pub fn has_pending_progress(identity_dir: &std::path::Path) -> bool {
    match journal_store(identity_dir).read(&progress_key()) {
        Ok(stored) => stored.is_some_and(|stored| !stored.bytes.is_empty()),
        Err(_) => true,
    }
}

/// Clear the crash journal left immediately before an identity acquisition
/// only after startup has fully loaded and verified the resulting identity.
///
/// `AcquiringIdentity` protects the write transaction: before a valid
/// identity exists it must keep first-use chrome active. Once the complete
/// identity chain verifies, however, that marker can only be residue from a
/// process interruption between the identity commit and the next journal
/// update. Treating it as post-identity setup would incorrectly send an
/// established user back through recovery on every login.
///
/// # Errors
///
/// Fails when the journal cannot be read, is malformed, or cannot be removed.
pub fn clear_verified_pre_acquisition_progress(
    identity_dir: &std::path::Path,
) -> std::io::Result<bool> {
    if load_progress(identity_dir)? != Some(PersistedStage::AcquiringIdentity) {
        return Ok(false);
    }
    clear_progress(identity_dir)?;
    Ok(true)
}

pub(super) fn load_progress(
    identity_dir: &std::path::Path,
) -> std::io::Result<Option<PersistedStage>> {
    load_progress_in(journal_store(identity_dir).as_ref())
}

fn load_progress_in(store: &dyn DurableStore) -> std::io::Result<Option<PersistedStage>> {
    let bytes = match store
        .read(&progress_key())
        .map_err(|error| store_error(&error))?
    {
        Some(stored) if !stored.bytes.is_empty() => stored.bytes,
        _ => return Ok(None),
    };
    if bytes.len() as u64 > PROGRESS_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "onboarding progress must be small",
        ));
    }
    let stored: PersistedProgress = serde_json::from_slice(&bytes).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid onboarding progress: {error}"),
        )
    })?;
    if stored.version != PROGRESS_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unsupported onboarding progress version {}", stored.version),
        ));
    }
    Ok(Some(stored.stage))
}

/// Write `change` and wait for it to reach the disk.
fn write_progress(identity_dir: &std::path::Path, change: JournalChange) -> std::io::Result<()> {
    let mut journal = Journal::at(identity_dir);
    journal.write(change)?;
    journal.flush()
}

#[cfg(test)]
pub(super) fn store_progress(
    identity_dir: &std::path::Path,
    stage: PersistedStage,
) -> std::io::Result<()> {
    write_progress(identity_dir, JournalChange::Store(stage))
}

pub(super) fn clear_progress(identity_dir: &std::path::Path) -> std::io::Result<()> {
    write_progress(identity_dir, JournalChange::Clear)
}
