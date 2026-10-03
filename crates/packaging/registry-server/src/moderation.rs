//! Durable pending-submission, review, and quarantine lifecycle.
//!
//! Approval and restoration re-verify publisher material at the point it can
//! become public. Rejection intentionally accepts malformed/revoked requests,
//! preserving their original bytes in an audit record while draining the queue.

use std::sync::{Arc, Mutex};

use registry_types::{
    EntryAction, EntryChangeRequest, RegistryEntry, VerifyError, verify_change_request,
    verify_entry_signature,
};
use thiserror::Error;

use crate::allowlist::PublisherAllowlist;
use crate::storage::{
    PendingSubmission, ReviewDecision, ReviewRecord, ReviewSubject, SnapshotLock, Storage,
    StorageError,
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
        let _transaction = self.storage.lock_snapshot()?;
        let _thread_guard = thread_lock(&self.transition_lock)?;
        let package_id = &change.entry.package_id;
        let public = self.storage.load_entry(package_id)?;
        let pending = self.storage.load_pending(package_id)?;
        let deferred = self.storage.load_deferred_pending(package_id)?;
        if let Some(existing) = pending.as_ref()
            && existing.change == change
        {
            return Ok(existing.clone());
        }
        validate_submission(
            &change,
            public.as_ref(),
            deferred.as_ref().or(pending.as_ref()),
            &self.artifact_hosts(),
        )?;
        let submission = PendingSubmission {
            // The package id is a single atomic pending slot: replacing a
            // revision is one rename rather than a delete/create window.
            submission_id: package_id.clone(),
            submitted_at_unix,
            change,
        };
        let reserved = self
            .storage
            .load_promotion_transaction()?
            .is_some_and(|journal| {
                journal
                    .submissions
                    .iter()
                    .any(|selected| selected.submission_id == submission.submission_id)
            });
        if reserved {
            self.storage.save_deferred_pending(&submission)?;
        } else {
            self.storage.save_pending(&submission)?;
        }
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
        let _transaction = self.storage.lock_snapshot()?;
        let _thread_guard = thread_lock(&self.transition_lock)?;
        self.approve_locked(
            reviewed_submission,
            reviewer.into(),
            reason.into(),
            reviewed_at_unix,
            allowlist,
        )
    }

    /// Approve an exact bounded batch under one moderation lock. Every pending
    /// slot is compared and validated before the first mutation. Missing slots
    /// are accepted only when a previous crash durably completed the exact
    /// review and public state, which makes promotion-journal replay safe.
    ///
    /// # Errors
    ///
    /// Returns an error if any selected submission changed, disappeared
    /// without a completed review, fails trust validation, conflicts with the
    /// public index, or cannot be persisted.
    pub fn approve_batch(
        &self,
        reviewed_submissions: &[PendingSubmission],
        reviewer: &str,
        reason: &str,
        reviewed_at_unix: u64,
        allowlist: &PublisherAllowlist,
    ) -> Result<Vec<ReviewRecord>, ModerationError> {
        let _transaction = self.storage.lock_snapshot()?;
        let _thread_guard = thread_lock(&self.transition_lock)?;
        self.approve_batch_locked(
            reviewed_submissions,
            reviewer,
            reason,
            reviewed_at_unix,
            allowlist,
            false,
        )
    }

    /// Approve a batch while the caller retains the registry mutation lock.
    /// This joins promotion selection, its durable journal, moderation, and
    /// snapshot publication into one cross-process transaction boundary.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::approve_batch`].
    pub fn approve_batch_in_transaction(
        &self,
        _transaction: &SnapshotLock<'_>,
        reviewed_submissions: &[PendingSubmission],
        reviewer: &str,
        reason: &str,
        reviewed_at_unix: u64,
        allowlist: &PublisherAllowlist,
    ) -> Result<Vec<ReviewRecord>, ModerationError> {
        let _thread_guard = thread_lock(&self.transition_lock)?;
        self.approve_batch_locked(
            reviewed_submissions,
            reviewer,
            reason,
            reviewed_at_unix,
            allowlist,
            true,
        )
    }

    fn approve_batch_locked(
        &self,
        reviewed_submissions: &[PendingSubmission],
        reviewer: &str,
        reason: &str,
        reviewed_at_unix: u64,
        allowlist: &PublisherAllowlist,
        recover_replaced_slots: bool,
    ) -> Result<Vec<ReviewRecord>, ModerationError> {
        let mut displaced = Vec::new();
        for reviewed in reviewed_submissions {
            match self.storage.load_pending(&reviewed.submission_id)? {
                Some(pending) if pending == *reviewed => {
                    verify_pending(&pending, allowlist)?;
                    let public = self.storage.load_entry(&pending.change.entry.package_id)?;
                    let _already_applied = validate_approval(
                        &pending.change,
                        public.as_ref(),
                        &self.artifact_hosts(),
                    )?;
                }
                Some(pending) => {
                    if self.completed_review(reviewed)?.is_some() {
                        continue;
                    }
                    if !recover_replaced_slots {
                        return Err(ModerationError::PendingChanged(
                            reviewed.submission_id.clone(),
                        ));
                    }
                    self.validate_deferred_successor(reviewed, &pending)?;
                    displaced.push((reviewed.clone(), pending));
                }
                None if self.completed_review(reviewed)?.is_some() => {}
                None => {
                    return Err(ModerationError::NoPendingSubmission(
                        reviewed.submission_id.clone(),
                    ));
                }
            }
        }
        for (reviewed, pending) in &displaced {
            self.storage.save_deferred_pending(pending)?;
            self.storage.save_pending(reviewed)?;
        }
        reviewed_submissions
            .iter()
            .map(|reviewed| {
                if let Some(review) = self.completed_review(reviewed)? {
                    return Ok(review);
                }
                self.approve_locked(
                    reviewed,
                    reviewer.to_string(),
                    reason.to_string(),
                    reviewed_at_unix,
                    allowlist,
                )
            })
            .collect()
    }

    /// Restore submissions that arrived while a durable promotion was active.
    /// Calling this repeatedly is safe, including after a crash between the
    /// pending write and deferred-file removal.
    ///
    /// # Errors
    ///
    /// Returns an error if deferred state cannot be read or moved atomically.
    pub fn activate_deferred_in_transaction(
        &self,
        _transaction: &SnapshotLock<'_>,
        reviewed_submissions: &[PendingSubmission],
    ) -> Result<(), ModerationError> {
        let _thread_guard = thread_lock(&self.transition_lock)?;
        self.activate_deferred_locked(reviewed_submissions)
    }

    fn activate_deferred_locked(
        &self,
        reviewed_submissions: &[PendingSubmission],
    ) -> Result<(), ModerationError> {
        for reviewed in reviewed_submissions {
            let Some(deferred) = self
                .storage
                .load_deferred_pending(&reviewed.submission_id)?
            else {
                continue;
            };
            match self.storage.load_pending(&reviewed.submission_id)? {
                Some(current) if current != deferred => {
                    return Err(ModerationError::PendingChanged(
                        reviewed.submission_id.clone(),
                    ));
                }
                None => self.storage.save_pending(&deferred)?,
                Some(_) => {}
            }
            let _deleted = self
                .storage
                .delete_deferred_pending(&reviewed.submission_id)?;
        }
        Ok(())
    }

    /// Restore deferred successors and consume the completed promotion's
    /// journal and staged manifest while the registry mutation lock is held.
    /// New submissions cannot observe a journal that cleanup will remove.
    /// Replaying this after a crash is safe because every transition and
    /// deletion is idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error if deferred state cannot be restored or durable
    /// promotion state cannot be removed.
    pub fn complete_promotion_in_transaction(
        &self,
        _transaction: &SnapshotLock<'_>,
        reviewed_submissions: &[PendingSubmission],
    ) -> Result<(), ModerationError> {
        let _thread_guard = thread_lock(&self.transition_lock)?;
        self.activate_deferred_locked(reviewed_submissions)?;
        let _deleted = self.storage.delete_staged_promotion()?;
        let _deleted = self.storage.delete_promotion_transaction()?;
        Ok(())
    }

    fn validate_deferred_successor(
        &self,
        reviewed: &PendingSubmission,
        successor: &PendingSubmission,
    ) -> Result<(), ModerationError> {
        if reviewed.submission_id != successor.submission_id {
            return Err(ModerationError::PendingChanged(
                reviewed.submission_id.clone(),
            ));
        }
        let public = self.storage.load_entry(&reviewed.submission_id)?;
        validate_submission(
            &successor.change,
            public.as_ref(),
            Some(reviewed),
            &self.artifact_hosts(),
        )
    }

    /// Where this registry accepts version artifacts to be hosted: the
    /// official artifact tree, plus this registry's own `/artifacts/` when it
    /// is a loopback development instance.
    fn artifact_hosts(&self) -> ArtifactHosts {
        ArtifactHosts {
            local_prefix: self
                .storage
                .load_identity()
                .ok()
                .and_then(|identity| local_artifact_prefix(&identity.base_url)),
        }
    }

    fn ensure_not_reserved_by_promotion(&self, package_id: &str) -> Result<(), ModerationError> {
        if self
            .storage
            .load_promotion_transaction()?
            .is_some_and(|journal| {
                journal
                    .submissions
                    .iter()
                    .any(|selected| selected.submission_id == package_id)
            })
        {
            return Err(ModerationError::PromotionInProgress(package_id.into()));
        }
        Ok(())
    }

    fn approve_locked(
        &self,
        reviewed_submission: &PendingSubmission,
        reviewer: String,
        reason: String,
        reviewed_at_unix: u64,
        allowlist: &PublisherAllowlist,
    ) -> Result<ReviewRecord, ModerationError> {
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
        let already_applied =
            validate_approval(&pending.change, public.as_ref(), &self.artifact_hosts())?;

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
            reviewer,
            reason,
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

    fn completed_review(
        &self,
        submission: &PendingSubmission,
    ) -> Result<Option<ReviewRecord>, ModerationError> {
        let Some(review) = self
            .storage
            .load_review(&submission_review_id(submission))?
        else {
            return Ok(None);
        };
        let applied = match submission.change.action {
            EntryAction::Add | EntryAction::Update => {
                self.storage
                    .load_entry(&submission.change.entry.package_id)?
                    .as_ref()
                    == Some(&submission.change.entry)
            }
            EntryAction::Delete => self
                .storage
                .load_entry(&submission.change.entry.package_id)?
                .is_none(),
        };
        if applied && review.decision == ReviewDecision::Approved {
            Ok(Some(review))
        } else {
            Ok(None)
        }
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
        let _transaction = self.storage.lock_snapshot()?;
        let _thread_guard = thread_lock(&self.transition_lock)?;
        self.ensure_not_reserved_by_promotion(submission_id)?;
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
        let _transaction = self.storage.lock_snapshot()?;
        let _thread_guard = thread_lock(&self.transition_lock)?;
        self.ensure_not_reserved_by_promotion(package_id)?;
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
        let _transaction = self.storage.lock_snapshot()?;
        let _thread_guard = thread_lock(&self.transition_lock)?;
        self.ensure_not_reserved_by_promotion(package_id)?;
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

/// Hosts from which a version artifact may be reviewed and served.
#[derive(Clone, Debug, Default)]
struct ArtifactHosts {
    local_prefix: Option<String>,
}

const OFFICIAL_ARTIFACT_PREFIX: &str = "https://apps.tilo.so/artifacts/";

impl ArtifactHosts {
    fn allows(&self, url: &str) -> bool {
        url.starts_with(OFFICIAL_ARTIFACT_PREFIX)
            || self
                .local_prefix
                .as_deref()
                .is_some_and(|prefix| url.starts_with(prefix))
    }
}

/// The URL a version names for the artifact `blake3_hex` this registry hosts:
/// its loopback `<base_url>/artifacts/` in local development, the official
/// artifact origin otherwise.
pub(crate) fn artifact_url(base_url: &str, blake3_hex: &str) -> String {
    let prefix =
        local_artifact_prefix(base_url).unwrap_or_else(|| OFFICIAL_ARTIFACT_PREFIX.to_owned());
    format!("{prefix}{}.tapp", blake3_hex.to_ascii_lowercase())
}

/// `<base_url>/artifacts/` when `base_url` is a loopback HTTP origin without
/// credentials, path, query or fragment; `None` for every public deployment.
fn local_artifact_prefix(base_url: &str) -> Option<String> {
    let base = base_url.strip_suffix('/').unwrap_or(base_url);
    let authority = base.strip_prefix("http://")?;
    let port = ["127.0.0.1:", "localhost:", "[::1]:"]
        .iter()
        .find_map(|host| authority.strip_prefix(host))?;
    (!port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| format!("{base}/artifacts/"))
}

fn validate_submission(
    change: &EntryChangeRequest,
    public: Option<&RegistryEntry>,
    pending: Option<&PendingSubmission>,
    artifacts: &ArtifactHosts,
) -> Result<(), ModerationError> {
    match change.action {
        EntryAction::Add if public.is_some() => {
            if public == Some(&change.entry) {
                return Ok(());
            }
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
    if matches!(change.action, EntryAction::Add | EntryAction::Update) {
        validate_version_history(&change.entry, public, artifacts)?;
    }
    if let Some(pending) = pending {
        if pending.change == *change {
            return Ok(());
        }
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
        if matches!(change.action, EntryAction::Add | EntryAction::Update) {
            validate_version_history(&change.entry, Some(&pending.change.entry), artifacts)?;
        }
    }
    Ok(())
}

fn validate_approval(
    change: &EntryChangeRequest,
    public: Option<&RegistryEntry>,
    artifacts: &ArtifactHosts,
) -> Result<bool, ModerationError> {
    if change.action == EntryAction::Add {
        validate_version_history(&change.entry, None, artifacts)?;
    }
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
                validate_version_history(&change.entry, Some(current), artifacts)?;
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

fn validate_version_history(
    proposed: &RegistryEntry,
    baseline: Option<&RegistryEntry>,
    artifacts: &ArtifactHosts,
) -> Result<(), ModerationError> {
    for (index, version) in proposed.versions.iter().enumerate() {
        if proposed
            .versions
            .get(..index)
            .unwrap_or_default()
            .iter()
            .any(|other| same_version_identity(other, version))
        {
            return Err(ModerationError::DuplicateVersionIdentity {
                package_id: proposed.package_id.clone(),
                version: version.version.clone(),
            });
        }
        validate_bound_version(proposed, version, artifacts)?;
    }
    if let Some(current) = baseline {
        for old in &current.versions {
            let Some(new) = proposed
                .versions
                .iter()
                .find(|version| same_version_identity(version, old))
            else {
                // Only an entry at its version limit drops versions, the
                // oldest first, and only one superseded by a newer version
                // for its target, so no target loses its current release.
                let at_limit = proposed.versions.len() == registry_types::entry::MAX_ENTRY_VERSIONS;
                let superseded = at_limit
                    && proposed.versions.iter().any(|version| {
                        same_version_target(version, old)
                            && tilo_app_package::version::compare(&version.version, &old.version)
                                .is_gt()
                    });
                if superseded {
                    continue;
                }
                return Err(ModerationError::VersionHistoryRemoved {
                    package_id: proposed.package_id.clone(),
                    version: old.version.clone(),
                });
            };
            if new != old {
                return Err(ModerationError::VersionIdentityReused {
                    package_id: proposed.package_id.clone(),
                    version: old.version.clone(),
                });
            }
        }
        for version in &proposed.versions {
            if current
                .versions
                .iter()
                .any(|old| same_version_identity(old, version))
            {
                continue;
            }
            let latest = current
                .versions
                .iter()
                .filter(|old| same_version_target(old, version))
                .max_by(|left, right| {
                    tilo_app_package::version::compare(&left.version, &right.version)
                });
            if let Some(latest) = latest
                && !tilo_app_package::version::compare(&version.version, &latest.version).is_gt()
            {
                return Err(ModerationError::VersionNotNewer {
                    package_id: proposed.package_id.clone(),
                    version: version.version.clone(),
                    current: latest.version.clone(),
                });
            }
        }
    }
    Ok(())
}

fn same_version_identity(
    left: &registry_types::RegistryVersion,
    right: &registry_types::RegistryVersion,
) -> bool {
    tilo_app_package::version::compare(&left.version, &right.version).is_eq()
        && same_version_target(left, right)
}

fn same_version_target(
    left: &registry_types::RegistryVersion,
    right: &registry_types::RegistryVersion,
) -> bool {
    left.target.platform == right.target.platform
        && left.target.architecture == right.target.architecture
}

fn validate_bound_version(
    entry: &RegistryEntry,
    version: &registry_types::RegistryVersion,
    artifacts: &ArtifactHosts,
) -> Result<(), ModerationError> {
    let hash_ok = version.tapp_blake3_hex.len() == 64
        && version
            .tapp_blake3_hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit());
    let target = &version.target;
    let target_ok = match target.platform.as_str() {
        registry_types::TILO_OS_PLATFORM => matches!(
            (target.architecture.as_str(), target.isa.as_str()),
            ("x86_64", "x86_64" | "x86_64-v2" | "x86_64-v3" | "x86_64-v4")
                | (
                    "aarch64",
                    "aarch64" | "aarch64-v8.2" | "aarch64-v8.4" | "aarch64-v9"
                )
        ),
        registry_types::WEB_PLATFORM
        | registry_types::LINUX_PLATFORM
        | registry_types::WINDOWS_PLATFORM
        | registry_types::MACOS_PLATFORM
        | registry_types::ANDROID_PLATFORM => target.validate().is_ok(),
        _ => false,
    };
    let canonical_url = artifacts.allows(&version.tapp_url);
    if !hash_ok || !target_ok || !canonical_url || version.tapp_size_bytes == 0 {
        return Err(ModerationError::UnsupportedPackageTarget {
            package_id: entry.package_id.clone(),
            version: version.version.clone(),
        });
    }
    Ok(())
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

/// Failures from moderation validation, persistence, or verification.
#[derive(Debug, Error)]
pub enum ModerationError {
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
    #[error("verification failed: {0}")]
    Verification(VerifyError),
    #[error("the moderation transition lock was poisoned")]
    TransitionLockPoisoned,
    #[error("no pending submission for {0}")]
    NoPendingSubmission(String),
    #[error("pending submission for {0} changed after review; inspect it again")]
    PendingChanged(String),
    #[error("package {0} is reserved by an active first-party promotion")]
    PromotionInProgress(String),
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
    #[error("entry {package_id} repeats version identity {version}")]
    DuplicateVersionIdentity { package_id: String, version: String },
    #[error("entry {package_id} removes immutable version {version}")]
    VersionHistoryRemoved { package_id: String, version: String },
    #[error("entry {package_id} reuses version identity {version} for different bytes")]
    VersionIdentityReused { package_id: String, version: String },
    #[error(
        "entry {package_id} version {version} is not newer than current {current} for its target"
    )]
    VersionNotNewer {
        package_id: String,
        version: String,
        current: String,
    },
    #[error("entry {package_id} version {version} lacks a supported architecture artifact binding")]
    UnsupportedPackageTarget { package_id: String, version: String },
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
    use crate::storage::{PromotionJournal, StagedPromotion};
    use ed25519_dalek::SigningKey;
    use getrandom::{SysRng, rand_core::UnwrapErr};
    use registry_types::{RegistryVersion, sign_change_request, sign_entry};

    fn setup() -> (tempfile::TempDir, ModerationService) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        (dir, ModerationService::new(storage))
    }
    fn entry(key: &SigningKey, timestamp: u64) -> RegistryEntry {
        RegistryEntry {
            package_id: "so.tilo.editor".into(),
            name: "Editor".into(),
            description: "Native text editor".into(),
            tagline: "Edit together".into(),
            images: registry_types::LocalizedImages::english_icon(
                "https://apps.tilo.so/images/test-icon.svg",
                "Test icon",
            ),
            supported_languages: vec!["en".into()],
            categories: vec![],
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
            versions: vec![RegistryVersion {
                version: format!("1.{timestamp}"),
                tapp_url: "https://apps.tilo.so/artifacts/editor/e.tapp".into(),
                tapp_root_hash_hex: "ab".repeat(32),
                tapp_size_bytes: 1,
                released_at_unix: timestamp,
                tapp_blake3_hex: "cd".repeat(32),
                target: registry_types::RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                },
            }],
            updated_at_unix: timestamp,
            signature_hex: String::new(),
        }
    }
    fn change(key: &SigningKey, action: EntryAction, timestamp: u64) -> EntryChangeRequest {
        sign_change_request(action, entry(key, timestamp), key)
    }
    fn append_change(
        key: &SigningKey,
        base: &EntryChangeRequest,
        timestamp: u64,
    ) -> EntryChangeRequest {
        let mut next = base.entry.clone();
        next.versions.extend(entry(key, timestamp).versions);
        next.updated_at_unix = timestamp;
        next.signature_hex.clear();
        sign_change_request(base.action, next, key)
    }
    fn change_for(key: &SigningKey, package_id: &str, timestamp: u64) -> EntryChangeRequest {
        let mut value = entry(key, timestamp);
        value.package_id = package_id.to_string();
        sign_change_request(EntryAction::Add, value, key)
    }
    fn allow(key: &SigningKey) -> PublisherAllowlist {
        PublisherAllowlist {
            publishers: vec![PublisherRow {
                pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
                can_add_new: true,
                note: String::new(),
                namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
            }],
        }
    }

    #[test]
    fn an_entry_at_its_version_limit_drops_only_superseded_versions() {
        let key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let official = ArtifactHosts::default();
        let limit = registry_types::entry::MAX_ENTRY_VERSIONS;
        let version = |n: usize, architecture: &str| {
            let mut v = entry(&key, 1).versions[0].clone();
            v.version = format!("1.{n}");
            v.target.architecture = architecture.into();
            if architecture == "aarch64" {
                v.target.isa = "aarch64".into();
            }
            v
        };
        // A full entry: one old aarch64 release and x86_64 releases 1..limit.
        let mut current = entry(&key, 1);
        current.versions = std::iter::once(version(0, "aarch64"))
            .chain((1..limit).map(|n| version(n, "x86_64")))
            .collect();

        // A new x86_64 release drops the oldest superseded x86_64 one.
        let mut next = current.clone();
        next.versions.retain(|v| v.version != "1.1");
        next.versions.push(version(limit, "x86_64"));
        assert_eq!(next.versions.len(), limit);
        assert!(validate_version_history(&next, Some(&current), &official).is_ok());

        // Dropping a target's only release is still removing history.
        let mut lost = current.clone();
        lost.versions.retain(|v| v.target.architecture != "aarch64");
        lost.versions.push(version(limit, "x86_64"));
        assert!(matches!(
            validate_version_history(&lost, Some(&current), &official),
            Err(ModerationError::VersionHistoryRemoved { .. })
        ));

        // Below the limit nothing may be dropped.
        let mut short = current.clone();
        short
            .versions
            .retain(|v| v.version != "1.1" && v.version != "1.2");
        short.versions.push(version(limit, "x86_64"));
        assert!(matches!(
            validate_version_history(&short, Some(&current), &official),
            Err(ModerationError::VersionHistoryRemoved { .. })
        ));
    }

    #[test]
    fn submissions_keep_append_only_per_target_history() {
        // phase: version_history_is_monotonic_per_target
        {
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let mut current = entry(&key, 42);
            current.versions[0].version = "0.1.42+gnew".into();

            let mut stale = current.clone();
            let mut stale_x86 = stale.versions[0].clone();
            stale_x86.version = "0.1.41+gold".into();
            stale.versions.push(stale_x86);
            let official = ArtifactHosts::default();
            assert!(matches!(
                validate_version_history(&stale, Some(&current), &official),
                Err(ModerationError::VersionNotNewer { version, current, .. })
                    if version == "0.1.41+gold" && current == "0.1.42+gnew"
            ));

            let mut reused = current.clone();
            reused.versions[0].version = "0.1.42+gdifferent".into();
            assert!(matches!(
                validate_version_history(&reused, Some(&current), &official),
                Err(ModerationError::VersionIdentityReused { .. })
            ));

            let mut new_target = current.clone();
            let mut arm = new_target.versions[0].clone();
            arm.version = "0.1.1+garm".into();
            arm.target.architecture = "aarch64".into();
            arm.target.isa = "aarch64-v8.2".into();
            new_target.versions.push(arm);
            assert!(validate_version_history(&new_target, Some(&current), &official).is_ok());
        }
        // phase: pending_replacement_requires_append_only_history_and_same_owner
        {
            let (_dir, service) = setup();
            let first = SigningKey::generate(&mut UnwrapErr(SysRng));
            let other = SigningKey::generate(&mut UnwrapErr(SysRng));
            let first_pending = service
                .submit(change(&first, EntryAction::Add, 10), 100)
                .unwrap();
            assert!(matches!(
                service.submit(change(&first, EntryAction::Add, 11), 101),
                Err(ModerationError::VersionHistoryRemoved { .. })
            ));
            service
                .submit(append_change(&first, &first_pending.change, 11), 101)
                .unwrap();
            assert_eq!(
                service
                    .pending("so.tilo.editor")
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
        // phase: both_architectures_are_preserved_when_a_stale_submission_arrives
        {
            let (_dir, service) = setup();
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let mut aggregate = entry(&key, 10);
            let mut arm = aggregate.versions[0].clone();
            let target = &mut arm.target;
            target.architecture = "aarch64".into();
            target.isa = "aarch64-v8.2".into();
            aggregate.versions.push(arm);
            sign_entry(&mut aggregate, &key);
            let request = sign_change_request(EntryAction::Add, aggregate, &key);
            let pending = service.submit(request, 100).unwrap();
            assert_eq!(pending.change.entry.versions.len(), 2);
            assert!(matches!(
                service.submit(change(&key, EntryAction::Add, 11), 101),
                Err(ModerationError::VersionHistoryRemoved { .. })
            ));
            assert_eq!(service.pending("so.tilo.editor").unwrap(), Some(pending));
        }
        // phase: retrying_the_same_submission_is_idempotent
        {
            let (_dir, service) = setup();
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let request = change(&key, EntryAction::Add, 10);
            let first = service.submit(request.clone(), 100).unwrap();
            let retry = service.submit(request, 999).unwrap();

            assert_eq!(retry, first);
            assert_eq!(service.pending_submissions().unwrap(), vec![first]);
        }
    }

    #[test]
    fn web_versions_are_a_separate_target_history() {
        let key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let current = entry(&key, 42);
        let official = ArtifactHosts::default();
        let mut with_web = current.clone();
        let mut web = with_web.versions[0].clone();
        web.version = "0.0.1".into();
        web.target = registry_types::RegistryTarget {
            platform: registry_types::WEB_PLATFORM.into(),
            architecture: registry_types::WASM32_ARCHITECTURE.into(),
            isa: "wasm32-mvp+bulk-memory+sign-ext".into(),
            runtime: Some("tilo-runtime/2026.07+wasm.00112233445566778899aabbccddeeff".into()),
        };
        with_web.versions.push(web);
        assert!(validate_version_history(&with_web, Some(&current), &official).is_ok());

        let mut unordered = with_web.clone();
        unordered.versions[1].target.isa = "wasm32-mvp+sign-ext+bulk-memory".into();
        assert!(matches!(
            validate_version_history(&unordered, Some(&current), &official),
            Err(ModerationError::UnsupportedPackageTarget { .. })
        ));
        let mut no_abi = with_web;
        no_abi.versions[1].target.runtime = Some("tilo-runtime/2026.07".into());
        assert!(matches!(
            validate_version_history(&no_abi, Some(&current), &official),
            Err(ModerationError::UnsupportedPackageTarget { .. })
        ));
    }

    #[test]
    fn android_versions_are_a_separate_target_history() {
        let key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let current = entry(&key, 42);
        let official = ArtifactHosts::default();
        let mut with_android = current.clone();
        for (architecture, isa) in [("aarch64", "aarch64"), ("x86_64", "x86_64")] {
            let mut android = with_android.versions[0].clone();
            android.version = "0.0.1".into();
            android.target = registry_types::RegistryTarget {
                platform: registry_types::ANDROID_PLATFORM.into(),
                architecture: architecture.into(),
                isa: isa.into(),
                runtime: Some("tilo-runtime/2026.07".into()),
            };
            with_android.versions.push(android);
        }
        assert!(validate_version_history(&with_android, Some(&current), &official).is_ok());

        for (architecture, isa) in [("armv7", "armv7"), ("aarch64", "")] {
            let mut bad = with_android.clone();
            bad.versions[1].target.architecture = architecture.into();
            bad.versions[1].target.isa = isa.into();
            assert!(matches!(
                validate_version_history(&bad, Some(&current), &official),
                Err(ModerationError::UnsupportedPackageTarget { .. })
            ));
        }
    }

    #[test]
    fn only_a_loopback_registry_hosts_its_own_artifacts() {
        assert_eq!(
            local_artifact_prefix("http://127.0.0.1:8787/").as_deref(),
            Some("http://127.0.0.1:8787/artifacts/")
        );
        assert_eq!(
            local_artifact_prefix("http://localhost:8787").as_deref(),
            Some("http://localhost:8787/artifacts/")
        );
        for public in [
            "https://apps.tilo.so",
            "https://apps.acme.example",
            "http://127.0.0.1.evil.example:80",
            "http://127.0.0.1:80@evil.example",
            "http://localhost:8787/nested",
            "http://127.0.0.1",
        ] {
            assert_eq!(local_artifact_prefix(public), None, "{public}");
        }
        let hosts = ArtifactHosts {
            local_prefix: local_artifact_prefix("http://127.0.0.1:8787"),
        };
        assert!(hosts.allows("http://127.0.0.1:8787/artifacts/ab.tapp"));
        assert!(hosts.allows("https://apps.tilo.so/artifacts/ab.tapp"));
        assert!(!hosts.allows("http://127.0.0.1:8788/artifacts/ab.tapp"));
        assert!(!ArtifactHosts::default().allows("http://127.0.0.1:8787/artifacts/ab.tapp"));
    }

    #[test]
    fn approval_rechecks_the_reviewed_state_and_is_replay_safe() {
        // phase: approval_rechecks_allowlist_and_is_replay_safe_after_public_write
        {
            let (_dir, service) = setup();
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
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
            assert!(service.pending("so.tilo.editor").unwrap().is_none());
            assert_eq!(
                service.storage.load_review(&review.review_id).unwrap(),
                Some(review)
            );
        }
        // phase: approval_refuses_a_submission_replaced_after_review
        {
            let (_dir, service) = setup();
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let reviewed = service
                .submit(change(&key, EntryAction::Add, 10), 100)
                .unwrap();
            service
                .submit(append_change(&key, &reviewed.change, 11), 101)
                .unwrap();

            assert!(matches!(
                service.approve(&reviewed, "m", "", 102, &allow(&key)),
                Err(ModerationError::PendingChanged(package)) if package == "so.tilo.editor"
            ));
            assert!(
                service
                    .storage
                    .load_entry("so.tilo.editor")
                    .unwrap()
                    .is_none()
            );
        }
        // phase: batch_preflight_prevents_partial_approval_when_any_slot_changed
        {
            let (_dir, service) = setup();
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let first = service
                .submit(change_for(&key, "so.tilo.a", 10), 100)
                .unwrap();
            let second = service
                .submit(change_for(&key, "so.tilo.b", 10), 100)
                .unwrap();
            service
                .submit(append_change(&key, &second.change, 11), 101)
                .unwrap();

            assert!(matches!(
                service.approve_batch(&[first.clone(), second], "auto", "", 102, &allow(&key)),
                Err(ModerationError::PendingChanged(package)) if package == "so.tilo.b"
            ));
            assert!(service.storage.load_entry("so.tilo.a").unwrap().is_none());
            assert_eq!(service.pending("so.tilo.a").unwrap(), Some(first));
        }
        // phase: batch_retry_recovers_an_approval_completed_before_a_crash
        {
            let (_dir, service) = setup();
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let first = service
                .submit(change_for(&key, "so.tilo.a", 10), 100)
                .unwrap();
            let second = service
                .submit(change_for(&key, "so.tilo.b", 10), 100)
                .unwrap();
            service
                .approve(&first, "auto", "", 101, &allow(&key))
                .unwrap();

            let reviews = service
                .approve_batch(&[first, second], "auto", "", 102, &allow(&key))
                .unwrap();
            assert_eq!(reviews.len(), 2);
            assert!(service.pending_submissions().unwrap().is_empty());
            assert!(service.storage.load_entry("so.tilo.a").unwrap().is_some());
            assert!(service.storage.load_entry("so.tilo.b").unwrap().is_some());
        }
    }

    #[test]
    fn delete_and_restore_requests_are_moderated() {
        // phase: delete_is_queued_and_rejection_can_drain_a_revoked_request
        {
            let (_dir, service) = setup();
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let mut public = entry(&key, 10);
            sign_entry(&mut public, &key);
            service.storage.save_entry(&public).unwrap();
            service
                .submit(sign_change_request(EntryAction::Delete, public, &key), 100)
                .unwrap();
            let review = service
                .reject("so.tilo.editor", "m", "revoked", 101)
                .unwrap();
            assert_eq!(review.decision, ReviewDecision::Rejected);
            assert!(
                service
                    .storage
                    .load_entry("so.tilo.editor")
                    .unwrap()
                    .is_some()
            );
        }
        // phase: approved_delete_removes_the_public_entry
        {
            let (_dir, service) = setup();
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
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
                    .load_entry("so.tilo.editor")
                    .unwrap()
                    .is_none()
            );
        }
        // phase: restore_requires_current_allowlist
        {
            let (_dir, service) = setup();
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let mut public = entry(&key, 10);
            sign_entry(&mut public, &key);
            service.storage.save_entry(&public).unwrap();
            service
                .quarantine("so.tilo.editor", "m", "hold", 100)
                .unwrap();
            assert!(matches!(
                service.restore_quarantined(
                    "so.tilo.editor",
                    "m",
                    "",
                    101,
                    &PublisherAllowlist::empty()
                ),
                Err(ModerationError::PublisherNoLongerAllowed(_))
            ));
            service
                .restore_quarantined("so.tilo.editor", "m", "", 102, &allow(&key))
                .unwrap();
            assert!(
                service
                    .storage
                    .load_entry("so.tilo.editor")
                    .unwrap()
                    .is_some()
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn promotion_recovery_keeps_newer_submissions_reachable() {
        // phase: promotion_recovery_preserves_a_newer_submission_for_the_same_slot
        {
            let (_dir, service) = setup();
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let selected = service
                .submit(change(&key, EntryAction::Add, 10), 100)
                .unwrap();
            let candidate = registry_types::SignedRegistrySnapshot {
                protocol: registry_types::SNAPSHOT_PROTOCOL,
                sequence: 1,
                issued_at_unix: 101,
                expires_at_unix: 201,
                previous_snapshot_hash_hex: String::new(),
                moderation_head_hex: String::new(),
                publishers: Vec::new(),
                entries: vec![selected.change.entry.clone()],
                owner_signature_hex: "test fixture".into(),
                channel_authorization: None,
            };
            service
                .storage
                .save_promotion_transaction(&PromotionJournal {
                    publisher_pubkey_hex: selected.change.entry.publisher_pubkey_hex.clone(),
                    reviewer: "auto".into(),
                    reason: "first party".into(),
                    submissions: vec![selected.clone()],
                    candidate_snapshot: candidate,
                })
                .unwrap();

            // A submit arriving after a crash sees the durable journal. It must
            // not replace the selected slot and make the journal unreplayable.
            let successor = service
                .submit(append_change(&key, &selected.change, 11), 102)
                .unwrap();
            assert_eq!(
                service.pending(&selected.submission_id).unwrap(),
                Some(selected.clone())
            );
            assert_eq!(
                service
                    .storage
                    .load_deferred_pending(&selected.submission_id)
                    .unwrap(),
                Some(successor.clone())
            );

            let transaction = service.storage.lock_snapshot().unwrap();
            let reviews = service
                .approve_batch_in_transaction(
                    &transaction,
                    std::slice::from_ref(&selected),
                    "auto",
                    "first party",
                    103,
                    &allow(&key),
                )
                .unwrap();
            service
                .complete_promotion_in_transaction(&transaction, std::slice::from_ref(&selected))
                .unwrap();
            drop(transaction);

            assert_eq!(reviews.len(), 1);
            assert_eq!(
                service.storage.load_entry(&selected.submission_id).unwrap(),
                Some(selected.change.entry)
            );
            assert_eq!(
                service.pending(&successor.submission_id).unwrap(),
                Some(successor)
            );
            assert!(
                service
                    .storage
                    .load_deferred_pending(&selected.submission_id)
                    .unwrap()
                    .is_none()
            );
            assert!(
                service
                    .storage
                    .load_promotion_transaction()
                    .unwrap()
                    .is_none()
            );
        }
        // phase: promotion_cleanup_keeps_a_concurrent_successor_reachable
        {
            let (_dir, service) = setup();
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let selected = service
                .submit(change(&key, EntryAction::Add, 10), 100)
                .unwrap();
            let candidate = registry_types::SignedRegistrySnapshot {
                protocol: registry_types::SNAPSHOT_PROTOCOL,
                sequence: 1,
                issued_at_unix: 101,
                expires_at_unix: 201,
                previous_snapshot_hash_hex: String::new(),
                moderation_head_hex: String::new(),
                publishers: Vec::new(),
                entries: vec![selected.change.entry.clone()],
                owner_signature_hex: "test fixture".into(),
                channel_authorization: None,
            };
            service
                .storage
                .save_promotion_transaction(&PromotionJournal {
                    publisher_pubkey_hex: selected.change.entry.publisher_pubkey_hex.clone(),
                    reviewer: "auto".into(),
                    reason: "first party".into(),
                    submissions: vec![selected.clone()],
                    candidate_snapshot: candidate,
                })
                .unwrap();
            service
                .storage
                .save_staged_promotion(&StagedPromotion {
                    publisher_pubkey_hex: selected.change.entry.publisher_pubkey_hex.clone(),
                    collected_at_unix: 100,
                    submissions: vec![selected.clone()],
                    artifacts: Vec::new(),
                })
                .unwrap();

            let transaction = service.storage.lock_snapshot().unwrap();
            service
                .approve_batch_in_transaction(
                    &transaction,
                    std::slice::from_ref(&selected),
                    "auto",
                    "first party",
                    102,
                    &allow(&key),
                )
                .unwrap();
            let mut successor_entry = selected.change.entry.clone();
            successor_entry.versions.extend(entry(&key, 11).versions);
            successor_entry.updated_at_unix = 11;
            successor_entry.signature_hex.clear();
            let successor_change = sign_change_request(EntryAction::Update, successor_entry, &key);
            let submitter_service = service.clone();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let submitter = std::thread::spawn(move || {
                ready_tx.send(()).unwrap();
                submitter_service.submit(successor_change, 103).unwrap()
            });
            ready_rx.recv().unwrap();

            service
                .complete_promotion_in_transaction(&transaction, std::slice::from_ref(&selected))
                .unwrap();
            assert!(
                service
                    .storage
                    .load_promotion_transaction()
                    .unwrap()
                    .is_none()
            );
            assert!(service.storage.load_staged_promotion().unwrap().is_none());
            drop(transaction);

            let successor = submitter.join().unwrap();
            assert_eq!(
                service.pending(&successor.submission_id).unwrap(),
                Some(successor)
            );
        }
    }
}
