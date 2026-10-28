//! Certified-local publishing bridge for record replication.
//!
//! The caller must construct this bridge from a currently certified local
//! mount identity. This module verifies possession of the claimed device key,
//! but it does not resolve the human root, load device revocations, or keep a
//! certificate live. Consequently [`LocalOwnerAuthorizer`] is deliberately
//! suitable only for local publication, never peer admission or share access.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use collab_types::CanonicalError;
use fs_protocol::{
    CONTENT_REVISION_VERSION, ConflictStatus, ContentRevisionV1, FileActor, FileBindingV1,
    FileConflictV1, FileMetadataV1, FileMutationTransaction, FileMutationTransactionManifest,
    FileRecordError, FileRecordId, FileVersionId, FileVersionV1,
};
use thiserror::Error;
use uuid::Uuid;

use crate::record_replication::{
    RecordAction, RecordAuthorizer, RecordEnvelopeV1, RecordOperationV1, RecordReplicationError,
    RecordSnapshot,
};
use crate::record_repository::{
    MAX_RECORD_BATCH_OPERATIONS, MAX_RECORD_CONFLICTS, RecordRepository, RecordRepositoryError,
    RecordRepositoryKey,
};

const MAX_SEQUENCE_RETRIES: usize = 8;

/// Local-owner policy produced only after the publisher proves possession of
/// `actor.device`. It is scoped to one actor and one stable record.
#[derive(Clone, Copy, Debug)]
pub struct LocalOwnerAuthorizer {
    actor: FileActor,
    record_id: FileRecordId,
}

struct LocalTransactionAuthorizer {
    actor: FileActor,
    records: BTreeSet<FileRecordId>,
}

impl RecordAuthorizer for LocalTransactionAuthorizer {
    fn actor_is_certified(&self, actor: &FileActor) -> bool {
        *actor == self.actor
    }

    fn allows(&self, actor: &FileActor, record_id: FileRecordId, _action: RecordAction) -> bool {
        *actor == self.actor && self.records.contains(&record_id)
    }
}

impl RecordAuthorizer for LocalOwnerAuthorizer {
    fn actor_is_certified(&self, actor: &FileActor) -> bool {
        *actor == self.actor
    }

    fn allows(&self, actor: &FileActor, record_id: FileRecordId, _action: RecordAction) -> bool {
        *actor == self.actor && record_id == self.record_id
    }
}

/// Publishes local metadata and immutable history through signed repository
/// operations for exactly one workspace and certified actor.
pub struct RecordPublisher {
    repository: Arc<RecordRepository>,
    workspace_id: Uuid,
    actor: FileActor,
    signing_key: ed25519_dalek::SigningKey,
    /// Serializes publication batches from this publisher instance. Competing
    /// instances remain safe through repository replay checks and retries.
    publish_lock: Mutex<()>,
}

impl RecordPublisher {
    /// Create a publisher bound to one workspace and certified actor.
    pub fn new(
        repository: Arc<RecordRepository>,
        workspace_id: Uuid,
        actor: FileActor,
        signing_key: ed25519_dalek::SigningKey,
    ) -> Result<Self, RecordPublisherError> {
        if workspace_id.is_nil() {
            return Err(RecordPublisherError::InvalidWorkspace);
        }
        if signing_key.verifying_key().to_bytes() != *actor.device.as_bytes() {
            return Err(RecordPublisherError::WrongDeviceKey);
        }
        ed25519_dalek::VerifyingKey::from_bytes(actor.person.as_bytes())
            .map_err(|_| RecordPublisherError::InvalidPersonKey)?;
        Ok(Self {
            repository,
            workspace_id,
            actor,
            signing_key,
            publish_lock: Mutex::new(()),
        })
    }

    #[must_use]
    pub fn actor(&self) -> FileActor {
        self.actor
    }

    pub(crate) fn publication_lease(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, ()>, RecordPublisherError> {
        self.publish_lock
            .lock()
            .map_err(|_| RecordPublisherError::PublicationUnavailable)
    }

    pub(crate) fn repository(&self) -> &Arc<RecordRepository> {
        &self.repository
    }

    /// Start a publish batch on this thread: what this publisher signs from
    /// here on is kept back from peers until [`Self::commit_changeset`]
    /// writes it as one signed changeset.
    pub fn begin_changeset(&self) -> Result<(), RecordPublisherError> {
        Ok(self
            .repository
            .begin_changeset(self.workspace_id, self.actor)?)
    }

    /// Sign the batch started by [`Self::begin_changeset`] once and make it
    /// durable in one write. `None` when the batch published nothing.
    ///
    /// # Errors
    ///
    /// Returns an error when signing or the durable write fails; the batch
    /// then stays unpublished and the next one carries it.
    pub fn commit_changeset(
        &self,
    ) -> Result<Option<Arc<crate::record_changeset::SignedChangesetV1>>, RecordPublisherError> {
        Ok(self
            .repository
            .commit_changeset(&self.signing_key, crate::clock::unix_now_ms())?)
    }

    /// Build the exact bounded restore batch while the caller retains P and R.
    pub(crate) fn sign_checked_restore(
        &self,
        manifest: FileMutationTransactionManifest,
        previous_sequence: u64,
    ) -> Result<crate::record_transaction::RecordTransactionJournalEntry, RecordPublisherError>
    {
        if manifest.workspace_id != self.workspace_id
            || manifest.actor != self.actor
            || !matches!(
                manifest.operation,
                fs_protocol::FileMutationOperation::RestoreContent { .. }
            )
        {
            return Err(RecordPublisherError::TransactionScopeMismatch);
        }
        let transaction = FileMutationTransaction::signed(manifest, &self.signing_key)
            .map_err(|_| RecordPublisherError::InvalidTransaction)?;
        #[expect(
            clippy::indexing_slicing,
            reason = "`FileMutationTransaction::signed` validates the manifest, which rejects an empty participant list"
        )]
        let record_id = transaction.manifest.participants[0].record_id;
        let prepare = previous_sequence
            .checked_add(1)
            .ok_or(RecordPublisherError::SequenceContention)?;
        let commit = prepare
            .checked_add(1)
            .ok_or(RecordPublisherError::SequenceContention)?;
        Ok(crate::record_transaction::RecordTransactionJournalEntry {
            schema_version: crate::record_transaction::RECORD_TRANSACTION_JOURNAL_VERSION,
            participant_batches: vec![
                crate::record_transaction::RecordTransactionParticipantBatch {
                    record_id,
                    envelopes: vec![
                        RecordEnvelopeV1::signed(
                            record_id,
                            self.actor,
                            prepare,
                            RecordOperationV1::PrepareTransaction(transaction.clone()),
                            &self.signing_key,
                        )?,
                        RecordEnvelopeV1::signed(
                            record_id,
                            self.actor,
                            commit,
                            RecordOperationV1::CommitTransaction {
                                transaction_id: transaction.manifest.transaction_id,
                            },
                            &self.signing_key,
                        )?,
                    ],
                },
            ],
            transaction,
            phase: crate::record_transaction::RecordTransactionPhase::Prepared,
            namespace_delta: None,
            external_native_effect: None,
        })
    }

    /// Sign, journal, commit, persist, and broadcast one replacement rename
    /// while holding the same local mutation lock as ordinary publication.
    pub fn execute_replacement_transaction<F>(
        &self,
        manifest: FileMutationTransactionManifest,
        journal: &mut crate::record_transaction::RecordTransactionJournal,
        history: &crate::history::FileHistoryStore,
        namespace: &Arc<Mutex<crate::namespace::Namespace>>,
        namespace_path: &Path,
        broadcast: F,
    ) -> Result<(), RecordPublisherError>
    where
        F: FnMut(Vec<u8>) -> Result<(), std::io::Error>,
    {
        let _guard = self
            .publish_lock
            .lock()
            .map_err(|_| RecordPublisherError::PublicationUnavailable)?;
        let entry = self.prepare_replacement_transaction(manifest)?;
        let authorizer = LocalTransactionAuthorizer {
            actor: self.actor,
            records: entry
                .transaction
                .manifest
                .participants
                .iter()
                .map(|participant| participant.record_id)
                .collect(),
        };
        crate::transaction_coordinator::complete_replacement_transaction(
            &self.repository,
            journal,
            history,
            namespace,
            namespace_path,
            &entry,
            &authorizer,
            broadcast,
        )?;
        Ok(())
    }

    /// Build exactly the canonical signed intent while this publisher's
    /// mutation lease is held. A future external coordinator must separately
    /// retain certified source admission and observed native-effect custody;
    /// this private signing extraction does not bypass external replay fences.
    pub(crate) fn prepare_replacement_transaction(
        &self,
        manifest: FileMutationTransactionManifest,
    ) -> Result<crate::record_transaction::RecordTransactionJournalEntry, RecordPublisherError>
    {
        if manifest.workspace_id != self.workspace_id || manifest.actor != self.actor {
            return Err(RecordPublisherError::TransactionScopeMismatch);
        }
        let transaction = FileMutationTransaction::signed(manifest, &self.signing_key)
            .map_err(|_| RecordPublisherError::InvalidTransaction)?;
        let mut participant_batches = Vec::with_capacity(transaction.manifest.participants.len());
        for participant in &transaction.manifest.participants {
            let key = RecordRepositoryKey {
                workspace_id: self.workspace_id,
                record_id: participant.record_id,
            };
            let sequence = self
                .repository
                .highest_sequence(key, &self.actor)
                .unwrap_or(0);
            let prepare_sequence = sequence
                .checked_add(1)
                .ok_or(RecordPublisherError::SequenceContention)?;
            let commit_sequence = prepare_sequence
                .checked_add(1)
                .ok_or(RecordPublisherError::SequenceContention)?;
            participant_batches.push(
                crate::record_transaction::RecordTransactionParticipantBatch {
                    record_id: participant.record_id,
                    envelopes: vec![
                        RecordEnvelopeV1::signed(
                            participant.record_id,
                            self.actor,
                            prepare_sequence,
                            RecordOperationV1::PrepareTransaction(transaction.clone()),
                            &self.signing_key,
                        )?,
                        RecordEnvelopeV1::signed(
                            participant.record_id,
                            self.actor,
                            commit_sequence,
                            RecordOperationV1::CommitTransaction {
                                transaction_id: transaction.manifest.transaction_id,
                            },
                            &self.signing_key,
                        )?,
                    ],
                },
            );
        }
        let entry = crate::record_transaction::RecordTransactionJournalEntry {
            schema_version: crate::record_transaction::RECORD_TRANSACTION_JOURNAL_VERSION,
            transaction,
            participant_batches,
            phase: crate::record_transaction::RecordTransactionPhase::Prepared,
            namespace_delta: None,
            external_native_effect: None,
        };
        entry
            .validate()
            .map_err(|_| RecordPublisherError::InvalidTransaction)?;
        Ok(entry)
    }

    /// Resume every journaled transaction before a mount becomes visible.
    pub fn recover_replacement_transactions<F>(
        &self,
        journal: &mut crate::record_transaction::RecordTransactionJournal,
        history: &crate::history::FileHistoryStore,
        namespace: &Arc<Mutex<crate::namespace::Namespace>>,
        namespace_path: &Path,
        mut broadcast: F,
    ) -> Result<(), RecordPublisherError>
    where
        F: FnMut(Vec<u8>) -> Result<(), std::io::Error>,
    {
        let _guard = self
            .publish_lock
            .lock()
            .map_err(|_| RecordPublisherError::PublicationUnavailable)?;
        let entries = journal.entries().cloned().collect::<Vec<_>>();
        for entry in entries {
            if entry.phase == crate::record_transaction::RecordTransactionPhase::BroadcastCommitted
                && matches!(
                    entry.transaction.manifest.operation,
                    fs_protocol::FileMutationOperation::RestoreContent { .. }
                )
            {
                if entry.transaction.manifest.workspace_id != self.workspace_id {
                    return Err(RecordPublisherError::TransactionScopeMismatch);
                }
                continue;
            }
            if entry.transaction.manifest.workspace_id != self.workspace_id
                || entry.transaction.manifest.actor != self.actor
            {
                return Err(RecordPublisherError::TransactionScopeMismatch);
            }
            let authorizer = LocalTransactionAuthorizer {
                actor: self.actor,
                records: entry
                    .transaction
                    .manifest
                    .participants
                    .iter()
                    .map(|participant| participant.record_id)
                    .collect(),
            };
            crate::transaction_coordinator::complete_replacement_transaction(
                &self.repository,
                journal,
                history,
                namespace,
                namespace_path,
                &entry,
                &authorizer,
                &mut broadcast,
            )?;
        }
        Ok(())
    }

    #[must_use]
    pub fn workspace_id(&self) -> Uuid {
        self.workspace_id
    }

    /// Return the narrowly scoped local authorizer used for this record's
    /// repository operations.
    #[must_use]
    pub fn local_authorizer(&self, record_id: FileRecordId) -> LocalOwnerAuthorizer {
        LocalOwnerAuthorizer {
            actor: self.actor,
            record_id,
        }
    }

    /// Verified converged state for a locally owned record, used to rebuild
    /// the non-Automerge signed content projection after restart/cutover.
    pub fn snapshot(
        &self,
        record_id: FileRecordId,
    ) -> Result<Option<RecordSnapshot>, RecordPublisherError> {
        let key = RecordRepositoryKey {
            workspace_id: self.workspace_id,
            record_id,
        };
        self.snapshot_if_present(key, &self.local_authorizer(record_id))
    }

    /// Publish the authenticated structural binding for one stable record.
    /// This is separate from content metadata so a rename never needs to
    /// rewrite or reinterpret content authority.
    pub fn publish_binding(
        &self,
        binding: &FileBindingV1,
    ) -> Result<BindingPublishStatus, RecordPublisherError> {
        let _guard = self
            .publish_lock
            .lock()
            .map_err(|_| RecordPublisherError::PublicationUnavailable)?;
        binding.validate()?;
        validate_record(binding.record_id)?;
        if binding.actor != self.actor {
            return Err(RecordPublisherError::ActorMismatch);
        }
        let key = RecordRepositoryKey {
            workspace_id: self.workspace_id,
            record_id: binding.record_id,
        };
        let authorizer = self.local_authorizer(binding.record_id);
        let current = self
            .snapshot_if_present(key, &authorizer)?
            .and_then(|snapshot| snapshot.binding);
        if current.as_ref() == Some(binding) {
            return Ok(BindingPublishStatus::Unchanged);
        }
        if current
            .as_ref()
            .is_some_and(|current| current.observed_at_ms > binding.observed_at_ms)
        {
            return Err(RecordPublisherError::StaleBinding);
        }
        self.apply_idempotent(
            key,
            RecordOperationV1::SetBinding(binding.clone()),
            &authorizer,
        )?;
        Ok(BindingPublishStatus::Published)
    }

    /// Publish one immutable structural history node for records, such as
    /// directories, which have no content metadata projection.
    pub fn publish_structural_version(
        &self,
        version: &FileVersionV1,
    ) -> Result<bool, RecordPublisherError> {
        let _guard = self
            .publish_lock
            .lock()
            .map_err(|_| RecordPublisherError::PublicationUnavailable)?;
        version.validate()?;
        validate_record(version.record_id)?;
        if version.actor != self.actor {
            return Err(RecordPublisherError::ActorMismatch);
        }
        let key = RecordRepositoryKey {
            workspace_id: self.workspace_id,
            record_id: version.record_id,
        };
        self.apply_idempotent(
            key,
            RecordOperationV1::AppendVersion(version.clone()),
            &self.local_authorizer(version.record_id),
        )
    }

    /// Record which agent caused this publisher's own `version_id` of
    /// `record_id`, as the session authenticated it: a signed operation of
    /// its own, so peers learn it from the same device that signed the
    /// version. Republishing the same attribution is a no-op.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid attribution, a version this actor did
    /// not sign, a different attribution already recorded, or a repository
    /// failure.
    pub fn attribute(
        &self,
        record_id: FileRecordId,
        version_id: FileVersionId,
        attribution: &fs_protocol::FileAttributionV1,
    ) -> Result<bool, RecordPublisherError> {
        let _guard = self
            .publish_lock
            .lock()
            .map_err(|_| RecordPublisherError::PublicationUnavailable)?;
        attribution.validate()?;
        validate_record(record_id)?;
        let key = RecordRepositoryKey {
            workspace_id: self.workspace_id,
            record_id,
        };
        self.apply_idempotent(
            key,
            RecordOperationV1::AttributeVersion(fs_protocol::FileVersionAttributionV1 {
                version_id,
                attribution: attribution.clone(),
            }),
            &self.local_authorizer(record_id),
        )
    }

    /// Publish missing immutable versions followed by changed metadata.
    /// Completed prefixes are safe to retry after a crash or restart.
    pub fn publish(
        &self,
        metadata: &FileMetadataV1,
        versions: &[FileVersionV1],
    ) -> Result<PublishReport, RecordPublisherError> {
        self.publish_state(metadata, versions, &[])
    }

    /// Publish one deterministic open conflict when the replicated version
    /// DAG has multiple live heads. The operation is signed by this certified
    /// local device; deterministic identity makes concurrent detectors
    /// idempotent across replicas.
    pub fn publish_concurrent_head_conflict(
        &self,
        record_id: FileRecordId,
    ) -> Result<bool, RecordPublisherError> {
        const DOMAIN: &[u8] = b"tilo.fs.record.concurrent-head-conflict.v1\0";

        #[cfg(test)]
        self.repository.note_concurrent_head_check();
        let _guard = self
            .publish_lock
            .lock()
            .map_err(|_| RecordPublisherError::PublicationUnavailable)?;
        validate_record(record_id)?;
        let key = RecordRepositoryKey {
            workspace_id: self.workspace_id,
            record_id,
        };
        let authorizer = self.local_authorizer(record_id);
        let Some(snapshot) = self.snapshot_if_present(key, &authorizer)? else {
            return Ok(false);
        };
        let parents = snapshot
            .versions
            .iter()
            .flat_map(|version| version.parents.iter().copied())
            .collect::<std::collections::BTreeSet<_>>();
        let mut heads = snapshot
            .versions
            .iter()
            .map(|version| version.id)
            .filter(|version| !parents.contains(version))
            .collect::<Vec<_>>();
        heads.sort();
        heads.dedup();
        if heads.len() < 2 {
            return Ok(false);
        }
        if snapshot.conflicts.iter().any(|conflict| {
            let mut versions = conflict.versions.clone();
            versions.sort();
            versions == heads
        }) {
            return Ok(false);
        }
        let mut digest = blake3::Hasher::new();
        digest.update(DOMAIN);
        digest.update(record_id.0.as_bytes());
        for version in &heads {
            digest.update(version.0.as_bytes());
        }
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest.finalize().as_bytes()[..16]);
        bytes[6] = (bytes[6] & 0x0f) | 0x50;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let conflict = FileConflictV1 {
            id: uuid::Uuid::from_bytes(bytes),
            record_id,
            versions: heads,
            status: ConflictStatus::Open,
            resolution: None,
        };
        self.apply_idempotent(
            key,
            RecordOperationV1::AppendConflict(conflict),
            &authorizer,
        )
    }

    /// Publish the complete bounded record state, including first-class
    /// conflicts. Resolved conflicts are emitted as an open-conflict append
    /// followed by a separately signed resolution transition.
    ///
    /// # Errors
    ///
    /// Returns an error if state is invalid, stale, inconsistent, too large,
    /// or cannot be durably published.
    // Validation and publication are deliberately serialized as one ordered,
    // retry-safe state transition.
    #[allow(
        clippy::too_many_lines,
        reason = "One publication lock covers validation and ordered version, content-revision, conflict, and metadata updates for retry safety."
    )]
    pub fn publish_state(
        &self,
        metadata: &FileMetadataV1,
        versions: &[FileVersionV1],
        conflicts: &[FileConflictV1],
    ) -> Result<PublishReport, RecordPublisherError> {
        let _guard = self
            .publish_lock
            .lock()
            .map_err(|_| RecordPublisherError::PublicationUnavailable)?;
        metadata.validate()?;
        let record_id = metadata.record_id;
        validate_record(record_id)?;
        if metadata.latest_actor != Some(self.actor) {
            return Err(RecordPublisherError::ActorMismatch);
        }
        if versions.len() > self.repository.max_record_versions() {
            return Err(RecordPublisherError::HistoryTooLarge);
        }
        if conflicts.len() > MAX_RECORD_CONFLICTS {
            return Err(RecordPublisherError::ConflictHistoryTooLarge);
        }
        let mut requested_versions = HashMap::new();
        for version in versions {
            version.validate()?;
            if version.record_id != record_id {
                return Err(RecordPublisherError::RecordMismatch);
            }
            if version.actor != self.actor {
                return Err(RecordPublisherError::ActorMismatch);
            }
            if let Some(existing) = requested_versions.insert(version.id, version)
                && existing != version
            {
                return Err(RecordPublisherError::VersionCollision(version.id));
            }
        }
        let mut requested_conflicts = HashMap::new();
        for conflict in conflicts {
            conflict.validate()?;
            if conflict.record_id != record_id {
                return Err(RecordPublisherError::RecordMismatch);
            }
            if let Some(existing) = requested_conflicts.insert(conflict.id, conflict)
                && existing != conflict
            {
                return Err(RecordPublisherError::ConflictCollision(conflict.id));
            }
        }

        let key = RecordRepositoryKey {
            workspace_id: self.workspace_id,
            record_id,
        };
        let authorizer = self.local_authorizer(record_id);
        let initial = self.snapshot_if_present(key, &authorizer)?;
        let current_metadata = initial
            .as_ref()
            .and_then(|snapshot| snapshot.metadata.as_ref());
        if current_metadata != Some(metadata)
            && current_metadata
                .is_some_and(|current| current.observed_at_ms > metadata.observed_at_ms)
        {
            // A refused namespace tuple must not publish its content first:
            // that would leave accepted metadata attached to a different edit.
            return Err(RecordPublisherError::StaleMetadata);
        }
        let mut existing_versions: HashMap<_, _> = initial
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .versions
                    .iter()
                    .map(|version| (version.id, version.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let mut report = PublishReport {
            metadata: MetadataPublishStatus::Unchanged,
            published_versions: 0,
            skipped_versions: 0,
            published_conflicts: 0,
            skipped_conflicts: 0,
        };
        // Every operation this publication still needs, in the order they
        // were applied one by one before: versions, the content revision,
        // conflicts and their resolutions, metadata. They are signed with
        // consecutive sequences and committed as one batch: one durable
        // write of the record instead of one per operation.
        let mut pending: Vec<(Pending, RecordOperationV1)> = Vec::new();
        for version in versions {
            match existing_versions.get(&version.id) {
                Some(existing) if existing == version => {
                    report.skipped_versions = report.skipped_versions.saturating_add(1);
                    continue;
                }
                Some(_) => return Err(RecordPublisherError::VersionCollision(version.id)),
                None => {}
            }
            pending.push((
                Pending::Version,
                RecordOperationV1::AppendVersion(version.clone()),
            ));
            existing_versions.insert(version.id, version.clone());
        }

        // The current namespace tuple is a separately signed operation so its
        // byte length is authenticated rather than merely associated with a
        // signed version UUID. Select only causal tips with matching content;
        // timestamp/id orders incomparable tips, never an edit and its parent.
        // The signed IDs and times remain unchanged across retries/restarts.
        let matching_versions: Vec<_> = versions
            .iter()
            .filter(|version| version.content_hash == metadata.content_hash)
            .collect();
        let mut ancestors = std::collections::HashSet::new();
        let mut pending_parents: Vec<_> = matching_versions
            .iter()
            .flat_map(|version| version.parents.iter().copied())
            .collect();
        while let Some(parent) = pending_parents.pop() {
            if ancestors.insert(parent)
                && let Some(version) = existing_versions.get(&parent)
            {
                pending_parents.extend(version.parents.iter().copied());
            }
        }
        if let Some(current_version) = matching_versions
            .into_iter()
            .filter(|version| !ancestors.contains(&version.id))
            .max_by_key(|version| (version.timestamp_ms, version.id))
        {
            let revision = ContentRevisionV1 {
                schema_version: CONTENT_REVISION_VERSION,
                record_id,
                version_id: current_version.id,
                content_hash: current_version.content_hash,
                byte_len: metadata.byte_len,
                metadata_hash: current_version.metadata_hash,
            };
            let current_revision = initial
                .as_ref()
                .and_then(|snapshot| snapshot.content_revision);
            if current_revision != Some(revision) {
                pending.push((
                    Pending::Revision,
                    RecordOperationV1::SetContentRevision(revision),
                ));
            }
        }

        for conflict in conflicts {
            if conflict
                .versions
                .iter()
                .any(|version| !existing_versions.contains_key(version))
            {
                return Err(RecordPublisherError::ConflictReferencesUnknownVersion);
            }
            let mut open = conflict.clone();
            open.status = ConflictStatus::Open;
            open.resolution = None;
            let append = RecordOperationV1::AppendConflict(open);
            if operation_is_present(&append, initial.as_ref())? {
                report.skipped_conflicts = report.skipped_conflicts.saturating_add(1);
            } else {
                pending.push((Pending::Conflict, append));
            }
            if let Some(resolution) = &conflict.resolution {
                let resolve = RecordOperationV1::ResolveConflict {
                    conflict_id: conflict.id,
                    resolution: resolution.clone(),
                };
                if operation_is_present(&resolve, initial.as_ref())? {
                    report.skipped_conflicts = report.skipped_conflicts.saturating_add(1);
                } else {
                    pending.push((Pending::Conflict, resolve));
                }
            }
        }

        if current_metadata != Some(metadata) {
            pending.push((
                Pending::Metadata,
                RecordOperationV1::SetMetadata(metadata.clone()),
            ));
        }
        let applied = self.apply_all(key, &pending, &authorizer)?;
        for (kind, published) in applied {
            match (kind, published) {
                (Pending::Version, true) => {
                    report.published_versions = report.published_versions.saturating_add(1);
                }
                (Pending::Version, false) => {
                    report.skipped_versions = report.skipped_versions.saturating_add(1);
                }
                (Pending::Conflict, true) => {
                    report.published_conflicts = report.published_conflicts.saturating_add(1);
                }
                (Pending::Conflict, false) => {
                    report.skipped_conflicts = report.skipped_conflicts.saturating_add(1);
                }
                (Pending::Metadata, true) => report.metadata = MetadataPublishStatus::Published,
                (Pending::Revision | Pending::Metadata, _) => {}
            }
        }
        Ok(report)
    }

    /// Signs `operations` with consecutive sequences and commits them as one
    /// batch. When another publisher took a sequence meanwhile, each is
    /// applied on its own instead, as [`Self::apply_idempotent`] does.
    /// Returns, per operation, whether this call published it.
    fn apply_all(
        &self,
        key: RecordRepositoryKey,
        operations: &[(Pending, RecordOperationV1)],
        authorizer: &LocalOwnerAuthorizer,
    ) -> Result<Vec<(Pending, bool)>, RecordPublisherError> {
        let mut applied = Vec::with_capacity(operations.len());
        for chunk in operations.chunks(MAX_RECORD_BATCH_OPERATIONS) {
            let first = self
                .repository
                .highest_sequence(key, &self.actor)
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(RecordPublisherError::SequenceExhausted)?;
            let mut envelopes = Vec::with_capacity(chunk.len());
            for (offset, (_, operation)) in chunk.iter().enumerate() {
                let sequence = first
                    .checked_add(offset as u64)
                    .ok_or(RecordPublisherError::SequenceExhausted)?;
                envelopes.push(RecordEnvelopeV1::signed(
                    key.record_id,
                    self.actor,
                    sequence,
                    operation.clone(),
                    &self.signing_key,
                )?);
            }
            match self.repository.apply_own_batch(key, &envelopes, authorizer) {
                Ok(()) => applied.extend(chunk.iter().map(|(kind, _)| (*kind, true))),
                Err(RecordRepositoryError::Replication(RecordReplicationError::Replay {
                    ..
                })) => {
                    for (kind, operation) in chunk {
                        let published =
                            self.apply_idempotent(key, operation.clone(), authorizer)?;
                        applied.push((*kind, published));
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(applied)
    }

    fn snapshot_if_present(
        &self,
        key: RecordRepositoryKey,
        authorizer: &LocalOwnerAuthorizer,
    ) -> Result<Option<RecordSnapshot>, RecordPublisherError> {
        if !self.repository.contains(key) {
            return Ok(None);
        }
        Ok(Some(self.repository.snapshot(
            key,
            &self.actor,
            authorizer,
        )?))
    }

    /// Returns `true` if this call appended the operation and `false` if a
    /// competing publisher already made the exact state visible.
    // Ownership freezes the requested operation while bounded retries clone it
    // into independently signed envelopes.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "The owned operation is the retry snapshot cloned into independently signed envelopes at each candidate sequence."
    )]
    fn apply_idempotent(
        &self,
        key: RecordRepositoryKey,
        operation: RecordOperationV1,
        authorizer: &LocalOwnerAuthorizer,
    ) -> Result<bool, RecordPublisherError> {
        for _ in 0..MAX_SEQUENCE_RETRIES {
            if operation_is_present(
                &operation,
                self.snapshot_if_present(key, authorizer)?.as_ref(),
            )? {
                return Ok(false);
            }
            let next = self
                .repository
                .highest_sequence(key, &self.actor)
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(RecordPublisherError::SequenceExhausted)?;
            let envelope = RecordEnvelopeV1::signed(
                key.record_id,
                self.actor,
                next,
                operation.clone(),
                &self.signing_key,
            )?;
            match self
                .repository
                .apply_own_batch(key, std::slice::from_ref(&envelope), authorizer)
            {
                Ok(()) => return Ok(true),
                Err(RecordRepositoryError::Replication(RecordReplicationError::Replay {
                    ..
                })) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(RecordPublisherError::SequenceContention)
    }
}

fn operation_is_present(
    operation: &RecordOperationV1,
    snapshot: Option<&RecordSnapshot>,
) -> Result<bool, RecordPublisherError> {
    let Some(snapshot) = snapshot else {
        return Ok(false);
    };
    match operation {
        RecordOperationV1::SetBinding(binding) => Ok(snapshot.binding.as_ref() == Some(binding)),
        RecordOperationV1::SetMetadata(metadata) => {
            Ok(snapshot.metadata.as_ref() == Some(metadata))
        }
        RecordOperationV1::AppendVersion(version) => {
            match snapshot
                .versions
                .iter()
                .find(|current| current.id == version.id)
            {
                Some(current) if current == version => Ok(true),
                Some(_) => Err(RecordPublisherError::VersionCollision(version.id)),
                None => Ok(false),
            }
        }
        RecordOperationV1::SetContentRevision(revision) => {
            Ok(snapshot.content_revision.as_ref() == Some(revision))
        }
        RecordOperationV1::AppendConflict(conflict) => {
            match snapshot
                .conflicts
                .iter()
                .find(|current| current.id == conflict.id)
            {
                Some(current)
                    if current == conflict
                        || (current.status == ConflictStatus::Resolved
                            && current.record_id == conflict.record_id
                            && current.versions == conflict.versions) =>
                {
                    Ok(true)
                }
                Some(_) => Err(RecordPublisherError::ConflictCollision(conflict.id)),
                None => Ok(false),
            }
        }
        RecordOperationV1::ResolveConflict {
            conflict_id,
            resolution,
        } => match snapshot
            .conflicts
            .iter()
            .find(|current| current.id == *conflict_id)
        {
            Some(current) if current.resolution.as_ref() == Some(resolution) => Ok(true),
            Some(current)
                if current.resolution.as_ref().is_some_and(|existing| {
                    !fs_protocol::prefer_conflict_resolution(resolution, existing)
                }) =>
            {
                Ok(true)
            }
            _ => Ok(false),
        },
        RecordOperationV1::PrepareTransaction(_) | RecordOperationV1::CommitTransaction { .. } => {
            Ok(false)
        }
        RecordOperationV1::AttributeVersion(attributed) => {
            match snapshot
                .attributions
                .iter()
                .find(|current| current.version_id == attributed.version_id)
            {
                Some(current) if current == attributed => Ok(true),
                Some(_) => Err(RecordPublisherError::VersionCollision(
                    attributed.version_id,
                )),
                None => Ok(false),
            }
        }
    }
}

/// What a pending operation of a publication is, for its report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Version,
    Revision,
    Conflict,
    Metadata,
}

fn validate_record(record_id: FileRecordId) -> Result<(), RecordPublisherError> {
    if record_id.0.is_nil() {
        return Err(RecordPublisherError::InvalidRecord);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataPublishStatus {
    Published,
    Unchanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingPublishStatus {
    Published,
    Unchanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishReport {
    pub metadata: MetadataPublishStatus,
    pub published_versions: usize,
    pub skipped_versions: usize,
    pub published_conflicts: usize,
    pub skipped_conflicts: usize,
}

#[derive(Debug, Error)]
pub enum RecordPublisherError {
    #[error("workspace id must be non-nil")]
    InvalidWorkspace,
    #[error("record id must be non-nil")]
    InvalidRecord,
    #[error("local human root key is not valid Ed25519")]
    InvalidPersonKey,
    #[error("signing key does not match actor.device")]
    WrongDeviceKey,
    #[error("metadata/version actor does not match the certified local actor")]
    ActorMismatch,
    #[error("history contains a version for another record")]
    RecordMismatch,
    #[error("history exceeds the repository snapshot bound")]
    HistoryTooLarge,
    #[error("conflict history exceeds the repository snapshot bound")]
    ConflictHistoryTooLarge,
    #[error("immutable version id {0:?} names different content")]
    VersionCollision(FileVersionId),
    #[error("conflict id {0} names different conflict records")]
    ConflictCollision(Uuid),
    #[error("conflict references an immutable version that was not published")]
    ConflictReferencesUnknownVersion,
    #[error("metadata observation predates the current durable projection")]
    StaleMetadata,
    #[error("structural binding observation predates the current durable projection")]
    StaleBinding,
    #[error("signed content projection rejected record {0:?}")]
    ProjectionRejected(FileRecordId),
    #[error("per-actor sequence space is exhausted")]
    SequenceExhausted,
    #[error("sequence allocation remained contended after bounded retries")]
    SequenceContention,
    #[error("local publication lock is unavailable")]
    PublicationUnavailable,
    #[error("transaction workspace or actor does not match this publisher")]
    TransactionScopeMismatch,
    #[error("transaction manifest or signature is invalid")]
    InvalidTransaction,
    #[error("file metadata/version validation failed: {0}")]
    InvalidRecordPayload(#[from] FileRecordError),
    #[error("record envelope signing failed: {0}")]
    Signing(#[from] CanonicalError),
    #[error("record repository rejected publication: {0}")]
    Repository(#[from] RecordRepositoryError),
    #[error("transaction coordinator rejected publication: {0}")]
    TransactionCoordinator(
        #[from] crate::transaction_coordinator::RecordTransactionCoordinatorError,
    ),
}

impl RecordPublisherError {
    /// Whether the record (or repository) has reached a size bound, so the
    /// same publication will keep failing however often it is retried.
    #[must_use]
    pub const fn is_capacity_exhausted(&self) -> bool {
        matches!(
            self,
            Self::HistoryTooLarge
                | Self::ConflictHistoryTooLarge
                | Self::SequenceExhausted
                | Self::Repository(
                    RecordRepositoryError::TooManyVersions
                        | RecordRepositoryError::TooManyOperations
                        | RecordRepositoryError::TooManyConflicts
                        | RecordRepositoryError::TooManyRecords
                )
        )
    }
}

#[cfg(test)]
mod tests {
    use collab_types::generate_keypair;
    use fs_protocol::{
        ConflictResolution, ConflictStatus, ConflictStrategy, ContentHash, FileConflictV1,
        FileProvenance, IndexStatus,
    };

    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        root: std::path::PathBuf,
        repository: Arc<RecordRepository>,
        workspace_id: Uuid,
        actor: FileActor,
        device_key: ed25519_dalek::SigningKey,
        record_id: FileRecordId,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("records");
            let repository = Arc::new(RecordRepository::open(&root).unwrap());
            let (device_key, device) = generate_keypair();
            let (_, person) = generate_keypair();
            Self {
                _dir: dir,
                root,
                repository,
                workspace_id: Uuid::from_u128(101),
                actor: FileActor { person, device },
                device_key,
                record_id: FileRecordId(Uuid::from_u128(102)),
            }
        }

        fn publisher(&self) -> RecordPublisher {
            RecordPublisher::new(
                self.repository.clone(),
                self.workspace_id,
                self.actor,
                self.device_key.clone(),
            )
            .unwrap()
        }

        fn metadata(&self, revision: u8) -> FileMetadataV1 {
            FileMetadataV1 {
                schema_version: 1,
                record_id: self.record_id,
                content_hash: Some(ContentHash([revision; 32])),
                byte_len: u64::from(revision),
                mime: Some("text/plain".into()),
                previews: Vec::new(),
                latest_actor: Some(self.actor),
                observed_at_ms: u64::from(revision),
                index_status: IndexStatus::Indexed,
                properties: None,
            }
        }

        fn version(&self, id: FileVersionId, revision: u8) -> FileVersionV1 {
            FileVersionV1 {
                id,
                record_id: self.record_id,
                parents: Vec::new(),
                actor: self.actor,
                timestamp_ms: u64::from(revision),
                content_hash: Some(ContentHash([revision; 32])),
                metadata_hash: ContentHash([revision.wrapping_add(1); 32]),
                provenance: FileProvenance::Edit,
            }
        }

        fn key(&self) -> RecordRepositoryKey {
            RecordRepositoryKey {
                workspace_id: self.workspace_id,
                record_id: self.record_id,
            }
        }
    }

    #[test]
    fn equal_time_native_edit_keeps_signed_revision_with_current_metadata() {
        let fixture = Fixture::new();
        let publisher = fixture.publisher();
        let first = fixture.version(FileVersionId(Uuid::from_u128(2)), 1);
        let mut second = fixture.version(FileVersionId(Uuid::from_u128(1)), 2);
        second.parents = vec![first.id];
        second.timestamp_ms = first.timestamp_ms;
        publisher
            .publish(&fixture.metadata(1), std::slice::from_ref(&first))
            .unwrap();
        let mut metadata = fixture.metadata(2);
        metadata.observed_at_ms = first.timestamp_ms;
        publisher
            .publish(&metadata, &[first, second.clone()])
            .unwrap();
        let snapshot = fixture
            .repository
            .snapshot(
                fixture.key(),
                &fixture.actor,
                &publisher.local_authorizer(fixture.record_id),
            )
            .unwrap();
        assert_eq!(snapshot.metadata, Some(metadata));
        assert_eq!(snapshot.content_revision.unwrap().version_id, second.id);
    }

    #[test]
    fn repeated_content_keeps_the_signed_descendant_identity() {
        let fixture = Fixture::new();
        let publisher = fixture.publisher();
        let first = fixture.version(FileVersionId(Uuid::from_u128(2)), 1);
        let mut second = fixture.version(FileVersionId(Uuid::from_u128(1)), 2);
        second.parents = vec![first.id];
        second.timestamp_ms = first.timestamp_ms;
        second.content_hash = first.content_hash;
        let metadata = fixture.metadata(1);
        publisher
            .publish(&metadata, std::slice::from_ref(&first))
            .unwrap();
        publisher
            .publish(&metadata, &[first, second.clone()])
            .unwrap();
        let snapshot = fixture
            .repository
            .snapshot(
                fixture.key(),
                &fixture.actor,
                &publisher.local_authorizer(fixture.record_id),
            )
            .unwrap();
        assert_eq!(snapshot.metadata, Some(metadata));
        assert_eq!(snapshot.content_revision.unwrap().version_id, second.id);
        let key = fixture.key();
        let actor = fixture.actor;
        let authorizer = publisher.local_authorizer(fixture.record_id);
        drop(publisher);
        let Fixture {
            _dir,
            root,
            repository,
            ..
        } = fixture;
        drop(repository);
        let reopened = RecordRepository::open(root).unwrap();
        assert_eq!(
            reopened.snapshot(key, &actor, &authorizer).unwrap(),
            snapshot
        );
    }

    #[test]
    fn stale_metadata_refuses_before_signed_content_changes() {
        let fixture = Fixture::new();
        let publisher = fixture.publisher();
        let first = fixture.version(FileVersionId(Uuid::from_u128(2)), 2);
        publisher
            .publish(&fixture.metadata(2), std::slice::from_ref(&first))
            .unwrap();
        let before = publisher.snapshot(fixture.record_id).unwrap().unwrap();
        let mut stale = fixture.version(FileVersionId(Uuid::from_u128(1)), 1);
        stale.parents = vec![first.id];
        assert!(matches!(
            publisher.publish(&fixture.metadata(1), &[first, stale]),
            Err(RecordPublisherError::StaleMetadata)
        ));
        assert_eq!(publisher.snapshot(fixture.record_id).unwrap(), Some(before));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn publisher_seeds_and_resumes_only_deltas_for_the_certified_device() {
        {
            let fixture = Fixture::new();
            let publisher = fixture.publisher();
            let first = fixture.version(FileVersionId::new(), 1);
            let mut second = fixture.version(FileVersionId::new(), 2);
            second.parents = vec![first.id];
            let versions = [first, second];
            let report = publisher.publish(&fixture.metadata(2), &versions).unwrap();
            assert_eq!(report.metadata, MetadataPublishStatus::Published);
            assert_eq!(report.published_versions, 2);
            assert_eq!(report.skipped_versions, 0);
            let snapshot = fixture
                .repository
                .snapshot(
                    fixture.key(),
                    &fixture.actor,
                    &publisher.local_authorizer(fixture.record_id),
                )
                .unwrap();
            assert_eq!(snapshot.metadata, Some(fixture.metadata(2)));
            assert_eq!(snapshot.versions.len(), 2);
            assert_eq!(
                snapshot.content_revision.unwrap().version_id,
                versions[1].id
            );
            assert_eq!(
                fixture
                    .repository
                    .highest_sequence(fixture.key(), &fixture.actor),
                Some(4)
            );
        }
        {
            let fixture = Fixture::new();
            let publisher = fixture.publisher();
            let first = fixture.version(FileVersionId::new(), 1);
            let metadata = fixture.metadata(1);
            publisher
                .publish(&metadata, std::slice::from_ref(&first))
                .unwrap();
            let sequence = fixture
                .repository
                .highest_sequence(fixture.key(), &fixture.actor);
            let unchanged = publisher
                .publish(&metadata, std::slice::from_ref(&first))
                .unwrap();
            assert_eq!(unchanged.metadata, MetadataPublishStatus::Unchanged);
            assert_eq!(unchanged.published_versions, 0);
            assert_eq!(unchanged.skipped_versions, 1);
            assert_eq!(
                fixture
                    .repository
                    .highest_sequence(fixture.key(), &fixture.actor),
                sequence
            );

            let mut second = fixture.version(FileVersionId::new(), 2);
            second.parents = vec![first.id];
            let updated = publisher
                .publish(&fixture.metadata(2), &[first, second])
                .unwrap();
            assert_eq!(updated.metadata, MetadataPublishStatus::Published);
            assert_eq!(updated.published_versions, 1);
            assert_eq!(updated.skipped_versions, 1);
        }
        {
            let fixture = Fixture::new();
            let first = fixture.version(FileVersionId::new(), 1);
            fixture
                .publisher()
                .publish(&fixture.metadata(1), std::slice::from_ref(&first))
                .unwrap();
            let before = fixture
                .repository
                .highest_sequence(fixture.key(), &fixture.actor)
                .unwrap();
            drop(fixture.repository.clone());
            let reopened = Arc::new(RecordRepository::open(&fixture.root).unwrap());
            let publisher = RecordPublisher::new(
                reopened.clone(),
                fixture.workspace_id,
                fixture.actor,
                fixture.device_key.clone(),
            )
            .unwrap();
            let skipped = publisher
                .publish(&fixture.metadata(1), std::slice::from_ref(&first))
                .unwrap();
            assert_eq!(skipped.metadata, MetadataPublishStatus::Unchanged);
            assert_eq!(skipped.skipped_versions, 1);
            assert_eq!(
                reopened.highest_sequence(fixture.key(), &fixture.actor),
                Some(before)
            );
            let mut second = fixture.version(FileVersionId::new(), 2);
            second.parents = vec![first.id];
            publisher
                .publish(&fixture.metadata(2), &[first, second])
                .unwrap();
            assert_eq!(
                reopened.highest_sequence(fixture.key(), &fixture.actor),
                Some(before + 3)
            );
        }
        {
            let fixture = Fixture::new();
            let (wrong, _) = generate_keypair();
            assert!(matches!(
                RecordPublisher::new(
                    fixture.repository.clone(),
                    fixture.workspace_id,
                    fixture.actor,
                    wrong
                ),
                Err(RecordPublisherError::WrongDeviceKey)
            ));
            assert!(fixture.repository.is_empty());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn publisher_signs_bindings_and_conflicts_deterministically_and_durably() {
        {
            let fixture = Fixture::new();
            let publisher = fixture.publisher();
            let base = fixture.version(FileVersionId::new(), 1);
            let mut left = fixture.version(FileVersionId::new(), 2);
            left.parents = vec![base.id];
            publisher
                .publish(&fixture.metadata(2), &[base.clone(), left.clone()])
                .unwrap();
            let mut right = fixture.version(FileVersionId::new(), 3);
            right.parents = vec![base.id];
            publisher.publish_structural_version(&right).unwrap();

            assert!(
                publisher
                    .publish_concurrent_head_conflict(fixture.record_id)
                    .unwrap()
            );
            assert!(
                !fixture
                    .publisher()
                    .publish_concurrent_head_conflict(fixture.record_id)
                    .unwrap(),
                "restart/duplicate synthesis must be idempotent"
            );
            let snapshot = publisher.snapshot(fixture.record_id).unwrap().unwrap();
            assert_eq!(snapshot.conflicts.len(), 1);
            let conflict = &snapshot.conflicts[0];
            assert_eq!(conflict.status, ConflictStatus::Open);
            let mut expected = vec![left.id, right.id];
            expected.sort();
            assert_eq!(conflict.versions, expected);
        }
        {
            let fixture = Fixture::new();
            let publisher = fixture.publisher();
            let binding = FileBindingV1 {
                schema_version: fs_protocol::FILE_BINDING_VERSION,
                record_id: fixture.record_id,
                parent: Some(FileRecordId::new()),
                name: "first.txt".into(),
                kind: fs_protocol::FileNodeKind::File,
                tombstoned: false,
                actor: fixture.actor,
                observed_at_ms: 10,
            };
            assert_eq!(
                publisher.publish_binding(&binding).unwrap(),
                BindingPublishStatus::Published
            );
            let sequence = fixture
                .repository
                .highest_sequence(fixture.key(), &fixture.actor);
            assert_eq!(
                publisher.publish_binding(&binding).unwrap(),
                BindingPublishStatus::Unchanged
            );
            assert_eq!(
                fixture
                    .repository
                    .highest_sequence(fixture.key(), &fixture.actor),
                sequence
            );
            let stale = FileBindingV1 {
                name: "stale.txt".into(),
                observed_at_ms: 9,
                ..binding
            };
            assert!(matches!(
                publisher.publish_binding(&stale),
                Err(RecordPublisherError::StaleBinding)
            ));
        }
        {
            let fixture = Fixture::new();
            let first = fixture.version(FileVersionId::new(), 1);
            let mut second = fixture.version(FileVersionId::new(), 2);
            second.parents = vec![first.id];
            let result = FileVersionV1 {
                id: FileVersionId::new(),
                record_id: first.record_id,
                parents: vec![first.id, second.id],
                actor: first.actor,
                timestamp_ms: 3,
                content_hash: Some(ContentHash([3; 32])),
                metadata_hash: ContentHash([4; 32]),
                provenance: FileProvenance::ConflictResolution,
            };
            let resolution = ConflictResolution {
                strategy: ConflictStrategy::Merge,
                result: result.id,
                actor: fixture.actor,
                timestamp_ms: result.timestamp_ms,
            };
            let conflict = FileConflictV1 {
                id: Uuid::new_v4(),
                record_id: fixture.record_id,
                versions: vec![first.id, second.id],
                status: ConflictStatus::Resolved,
                resolution: Some(resolution.clone()),
            };
            let versions = vec![first, second, result];
            let first_report = fixture
                .publisher()
                .publish_state(
                    &fixture.metadata(3),
                    &versions,
                    std::slice::from_ref(&conflict),
                )
                .unwrap();
            assert_eq!(first_report.published_conflicts, 2);

            let reopened = Arc::new(RecordRepository::open(&fixture.root).unwrap());
            let publisher = RecordPublisher::new(
                reopened.clone(),
                fixture.workspace_id,
                fixture.actor,
                fixture.device_key.clone(),
            )
            .unwrap();
            let snapshot = reopened
                .snapshot(
                    fixture.key(),
                    &fixture.actor,
                    &publisher.local_authorizer(fixture.record_id),
                )
                .unwrap();
            assert_eq!(snapshot.conflicts, vec![conflict.clone()]);
            assert_eq!(snapshot.conflicts[0].resolution, Some(resolution));

            let sequence = reopened
                .highest_sequence(fixture.key(), &fixture.actor)
                .unwrap();
            let repeated = publisher
                .publish_state(&fixture.metadata(3), &versions, &[conflict])
                .unwrap();
            assert_eq!(repeated.published_conflicts, 0);
            assert_eq!(repeated.skipped_conflicts, 2);
            assert_eq!(
                reopened.highest_sequence(fixture.key(), &fixture.actor),
                Some(sequence)
            );
        }
    }
}
