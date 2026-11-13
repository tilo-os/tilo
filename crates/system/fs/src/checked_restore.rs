//! Checked history: review a file's past, restore one of its versions.
//!
//! A review names the record's exact history state; a restore is admitted
//! against it and lands as one document change: the restored content
//! revision, the new version, and the receipt that answers the request. The
//! tree document is the only record, so an interrupted restore either
//! happened whole or not at all.
use super::{
    Access, EBUSY, EINVAL, EIO, ENOENT, EROFS, Hash, Namespace, QuickFS, RecordKind, now_ms,
};
use crate::content_kind::RecordContentKind;
use crate::namespace::history::VersionMetadataDigest;
use fs_protocol::files::{
    CheckedHistoryOperationV1 as Operation, CheckedHistoryOutcomeV1 as Outcome,
    CheckedHistoryRequestV1 as CheckedRequest, FileRestoreReceiptV1 as Receipt,
    FileRestoreReviewV1 as Review,
};
use fs_protocol::{ContentHash, ContentRevisionV1, FileProvenance, FileVersionV1};

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
        let workspace = self.checked_history_workspace.ok_or(Outcome::Unavailable)?;
        if workspace.is_nil() {
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

    /// The frontier a restore is admitted against: everything a later restore
    /// must find unchanged, as one hash.
    fn reviewed_frontier(
        &self,
        state: &crate::namespace::history::RecordHistorySnapshot,
        revision: &ContentRevisionV1,
        path: &str,
    ) -> Result<Review, Outcome> {
        let workspace_id = self.restore_workspace()?;
        let mut versions = state.versions.clone();
        versions.sort_by_key(|version| version.id);
        let mut conflicts = state.conflicts.clone();
        conflicts.sort_by_key(|conflict| conflict.id);
        let bytes = serde_json::to_vec(&(
            "tilo.checked-files-review.v1",
            workspace_id,
            self.mount_lifetime,
            state.record_id,
            &versions,
            &conflicts,
            revision,
            path,
        ))
        .map_err(|_| Outcome::Unavailable)?;
        Ok(Review {
            schema_version: 1,
            workspace_id,
            mount_lifetime: self.mount_lifetime,
            record_id: state.record_id,
            frontier: *blake3::hash(&bytes).as_bytes(),
        })
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
                  history and ACL; no guard may be released before the review"
    )]
    pub fn checked_history_review(&self, ino: u64) -> Result<Review, Outcome> {
        self.restore_workspace()?;
        self.history.as_ref().ok_or(Outcome::Unavailable)?;
        let buffers = self
            .write_buffers
            .lock()
            .map_err(|_| Outcome::Unavailable)?;
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
        if buffers.contains_key(&ino) {
            return Err(Outcome::PendingWrites);
        }
        Self::restore_kind(&namespace, ino)?;
        let record_id = namespace.record_id_for(ino).ok_or(Outcome::NotVisible)?;
        let state = namespace
            .record_history(record_id)
            .ok_or(Outcome::Unavailable)?;
        let revision = namespace
            .observed_content_revision(ino)
            .ok_or(Outcome::Unavailable)?;
        self.reviewed_frontier(&state, &revision, &path)
    }

    /// Read the exact durable intent result without resuming or mutating it.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the receipt is read while the namespace and ACL that authorized the caller stay \
                  held"
    )]
    pub fn checked_history_result(&self, ino: u64, intent_id: uuid::Uuid) -> Outcome {
        let read = || -> Result<Outcome, Outcome> {
            self.restore_workspace()?;
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
            let Some((_, receipt)) = namespace.restore_record(intent_id) else {
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
            if namespace.record_id_for(ino) != Some(receipt.record_id) {
                return Err(Outcome::ReviewRequired);
            }
            Ok(Outcome::Applied { receipt })
        };
        match read() {
            Ok(result) | Err(result) => result,
        }
    }

    /// Restore `ino` as `request` asks, or say why not. Content that is not
    /// local yet is hydrated by a history job and the answer is
    /// `RecoveryPending`; the same request, repeated once the job is ready,
    /// commits.
    pub fn checked_history_restore(&self, ino: u64, request: Request) -> Outcome {
        match self.restore_checked_inner(ino, request) {
            Ok(Step::Settled(value)) | Err(value) => value,
            Ok(Step::Hydrating) => Outcome::RecoveryPending {
                intent_id: request.intent_id,
            },
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
        let Some(revision) = namespace.observed_content_revision(ino) else {
            return;
        };
        if registry
            .get(record_id)
            .is_none_or(|record| record.current_version != Some(revision.version_id))
        {
            let parent = namespace
                .parent_ino(ino)
                .and_then(|parent| namespace.record_id_for(parent));
            let _ = registry.track_current_revision(revision, RecordKind::File, parent, now_ms());
        }
    }

    /// The version a request restores to, and the deferred receipt it
    /// compensates.
    fn selected_version(
        &self,
        ino: u64,
        request: Request,
    ) -> Result<(fs_protocol::FileVersionId, Option<uuid::Uuid>), Outcome> {
        let (selected, compensates) = match request.operation {
            Operation::Restore { version_id } => (version_id, None),
            Operation::Undo { restore_intent }
            | Operation::Redo {
                undo_intent: restore_intent,
            } => {
                let (prior_request, receipt) = self
                    .ns()
                    .restore_record(restore_intent)
                    .ok_or(Outcome::InvalidRequest)?;
                if !matches!(
                    (request.operation, prior_request.operation),
                    (
                        Operation::Undo { .. },
                        Operation::Restore { .. } | Operation::Redo { .. }
                    ) | (Operation::Redo { .. }, Operation::Undo { .. })
                ) {
                    return Err(Outcome::InvalidRequest);
                }
                if receipt.record_id != request.review.record_id
                    || receipt.workspace_id != request.review.workspace_id
                {
                    return Err(Outcome::ReviewRequired);
                }
                if self
                    .ns()
                    .observed_content_revision(ino)
                    .is_none_or(|revision| revision != receipt.restored_revision)
                {
                    return Err(Outcome::ReviewRequired);
                }
                (receipt.prior_revision.version_id, Some(restore_intent))
            }
        };
        let _ = ino;
        Ok((selected, compensates))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one admission-to-commit sequence whose guards (buffers, namespace, ACL) are \
                  acquired and released in a fixed order"
    )]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the buffers, namespace and ACL that admit the restore stay held together so the \
                  frontier it was reviewed against cannot move underneath it"
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
        self.restore_workspace()?;
        let actor = *self.history.as_ref().ok_or(Outcome::Unavailable)?;
        if self.ns().record_id_for(ino) != Some(request.review.record_id) {
            return Err(Outcome::ReviewRequired);
        }
        // An admitted request answers with its own receipt, whatever the
        // mount has done since.
        let known = self.ns().restore_record(request.intent_id);
        if let Some((admitted, receipt)) = known {
            if admitted != request {
                return Err(Outcome::InvalidRequest);
            }
            self.project_checked_availability(receipt.record_id);
            return Ok(Step::Settled(Outcome::Applied { receipt }));
        }
        if request.review.mount_lifetime != self.mount_lifetime {
            return Err(Outcome::ReviewRequired);
        }
        let (selected_id, compensates) = self.selected_version(ino, request)?;
        let (selected, kind, current_hash) = {
            let namespace = self.ns();
            let selected = namespace
                .record_version(request.review.record_id, selected_id)
                .0
                .ok_or(Outcome::InvalidRequest)?;
            let kind = Self::restore_kind(&namespace, ino)?;
            (selected, kind, namespace.hash_for(ino))
        };
        // The selected version must be local before anything commits; a
        // serialized document also merges into the current one. What is
        // missing hydrates in a history job, never on this FUSE worker.
        let mut needed = vec![selected.content_hash];
        if kind.is_serialized_document() {
            needed.push(current_hash.map(|hash| ContentHash(*hash.as_bytes())));
        }
        match self.history_content_local(ino, request.intent_id, &needed) {
            Ok(()) => {}
            Err(libc::EINPROGRESS) => return Ok(Step::Hydrating),
            // A job for this intent is still fetching other content: the
            // current document changed under it.
            Err(EBUSY) => return Err(Outcome::ReviewRequired),
            Err(_) => return Err(Outcome::Unavailable),
        }
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
        let committed = {
            let buffers = self
                .write_buffers
                .lock()
                .map_err(|_| Outcome::Unavailable)?;
            let mut namespace = self.namespace.lock().map_err(|_| Outcome::Unavailable)?;
            let acl = self
                .enforcement
                .as_ref()
                .map(|(acl, _)| acl.admission_lease())
                .transpose()
                .map_err(|()| Outcome::NotVisible)?;
            let path = namespace.path_for(ino).ok_or(Outcome::NotVisible)?;
            let identity = self.enforcement.as_ref().map(|(_, id)| id);
            access(acl.as_deref(), identity, &path, Access::Write)?;
            if namespace.record_id_for(ino) != Some(request.review.record_id)
                || Self::restore_kind(&namespace, ino)? != kind
            {
                return Err(Outcome::ReviewRequired);
            }
            if buffers.contains_key(&ino) {
                return Err(Outcome::PendingWrites);
            }
            let state = namespace
                .record_history(request.review.record_id)
                .ok_or(Outcome::Unavailable)?;
            let prior = namespace
                .observed_content_revision(ino)
                .ok_or(Outcome::Unavailable)?;
            if !state
                .versions
                .iter()
                .any(|version| version.id == prior.version_id)
            {
                return Err(Outcome::Unavailable);
            }
            let current_review = self.reviewed_frontier(&state, &prior, &path)?;
            if current_review != request.review {
                return Err(Outcome::ReviewRequired);
            }
            if namespace.hash_for(ino) != current_hash {
                return Err(Outcome::ReviewRequired);
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
            let timestamp = now_ms().max(
                state
                    .versions
                    .iter()
                    .map(|version| version.timestamp_ms)
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
                actor,
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
            let receipt = Receipt {
                schema_version: 1,
                intent_id: request.intent_id,
                workspace_id: request.review.workspace_id,
                mount_lifetime: self.mount_lifetime,
                record_id: proposed.record_id,
                selected_version: selected_id,
                prior_revision: prior,
                restored_revision: revision,
                compensates,
            };
            // One document change carries the restored revision, its version
            // and the receipt: a mount either has all of them or none.
            namespace.note_change();
            namespace
                .append_version(proposed.clone())
                .map_err(|_| Outcome::Unavailable)?;
            if !namespace.set_content_revision(ino, &revision, timestamp) {
                return Err(Outcome::Unavailable);
            }
            namespace
                .record_restore(request.intent_id, &request, &receipt)
                .map_err(|_| Outcome::Unavailable)?;
            namespace.note_change();
            if let Some(persistence) = &self.namespace_persistence {
                crate::durable_backup::write(
                    &persistence.namespace_path,
                    &namespace.save(),
                    |bytes| Namespace::from_bytes("checked-restore-validator", bytes).is_ok(),
                )
                .map_err(|_| Outcome::Unavailable)?;
            }
            (proposed, receipt, path)
        };
        let (proposed, receipt, path) = committed;
        self.project_checked_availability(request.review.record_id);
        self.publish();
        self.attribute_version(&proposed, Some(&path));
        Ok(Step::Settled(Outcome::Applied { receipt }))
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
