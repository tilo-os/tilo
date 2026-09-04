//! Durable pending-submission, review, and quarantine lifecycle.
//!
//! Approval and restoration re-verify publisher material at the point it can
//! become public. Rejection intentionally accepts malformed/revoked requests,
//! preserving their original bytes in an audit record while draining the queue.

use std::sync::{Arc, Mutex, OnceLock};

#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::fd::AsRawFd;

use registry_types::{
    EntryAction, EntryChangeRequest, RegistryEntry, VerifyError, verify_change_request,
    verify_entry_signature,
};
use thiserror::Error;

use crate::allowlist::PublisherAllowlist;
use crate::storage::{
    PendingSubmission, ReviewDecision, ReviewRecord, ReviewSubject, Storage, StorageError,
};

/// Thread-safe moderation façade. Clones share the in-process transition lock.
#[derive(Clone)]
pub struct ModerationService {
    storage: Storage,
    transition_lock: Arc<Mutex<()>>,
}

impl ModerationService {
    /// Create a moderation service backed by `storage`.
    pub fn new(storage: Storage) -> Self {
        Self {
            storage,
            transition_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Queue a signed publisher add, update, or delete request.
    ///
    /// Signature and allowlist verification deliberately occur at approval.
    /// This method nevertheless validates public and pending ownership and
    /// timestamps, giving publishers immediate feedback on stale submissions.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid ownership or timestamps, lock failures, or
    /// persistence failures.
    pub fn submit(
        &self,
        change: EntryChangeRequest,
        submitted_at_unix: u64,
    ) -> Result<PendingSubmission, ModerationError> {
        let _thread_guard = thread_lock(&self.transition_lock)?;
        let _process_guard = ProcessGuard::acquire(&self.storage)?;
        let package_id = &change.entry.package_id;
        let public = self.storage.load_entry(package_id)?;
        let pending = self.storage.load_pending(package_id)?;
        if let Some(existing) = pending.as_ref()
            && existing.change == change
        {
            return Ok(existing.clone());
        }
        validate_submission(&change, public.as_ref(), pending.as_ref())?;
        let submission = PendingSubmission {
            // The package id is a single atomic pending slot: replacing a
            // revision is one rename rather than a delete/create window.
            submission_id: package_id.clone(),
            submitted_at_unix,
            change,
        };
        self.storage.save_pending(&submission)?;
        Ok(submission)
    }

    /// Fetch a pending submission by package id.
    ///
    /// # Errors
    ///
    /// Returns an error if the durable submission cannot be read.
    pub fn pending(
        &self,
        submission_id: &str,
    ) -> Result<Option<PendingSubmission>, ModerationError> {
        Ok(self.storage.load_pending(submission_id)?)
    }

    /// List pending publisher submissions. Ordering is not defined.
    ///
    /// # Errors
    ///
    /// Returns an error if the durable queue cannot be read.
    pub fn pending_submissions(&self) -> Result<Vec<PendingSubmission>, ModerationError> {
        Ok(self.storage.load_pending_submissions()?)
    }

    /// Approve a pending request after re-verifying it against the current
    /// owner allowlist, then apply it to the public index.
    ///
    /// # Errors
    ///
    /// Returns an error when the submission is missing, no longer trusted,
    /// conflicts with public state, or cannot be persisted atomically.
    pub fn approve(
        &self,
        reviewed_submission: &PendingSubmission,
        reviewer: impl Into<String>,
        reason: impl Into<String>,
        reviewed_at_unix: u64,
        allowlist: &PublisherAllowlist,
    ) -> Result<ReviewRecord, ModerationError> {
        let _thread_guard = thread_lock(&self.transition_lock)?;
        let _process_guard = ProcessGuard::acquire(&self.storage)?;
        let pending = self.require_pending(&reviewed_submission.submission_id)?;
        if &pending != reviewed_submission {
            return Err(ModerationError::PendingChanged(
                reviewed_submission.submission_id.clone(),
            ));
        }
        verify_pending(&pending, allowlist)?;
        let review_id = submission_review_id(&pending);
        let prior_review = self.storage.load_review(&review_id)?;
        if let Some(review) = prior_review {
            // Public state is saved before audit, so this is crash cleanup.
            let _deleted = self
                .storage
                .delete_pending(&reviewed_submission.submission_id)?;
            return Ok(review);
        }
        let public = self.storage.load_entry(&pending.change.entry.package_id)?;
        let already_applied = validate_approval(&pending.change, public.as_ref())?;

        if !already_applied {
            match pending.change.action {
                EntryAction::Add | EntryAction::Update => {
                    self.storage.save_entry(&pending.change.entry)?;
                }
                EntryAction::Delete => {
                    let _deleted = self
                        .storage
                        .delete_entry(&pending.change.entry.package_id)?;
                }
            }
        }
        let review = ReviewRecord {
            review_id,
            decision: ReviewDecision::Approved,
            reviewer: reviewer.into(),
            reason: reason.into(),
            reviewed_at_unix,
            subject: ReviewSubject::Submission(pending),
        };
        self.persist_review_if_absent(&review)?;
        // save public state -> save audit -> remove pending. A crash in either
        // gap can be retried without overwriting a newer public revision.
        let _deleted = self
            .storage
            .delete_pending(&reviewed_submission.submission_id)?;
        Ok(review)
    }

    /// Reject a pending request without changing public state.
    ///
    /// Rejection intentionally does not re-verify, so a revoked or malformed
    /// publisher request cannot become permanently stuck in `pending/`.
    ///
    /// # Errors
    ///
    /// Returns an error when the submission is missing or the decision cannot
    /// be persisted atomically.
    pub fn reject(
        &self,
        submission_id: &str,
        reviewer: impl Into<String>,
        reason: impl Into<String>,
        reviewed_at_unix: u64,
    ) -> Result<ReviewRecord, ModerationError> {
        let _thread_guard = thread_lock(&self.transition_lock)?;
        let _process_guard = ProcessGuard::acquire(&self.storage)?;
        let pending = self.require_pending(submission_id)?;
        let review_id = submission_review_id(&pending);
        let review = self
            .storage
            .load_review(&review_id)?
            .unwrap_or_else(|| ReviewRecord {
                review_id,
                decision: ReviewDecision::Rejected,
                reviewer: reviewer.into(),
                reason: reason.into(),
                reviewed_at_unix,
                subject: ReviewSubject::Submission(pending),
            });
        self.persist_review_if_absent(&review)?;
        let _deleted = self.storage.delete_pending(submission_id)?;
        Ok(review)
    }

    /// Remove a public entry from visibility while retaining it in quarantine.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry is missing or the quarantine transition
    /// cannot be persisted atomically.
    pub fn quarantine(
        &self,
        package_id: &str,
        reviewer: impl Into<String>,
        reason: impl Into<String>,
        reviewed_at_unix: u64,
    ) -> Result<ReviewRecord, ModerationError> {
        let _thread_guard = thread_lock(&self.transition_lock)?;
        let _process_guard = ProcessGuard::acquire(&self.storage)?;
        let reviewer = reviewer.into();
        let reason = reason.into();
        let public = self.storage.load_entry(package_id)?;
        let quarantined = self.storage.load_quarantined_entry(package_id)?;
        let recovering = public.is_none() && quarantined.is_some();
        let entry = match (public, quarantined) {
            (Some(public), _) => public,
            (None, Some(quarantined)) => quarantined,
            (None, None) => return Err(ModerationError::NoPublicEntry(package_id.into())),
        };
        let review = (if recovering {
            self.find_entry_review(ReviewDecision::Quarantined, &entry)?
        } else {
            None
        })
        .unwrap_or_else(|| ReviewRecord {
            review_id: entry_review_id(
                ReviewDecision::Quarantined,
                &entry,
                reviewed_at_unix,
                &reviewer,
                &reason,
            ),
            decision: ReviewDecision::Quarantined,
            reviewer,
            reason,
            reviewed_at_unix,
            subject: ReviewSubject::Entry {
                action: EntryAction::Update,
                entry: entry.clone(),
            },
        });
        // Persist the private copy and audit before removing public visibility.
        self.storage.save_quarantined_entry(&entry)?;
        self.persist_review_if_absent(&review)?;
        let _deleted = self.storage.delete_entry(package_id)?;
        Ok(review)
    }

    /// Return a quarantined entry to visibility after fresh signature and
    /// allowlist verification.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry is not quarantined, is no longer
    /// trusted, conflicts with public state, or cannot be persisted atomically.
    pub fn restore_quarantined(
        &self,
        package_id: &str,
        reviewer: impl Into<String>,
        reason: impl Into<String>,
        reviewed_at_unix: u64,
        allowlist: &PublisherAllowlist,
    ) -> Result<ReviewRecord, ModerationError> {
        let _thread_guard = thread_lock(&self.transition_lock)?;
        let _process_guard = ProcessGuard::acquire(&self.storage)?;
        let reviewer = reviewer.into();
        let reason = reason.into();
        let Some(entry) = self.storage.load_quarantined_entry(package_id)? else {
            if let Some(public) = self.storage.load_entry(package_id)?
                && let Some(review) = self.find_entry_review(ReviewDecision::Restored, &public)?
            {
                return Ok(review);
            }
            return Err(ModerationError::NotQuarantined(package_id.into()));
        };
        verify_restorable_entry(&entry, allowlist)?;
        let public = self.storage.load_entry(package_id)?;
        if let Some(public) = public.as_ref()
            && public != &entry
        {
            return Err(ModerationError::PublicEntryConflict(package_id.into()));
        }
        let review = (if public.is_some() {
            self.find_entry_review(ReviewDecision::Restored, &entry)?
        } else {
            None
        })
        .unwrap_or_else(|| ReviewRecord {
            review_id: entry_review_id(
                ReviewDecision::Restored,
                &entry,
                reviewed_at_unix,
                &reviewer,
                &reason,
            ),
            decision: ReviewDecision::Restored,
            reviewer,
            reason,
            reviewed_at_unix,
            subject: ReviewSubject::Entry {
                action: EntryAction::Update,
                entry: entry.clone(),
            },
        });
        self.storage.save_entry(&entry)?;
        self.persist_review_if_absent(&review)?;
        let _deleted = self.storage.delete_quarantined_entry(package_id)?;
        Ok(review)
    }

    fn require_pending(&self, submission_id: &str) -> Result<PendingSubmission, ModerationError> {
        self.storage
            .load_pending(submission_id)?
            .ok_or_else(|| ModerationError::NoPendingSubmission(submission_id.into()))
    }

    fn persist_review_if_absent(&self, review: &ReviewRecord) -> Result<(), ModerationError> {
        if self.storage.load_review(&review.review_id)?.is_none() {
            self.storage.save_review(review)?;
        }
        Ok(())
    }

    fn find_entry_review(
        &self,
        decision: ReviewDecision,
        entry: &RegistryEntry,
    ) -> Result<Option<ReviewRecord>, ModerationError> {
        Ok(self.storage.load_reviews()?.into_iter().find(|review| {
            review.decision == decision
                && matches!(&review.subject, ReviewSubject::Entry { entry: reviewed, .. } if reviewed == entry)
        }))
    }
}

fn validate_submission(
    change: &EntryChangeRequest,
    public: Option<&RegistryEntry>,
    pending: Option<&PendingSubmission>,
) -> Result<(), ModerationError> {
    match change.action {
        EntryAction::Add if public.is_some() => {
            return Err(ModerationError::EntryAlreadyExists(
                change.entry.package_id.clone(),
            ));
        }
        EntryAction::Update | EntryAction::Delete => {
            let Some(public) = public else {
                return Err(ModerationError::NoPublicEntry(
                    change.entry.package_id.clone(),
                ));
            };
            validate_owner(public, &change.entry)?;
            if change.action == EntryAction::Update
                && change.entry.updated_at_unix <= public.updated_at_unix
            {
                return Err(ModerationError::StaleTimestamp {
                    package_id: change.entry.package_id.clone(),
                    current: public.updated_at_unix,
                    proposed: change.entry.updated_at_unix,
                });
            }
        }
        EntryAction::Add => {}
    }
    if let Some(pending) = pending {
        if !same_publisher(&pending.change.entry, &change.entry) {
            return Err(ModerationError::PendingOwnedByAnotherPublisher(
                change.entry.package_id.clone(),
            ));
        }
        if change.entry.updated_at_unix <= pending.change.entry.updated_at_unix {
            return Err(ModerationError::PendingTimestampNotNewer {
                package_id: change.entry.package_id.clone(),
                current: pending.change.entry.updated_at_unix,
                proposed: change.entry.updated_at_unix,
            });
        }
    }
    Ok(())
}

fn validate_approval(
    change: &EntryChangeRequest,
    public: Option<&RegistryEntry>,
) -> Result<bool, ModerationError> {
    match change.action {
        EntryAction::Add => match public {
            None => Ok(false),
            Some(current) if current == &change.entry => Ok(true),
            Some(_) => Err(ModerationError::EntryAlreadyExists(
                change.entry.package_id.clone(),
            )),
        },
        EntryAction::Update => match public {
            Some(current) if current == &change.entry => Ok(true),
            Some(current) => {
                validate_owner(current, &change.entry)?;
                if change.entry.updated_at_unix <= current.updated_at_unix {
                    return Err(ModerationError::StaleTimestamp {
                        package_id: change.entry.package_id.clone(),
                        current: current.updated_at_unix,
                        proposed: change.entry.updated_at_unix,
                    });
                }
                Ok(false)
            }
            None => Err(ModerationError::NoPublicEntry(
                change.entry.package_id.clone(),
            )),
        },
        EntryAction::Delete => match public {
            Some(current) => {
                validate_owner(current, &change.entry)?;
                Ok(false)
            }
            None => Err(ModerationError::NoPublicEntry(
                change.entry.package_id.clone(),
            )),
        },
    }
}

fn validate_owner(
    current: &RegistryEntry,
    proposed: &RegistryEntry,
) -> Result<(), ModerationError> {
    if same_publisher(current, proposed) {
        Ok(())
    } else {
        Err(ModerationError::WrongPublisher(current.package_id.clone()))
    }
}

fn same_publisher(left: &RegistryEntry, right: &RegistryEntry) -> bool {
    let Ok(left) = hex::decode(&left.publisher_pubkey_hex) else {
        return false;
    };
    let Ok(right) = hex::decode(&right.publisher_pubkey_hex) else {
        return false;
    };
    left.len() == 32 && right.len() == 32 && left == right
}

fn verify_pending(
    pending: &PendingSubmission,
    allowlist: &PublisherAllowlist,
) -> Result<(), ModerationError> {
    verify_change_request(&pending.change, &allowlist.all_pubkey_bytes())
        .map_err(ModerationError::Verification)
}

fn verify_restorable_entry(
    entry: &RegistryEntry,
    allowlist: &PublisherAllowlist,
) -> Result<(), ModerationError> {
    if !verify_entry_signature(entry) {
        return Err(ModerationError::InvalidQuarantinedSignature(
            entry.package_id.clone(),
        ));
    }
    let Ok(bytes) = hex::decode(&entry.publisher_pubkey_hex) else {
        return Err(ModerationError::PublisherNoLongerAllowed(
            entry.package_id.clone(),
        ));
    };
    if bytes.len() != 32 {
        return Err(ModerationError::PublisherNoLongerAllowed(
            entry.package_id.clone(),
        ));
    }
    let mut key = [0; 32];
    key.copy_from_slice(&bytes);
    if allowlist.get(&key).is_none() {
        return Err(ModerationError::PublisherNoLongerAllowed(
            entry.package_id.clone(),
        ));
    }
    Ok(())
}

fn submission_review_id(pending: &PendingSubmission) -> String {
    format!(
        "submission-{}-{}",
        pending.submitted_at_unix,
        hex::encode(pending.change.signing_message()),
    )
}

fn entry_review_id(
    decision: ReviewDecision,
    entry: &RegistryEntry,
    reviewed_at_unix: u64,
    reviewer: &str,
    reason: &str,
) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(entry.package_id.as_bytes());
    hash.update(&entry.signing_message());
    hash.update(&reviewed_at_unix.to_le_bytes());
    hash.update(reviewer.as_bytes());
    hash.update(reason.as_bytes());
    let decision_label: &[u8] = match decision {
        ReviewDecision::Approved => b"approved",
        ReviewDecision::Rejected => b"rejected",
        ReviewDecision::Quarantined => b"quarantined",
        ReviewDecision::Restored => b"restored",
    };
    hash.update(decision_label);
    format!("entry-{}", hash.finalize().to_hex())
}

fn thread_lock(lock: &Mutex<()>) -> Result<std::sync::MutexGuard<'_, ()>, ModerationError> {
    lock.lock()
        .map_err(|_| ModerationError::TransitionLockPoisoned)
}

// Covers separately-constructed services inside one process. The per-service
// `Arc<Mutex<_>>` above covers normal AppState clones; this global guard closes
// the remaining same-process gap before the OS-level lock is acquired.
static PROCESS_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();

/// The in-process mutex cannot serialize the server against a reviewer CLI.
#[cfg(unix)]
struct ProcessGuard {
    _process_mutex: std::sync::MutexGuard<'static, ()>,
    file: File,
}

#[cfg(unix)]
impl ProcessGuard {
    fn acquire(storage: &Storage) -> Result<Self, ModerationError> {
        let process_mutex = PROCESS_MUTEX
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map_err(|_| ModerationError::TransitionLockPoisoned)?;
        let path = storage.moderation_lock_path();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| ModerationError::ProcessLock(format!("open {}: {e}", path.display())))?;
        // Blocking exclusive advisory lock. The lock file is deliberately
        // retained, so a crash releases only the lock—not evidence of state.
        let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if status != 0 {
            return Err(ModerationError::ProcessLock(format!(
                "lock {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
        Ok(Self {
            _process_mutex: process_mutex,
            file,
        })
    }
}

#[cfg(unix)]
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let _result = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(not(unix))]
struct ProcessGuard {
    _process_mutex: std::sync::MutexGuard<'static, ()>,
}

#[cfg(not(unix))]
impl ProcessGuard {
    fn acquire(_storage: &Storage) -> Result<Self, ModerationError> {
        let guard = PROCESS_MUTEX
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map_err(|_| ModerationError::TransitionLockPoisoned)?;
        Ok(Self {
            _process_mutex: guard,
        })
    }
}

/// Failures from moderation validation, persistence, or verification.
#[derive(Debug, Error)]
pub enum ModerationError {
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
    #[error("verification failed: {0}")]
    Verification(VerifyError),
    #[error("the moderation transition lock was poisoned")]
    TransitionLockPoisoned,
    #[error("moderation process lock: {0}")]
    ProcessLock(String),
    #[error("no pending submission for {0}")]
    NoPendingSubmission(String),
    #[error("pending submission for {0} changed after review; inspect it again")]
    PendingChanged(String),
    #[error("entry {0} already exists")]
    EntryAlreadyExists(String),
    #[error("no public entry for {0}")]
    NoPublicEntry(String),
    #[error("entry {0} is not quarantined")]
    NotQuarantined(String),
    #[error("entry {0} is owned by another publisher")]
    WrongPublisher(String),
    #[error("pending submission for {0} is owned by another publisher")]
    PendingOwnedByAnotherPublisher(String),
    #[error("entry {package_id} timestamp {proposed} is not newer than current {current}")]
    StaleTimestamp {
        package_id: String,
        current: u64,
        proposed: u64,
    },
    #[error("pending entry {package_id} timestamp {proposed} is not newer than current {current}")]
    PendingTimestampNotNewer {
        package_id: String,
        current: u64,
        proposed: u64,
    },
    #[error("public entry {0} conflicts with the quarantined revision")]
    PublicEntryConflict(String),
    #[error("quarantined entry {0} has an invalid publisher signature")]
    InvalidQuarantinedSignature(String),
    #[error("publisher for quarantined entry {0} is no longer allowlisted")]
    PublisherNoLongerAllowed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allowlist::PublisherRow;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    use registry_types::{RegistryVersion, sign_change_request, sign_entry};

    fn setup() -> (tempfile::TempDir, ModerationService) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        (dir, ModerationService::new(storage))
    }
    fn entry(key: &SigningKey, timestamp: u64) -> RegistryEntry {
        RegistryEntry {
            package_id: "dev.qos.editor".into(),
            name: "Editor".into(),
            description: String::new(),
            categories: vec![],
            icon_url: None,
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
            versions: vec![RegistryVersion {
                version: format!("1.{timestamp}"),
                qpkg_url: "https://example.test/e.qpkg".into(),
                qpkg_root_hash_hex: "ab".repeat(32),
                qpkg_size_bytes: 1,
                released_at_unix: timestamp,
            }],
            updated_at_unix: timestamp,
            signature_hex: String::new(),
        }
    }
    fn change(key: &SigningKey, action: EntryAction, timestamp: u64) -> EntryChangeRequest {
        sign_change_request(action, entry(key, timestamp), key)
    }
    fn allow(key: &SigningKey) -> PublisherAllowlist {
        PublisherAllowlist {
            publishers: vec![PublisherRow {
                pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
                can_add_new: true,
                note: String::new(),
            }],
        }
    }

    #[test]
    fn pending_replacement_requires_same_owner_and_newer_timestamp() {
        let (_dir, service) = setup();
        let first = SigningKey::generate(&mut OsRng);
        let other = SigningKey::generate(&mut OsRng);
        service
            .submit(change(&first, EntryAction::Add, 10), 100)
            .unwrap();
        service
            .submit(change(&first, EntryAction::Add, 11), 101)
            .unwrap();
        assert_eq!(
            service
                .pending("dev.qos.editor")
                .unwrap()
                .unwrap()
                .change
                .entry
                .updated_at_unix,
            11
        );
        assert!(matches!(
            service.submit(change(&other, EntryAction::Add, 12), 102),
            Err(ModerationError::PendingOwnedByAnotherPublisher(_))
        ));
    }

    #[test]
    fn retrying_the_same_submission_is_idempotent() {
        let (_dir, service) = setup();
        let key = SigningKey::generate(&mut OsRng);
        let request = change(&key, EntryAction::Add, 10);
        let first = service.submit(request.clone(), 100).unwrap();
        let retry = service.submit(request, 999).unwrap();

        assert_eq!(retry, first);
        assert_eq!(service.pending_submissions().unwrap(), vec![first]);
    }

    #[test]
    fn approval_rechecks_allowlist_and_is_replay_safe_after_public_write() {
        let (_dir, service) = setup();
        let key = SigningKey::generate(&mut OsRng);
        let pending = service
            .submit(change(&key, EntryAction::Add, 10), 100)
            .unwrap();
        assert!(matches!(
            service.approve(&pending, "m", "", 101, &PublisherAllowlist::empty()),
            Err(ModerationError::Verification(
                VerifyError::PublisherNotAllowed
            ))
        ));
        service.storage.save_entry(&pending.change.entry).unwrap(); // simulated crash after apply
        let review = service
            .approve(&pending, "m", "", 102, &allow(&key))
            .unwrap();
        assert_eq!(review.decision, ReviewDecision::Approved);
        assert!(service.pending("dev.qos.editor").unwrap().is_none());
        assert_eq!(
            service.storage.load_review(&review.review_id).unwrap(),
            Some(review)
        );
    }

    #[test]
    fn delete_is_queued_and_rejection_can_drain_a_revoked_request() {
        let (_dir, service) = setup();
        let key = SigningKey::generate(&mut OsRng);
        let mut public = entry(&key, 10);
        sign_entry(&mut public, &key);
        service.storage.save_entry(&public).unwrap();
        service
            .submit(sign_change_request(EntryAction::Delete, public, &key), 100)
            .unwrap();
        let review = service
            .reject("dev.qos.editor", "m", "revoked", 101)
            .unwrap();
        assert_eq!(review.decision, ReviewDecision::Rejected);
        assert!(
            service
                .storage
                .load_entry("dev.qos.editor")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn approved_delete_removes_the_public_entry() {
        let (_dir, service) = setup();
        let key = SigningKey::generate(&mut OsRng);
        let mut public = entry(&key, 10);
        sign_entry(&mut public, &key);
        service.storage.save_entry(&public).unwrap();
        let pending = service
            .submit(sign_change_request(EntryAction::Delete, public, &key), 100)
            .unwrap();
        let review = service
            .approve(&pending, "m", "withdrawn", 101, &allow(&key))
            .unwrap();
        assert_eq!(review.decision, ReviewDecision::Approved);
        assert!(
            service
                .storage
                .load_entry("dev.qos.editor")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn approval_refuses_a_submission_replaced_after_review() {
        let (_dir, service) = setup();
        let key = SigningKey::generate(&mut OsRng);
        let reviewed = service
            .submit(change(&key, EntryAction::Add, 10), 100)
            .unwrap();
        service
            .submit(change(&key, EntryAction::Add, 11), 101)
            .unwrap();

        assert!(matches!(
            service.approve(&reviewed, "m", "", 102, &allow(&key)),
            Err(ModerationError::PendingChanged(package)) if package == "dev.qos.editor"
        ));
        assert!(
            service
                .storage
                .load_entry("dev.qos.editor")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn restore_requires_current_allowlist() {
        let (_dir, service) = setup();
        let key = SigningKey::generate(&mut OsRng);
        let mut public = entry(&key, 10);
        sign_entry(&mut public, &key);
        service.storage.save_entry(&public).unwrap();
        service
            .quarantine("dev.qos.editor", "m", "hold", 100)
            .unwrap();
        assert!(matches!(
            service.restore_quarantined(
                "dev.qos.editor",
                "m",
                "",
                101,
                &PublisherAllowlist::empty()
            ),
            Err(ModerationError::PublisherNoLongerAllowed(_))
        ));
        service
            .restore_quarantined("dev.qos.editor", "m", "", 102, &allow(&key))
            .unwrap();
        assert!(
            service
                .storage
                .load_entry("dev.qos.editor")
                .unwrap()
                .is_some()
        );
    }
}
