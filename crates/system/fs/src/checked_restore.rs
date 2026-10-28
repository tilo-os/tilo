//! Checked history service owned by the actual mounted filesystem.
use super::{
    Access, EBUSY, EINVAL, EIO, ENOENT, EROFS, Hash, Namespace, QuickFS, RecordKind,
    VersionMetadataDigest, now_ms,
};
// The test module reaches the whole filesystem scope through this module.
#[cfg(test)]
use super::*;
use crate::content_kind::RecordContentKind;
use crate::record_repository::{RecordRepositoryKey, RepositoryLease};
#[cfg(test)]
use crate::record_transaction::RecordTransactionJournalEntry;
use crate::record_transaction::RecordTransactionPhase as Phase;
use fs_protocol::files::{
    CheckedHistoryOperationV1 as Operation, CheckedHistoryOutcomeV1 as Outcome,
    CheckedHistoryRequestV1 as CheckedRequest, FileRestoreReceiptV1 as Receipt,
    FileRestoreReviewV1 as Review,
};
use fs_protocol::{
    ContentHash, ContentRevisionV1, FileMutationOperation, FileMutationParticipant,
    FileMutationParticipantRole, FileMutationTransactionManifest, FileProvenance, FileVersionV1,
};

type Request = CheckedRequest;

/// Why a serialized document version could not be restored.
#[derive(Debug)]
pub(super) enum DocumentRestoreError {
    /// The current version's bytes could not be read.
    Unavailable,
    /// One side is not a document of the record's schema.
    NotADocument(String),
}

impl DocumentRestoreError {
    fn outcome(self) -> Outcome {
        match self {
            Self::Unavailable => Outcome::Unavailable,
            Self::NotADocument(reason) => Outcome::UnsupportedOwner {
                mapping: format!(
                    "serialized document: {}",
                    reason.chars().take(96).collect::<String>()
                ),
            },
        }
    }
}

#[expect(
    clippy::result_large_err,
    reason = "the error is the checked-history wire answer, returned by value to the caller unchanged"
)]
fn access(
    acl: Option<&crate::acl::WorkspaceAcl>,
    identity: Option<&crate::acl::IdentityKey>,
    path: &str,
    mode: Access,
) -> Result<(), Outcome> {
    let (Some(acl), Some(identity)) = (acl, identity) else {
        return Ok(());
    };
    if acl.permits(identity, path, mode) {
        return Ok(());
    }
    if mode == Access::Write && acl.visible(identity, path) {
        Err(Outcome::ReadOnly)
    } else {
        Err(Outcome::NotVisible)
    }
}

#[cfg(test)]
fn receipt(entry: &RecordTransactionJournalEntry) -> Option<Receipt> {
    let FileMutationOperation::RestoreContent {
        request,
        selected_version,
        prior_revision,
        compensates,
    } = &entry.transaction.manifest.operation
    else {
        return None;
    };
    Some(Receipt {
        schema_version: 1,
        intent_id: request.intent_id,
        workspace_id: request.review.workspace_id,
        mount_lifetime: request.review.mount_lifetime,
        record_id: request.review.record_id,
        selected_version: *selected_version,
        prior_revision: *prior_revision,
        restored_revision: entry
            .transaction
            .manifest
            .participants
            .first()?
            .proposed_content_revision?,
        compensates: *compensates,
    })
}

#[expect(
    clippy::result_large_err,
    reason = "every checked-history early return is the final wire answer for the caller; boxing it \
              would only add an allocation on the answer path and change the collab-types protocol type"
)]
impl QuickFS {
    #[must_use]
    pub fn with_checked_history_workspace(mut self, workspace: uuid::Uuid) -> Self {
        self.checked_history_workspace = Some(workspace);
        self
    }

    pub(super) fn restore_workspace(&self) -> Result<uuid::Uuid, Outcome> {
        let workspace = self
            .record_publisher
            .as_ref()
            .map(|value| value.workspace_id())
            .or(self.checked_history_workspace)
            .ok_or(Outcome::Unavailable)?;
        if workspace.is_nil()
            || self
                .checked_history_workspace
                .is_some_and(|value| value != workspace)
        {
            return Err(Outcome::Unavailable);
        }
        Ok(workspace)
    }

    /// How `ino`'s content is restored, from record metadata alone. A
    /// namespace-held document has no checked restore route yet: restoring it
    /// needs its CRDT heads recorded with each version.
    pub(super) fn restore_kind(
        namespace: &Namespace,
        ino: u64,
    ) -> Result<RecordContentKind, Outcome> {
        match RecordContentKind::of(namespace, ino).ok_or(Outcome::NotVisible)? {
            RecordContentKind::NamespaceDocument { codec } => Err(Outcome::UnsupportedOwner {
                mapping: format!(
                    "document codec {}: no checked content-owner restore route",
                    codec.chars().take(64).collect::<String>()
                ),
            }),
            kind => Ok(kind),
        }
    }

    /// The bytes that restore a serialized document to `selected`, whose
    /// bytes are `data`: the current document plus one change that reaches
    /// the selected state, so its history continues instead of forking. With
    /// no document on one side there is nothing to merge, and the selected
    /// bytes are the result. The current document's bytes must already be
    /// local; this never fetches them.
    pub(super) fn restored_document(
        &self,
        current_hash: Option<Hash>,
        selected: Option<ContentHash>,
        data: &[u8],
    ) -> Result<Vec<u8>, DocumentRestoreError> {
        let Some(current_hash) = current_hash else {
            return Ok(data.to_vec());
        };
        if data.is_empty() || Some(ContentHash(*current_hash.as_bytes())) == selected {
            return Ok(data.to_vec());
        }
        let current = self
            .local_version_bytes(Some(ContentHash(*current_hash.as_bytes())))
            .ok_or(DocumentRestoreError::Unavailable)?;
        match tilo_doc::restore::restore_serialized(&current, data, None) {
            Ok(Some(restored)) => Ok(restored),
            Ok(None) => Ok(current),
            Err(error) => Err(DocumentRestoreError::NotADocument(error.to_string())),
        }
    }

    fn review_locked(
        &self,
        ino: u64,
        namespace: &Namespace,
        history: &crate::history::HistoryLease<'_>,
        repository: &RepositoryLease<'_>,
    ) -> Result<Review, Outcome> {
        let workspace_id = self.restore_workspace()?;
        let record_id = namespace.record_id_for(ino).ok_or(Outcome::NotVisible)?;
        let key = RecordRepositoryKey {
            workspace_id,
            record_id,
        };
        let signed = repository.snapshot(key).map_err(|_| Outcome::Unavailable)?;
        let stored = history.snapshot(record_id).ok_or(Outcome::Unavailable)?;
        let mut versions = signed.versions.clone();
        versions.sort_by_key(|v| v.id);
        let mut history_versions = stored.versions.clone();
        history_versions.sort_by_key(|v| v.id);
        let mut conflicts = signed.conflicts.clone();
        conflicts.sort_by_key(|v| v.id);
        let mut history_conflicts = stored.conflicts.clone();
        history_conflicts.sort_by_key(|v| v.id);
        let revision = namespace
            .content_revision_for(ino)
            .ok_or(Outcome::Unavailable)?;
        let binding = signed.binding.as_ref().ok_or(Outcome::Unavailable)?;
        let current_binding = namespace
            .signed_binding_for(ino, binding.actor, binding.observed_at_ms)
            .ok_or(Outcome::Unavailable)?;
        if versions != history_versions
            || conflicts != history_conflicts
            || signed.content_revision != Some(revision)
            || *binding != current_binding
        {
            return Err(Outcome::Unavailable);
        }
        let streams = repository.frontier(key).map_err(|_| Outcome::Unavailable)?;
        let registers = namespace
            .checked_record_frontier(ino)
            .ok_or(Outcome::Unavailable)?;
        let bytes = serde_json::to_vec(&(
            "tilo.checked-files-review.v1",
            workspace_id,
            self.mount_lifetime,
            record_id,
            streams,
            stored,
            current_binding,
            revision,
            namespace.path_for(ino),
            registers,
        ))
        .map_err(|_| Outcome::Unavailable)?;
        Ok(Review {
            schema_version: 1,
            workspace_id,
            mount_lifetime: self.mount_lifetime,
            record_id,
            frontier: *blake3::hash(&bytes).as_bytes(),
        })
    }

    /// The content revision `ino` currently points at, which must be one of the
    /// record's history versions.
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "a history version's `id` is the version id the content revision points at"
    )]
    fn observed_revision(
        namespace: &Namespace,
        ino: u64,
        history: &crate::history::RecordHistorySnapshot,
    ) -> Result<ContentRevisionV1, Outcome> {
        let revision = namespace
            .content_revision_for(ino)
            .ok_or(Outcome::Unavailable)?;
        history
            .versions
            .iter()
            .any(|value| {
                value.id == revision.version_id
                    && value.content_hash == revision.content_hash
                    && value.metadata_hash == revision.metadata_hash
            })
            .then_some(revision)
            .ok_or(Outcome::Unavailable)
    }

    /// Review the current checked-history frontier of `ino` for a later restore.
    ///
    /// # Errors
    ///
    /// Returns the checked-history answer that prevents a review: the file is
    /// not visible to the caller, has pending writes, has no checked
    /// content-owner restore route, or its history state is unavailable.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the frontier is computed from one consistent snapshot of buffers, namespace, \
                  history, repository and ACL; no guard may be released before the review"
    )]
    pub fn checked_history_review(&self, ino: u64) -> Result<Review, Outcome> {
        self.replacement_transactions
            .as_ref()
            .ok_or(Outcome::Unavailable)?;
        let publisher = self.record_publisher.as_ref().ok_or(Outcome::Unavailable)?;
        let (history, _) = self.history.as_ref().ok_or(Outcome::Unavailable)?;
        let buffers = self
            .write_buffers
            .lock()
            .map_err(|_| Outcome::Unavailable)?;
        let namespace = self.namespace.lock().map_err(|_| Outcome::Unavailable)?;
        let history = history.lease().map_err(|_| Outcome::Unavailable)?;
        let repository = publisher
            .repository()
            .lease()
            .map_err(|_| Outcome::Unavailable)?;
        let acl = self
            .enforcement
            .as_ref()
            .map(|(acl, _)| acl.admission_lease())
            .transpose()
            .map_err(|()| Outcome::NotVisible)?;
        let path = namespace.path_for(ino).ok_or(Outcome::NotVisible)?;
        access(
            acl.as_deref(),
            self.enforcement.as_ref().map(|(_, id)| id),
            &path,
            Access::Read,
        )?;
        if buffers.contains_key(&ino) {
            return Err(Outcome::PendingWrites);
        }
        Self::restore_kind(&namespace, ino)?;
        self.review_locked(ino, &namespace, &history, &repository)
    }

    /// Restore `ino` as `request` asks. Content that is not local yet is
    /// hydrated by a history job and the answer is `RecoveryPending`; the
    /// same request, repeated once the job is ready, commits.
    pub fn checked_history_restore(&self, ino: u64, request: Request) -> Outcome {
        match self.restore_checked_inner(ino, request) {
            Ok(Step::Settled(value)) | Err(value) => value,
            Ok(Step::Hydrating) => Outcome::RecoveryPending {
                intent_id: request.intent_id,
            },
        }
    }

    /// Read the exact durable intent result without resuming or mutating it.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the receipt is read while the namespace and ACL that authorized the caller stay held"
    )]
    pub fn checked_history_result(&self, ino: u64, intent_id: uuid::Uuid) -> Outcome {
        let read = || -> Result<Outcome, Outcome> {
            let runtime = self
                .replacement_transactions
                .as_ref()
                .ok_or(Outcome::Unavailable)?;
            let journal = runtime.journal.lock().map_err(|_| Outcome::Unavailable)?;
            let namespace = self.namespace.lock().map_err(|_| Outcome::Unavailable)?;
            let acl = self
                .enforcement
                .as_ref()
                .map(|(acl, _)| acl.admission_lease())
                .transpose()
                .map_err(|()| Outcome::NotVisible)?;
            let path = namespace.path_for(ino).ok_or(Outcome::NotVisible)?;
            access(
                acl.as_deref(),
                self.enforcement.as_ref().map(|(_, id)| id),
                &path,
                Access::Read,
            )?;
            let Some(receipt) = journal.checked_receipt(intent_id) else {
                // Not admitted yet: its content may still be hydrating.
                return Err(
                    if matches!(
                        self.history_job(ino, intent_id),
                        Some(fs_protocol::files::HistoryJobStateV1::Hydrating { .. })
                    ) {
                        Outcome::RecoveryPending { intent_id }
                    } else {
                        Outcome::Unavailable
                    },
                );
            };
            if namespace.record_id_for(ino) != Some(receipt.record_id)
                || self.restore_workspace()? != receipt.workspace_id
            {
                return Err(Outcome::ReviewRequired);
            }
            Ok(
                if journal.checked_phase(intent_id) == Some(Phase::BroadcastCommitted) {
                    Outcome::Applied { receipt }
                } else {
                    Outcome::RecoveryPending { intent_id }
                },
            )
        };
        match read() {
            Ok(result) | Err(result) => result,
        }
    }

    pub(super) fn checked_restore_xattr(&self, ino: u64, value: &[u8]) -> Result<(), i32> {
        if value.len() > fs_protocol::files::MAX_CHECKED_HISTORY_REQUEST_BYTES {
            return Err(EINVAL);
        }
        let request: Request = serde_json::from_slice(value).map_err(|_| EINVAL)?;
        // EALREADY is the explicit successful no-op receipt for this transport;
        // applied/pending use the durable exact-ID result xattr. Never downgrade.
        // EINPROGRESS: the selected content is hydrating in a history job;
        // repeat the request once it is ready.
        let outcome = match self.restore_checked_inner(ino, request) {
            Ok(Step::Hydrating) => return Err(libc::EINPROGRESS),
            Ok(Step::Settled(value)) | Err(value) => value,
        };
        match outcome {
            Outcome::Applied { .. } | Outcome::RecoveryPending { .. } => Ok(()),
            Outcome::Unchanged { .. } => Err(libc::EALREADY),
            Outcome::NotVisible => Err(ENOENT),
            Outcome::ReadOnly => Err(EROFS),
            Outcome::ReviewRequired => Err(EBUSY),
            Outcome::PendingWrites => Err(libc::EAGAIN),
            Outcome::UnsupportedOwner { .. } => Err(libc::ENOTSUP),
            Outcome::Unavailable => Err(EIO),
            Outcome::InvalidRequest => Err(EINVAL),
        }
    }

    fn known_restore(&self, ino: u64, request: Request) -> Result<Option<Outcome>, Outcome> {
        let runtime = self
            .replacement_transactions
            .as_ref()
            .ok_or(Outcome::Unavailable)?;
        let mut journal = runtime.journal.lock().map_err(|_| Outcome::Unavailable)?;
        let Some(admitted) = journal.checked_request(request.intent_id) else {
            if journal.get(request.intent_id).is_some() {
                return Err(Outcome::InvalidRequest);
            }
            return Ok(None);
        };
        if admitted != request {
            return Err(Outcome::InvalidRequest);
        }
        #[expect(
            clippy::significant_drop_tightening,
            reason = "the record binding is checked while the namespace and ACL that authorized the \
                      caller stay held"
        )]
        let authorize = || {
            let namespace = self.namespace.lock().map_err(|_| Outcome::Unavailable)?;
            let acl = self
                .enforcement
                .as_ref()
                .map(|(acl, _)| acl.admission_lease())
                .transpose()
                .map_err(|()| Outcome::NotVisible)?;
            let path = namespace.path_for(ino).ok_or(Outcome::NotVisible)?;
            access(
                acl.as_deref(),
                self.enforcement.as_ref().map(|(_, id)| id),
                &path,
                Access::Write,
            )?;
            if namespace.record_id_for(ino) != Some(request.review.record_id)
                || self.restore_workspace()? != request.review.workspace_id
            {
                return Err(Outcome::ReviewRequired);
            }
            Ok(())
        };
        authorize()?;
        if journal.checked_phase(request.intent_id) != Some(Phase::BroadcastCommitted)
            && let (Some(publisher), Some((history, _))) = (&self.record_publisher, &self.history)
        {
            let sync = self.sync.clone();
            let _ = publisher.recover_replacement_transactions(
                &mut journal,
                history,
                &self.namespace,
                &runtime.namespace_path,
                move |delta| {
                    if let Some(sync) = &sync {
                        sync.broadcast_delta(delta);
                    }
                    Ok(())
                },
            );
        }
        authorize()?;
        Ok(Some(
            if journal.checked_phase(request.intent_id) == Some(Phase::BroadcastCommitted) {
                Outcome::Applied {
                    receipt: journal
                        .checked_receipt(request.intent_id)
                        .ok_or(Outcome::Unavailable)?,
                }
            } else {
                Outcome::RecoveryPending {
                    intent_id: request.intent_id,
                }
            },
        ))
    }

    fn project_checked_availability(&self, record_id: fs_protocol::FileRecordId) {
        let Some(registry) = &self.availability else {
            return;
        };
        let Ok(mut registry) = registry.lock() else {
            return;
        };
        let Ok(mut namespace) = self.namespace.lock() else {
            return;
        };
        let Some(ino) = namespace.ensure_ino_for_record_id(record_id) else {
            return;
        };
        let Some(revision) = namespace.content_revision_for(ino) else {
            return;
        };
        if registry
            .get(record_id)
            .is_none_or(|record| record.current_version != Some(revision.version_id))
        {
            let parent = namespace
                .parent_ino(ino)
                .and_then(|parent| namespace.record_id_for(parent));
            // Reuse the owner's reset of old residency/replica evidence. The
            // admitted receipt remains independent of this current projection.
            let _ = registry.track_current_revision(revision, RecordKind::File, parent, now_ms());
        }
    }

    /// Resolve the version a request restores to and the intent it compensates.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the undo/redo precondition compares the journal receipt with the current \
                  namespace revision while the journal is held"
    )]
    fn restore_selection(
        &self,
        runtime: &super::ReplacementTransactionRuntime,
        ino: u64,
        request: Request,
    ) -> Result<(fs_protocol::FileVersionId, Option<uuid::Uuid>), Outcome> {
        let journal = runtime.journal.lock().map_err(|_| Outcome::Unavailable)?;
        Ok(match request.operation {
            Operation::Restore { version_id } => (version_id, None),
            Operation::Undo { restore_intent }
            | Operation::Redo {
                undo_intent: restore_intent,
            } => {
                let previous = journal
                    .checked_receipt(restore_intent)
                    .ok_or(Outcome::InvalidRequest)?;
                let prior_action = journal
                    .checked_request(restore_intent)
                    .ok_or(Outcome::InvalidRequest)?
                    .operation;
                if !matches!(
                    (request.operation, prior_action),
                    (
                        Operation::Undo { .. },
                        Operation::Restore { .. } | Operation::Redo { .. }
                    ) | (Operation::Redo { .. }, Operation::Undo { .. })
                ) {
                    return Err(Outcome::InvalidRequest);
                }

                if journal.checked_phase(restore_intent) != Some(Phase::BroadcastCommitted)
                    || previous.record_id != request.review.record_id
                    || previous.workspace_id != request.review.workspace_id
                {
                    return Err(Outcome::ReviewRequired);
                }
                if self.ns().content_revision_for(ino) != Some(previous.restored_revision) {
                    return Err(Outcome::ReviewRequired);
                }
                (previous.prior_revision.version_id, Some(restore_intent))
            }
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one admission-to-broadcast sequence whose guards (journal, publication, buffers, \
                  namespace, history, repository, ACL) are acquired and released in a fixed order; \
                  splitting it would scatter that ordering"
    )]
    fn restore_checked_inner(&self, ino: u64, request: Request) -> Result<Step, Outcome> {
        request.validate().map_err(|_| Outcome::InvalidRequest)?;
        self.check_access(ino, Access::Write).map_err(|errno| {
            if errno == EROFS {
                Outcome::ReadOnly
            } else {
                Outcome::NotVisible
            }
        })?;
        let runtime = self
            .replacement_transactions
            .as_ref()
            .ok_or(Outcome::Unavailable)?;
        let publisher = self.record_publisher.as_ref().ok_or(Outcome::Unavailable)?;
        let (history_store, actor) = self.history.as_ref().ok_or(Outcome::Unavailable)?;
        if self.ns().record_id_for(ino) != Some(request.review.record_id)
            || self.restore_workspace()? != request.review.workspace_id
        {
            return Err(Outcome::ReviewRequired);
        }
        // An admitted request is looked up before mount-lifetime freshness. Its
        // exact signed receipt remains queryable after reopen; a new old-token
        // request still fails the lifetime comparison below.
        if let Some(outcome) = self.known_restore(ino, request)? {
            if matches!(outcome, Outcome::Applied { .. }) {
                self.project_checked_availability(request.review.record_id);
            }
            return Ok(Step::Settled(outcome));
        }
        let (selected_id, compensates) = self.restore_selection(runtime, ino, request)?;
        if request.review.mount_lifetime != self.mount_lifetime {
            return Err(Outcome::ReviewRequired);
        }
        let selected = history_store
            .snapshot(request.review.record_id)
            .and_then(|state| {
                state
                    .versions
                    .into_iter()
                    .find(|value| value.id == selected_id)
            })
            .ok_or(Outcome::InvalidRequest)?;
        let kind = Self::restore_kind(&self.ns(), ino)?;
        // The selected version must be local before anything commits; a
        // serialized document also merges into the current one. What is
        // missing hydrates in a history job, never on this FUSE worker.
        let mut needed = vec![selected.content_hash];
        if kind.is_serialized_document() {
            needed.push(
                self.ns()
                    .hash_for(ino)
                    .map(|hash| ContentHash(*hash.as_bytes())),
            );
        }
        match self.history_content_local(ino, request.intent_id, &needed) {
            Ok(()) => {}
            Err(libc::EINPROGRESS) => return Ok(Step::Hydrating),
            // A job for this intent is still fetching other content: the
            // current document changed under it.
            Err(EBUSY) => return Err(Outcome::ReviewRequired),
            Err(_) => return Err(Outcome::Unavailable),
        }
        #[cfg(test)]
        test_pause(request.intent_id, TestPause::Hydrated);
        let (current_hash, current_size) = {
            let namespace = self.ns();
            (
                namespace.hash_for(ino),
                namespace.getattr(ino).map(|attr| attr.size),
            )
        };
        let current_len = match current_hash {
            Some(_) => current_size.ok_or(Outcome::NotVisible)?,
            None => 0,
        };
        // The bytes the restore makes current. Opaque content is the selected
        // version's own, already in the store: it is committed by hash and
        // never read. A serialized document is the current document plus a
        // change reaching the selected state, written to the store before
        // any lock is taken.
        let (restored_hash, restored_len) = if kind.is_serialized_document() {
            let data = self
                .local_version_bytes(selected.content_hash)
                .ok_or(Outcome::Unavailable)?;
            let restored = self
                .restored_document(current_hash, selected.content_hash, &data)
                .map_err(DocumentRestoreError::outcome)?;
            let hash = self
                .write_content_blocking(&restored)
                .map_err(|_| Outcome::Unavailable)?
                .map(|hash| ContentHash(*hash.as_bytes()));
            (hash, restored.len() as u64)
        } else {
            (
                selected.content_hash,
                self.local_version_len(selected.content_hash)
                    .ok_or(Outcome::Unavailable)?,
            )
        };
        let mut journal = runtime.journal.lock().map_err(|_| Outcome::Unavailable)?;
        let _publication = publisher
            .publication_lease()
            .map_err(|_| Outcome::Unavailable)?;
        #[cfg(test)]
        test_pause(request.intent_id, TestPause::BeforeBuffers);
        let buffers = self
            .write_buffers
            .lock()
            .map_err(|_| Outcome::Unavailable)?;
        let mut namespace = self.namespace.lock().map_err(|_| Outcome::Unavailable)?;
        let mut history = history_store.lease().map_err(|_| Outcome::Unavailable)?;
        let mut repository = publisher
            .repository()
            .lease()
            .map_err(|_| Outcome::Unavailable)?;
        let acl = self
            .enforcement
            .as_ref()
            .map(|(acl, _)| acl.admission_lease())
            .transpose()
            .map_err(|()| Outcome::NotVisible)?;
        let path = namespace.path_for(ino).ok_or(Outcome::NotVisible)?;
        let identity = self.enforcement.as_ref().map(|(_, id)| id);
        access(acl.as_deref(), identity, &path, Access::Write)?;
        if namespace.record_id_for(ino) != Some(request.review.record_id) {
            return Err(Outcome::ReviewRequired);
        }
        if let Some(admitted) = journal.checked_request(request.intent_id) {
            if admitted != request {
                return Err(Outcome::InvalidRequest);
            }
            return Ok(Step::Settled(
                if journal.checked_phase(request.intent_id) == Some(Phase::BroadcastCommitted) {
                    Outcome::Applied {
                        receipt: journal
                            .checked_receipt(request.intent_id)
                            .ok_or(Outcome::Unavailable)?,
                    }
                } else {
                    Outcome::RecoveryPending {
                        intent_id: request.intent_id,
                    }
                },
            ));
        }
        if journal.get(request.intent_id).is_some() {
            return Err(Outcome::InvalidRequest);
        }
        if buffers.contains_key(&ino) {
            return Err(Outcome::PendingWrites);
        }
        if Self::restore_kind(&namespace, ino)? != kind {
            return Err(Outcome::ReviewRequired);
        }
        let current_review = self.review_locked(ino, &namespace, &history, &repository)?;
        if current_review != request.review {
            return Err(Outcome::ReviewRequired);
        }
        let state = history
            .snapshot(request.review.record_id)
            .ok_or(Outcome::Unavailable)?;
        if !state.versions.iter().any(|value| value == &selected) {
            return Err(Outcome::ReviewRequired);
        }
        let prior = Self::observed_revision(&namespace, ino, &state)?;
        if namespace.hash_for(ino) != current_hash {
            return Err(Outcome::ReviewRequired);
        }
        if prior.byte_len != current_len {
            return Err(Outcome::Unavailable);
        }
        if prior.content_hash == restored_hash && prior.byte_len == restored_len {
            return Ok(Step::Settled(Outcome::Unchanged {
                intent_id: request.intent_id,
                review: current_review,
            }));
        }
        if state.heads.len() > fs_protocol::files::MAX_VERSION_PARENTS {
            return Err(Outcome::Unavailable);
        }
        if journal.retained_count() >= crate::record_transaction::MAX_RECORD_TRANSACTION_ENTRIES {
            return Err(Outcome::Unavailable);
        }
        let timestamp = now_ms().max(
            state
                .versions
                .iter()
                .map(|v| v.timestamp_ms)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(Outcome::Unavailable)?,
        );
        let metadata = VersionMetadataDigest {
            path: Some(path.clone()),
            kind: 2,
            byte_len: restored_len,
            deleted: false,
        };
        let metadata_hash = ContentHash(
            *blake3::hash(&postcard::to_allocvec(&metadata).map_err(|_| Outcome::Unavailable)?)
                .as_bytes(),
        );
        let proposed = FileVersionV1 {
            id: fs_protocol::FileVersionId::new(),
            record_id: request.review.record_id,
            parents: state.heads,
            actor: *actor,
            timestamp_ms: timestamp,
            content_hash: restored_hash,
            metadata_hash,
            provenance: FileProvenance::Restore { from: selected_id },
        };
        let revision = ContentRevisionV1 {
            schema_version: fs_protocol::CONTENT_REVISION_VERSION,
            record_id: proposed.record_id,
            version_id: proposed.id,
            content_hash: proposed.content_hash,
            byte_len: restored_len,
            metadata_hash,
        };
        let mut participant = FileMutationParticipant {
            record_id: proposed.record_id,
            role: FileMutationParticipantRole::RestoreContent,
            expected_version: prior.version_id,
            proposed_binding: namespace
                .signed_binding_for(ino, *actor, timestamp)
                .ok_or(Outcome::Unavailable)?,
            proposed_version: proposed.clone(),
            proposed_content_revision: Some(revision),
            projection_hash: ContentHash([0; 32]),
        };
        participant.projection_hash = participant
            .computed_projection_hash()
            .map_err(|_| Outcome::Unavailable)?;
        let key = RecordRepositoryKey {
            workspace_id: request.review.workspace_id,
            record_id: proposed.record_id,
        };
        let history_candidate = history
            .prepare_versions(std::slice::from_ref(&proposed))
            .map_err(|_| Outcome::Unavailable)?;
        let result_receipt = Receipt {
            schema_version: 1,
            intent_id: request.intent_id,
            workspace_id: key.workspace_id,
            mount_lifetime: self.mount_lifetime,
            record_id: proposed.record_id,
            selected_version: selected_id,
            prior_revision: prior,
            restored_revision: revision,
            compensates,
        };
        let manifest = FileMutationTransactionManifest {
            schema_version: fs_protocol::files::FILE_MUTATION_TRANSACTION_VERSION_V3,
            transaction_id: request.intent_id,
            workspace_id: key.workspace_id,
            actor: *actor,
            timestamp_ms: timestamp,
            operation: FileMutationOperation::RestoreContent {
                request,
                selected_version: selected_id,
                prior_revision: prior,
                compensates,
            },
            participants: vec![participant],
        };
        let mut entry = publisher
            .sign_checked_restore(manifest, repository.highest_sequence(key, actor))
            .map_err(|_| Outcome::Unavailable)?;
        let mut namespace_candidate = namespace.clone();
        let _ = namespace_candidate.save_incremental();
        let projection_actor = namespace_candidate.begin_restore_projection(
            request.review.workspace_id,
            request.review.record_id,
            request.intent_id,
        );
        let delta = namespace_candidate
            .apply_mutation_transaction(&entry.transaction)
            .map_err(|_| Outcome::Unavailable)?
            .ok_or(Outcome::Unavailable)?;
        namespace_candidate.end_restore_projection(projection_actor);
        entry.namespace_delta = Some(delta.clone());
        let encoded_len = serde_json::to_vec(&entry)
            .map_err(|_| Outcome::Unavailable)?
            .len();
        // Reserve enough serialized space for the longest subsequent phase name.
        if encoded_len.saturating_add(64)
            > crate::record_transaction::MAX_RECORD_TRANSACTION_ENTRY_BYTES
        {
            return Err(Outcome::Unavailable);
        }
        let candidate = repository
            .prepare(
                &journal,
                &entry,
                &publisher.local_authorizer(request.review.record_id),
            )
            .map_err(|_| Outcome::Unavailable)?;
        #[cfg(test)]
        test_pause(request.intent_id, TestPause::Admission);
        // Last authority decision, including grant expiry, precedes the first
        // durable mutation. A remains borrowed through Prepared insertion.
        access(acl.as_deref(), identity, &path, Access::Write)?;
        let pending = || Outcome::RecoveryPending {
            intent_id: request.intent_id,
        };
        repository.commit(&mut journal, candidate, |point| {
            #[cfg(test)]
            match point {
                crate::record_repository::RecordTransactionCheckpoint::PreparedJournalDurable => test_fault(request.intent_id, TestFault::Prepared)?,
                crate::record_repository::RecordTransactionCheckpoint::ParticipantRecordDurable { .. } => test_fault(request.intent_id, TestFault::Participant)?,
                crate::record_repository::RecordTransactionCheckpoint::ParticipantPhaseDurable => {}
            }
            let _ = point; Ok(())
        }).map_err(|_| pending())?;
        drop(acl);
        drop(repository);
        #[cfg(test)]
        test_fault(request.intent_id, TestFault::Records).map_err(|_| pending())?;
        history.install(history_candidate).map_err(|_| pending())?;
        journal
            .advance(request.intent_id, Phase::HistoryCommitted, None)
            .map_err(|_| pending())?;
        #[cfg(test)]
        test_fault(request.intent_id, TestFault::History).map_err(|_| pending())?;
        journal
            .stage_namespace_delta(request.intent_id, delta.clone())
            .map_err(|_| pending())?;
        #[cfg(test)]
        test_fault(request.intent_id, TestFault::Delta).map_err(|_| pending())?;
        crate::durable_backup::write(
            &runtime.namespace_path,
            &namespace_candidate.save(),
            |bytes| Namespace::from_bytes("checked-restore-validator", bytes).is_ok(),
        )
        .map_err(|_| pending())?;
        *namespace = namespace_candidate;
        #[cfg(test)]
        test_fault(request.intent_id, TestFault::Namespace).map_err(|_| pending())?;
        journal
            .advance(
                request.intent_id,
                Phase::NamespaceCommitted,
                Some(delta.clone()),
            )
            .map_err(|_| pending())?;
        drop(history);
        drop(namespace);
        drop(buffers);
        self.project_checked_availability(request.review.record_id);
        self.publish(delta.clone());
        #[cfg(test)]
        test_fault(request.intent_id, TestFault::Broadcast).map_err(|_| pending())?;
        journal
            .advance(request.intent_id, Phase::BroadcastCommitted, Some(delta))
            .map_err(|_| pending())?;
        drop(journal);
        self.attribute_version(&proposed, Some(&path));
        Ok(Step::Settled(Outcome::Applied {
            receipt: result_receipt,
        }))
    }
}

/// How far one pass of a checked restore got.
#[expect(
    clippy::large_enum_variant,
    reason = "lives only on the stack of one restore call, beside the same wire answer the \
              error side already returns by value"
)]
enum Step {
    /// The final answer for this pass.
    Settled(Outcome),
    /// The content is hydrating in a history job; nothing was admitted.
    Hydrating,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestFault {
    Prepared,
    Participant,
    Records,
    History,
    Delta,
    Namespace,
    Broadcast,
}
#[cfg(test)]
static RESTORE_FAULTS: std::sync::OnceLock<
    Mutex<std::collections::HashMap<uuid::Uuid, TestFault>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
fn test_fault(id: uuid::Uuid, point: TestFault) -> Result<(), std::io::Error> {
    let fired = {
        let mut faults = RESTORE_FAULTS
            .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
            .lock()
            .unwrap();
        faults.get(&id) == Some(&point) && faults.remove(&id).is_some()
    };
    if fired {
        return Err(std::io::Error::other("checked restore fault"));
    }
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum TestPause {
    BeforeBuffers,
    Hydrated,
    Admission,
}
#[cfg(test)]
struct PauseGate {
    point: TestPause,
    entered: std::sync::mpsc::Sender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}
#[cfg(test)]
static RESTORE_PAUSES: std::sync::OnceLock<
    Mutex<std::collections::HashMap<uuid::Uuid, PauseGate>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
fn test_pause(id: uuid::Uuid, point: TestPause) {
    let gate = {
        let mut pauses = RESTORE_PAUSES
            .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
            .lock()
            .unwrap();
        if pauses.get(&id).is_some_and(|gate| gate.point == point) {
            pauses.remove(&id)
        } else {
            None
        }
    };
    if let Some(gate) = gate {
        gate.entered.send(()).unwrap();
        gate.resume
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap();
    }
}

#[cfg(test)]
#[path = "checked_restore/tests.rs"]
mod tests;
