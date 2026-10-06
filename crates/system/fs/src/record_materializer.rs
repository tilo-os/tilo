//! Exact-record projection from accepted replication envelopes into the live
//! namespace and immutable history store.
//!
//! Materialization never resolves a path or creates a node. Stable record
//! identity is looked up only in the live namespace, so rename is transparent
//! and tombstoned or unknown records fail closed.

use std::sync::{Arc, Mutex};

use collab_types::FileRecordId;
use thiserror::Error;
use uuid::Uuid;

use crate::history::{FileHistoryStore, HistoryError};
use crate::namespace::{ContentAuthority, Namespace, NodeKind};
use crate::record_access::RecordAccessBinding;
use crate::record_protocol::{RecordMutationSink, RecordMutationSinkError};
use crate::record_replication::{RecordEnvelopeV1, RecordOperationV1, RecordSnapshot};
use crate::record_repository::{RecordRepository, RecordRepositoryError};
use crate::record_transaction::RecordTransactionJournalError;
use crate::transaction_coordinator::RecordTransactionCoordinatorError;

#[derive(Clone)]
struct RemoteTransactionRuntime {
    journal: Arc<Mutex<crate::record_transaction::RecordTransactionJournal>>,
    namespace_path: std::path::PathBuf,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecordReconciliationReport {
    pub records_reconciled: usize,
    pub operations_replayed: usize,
    pub unknown_or_tombstoned_records: usize,
    pub transactions_reconciled: usize,
}

/// Projects accepted mutations for exactly one workspace.
#[derive(Clone)]
pub struct RecordMaterializer {
    workspace_id: Uuid,
    namespace: Arc<Mutex<Namespace>>,
    history: Option<Arc<FileHistoryStore>>,
    sync: Arc<std::sync::RwLock<Option<Arc<crate::sync::SyncHandle>>>>,
    transaction_runtime: Option<RemoteTransactionRuntime>,
    /// Records whose last projection found no live placement (not placed
    /// yet, or deleted): what a namespace change may newly place.
    unplaced: Arc<Mutex<std::collections::BTreeSet<crate::record_repository::RecordRepositoryKey>>>,
}

/// What [`RecordMaterializer::reconcile_batch`] projects besides the
/// records it is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileScope {
    /// Only those records.
    Touched,
    /// Also every record that had no placement so far (after the namespace
    /// changed).
    Unplaced,
    /// Every record (after a restart, which may have stopped between a
    /// durable record and its projection).
    Everything,
}

impl std::fmt::Debug for RecordMaterializer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecordMaterializer")
            .field("workspace_id", &self.workspace_id)
            .field("history_enabled", &self.history.is_some())
            .finish_non_exhaustive()
    }
}

impl RecordMaterializer {
    pub fn new(
        workspace_id: Uuid,
        namespace: Arc<Mutex<Namespace>>,
        history: Option<Arc<FileHistoryStore>>,
    ) -> Self {
        Self {
            workspace_id,
            namespace,
            history,
            sync: Arc::new(std::sync::RwLock::new(None)),
            transaction_runtime: None,
            unplaced: Arc::new(Mutex::new(std::collections::BTreeSet::new())),
        }
    }

    /// Use the workspace transaction journal for remote participant arrivals,
    /// giving them the same durable history/namespace/broadcast recovery
    /// boundary as locally authored replacement renames.
    #[must_use]
    pub fn with_transaction_recovery(
        mut self,
        journal: Arc<Mutex<crate::record_transaction::RecordTransactionJournal>>,
        namespace_path: std::path::PathBuf,
    ) -> Self {
        self.transaction_runtime = Some(RemoteTransactionRuntime {
            journal,
            namespace_path,
        });
        self
    }

    /// Attach the workspace broadcaster once routing has started. The sink is
    /// installed before the router, so this late binding avoids a startup
    /// cycle while still propagating accepted content updates immediately.
    pub fn attach_sync(&self, sync: Arc<crate::sync::SyncHandle>) {
        if let Ok(mut current) = self.sync.write() {
            *current = Some(sync);
        }
    }

    /// Materialize one already accepted envelope after independently checking
    /// its signature, actor, workspace, and exact live-record binding.
    ///
    /// # Errors
    ///
    /// Returns an error if the authenticated scope does not match the envelope
    /// or if the mutation cannot be projected safely.
    #[cfg(test)]
    pub fn apply(
        &self,
        scope: RecordAccessBinding,
        envelope: &RecordEnvelopeV1,
        snapshot: &RecordSnapshot,
    ) -> Result<(), RecordMaterializerError> {
        if scope.workspace_id != self.workspace_id {
            return Err(RecordMaterializerError::WrongWorkspace);
        }
        if scope.record_id != envelope.record_id
            || scope.record_id != snapshot.record_id
            || scope.person != envelope.actor.person
            || scope.device != envelope.actor.device
        {
            return Err(RecordMaterializerError::BindingMismatch);
        }
        self.apply_exact(scope.record_id, envelope, snapshot, None)
    }

    /// Projects one record's durable operations, as
    /// [`Self::reconcile_repository`] does for every record, without
    /// replaying the rest of the repository. A mesh pull that applied a
    /// batch to one record reconciles just that record; the full pass at the
    /// end of the pull still heals everything else.
    ///
    /// # Errors
    ///
    /// As [`Self::reconcile_repository`].
    pub fn reconcile_record(
        &self,
        repository: &RecordRepository,
        key: crate::record_repository::RecordRepositoryKey,
    ) -> Result<RecordReconciliationReport, RecordMaterializerError> {
        let mut report = RecordReconciliationReport::default();
        self.reconcile_key(repository, key, &mut report)?;
        Ok(report)
    }

    /// Projects the records `keys` just changed, plus what `scope` adds,
    /// with one durable history write and one namespace delta for all of
    /// them, then signals the change. The caller holds the repository's
    /// publication gate for writing, so no reader sees the records of a
    /// changeset half projected.
    ///
    /// # Errors
    ///
    /// As [`Self::reconcile_repository`].
    pub fn reconcile_batch(
        &self,
        repository: &RecordRepository,
        keys: &[crate::record_repository::RecordRepositoryKey],
        scope: ReconcileScope,
    ) -> Result<RecordReconciliationReport, RecordMaterializerError> {
        let deferral = self
            .history
            .as_ref()
            .map(|history| history.defer_durability());
        self.namespace
            .lock()
            .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?
            .begin_incremental_batch();
        let mut report = RecordReconciliationReport::default();
        let mut projected = keys
            .iter()
            .try_for_each(|key| self.reconcile_key(repository, *key, &mut report));
        if projected.is_ok() {
            let more = match scope {
                ReconcileScope::Touched => Vec::new(),
                ReconcileScope::Unplaced => self
                    .unplaced
                    .lock()
                    .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?
                    .iter()
                    .copied()
                    .collect(),
                ReconcileScope::Everything => repository.reconciliation_keys(self.workspace_id),
            };
            projected = more
                .into_iter()
                .try_for_each(|key| self.reconcile_key(repository, key, &mut report));
        }
        let everything = scope != ReconcileScope::Touched || !keys.is_empty();
        // One namespace delta for the whole batch.
        let delta = self
            .namespace
            .lock()
            .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?
            .end_incremental_batch();
        if !delta.is_empty()
            && let Ok(sync) = self.sync.read()
            && let Some(sync) = sync.as_ref()
        {
            sync.broadcast_delta(delta);
        }
        projected?;
        // A pull may complete a multi-record transaction.
        if everything {
            self.reconcile_transactions(repository, &mut report)?;
        }
        if let Some(deferral) = deferral {
            deferral
                .finish()
                .map_err(|_| RecordMaterializerError::HistoryUnavailable)?;
        }
        // A full pass only heals what an earlier change already signalled.
        if !keys.is_empty() {
            self.notify_projection();
        }
        Ok(report)
    }

    /// Tells what projects records onward (the native data plane) that the
    /// namespace's verified records changed.
    pub fn notify_projection(&self) {
        if let Ok(sync) = self.sync.read()
            && let Some(sync) = sync.as_ref()
        {
            sync.notify_record_index_changed();
        }
    }

    fn reconcile_key(
        &self,
        repository: &RecordRepository,
        key: crate::record_repository::RecordRepositoryKey,
        report: &mut RecordReconciliationReport,
    ) -> Result<(), RecordMaterializerError> {
        let unknown = report.unknown_or_tombstoned_records;
        self.reconcile_key_once(repository, key, report)?;
        if let Ok(mut unplaced) = self.unplaced.lock() {
            if report.unknown_or_tombstoned_records > unknown {
                unplaced.insert(key);
            } else {
                unplaced.remove(&key);
            }
        }
        Ok(())
    }

    fn reconcile_key_once(
        &self,
        repository: &RecordRepository,
        key: crate::record_repository::RecordRepositoryKey,
        report: &mut RecordReconciliationReport,
    ) -> Result<(), RecordMaterializerError> {
        let Some(batch) = repository.reconciliation_batch(key) else {
            return Ok(());
        };
        debug_assert_eq!(batch.key, key);

        for envelope in batch
            .envelopes
            .iter()
            .filter(|envelope| matches!(&envelope.operation, RecordOperationV1::AppendVersion(_)))
        {
            match self.apply_exact(key.record_id, envelope, &batch.snapshot, Some(repository)) {
                Ok(()) => {
                    report.operations_replayed = report.operations_replayed.saturating_add(1);
                }
                Err(RecordMaterializerError::UnknownOrTombstonedRecord(_)) => {
                    report.unknown_or_tombstoned_records =
                        report.unknown_or_tombstoned_records.saturating_add(1);
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        if let Some(envelope) = batch.envelopes.iter().rev().find(|envelope| {
            matches!(
                &envelope.operation,
                RecordOperationV1::SetContentRevision(_)
            )
        }) {
            match self.apply_exact(key.record_id, envelope, &batch.snapshot, Some(repository)) {
                Ok(()) => {
                    report.operations_replayed = report.operations_replayed.saturating_add(1);
                }
                Err(RecordMaterializerError::UnknownOrTombstonedRecord(_)) => {
                    report.unknown_or_tombstoned_records =
                        report.unknown_or_tombstoned_records.saturating_add(1);
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }
        // Project converged metadata/previews after the content revision
        // exists; losing metadata arrivals remain only in the signed log.
        if let Some(envelope) = batch
            .envelopes
            .iter()
            .rev()
            .find(|envelope| matches!(&envelope.operation, RecordOperationV1::SetMetadata(_)))
        {
            match self.apply_exact(key.record_id, envelope, &batch.snapshot, Some(repository)) {
                Ok(()) => {
                    report.operations_replayed = report.operations_replayed.saturating_add(1);
                }
                Err(RecordMaterializerError::UnknownOrTombstonedRecord(_)) => {
                    report.unknown_or_tombstoned_records =
                        report.unknown_or_tombstoned_records.saturating_add(1);
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }
        for operation_kind in [0_u8, 1_u8] {
            for envelope in batch.envelopes.iter().filter(|envelope| {
                matches!(
                    (&envelope.operation, operation_kind),
                    (RecordOperationV1::AppendConflict(_), 0)
                        | (RecordOperationV1::ResolveConflict { .. }, 1)
                )
            }) {
                match self.apply_exact(key.record_id, envelope, &batch.snapshot, Some(repository)) {
                    Ok(()) => {
                        report.operations_replayed = report.operations_replayed.saturating_add(1);
                    }
                    Err(RecordMaterializerError::UnknownOrTombstonedRecord(_)) => {
                        report.unknown_or_tombstoned_records =
                            report.unknown_or_tombstoned_records.saturating_add(1);
                        break;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        report.records_reconciled = report.records_reconciled.saturating_add(1);
        Ok(())
    }

    /// Heal any durable repository operations that were not projected before
    /// a crash. Reconciliation is bounded by the repository's record and
    /// operation limits and is idempotent for namespace state and history.
    ///
    /// # Errors
    ///
    /// Returns an error if repository state is inconsistent, required durable
    /// state is unavailable, or a mutation cannot be projected safely.
    pub fn reconcile_repository(
        &self,
        repository: &RecordRepository,
    ) -> Result<RecordReconciliationReport, RecordMaterializerError> {
        let mut report = RecordReconciliationReport::default();
        for key in repository.reconciliation_keys(self.workspace_id) {
            self.reconcile_key(repository, key, &mut report)?;
        }
        self.reconcile_transactions(repository, &mut report)?;
        Ok(report)
    }

    /// The transaction phase of [`Self::reconcile_repository`]: complete
    /// multi-record transactions go through the shared crash journal.
    #[allow(
        clippy::too_many_lines,
        clippy::significant_drop_tightening,
        reason = "Replay phases stay ordered, and the journal guard remains held through durable transaction coordination before reconciliation advances."
    )]
    fn reconcile_transactions(
        &self,
        repository: &RecordRepository,
        report: &mut RecordReconciliationReport,
    ) -> Result<(), RecordMaterializerError> {
        for transaction in repository
            .complete_transactions(self.workspace_id)
            .map_err(|_| RecordMaterializerError::TransactionRejected)?
        {
            let history = self
                .history
                .as_ref()
                .ok_or(RecordMaterializerError::HistoryUnavailable)?;
            let versions = transaction
                .manifest
                .participants
                .iter()
                .map(|participant| participant.proposed_version.clone())
                .collect::<Vec<_>>();
            let history_complete = versions.iter().all(|version| {
                history.snapshot(version.record_id).is_some_and(|snapshot| {
                    snapshot.versions.iter().any(|existing| existing == version)
                })
            });
            let namespace_already_complete = self
                .namespace
                .lock()
                .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?
                .clone()
                .apply_mutation_transaction(&transaction)
                .is_ok_and(|delta| delta.is_none());
            // Checked restores may still own a seeded Prepared reservation
            // even if both projections arrived independently. Their journal
            // completion must run before taking this projection-only fast path.
            if history_complete
                && namespace_already_complete
                && !matches!(
                    transaction.manifest.operation,
                    collab_types::FileMutationOperation::RestoreContent { .. }
                )
            {
                report.transactions_reconciled = report.transactions_reconciled.saturating_add(1);
                continue;
            }
            if let Some(runtime) = &self.transaction_runtime {
                let entry = repository
                    .completed_transaction_journal_entry(self.workspace_id, &transaction)
                    .map_err(|_| RecordMaterializerError::TransactionRejected)?;
                let mut journal = runtime
                    .journal
                    .lock()
                    .map_err(|_| RecordMaterializerError::TransactionRecoveryUnavailable)?;
                if let Some(existing) = journal.get(entry.transaction_id()) {
                    if existing.transaction != entry.transaction
                        || existing.participant_batches != entry.participant_batches
                    {
                        return Err(RecordMaterializerError::TransactionRejected);
                    }
                } else {
                    journal
                        .insert_prepared(entry.clone())
                        .map_err(|_| RecordMaterializerError::TransactionRecoveryUnavailable)?;
                }
                let sync = self.sync.clone();
                let broadcast = move |delta| {
                    if let Ok(current) = sync.read()
                        && let Some(current) = current.as_ref()
                    {
                        current.broadcast_delta(delta);
                    }
                    Ok(())
                };
                let completion = if matches!(
                    entry.transaction.manifest.operation,
                    collab_types::FileMutationOperation::RestoreContent { .. }
                ) {
                    crate::transaction_coordinator::complete_checked_restore_after_repository(
                        repository,
                        &mut journal,
                        history,
                        &self.namespace,
                        &runtime.namespace_path,
                        &entry,
                        broadcast,
                    )
                } else {
                    crate::transaction_coordinator::complete_replacement_transaction_after_repository(
                        &mut journal,
                        history,
                        &self.namespace,
                        &runtime.namespace_path,
                        &entry,
                        broadcast,
                    )
                };
                completion.map_err(|error| match error {
                    RecordTransactionCoordinatorError::History(HistoryError::Io(_))
                    | RecordTransactionCoordinatorError::Io(_)
                    | RecordTransactionCoordinatorError::NamespacePoisoned
                    | RecordTransactionCoordinatorError::Repository(
                        RecordRepositoryError::Io(_)
                        | RecordRepositoryError::Log(_)
                        | RecordRepositoryError::StatePoisoned
                        | RecordRepositoryError::TransactionJournal(
                            RecordTransactionJournalError::Io(_),
                        ),
                    ) => RecordMaterializerError::TransactionRecoveryUnavailable,
                    RecordTransactionCoordinatorError::History(_) => {
                        RecordMaterializerError::HistoryRejected
                    }
                    _ => RecordMaterializerError::TransactionRejected,
                })?;
            } else {
                history
                    .append_batch(&versions)
                    .map_err(|error| match error {
                        HistoryError::Io(_) => RecordMaterializerError::HistoryUnavailable,
                        _ => RecordMaterializerError::HistoryRejected,
                    })?;
                if let collab_types::FileMutationOperation::MergeContentConflict {
                    source_record,
                    conflict_id,
                    ..
                } = transaction.manifest.operation
                {
                    let source = transaction
                        .manifest
                        .participants
                        .first()
                        .ok_or(RecordMaterializerError::TransactionRejected)?;
                    history
                        .resolve(
                            source_record,
                            conflict_id,
                            collab_types::ConflictResolution {
                                strategy: collab_types::ConflictStrategy::Merge,
                                result: source.proposed_version.id,
                                actor: transaction.manifest.actor,
                                timestamp_ms: transaction.manifest.timestamp_ms,
                            },
                        )
                        .map_err(|_| RecordMaterializerError::HistoryRejected)?;
                }
                let delta = if matches!(
                    transaction.manifest.operation,
                    collab_types::FileMutationOperation::RestoreContent { .. }
                ) {
                    let mut namespace = self
                        .namespace
                        .lock()
                        .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?;
                    #[expect(
                        clippy::indexing_slicing,
                        reason = "complete repository transactions passed manifest validation, which rejects an empty participant list"
                    )]
                    let record_id = transaction.manifest.participants[0].record_id;
                    let projection = repository
                        .projection_lease(crate::record_repository::RecordRepositoryKey {
                            workspace_id: self.workspace_id,
                            record_id,
                        })
                        .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?;
                    if let Some(binding) = &projection.snapshot.binding {
                        namespace
                            .apply_signed_binding(binding)
                            .map_err(|_| RecordMaterializerError::BindingMismatch)?;
                    }
                    if let Some(ino) = namespace.ensure_ino_for_record_id(record_id)
                        && let Some(revision) = projection.snapshot.content_revision
                        && namespace.content_revision_for(ino) != Some(revision)
                    {
                        namespace.set_content_revision(
                            ino,
                            &revision,
                            transaction.manifest.timestamp_ms,
                        );
                    }
                    Some(namespace.save_incremental())
                } else {
                    self.namespace
                        .lock()
                        .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?
                        .apply_mutation_transaction(&transaction)
                        .map_err(|_| RecordMaterializerError::TransactionRejected)?
                };
                if let Some(delta) = delta
                    && let Ok(sync) = self.sync.read()
                    && let Some(sync) = sync.as_ref()
                {
                    sync.broadcast_delta(delta);
                }
            }
            report.transactions_reconciled = report.transactions_reconciled.saturating_add(1);
        }
        Ok(())
    }

    // Keep the exhaustive operation projection in one match so every accepted
    // operation has one visible fail-closed namespace/history policy.
    #[allow(
        clippy::too_many_lines,
        reason = "The operation match keeps record binding checks and each metadata, revision, history, conflict, and transaction projection policy together."
    )]
    fn apply_exact(
        &self,
        record_id: FileRecordId,
        envelope: &RecordEnvelopeV1,
        snapshot: &RecordSnapshot,
        repository: Option<&RecordRepository>,
    ) -> Result<(), RecordMaterializerError> {
        if envelope.record_id != record_id || snapshot.record_id != record_id {
            return Err(RecordMaterializerError::BindingMismatch);
        }
        // The repository verified every envelope it holds when it admitted
        // it; only an envelope from elsewhere is checked here.
        if repository.is_none() {
            envelope
                .verify()
                .map_err(|_| RecordMaterializerError::InvalidEnvelope)?;
        }

        // Resolve and validate liveness before either operation. History for a
        // deleted or unknown namespace record must not be guessed into place.
        let mut namespace = self
            .namespace
            .lock()
            .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?;
        let mut history_lease = if matches!(
            envelope.operation,
            RecordOperationV1::AppendConflict(_) | RecordOperationV1::ResolveConflict { .. }
        ) {
            Some(
                self.history
                    .as_ref()
                    .ok_or(RecordMaterializerError::HistoryUnavailable)?
                    .lease()
                    .map_err(|_| RecordMaterializerError::HistoryUnavailable)?,
            )
        } else {
            None
        };
        // Repository admission completes before this sink is called. Taking R
        // only after N prevents a delayed handoff from reinstalling an older
        // captured winner, while retaining R protects the actual installation.
        let projection = repository
            .map(|repository| {
                repository.projection_lease(crate::record_repository::RecordRepositoryKey {
                    workspace_id: self.workspace_id,
                    record_id,
                })
            })
            .transpose()
            .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?;
        let snapshot = projection
            .as_ref()
            .map_or(snapshot, |lease| &lease.snapshot);
        if matches!(&envelope.operation, RecordOperationV1::SetBinding(_)) {
            let binding = snapshot
                .binding
                .as_ref()
                .ok_or(RecordMaterializerError::BindingMismatch)?;
            if binding.record_id != record_id {
                return Err(RecordMaterializerError::BindingMismatch);
            }
            let changed = namespace
                .apply_signed_binding(binding)
                .map_err(|_| RecordMaterializerError::BindingMismatch)?;
            let delta = changed.then(|| namespace.save_incremental());
            drop(projection);
            drop(namespace);
            if let Some(delta) = delta
                && let Ok(sync) = self.sync.read()
                && let Some(sync) = sync.as_ref()
            {
                sync.broadcast_delta(delta);
            }
            return Ok(());
        }
        let ino = namespace.ensure_ino_for_record_id(record_id).ok_or(
            RecordMaterializerError::UnknownOrTombstonedRecord(record_id),
        )?;
        let kind =
            namespace
                .live_kind(ino)
                .ok_or(RecordMaterializerError::UnknownOrTombstonedRecord(
                    record_id,
                ))?;
        match &envelope.operation {
            RecordOperationV1::SetBinding(_) => {
                return Err(RecordMaterializerError::BindingMismatch);
            }
            RecordOperationV1::SetMetadata(_) => {
                if kind != NodeKind::File.to_file_type() {
                    return Err(RecordMaterializerError::NotAFile);
                }
                // The accepted envelope may lose the record's deterministic
                // LWW comparison. Project the converged repository winner,
                // never the merely-most-recent arrival.
                let metadata = snapshot
                    .metadata
                    .as_ref()
                    .ok_or(RecordMaterializerError::MissingConvergedMetadata)?;
                let changed = if namespace.content_authority() == ContentAuthority::SignedRecords {
                    let installed = snapshot.content_revision.as_ref().is_none_or(|revision| {
                        namespace.set_content_revision(ino, revision, metadata.observed_at_ms)
                    });
                    if installed && snapshot.content_revision.is_some() {
                        let thumbnail = metadata
                            .previews
                            .iter()
                            .filter(|preview| preview.status == collab_types::PreviewStatus::Ready)
                            .filter_map(|preview| {
                                preview
                                    .hash
                                    .map(|hash| (preview.max_edge, iroh_blobs::Hash::from(hash.0)))
                            })
                            .max_by_key(|(max_edge, _)| *max_edge)
                            .map(|(_, hash)| hash);
                        let _ = namespace.install_verified_thumbnail(record_id, thumbnail);
                    }
                    installed
                } else if let Some(revision) = snapshot.content_revision.as_ref() {
                    namespace.set_content_revision(ino, revision, metadata.observed_at_ms)
                } else {
                    let manifest = metadata
                        .content_hash
                        .map(|hash| iroh_blobs::Hash::from(hash.0));
                    namespace.set_content(
                        ino,
                        manifest.as_ref(),
                        metadata.byte_len,
                        metadata.observed_at_ms,
                    )
                };
                if !changed {
                    return Err(RecordMaterializerError::NamespaceUnavailable);
                }
                let delta = namespace.save_incremental();
                drop(projection);
                drop(namespace);
                if let Ok(sync) = self.sync.read()
                    && let Some(sync) = sync.as_ref()
                {
                    sync.broadcast_delta(delta);
                }
            }
            RecordOperationV1::AppendVersion(version) => {
                // Do not hold the namespace lock across durable history I/O.
                drop(projection);
                drop(namespace);
                let history = self
                    .history
                    .as_ref()
                    .ok_or(RecordMaterializerError::HistoryUnavailable)?;
                history
                    .append(version.clone())
                    .map_err(|error| match error {
                        HistoryError::Io(_) => RecordMaterializerError::HistoryUnavailable,
                        _ => RecordMaterializerError::HistoryRejected,
                    })?;
            }
            RecordOperationV1::SetContentRevision(_) => {
                if kind != NodeKind::File.to_file_type() {
                    return Err(RecordMaterializerError::NotAFile);
                }
                let revision = snapshot
                    .content_revision
                    .as_ref()
                    .ok_or(RecordMaterializerError::MissingConvergedContentRevision)?;
                let observed_at_ms = snapshot
                    .metadata
                    .as_ref()
                    .map_or(0, |metadata| metadata.observed_at_ms);
                if !namespace.set_content_revision(ino, revision, observed_at_ms) {
                    return Err(RecordMaterializerError::NamespaceUnavailable);
                }
                let delta = namespace.save_incremental();
                drop(projection);
                drop(namespace);
                if let Ok(sync) = self.sync.read()
                    && let Some(sync) = sync.as_ref()
                {
                    sync.broadcast_delta(delta);
                }
            }
            RecordOperationV1::AppendConflict(_) | RecordOperationV1::ResolveConflict { .. } => {
                let conflict_id = match &envelope.operation {
                    RecordOperationV1::AppendConflict(conflict) => conflict.id,
                    RecordOperationV1::ResolveConflict { conflict_id, .. } => *conflict_id,
                    #[expect(
                        clippy::unreachable,
                        reason = "the enclosing arm admits only AppendConflict and ResolveConflict"
                    )]
                    _ => unreachable!(),
                };
                let conflict = if repository.is_some() {
                    snapshot
                        .conflicts
                        .iter()
                        .find(|conflict| conflict.id == conflict_id)
                        .ok_or(RecordMaterializerError::HistoryRejected)?
                        .clone()
                } else {
                    // Pure projection tests supply a single-operation snapshot.
                    match &envelope.operation {
                        RecordOperationV1::AppendConflict(conflict) => conflict.clone(),
                        RecordOperationV1::ResolveConflict { resolution, .. } => {
                            history_lease
                                .as_mut()
                                .ok_or(RecordMaterializerError::HistoryUnavailable)?
                                .resolve(record_id, conflict_id, resolution.clone())
                                .map_err(history_projection_error)?;
                            return Ok(());
                        }
                        #[expect(
                            clippy::unreachable,
                            reason = "the enclosing arm admits only AppendConflict and ResolveConflict"
                        )]
                        _ => unreachable!(),
                    }
                };
                let history = history_lease
                    .as_mut()
                    .ok_or(RecordMaterializerError::HistoryUnavailable)?;
                if let Some(resolution) = &conflict.resolution {
                    let result = snapshot
                        .versions
                        .iter()
                        .find(|version| version.id == resolution.result)
                        .ok_or(RecordMaterializerError::HistoryRejected)?;
                    // A complete transaction can contribute its immutable result
                    // before the transaction projection pass. Import that exact
                    // verified version before projecting its resolved conflict.
                    history
                        .append_batch(std::slice::from_ref(result))
                        .map_err(history_projection_error)?;
                }
                let existing = history.snapshot(record_id).and_then(|state| {
                    state
                        .conflicts
                        .into_iter()
                        .find(|value| value.id == conflict_id)
                });
                if let Some(existing) = existing {
                    if existing.versions != conflict.versions {
                        return Err(RecordMaterializerError::HistoryRejected);
                    }
                    if let Some(resolution) = conflict.resolution {
                        history
                            .resolve(record_id, conflict_id, resolution)
                            .map_err(history_projection_error)?;
                    }
                } else {
                    history
                        .record_conflict(conflict)
                        .map_err(history_projection_error)?;
                }
            }
            RecordOperationV1::PrepareTransaction(_)
            | RecordOperationV1::CommitTransaction { .. } => {
                // A record-local arrival is deliberately invisible. Only the
                // repository-wide complete-participant materializer may
                // project a transaction.
                drop(namespace);
            }
        }
        Ok(())
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "a `map_err` adapter, which receives the error by value"
)]
fn history_projection_error(error: HistoryError) -> RecordMaterializerError {
    match error {
        HistoryError::Io(_) => RecordMaterializerError::HistoryUnavailable,
        _ => RecordMaterializerError::HistoryRejected,
    }
}

impl RecordMutationSink for RecordMaterializer {
    fn record_mutation(
        &self,
        repository: &RecordRepository,
        scope: RecordAccessBinding,
        envelope: &RecordEnvelopeV1,
        snapshot: &RecordSnapshot,
    ) -> Result<(), RecordMutationSinkError> {
        let result = if scope.workspace_id != self.workspace_id {
            Err(RecordMaterializerError::WrongWorkspace)
        } else if scope.record_id != envelope.record_id
            || scope.record_id != snapshot.record_id
            || scope.person != envelope.actor.person
            || scope.device != envelope.actor.device
        {
            Err(RecordMaterializerError::BindingMismatch)
        } else {
            self.apply_exact(scope.record_id, envelope, snapshot, Some(repository))
        };
        result.map_err(|error| match error {
            RecordMaterializerError::WrongWorkspace
            | RecordMaterializerError::BindingMismatch
            | RecordMaterializerError::InvalidEnvelope
            | RecordMaterializerError::MissingConvergedMetadata
            | RecordMaterializerError::MissingConvergedContentRevision
            | RecordMaterializerError::UnknownOrTombstonedRecord(_)
            | RecordMaterializerError::NotAFile
            | RecordMaterializerError::HistoryRejected
            | RecordMaterializerError::TransactionRejected => RecordMutationSinkError::Rejected,
            RecordMaterializerError::NamespaceUnavailable
            | RecordMaterializerError::HistoryUnavailable
            | RecordMaterializerError::TransactionRecoveryUnavailable => {
                RecordMutationSinkError::Unavailable
            }
        })
    }

    fn reconcile_repository(
        &self,
        repository: &RecordRepository,
    ) -> Result<(), RecordMutationSinkError> {
        RecordMaterializer::reconcile_repository(self, repository)
            .map(|_| ())
            .map_err(|error| match error {
                RecordMaterializerError::WrongWorkspace
                | RecordMaterializerError::BindingMismatch
                | RecordMaterializerError::InvalidEnvelope
                | RecordMaterializerError::MissingConvergedMetadata
                | RecordMaterializerError::MissingConvergedContentRevision
                | RecordMaterializerError::UnknownOrTombstonedRecord(_)
                | RecordMaterializerError::NotAFile
                | RecordMaterializerError::HistoryRejected
                | RecordMaterializerError::TransactionRejected => RecordMutationSinkError::Rejected,
                RecordMaterializerError::NamespaceUnavailable
                | RecordMaterializerError::HistoryUnavailable
                | RecordMaterializerError::TransactionRecoveryUnavailable => {
                    RecordMutationSinkError::Unavailable
                }
            })
    }
}

/// Finite, payload-bounded materialization failures.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RecordMaterializerError {
    #[error("record mutation belongs to another workspace")]
    WrongWorkspace,
    #[error("record mutation does not match its authenticated session binding")]
    BindingMismatch,
    #[error("record mutation envelope is invalid")]
    InvalidEnvelope,
    #[error("repository snapshot has no converged metadata for a metadata mutation")]
    MissingConvergedMetadata,
    #[error("repository snapshot has no converged signed content revision")]
    MissingConvergedContentRevision,
    #[error("record {0:?} is unknown or tombstoned")]
    UnknownOrTombstonedRecord(FileRecordId),
    #[error("record mutation target is not a file")]
    NotAFile,
    #[error("live namespace is unavailable")]
    NamespaceUnavailable,
    #[error("immutable history is unavailable")]
    HistoryUnavailable,
    #[error("immutable history rejected the version")]
    HistoryRejected,
    #[error("complete multi-record transaction was inconsistent with local state")]
    TransactionRejected,
    #[error("remote transaction recovery journal or durable projection is unavailable")]
    TransactionRecoveryUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::UNIX_EPOCH;

    use collab_types::{
        ConflictMergeCodecV1, ConflictResolution, ConflictStatus, ConflictStrategy, ContentHash,
        ContentRevisionV1, Ed25519PublicKey, FILE_BINDING_VERSION,
        FILE_MUTATION_TRANSACTION_VERSION, FileActor, FileBindingV1, FileConflictV1,
        FileMetadataV1, FileMutationOperation, FileMutationParticipant,
        FileMutationParticipantRole, FileMutationTransaction, FileMutationTransactionManifest,
        FileNodeKind, FileProvenance, FileVersionId, FileVersionV1, IndexStatus,
    };
    use ed25519_dalek::SigningKey;

    use crate::namespace::ROOT_INO;
    use crate::record_access::RecordAccessSessionId;

    struct Fixture {
        workspace: Uuid,
        namespace: Arc<Mutex<Namespace>>,
        history: Arc<FileHistoryStore>,
        materializer: RecordMaterializer,
        record: FileRecordId,
        ino: u64,
        actor: FileActor,
        device: SigningKey,
        temp: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let workspace = Uuid::new_v4();
            let mut namespace = Namespace::new("materializer-test").unwrap();
            let ino = namespace
                .create_file(ROOT_INO, "before.txt", None, 0, 1)
                .unwrap();
            let record = namespace.record_id_for(ino).unwrap();
            let namespace = Arc::new(Mutex::new(namespace));
            let temp = tempfile::tempdir().unwrap();
            let history =
                Arc::new(FileHistoryStore::open(temp.path().join("history.json")).unwrap());
            let device = SigningKey::from_bytes(&[7; 32]);
            let actor = FileActor {
                person: Ed25519PublicKey::from_bytes([8; 32]),
                device: Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
            };
            let materializer = RecordMaterializer::new(
                workspace,
                Arc::clone(&namespace),
                Some(Arc::clone(&history)),
            );
            Self {
                workspace,
                namespace,
                history,
                materializer,
                record,
                ino,
                actor,
                device,
                temp,
            }
        }

        fn binding(&self) -> RecordAccessBinding {
            RecordAccessBinding {
                session_id: RecordAccessSessionId(Uuid::new_v4()),
                grant_id: Uuid::new_v4(),
                workspace_id: self.workspace,
                record_id: self.record,
                person: self.actor.person,
                device: self.actor.device,
                expires_at_ms: u64::MAX,
            }
        }

        fn envelope(&self, sequence: u64, operation: RecordOperationV1) -> RecordEnvelopeV1 {
            RecordEnvelopeV1::signed(self.record, self.actor, sequence, operation, &self.device)
                .unwrap()
        }

        fn snapshot_for(&self, envelope: &RecordEnvelopeV1) -> RecordSnapshot {
            match &envelope.operation {
                RecordOperationV1::SetBinding(binding) => RecordSnapshot {
                    record_id: self.record,
                    binding: Some(binding.clone()),
                    metadata: None,
                    content_revision: None,
                    versions: vec![],
                    conflicts: vec![],
                },
                RecordOperationV1::SetMetadata(metadata) => RecordSnapshot {
                    record_id: self.record,
                    binding: None,
                    metadata: Some(metadata.clone()),
                    content_revision: None,
                    versions: vec![],
                    conflicts: vec![],
                },
                RecordOperationV1::AppendVersion(version) => RecordSnapshot {
                    record_id: self.record,
                    binding: None,
                    metadata: None,
                    content_revision: None,
                    versions: vec![version.clone()],
                    conflicts: vec![],
                },
                RecordOperationV1::AppendConflict(conflict) => RecordSnapshot {
                    record_id: self.record,
                    binding: None,
                    metadata: None,
                    content_revision: None,
                    versions: vec![],
                    conflicts: vec![conflict.clone()],
                },
                RecordOperationV1::ResolveConflict { .. }
                | RecordOperationV1::PrepareTransaction(_)
                | RecordOperationV1::CommitTransaction { .. } => RecordSnapshot {
                    record_id: self.record,
                    binding: None,
                    metadata: None,
                    content_revision: None,
                    versions: vec![],
                    conflicts: vec![],
                },
                RecordOperationV1::SetContentRevision(revision) => RecordSnapshot {
                    record_id: self.record,
                    binding: None,
                    metadata: None,
                    content_revision: Some(*revision),
                    versions: vec![],
                    conflicts: vec![],
                },
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn materialization_uses_current_signed_state_and_refuses_unknown_records() {
        // Phase `delayed_materialization_uses_current_repository_not_captured_snapshot`.
        {
            use crate::record_replication::{RecordAction, RecordAuthorizer};
            use crate::record_repository::RecordRepositoryKey;
            struct Allow;
            impl RecordAuthorizer for Allow {
                fn actor_is_certified(&self, _: &FileActor) -> bool {
                    true
                }
                fn allows(&self, _: &FileActor, _: FileRecordId, _: RecordAction) -> bool {
                    true
                }
            }
            let fixture = Fixture::new();
            let repository =
                RecordRepository::open(fixture.temp.path().join("delayed-records")).unwrap();
            let key = RecordRepositoryKey {
                workspace_id: fixture.workspace,
                record_id: fixture.record,
            };
            let old = FileMetadataV1 {
                schema_version: collab_types::files::FILE_METADATA_VERSION,
                record_id: fixture.record,
                content_hash: Some(ContentHash([41; 32])),
                byte_len: 41,
                mime: None,
                previews: vec![],
                latest_actor: Some(fixture.actor),
                observed_at_ms: 41,
                index_status: IndexStatus::Pending,
                properties: None,
            };
            let old_envelope = fixture.envelope(1, RecordOperationV1::SetMetadata(old.clone()));
            repository.apply(key, &old_envelope, &Allow).unwrap();
            let captured = repository.snapshot(key, &fixture.actor, &Allow).unwrap();
            let current = FileMetadataV1 {
                content_hash: Some(ContentHash([42; 32])),
                byte_len: 42,
                observed_at_ms: 42,
                ..old
            };
            let current_envelope = fixture.envelope(2, RecordOperationV1::SetMetadata(current));
            // Deterministically withhold N while the delayed sink hands off its
            // captured snapshot. The repository can accept a later operation while
            // projection waits; releasing N must select that current projection.
            let (entered, wait) = std::sync::mpsc::channel();
            std::thread::scope(|scope| {
                let namespace = fixture.namespace.lock().unwrap();
                let worker = scope.spawn(|| {
                    entered.send(()).unwrap();
                    fixture
                        .materializer
                        .record_mutation(&repository, fixture.binding(), &old_envelope, &captured)
                        .unwrap();
                });
                wait.recv_timeout(tilo_test_env::hang_bound()).unwrap();
                repository.apply(key, &current_envelope, &Allow).unwrap();
                drop(namespace);
                worker.join().unwrap();
            });
            let namespace = fixture.namespace.lock().unwrap();
            assert_eq!(
                namespace.hash_for(fixture.ino).unwrap().as_bytes(),
                &[42; 32]
            );
            assert_eq!(namespace.getattr(fixture.ino).unwrap().size, 42);
            drop(namespace);
        }
        // Phase `metadata_update_resolves_renamed_record_without_touching_name_or_parent`.
        {
            let fixture = Fixture::new();
            assert!(fixture.namespace.lock().unwrap().rename(
                ROOT_INO,
                "before.txt",
                ROOT_INO,
                "after.txt",
                2,
            ));
            let metadata = FileMetadataV1 {
                schema_version: collab_types::files::FILE_METADATA_VERSION,
                record_id: fixture.record,
                content_hash: Some(ContentHash([4; 32])),
                byte_len: 99,
                mime: Some("text/plain".into()),
                previews: vec![],
                latest_actor: Some(fixture.actor),
                observed_at_ms: 1234,
                index_status: IndexStatus::Pending,
                properties: None,
            };
            let converged = metadata.clone();
            let envelope = fixture.envelope(1, RecordOperationV1::SetMetadata(metadata));
            fixture
                .materializer
                .apply(
                    fixture.binding(),
                    &envelope,
                    &fixture.snapshot_for(&envelope),
                )
                .unwrap();

            let mut namespace = fixture.namespace.lock().unwrap();
            assert!(namespace.lookup(ROOT_INO, "before.txt").is_none());
            assert_eq!(
                namespace.lookup(ROOT_INO, "after.txt").unwrap().ino.0,
                fixture.ino
            );
            assert_eq!(
                namespace.hash_for(fixture.ino).unwrap().as_bytes(),
                &[4; 32]
            );
            let attr = namespace.getattr(fixture.ino).unwrap();
            assert_eq!(attr.size, 99);
            assert_eq!(
                attr.mtime.duration_since(UNIX_EPOCH).unwrap().as_millis(),
                1234
            );
            drop(namespace);

            // A later-arriving envelope can lose the replica's LWW comparison.
            // The live namespace must follow the converged snapshot, not arrival
            // order.
            let losing = fixture.envelope(
                2,
                RecordOperationV1::SetMetadata(FileMetadataV1 {
                    content_hash: Some(ContentHash([9; 32])),
                    byte_len: 7,
                    observed_at_ms: 10,
                    ..converged.clone()
                }),
            );
            fixture
                .materializer
                .apply(
                    fixture.binding(),
                    &losing,
                    &RecordSnapshot {
                        record_id: fixture.record,
                        binding: None,
                        metadata: Some(converged),
                        content_revision: None,
                        versions: vec![],
                        conflicts: vec![],
                    },
                )
                .unwrap();
            let namespace = fixture.namespace.lock().unwrap();
            assert_eq!(
                namespace.hash_for(fixture.ino).unwrap().as_bytes(),
                &[4; 32]
            );
            assert_eq!(namespace.getattr(fixture.ino).unwrap().size, 99);
            drop(namespace);
        }
        // Phase `signed_binding_materializes_move_and_exact_replay_is_a_noop`.
        {
            let fixture = Fixture::new();
            let parent = fixture
                .namespace
                .lock()
                .unwrap()
                .record_id_for(ROOT_INO)
                .unwrap();
            let binding = FileBindingV1 {
                schema_version: collab_types::FILE_BINDING_VERSION,
                record_id: fixture.record,
                parent: Some(parent),
                name: "after.txt".into(),
                kind: FileNodeKind::File,
                tombstoned: false,
                actor: fixture.actor,
                observed_at_ms: 2,
            };
            let envelope = fixture.envelope(1, RecordOperationV1::SetBinding(binding.clone()));
            let snapshot = RecordSnapshot {
                record_id: fixture.record,
                binding: Some(binding),
                metadata: None,
                content_revision: None,
                versions: vec![],
                conflicts: vec![],
            };
            fixture
                .materializer
                .apply(fixture.binding(), &envelope, &snapshot)
                .unwrap();
            fixture
                .materializer
                .apply(fixture.binding(), &envelope, &snapshot)
                .unwrap();
            let mut namespace = fixture.namespace.lock().unwrap();
            assert!(namespace.lookup(ROOT_INO, "before.txt").is_none());
            assert_eq!(
                namespace.lookup(ROOT_INO, "after.txt").unwrap().ino.0,
                fixture.ino
            );
            drop(namespace);
        }
        // Phase `unknown_tombstoned_and_wrong_scope_are_refused`.
        {
            let fixture = Fixture::new();
            let metadata = FileMetadataV1 {
                schema_version: collab_types::files::FILE_METADATA_VERSION,
                record_id: fixture.record,
                content_hash: None,
                byte_len: 1,
                mime: None,
                previews: vec![],
                latest_actor: Some(fixture.actor),
                observed_at_ms: 1,
                index_status: IndexStatus::Pending,
                properties: None,
            };
            let envelope = fixture.envelope(1, RecordOperationV1::SetMetadata(metadata));
            let snapshot = fixture.snapshot_for(&envelope);

            let mut wrong_workspace = fixture.binding();
            wrong_workspace.workspace_id = Uuid::new_v4();
            assert_eq!(
                fixture
                    .materializer
                    .apply(wrong_workspace, &envelope, &snapshot),
                Err(RecordMaterializerError::WrongWorkspace)
            );
            let mut wrong_record = fixture.binding();
            wrong_record.record_id = FileRecordId::new();
            assert_eq!(
                fixture
                    .materializer
                    .apply(wrong_record, &envelope, &snapshot),
                Err(RecordMaterializerError::BindingMismatch)
            );

            assert!(fixture.namespace.lock().unwrap().remove(fixture.ino));
            assert_eq!(
                fixture
                    .materializer
                    .apply(fixture.binding(), &envelope, &snapshot),
                Err(RecordMaterializerError::UnknownOrTombstonedRecord(
                    fixture.record
                ))
            );
        }
        // Phase `immutable_history_is_exact_and_exact_replay_is_idempotent`.
        {
            let fixture = Fixture::new();
            let version = FileVersionV1 {
                id: FileVersionId::new(),
                record_id: fixture.record,
                parents: vec![],
                actor: fixture.actor,
                timestamp_ms: 10,
                content_hash: Some(ContentHash([5; 32])),
                metadata_hash: ContentHash([6; 32]),
                provenance: FileProvenance::Create,
            };
            let envelope = fixture.envelope(1, RecordOperationV1::AppendVersion(version.clone()));
            let snapshot = fixture.snapshot_for(&envelope);
            fixture
                .materializer
                .apply(fixture.binding(), &envelope, &snapshot)
                .unwrap();
            fixture
                .materializer
                .apply(fixture.binding(), &envelope, &snapshot)
                .unwrap();

            let snapshot = fixture.history.snapshot(fixture.record).unwrap();
            assert_eq!(snapshot.versions, vec![version.clone()]);

            let mut collision = version;
            collision.metadata_hash = ContentHash([99; 32]);
            let collision_envelope =
                fixture.envelope(2, RecordOperationV1::AppendVersion(collision));
            let collision_snapshot = fixture.snapshot_for(&collision_envelope);
            assert_eq!(
                fixture.materializer.apply(
                    fixture.binding(),
                    &collision_envelope,
                    &collision_snapshot
                ),
                Err(RecordMaterializerError::HistoryRejected)
            );
            assert_eq!(
                fixture.history.snapshot(fixture.record).unwrap().versions,
                snapshot.versions
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn conflicts_project_repository_resolution_into_typed_history() {
        // Phase `delayed_open_conflict_projects_repository_resolution`.
        {
            use crate::record_replication::{RecordAction, RecordAuthorizer};
            use crate::record_repository::RecordRepositoryKey;
            struct Allow;
            impl RecordAuthorizer for Allow {
                fn actor_is_certified(&self, _: &FileActor) -> bool {
                    true
                }
                fn allows(&self, _: &FileActor, _: FileRecordId, _: RecordAction) -> bool {
                    true
                }
            }
            let fixture = Fixture::new();
            let repository =
                RecordRepository::open(fixture.temp.path().join("conflict-race")).unwrap();
            let key = RecordRepositoryKey {
                workspace_id: fixture.workspace,
                record_id: fixture.record,
            };
            let first = FileVersionV1 {
                id: FileVersionId::new(),
                record_id: fixture.record,
                parents: vec![],
                actor: fixture.actor,
                timestamp_ms: 1,
                content_hash: None,
                metadata_hash: ContentHash([1; 32]),
                provenance: FileProvenance::Create,
            };
            let second = FileVersionV1 {
                id: FileVersionId::new(),
                parents: vec![first.id],
                timestamp_ms: 2,
                provenance: FileProvenance::Edit,
                ..first
            };
            let result = FileVersionV1 {
                id: FileVersionId::new(),
                parents: vec![first.id, second.id],
                timestamp_ms: 3,
                provenance: FileProvenance::ConflictResolution,
                ..first
            };
            fixture
                .history
                .append_batch(&[first.clone(), second.clone(), result.clone()])
                .unwrap();
            let conflict = FileConflictV1 {
                id: Uuid::new_v4(),
                record_id: fixture.record,
                versions: vec![first.id, second.id],
                status: ConflictStatus::Open,
                resolution: None,
            };
            let resolution = ConflictResolution {
                strategy: ConflictStrategy::Merge,
                result: result.id,
                actor: fixture.actor,
                timestamp_ms: 3,
            };
            let operations = [
                RecordOperationV1::AppendVersion(first),
                RecordOperationV1::AppendVersion(second),
                RecordOperationV1::AppendConflict(conflict.clone()),
                RecordOperationV1::AppendVersion(result),
                RecordOperationV1::ResolveConflict {
                    conflict_id: conflict.id,
                    resolution: resolution.clone(),
                },
            ];
            let mut captured = None;
            for (index, operation) in operations.into_iter().enumerate() {
                let envelope = fixture.envelope(index as u64 + 1, operation);
                repository.apply(key, &envelope, &Allow).unwrap();
                if index == 2 {
                    captured = Some((
                        envelope,
                        repository.snapshot(key, &fixture.actor, &Allow).unwrap(),
                    ));
                }
            }
            let (envelope, snapshot) = captured.unwrap();
            fixture
                .materializer
                .record_mutation(&repository, fixture.binding(), &envelope, &snapshot)
                .unwrap();
            let state = fixture.history.snapshot(fixture.record).unwrap();
            assert_eq!(state.conflicts[0].status, ConflictStatus::Resolved);
            assert_eq!(state.conflicts[0].resolution, Some(resolution));
        }
        // Phase `accepted_conflict_transitions_project_into_typed_history`.
        {
            let fixture = Fixture::new();
            let first = FileVersionV1 {
                id: FileVersionId::new(),
                record_id: fixture.record,
                parents: vec![],
                actor: fixture.actor,
                timestamp_ms: 1,
                content_hash: Some(ContentHash([1; 32])),
                metadata_hash: ContentHash([11; 32]),
                provenance: FileProvenance::Create,
            };
            let second = FileVersionV1 {
                id: FileVersionId::new(),
                record_id: first.record_id,
                parents: vec![first.id],
                actor: first.actor,
                timestamp_ms: 2,
                content_hash: Some(ContentHash([2; 32])),
                metadata_hash: ContentHash([12; 32]),
                provenance: FileProvenance::Edit,
            };
            let result = FileVersionV1 {
                id: FileVersionId::new(),
                record_id: first.record_id,
                parents: vec![first.id, second.id],
                actor: first.actor,
                timestamp_ms: 3,
                content_hash: Some(ContentHash([3; 32])),
                metadata_hash: ContentHash([13; 32]),
                provenance: FileProvenance::ConflictResolution,
            };
            let conflict = FileConflictV1 {
                id: Uuid::new_v4(),
                record_id: fixture.record,
                versions: vec![first.id, second.id],
                status: ConflictStatus::Open,
                resolution: None,
            };
            let resolution = ConflictResolution {
                strategy: ConflictStrategy::Merge,
                result: result.id,
                actor: fixture.actor,
                timestamp_ms: result.timestamp_ms,
            };
            let operations = [
                RecordOperationV1::AppendVersion(first),
                RecordOperationV1::AppendVersion(second),
                RecordOperationV1::AppendConflict(conflict.clone()),
                RecordOperationV1::AppendVersion(result),
                RecordOperationV1::ResolveConflict {
                    conflict_id: conflict.id,
                    resolution: resolution.clone(),
                },
            ];
            for (index, operation) in operations.into_iter().enumerate() {
                let envelope = fixture.envelope(index as u64 + 1, operation);
                fixture
                    .materializer
                    .apply(
                        fixture.binding(),
                        &envelope,
                        &fixture.snapshot_for(&envelope),
                    )
                    .unwrap();
            }
            let projection = fixture.history.projection(fixture.record).unwrap();
            assert_eq!(projection.conflicts.len(), 1);
            assert_eq!(projection.conflicts[0].status, ConflictStatus::Resolved);
            assert_eq!(projection.conflicts[0].resolution, Some(resolution));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn remote_transactions_and_merges_materialize_exactly_and_heal_after_restart() {
        // Phase `startup_reconciliation_heals_committed_but_unprojected_operations`.
        {
            use crate::record_replication::{RecordAction, RecordAuthorizer};
            use crate::record_repository::{RecordRepository, RecordRepositoryKey};

            struct Allow(FileActor);
            impl RecordAuthorizer for Allow {
                fn actor_is_certified(&self, actor: &FileActor) -> bool {
                    *actor == self.0
                }

                fn allows(
                    &self,
                    actor: &FileActor,
                    _record_id: FileRecordId,
                    _action: RecordAction,
                ) -> bool {
                    *actor == self.0
                }
            }

            let fixture = Fixture::new();
            let repository =
                RecordRepository::open(fixture.temp.path().join("recovery-records")).unwrap();
            let key = RecordRepositoryKey {
                workspace_id: fixture.workspace,
                record_id: fixture.record,
            };
            let metadata = FileMetadataV1 {
                schema_version: collab_types::files::FILE_METADATA_VERSION,
                record_id: fixture.record,
                content_hash: Some(ContentHash([31; 32])),
                byte_len: 314,
                mime: Some("application/octet-stream".into()),
                previews: vec![],
                latest_actor: Some(fixture.actor),
                observed_at_ms: 31,
                index_status: IndexStatus::Pending,
                properties: None,
            };
            let version = FileVersionV1 {
                id: FileVersionId::new(),
                record_id: fixture.record,
                parents: vec![],
                actor: fixture.actor,
                timestamp_ms: 32,
                content_hash: Some(ContentHash([31; 32])),
                metadata_hash: ContentHash([32; 32]),
                provenance: FileProvenance::Edit,
            };
            let version_envelope =
                fixture.envelope(1, RecordOperationV1::AppendVersion(version.clone()));
            let revision = collab_types::ContentRevisionV1 {
                schema_version: collab_types::CONTENT_REVISION_VERSION,
                record_id: fixture.record,
                version_id: version.id,
                content_hash: version.content_hash,
                byte_len: 314,
                metadata_hash: version.metadata_hash,
            };
            let revision_envelope =
                fixture.envelope(2, RecordOperationV1::SetContentRevision(revision));
            let metadata_envelope = fixture.envelope(3, RecordOperationV1::SetMetadata(metadata));
            fixture
                .namespace
                .lock()
                .unwrap()
                .enable_signed_content_authority();
            repository
                .apply(key, &version_envelope, &Allow(fixture.actor))
                .unwrap();
            repository
                .apply(key, &revision_envelope, &Allow(fixture.actor))
                .unwrap();
            repository
                .apply(key, &metadata_envelope, &Allow(fixture.actor))
                .unwrap();

            assert!(
                fixture
                    .namespace
                    .lock()
                    .unwrap()
                    .hash_for(fixture.ino)
                    .is_none()
            );
            assert!(fixture.history.snapshot(fixture.record).is_none());

            let report = fixture
                .materializer
                .reconcile_repository(&repository)
                .unwrap();
            assert_eq!(report.records_reconciled, 1);
            assert_eq!(report.operations_replayed, 3);
            assert_eq!(report.unknown_or_tombstoned_records, 0);
            assert_eq!(
                fixture
                    .namespace
                    .lock()
                    .unwrap()
                    .hash_for(fixture.ino)
                    .unwrap()
                    .as_bytes(),
                &[31; 32]
            );
            assert_eq!(
                fixture.history.snapshot(fixture.record).unwrap().versions,
                vec![version]
            );
            assert_eq!(
                fixture
                    .namespace
                    .lock()
                    .unwrap()
                    .content_revision_for(fixture.ino),
                Some(revision)
            );

            fixture
                .materializer
                .reconcile_repository(&repository)
                .unwrap();
            assert_eq!(
                fixture
                    .history
                    .snapshot(fixture.record)
                    .unwrap()
                    .versions
                    .len(),
                1
            );
        }
        // Phase `remote_replacement_transaction_materializes_only_after_both_participants_in_either_order`.
        // Model the record-mesh delivery boundary: each participant stream is
        // persisted independently with the same signed prepare+commit pair, and
        // the materializer runs after every received stream batch.
        {
            use crate::record_replication::{RecordAction, RecordAuthorizer};
            use crate::record_repository::{RecordRepository, RecordRepositoryKey};

            #[derive(Clone, Copy)]
            struct AllowActor(FileActor);

            impl RecordAuthorizer for AllowActor {
                fn actor_is_certified(&self, actor: &FileActor) -> bool {
                    *actor == self.0
                }

                fn allows(
                    &self,
                    actor: &FileActor,
                    _record_id: FileRecordId,
                    _action: RecordAction,
                ) -> bool {
                    *actor == self.0
                }
            }

            for source_first in [true, false] {
                let temp = tempfile::tempdir().unwrap();
                let workspace = Uuid::new_v4();
                let device = SigningKey::from_bytes(&[41; 32]);
                let actor = FileActor {
                    person: Ed25519PublicKey::from_bytes([42; 32]),
                    device: Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
                };
                let authorizer = AllowActor(actor);

                let mut initial_namespace =
                    Namespace::new("remote-transaction-before-restart").unwrap();
                let source_ino = initial_namespace
                    .create_file(
                        ROOT_INO,
                        "source.txt",
                        Some(&iroh_blobs::Hash::from([1; 32])),
                        1,
                        1,
                    )
                    .unwrap();
                let replaced_ino = initial_namespace
                    .create_file(
                        ROOT_INO,
                        "destination.txt",
                        Some(&iroh_blobs::Hash::from([2; 32])),
                        1,
                        1,
                    )
                    .unwrap();
                let source_record = initial_namespace.record_id_for(source_ino).unwrap();
                let replaced_record = initial_namespace.record_id_for(replaced_ino).unwrap();
                let root_record = initial_namespace.record_id_for(ROOT_INO).unwrap();
                let source_expected = FileVersionId::new();
                let replaced_expected = FileVersionId::new();
                let initial = [
                    (
                        source_record,
                        source_expected,
                        "source.txt",
                        ContentHash([1; 32]),
                    ),
                    (
                        replaced_record,
                        replaced_expected,
                        "destination.txt",
                        ContentHash([2; 32]),
                    ),
                ];
                for (ino, (record_id, version_id, _, hash)) in
                    [source_ino, replaced_ino].into_iter().zip(initial)
                {
                    assert!(initial_namespace.set_content_revision(
                        ino,
                        &ContentRevisionV1 {
                            schema_version: collab_types::CONTENT_REVISION_VERSION,
                            record_id,
                            version_id,
                            content_hash: Some(hash),
                            byte_len: 1,
                            metadata_hash: ContentHash([3; 32]),
                        },
                        1,
                    ));
                }
                // This is the namespace snapshot already durable before record
                // mesh delivery starts. The staged participant itself must not
                // require or produce a namespace write.
                let persisted_namespace_before_transaction = initial_namespace.save();
                let namespace_path = temp.path().join("namespace.bin");
                tilo_files::native::atomic::write_creating_parent(
                    &namespace_path,
                    &persisted_namespace_before_transaction,
                )
                .unwrap();
                let journal_path = temp.path().join("transaction-journal");
                let namespace = Arc::new(Mutex::new(initial_namespace));
                let history_path = temp.path().join("history.json");
                let history = Arc::new(FileHistoryStore::open(&history_path).unwrap());
                let repository_path = temp.path().join("records");
                let repository = RecordRepository::open(&repository_path).unwrap();

                for (record_id, version_id, name, hash) in initial {
                    let binding = FileBindingV1 {
                        schema_version: FILE_BINDING_VERSION,
                        record_id,
                        parent: Some(root_record),
                        name: name.into(),
                        kind: FileNodeKind::File,
                        tombstoned: false,
                        actor,
                        observed_at_ms: 1,
                    };
                    let version = FileVersionV1 {
                        id: version_id,
                        record_id,
                        parents: vec![],
                        actor,
                        timestamp_ms: 1,
                        content_hash: Some(hash),
                        metadata_hash: ContentHash([3; 32]),
                        provenance: FileProvenance::Create,
                    };
                    let revision = ContentRevisionV1 {
                        schema_version: collab_types::CONTENT_REVISION_VERSION,
                        record_id,
                        version_id,
                        content_hash: Some(hash),
                        byte_len: 1,
                        metadata_hash: version.metadata_hash,
                    };
                    let envelopes = [
                        RecordEnvelopeV1::signed(
                            record_id,
                            actor,
                            1,
                            RecordOperationV1::SetBinding(binding),
                            &device,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            record_id,
                            actor,
                            2,
                            RecordOperationV1::AppendVersion(version),
                            &device,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            record_id,
                            actor,
                            3,
                            RecordOperationV1::SetContentRevision(revision),
                            &device,
                        )
                        .unwrap(),
                    ];
                    repository
                        .apply_batch(
                            RecordRepositoryKey {
                                workspace_id: workspace,
                                record_id,
                            },
                            &envelopes,
                            &authorizer,
                        )
                        .unwrap();
                }

                let materializer = RecordMaterializer::new(
                    workspace,
                    Arc::clone(&namespace),
                    Some(Arc::clone(&history)),
                )
                .with_transaction_recovery(
                    Arc::new(Mutex::new(
                        crate::record_transaction::RecordTransactionJournal::open(&journal_path)
                            .unwrap(),
                    )),
                    namespace_path.clone(),
                );
                materializer.reconcile_repository(&repository).unwrap();

                let participant = |record_id, expected_version, role, hash: ContentHash| {
                    let version = FileVersionV1 {
                        id: FileVersionId::new(),
                        record_id,
                        parents: vec![expected_version],
                        actor,
                        timestamp_ms: 2,
                        content_hash: Some(hash),
                        metadata_hash: ContentHash([3; 32]),
                        provenance: match role {
                            FileMutationParticipantRole::Source => FileProvenance::Rename,
                            FileMutationParticipantRole::Replaced => FileProvenance::Delete,
                            _ => unreachable!("replacement fixture role"),
                        },
                    };
                    let mut participant = FileMutationParticipant {
                        record_id,
                        role,
                        expected_version,
                        proposed_binding: FileBindingV1 {
                            schema_version: FILE_BINDING_VERSION,
                            record_id,
                            parent: Some(root_record),
                            name: "destination.txt".into(),
                            kind: FileNodeKind::File,
                            tombstoned: role == FileMutationParticipantRole::Replaced,
                            actor,
                            observed_at_ms: 2,
                        },
                        proposed_content_revision: Some(ContentRevisionV1 {
                            schema_version: collab_types::CONTENT_REVISION_VERSION,
                            record_id,
                            version_id: version.id,
                            content_hash: version.content_hash,
                            byte_len: 1,
                            metadata_hash: version.metadata_hash,
                        }),
                        proposed_version: version,
                        projection_hash: ContentHash([0; 32]),
                    };
                    participant.projection_hash = participant.computed_projection_hash().unwrap();
                    participant
                };
                let mut participants = vec![
                    participant(
                        source_record,
                        source_expected,
                        FileMutationParticipantRole::Source,
                        ContentHash([1; 32]),
                    ),
                    participant(
                        replaced_record,
                        replaced_expected,
                        FileMutationParticipantRole::Replaced,
                        ContentHash([2; 32]),
                    ),
                ];
                participants.sort_by_key(|value| value.record_id);
                let transaction = FileMutationTransaction::signed(
                    FileMutationTransactionManifest {
                        schema_version: FILE_MUTATION_TRANSACTION_VERSION,
                        transaction_id: Uuid::new_v4(),
                        workspace_id: workspace,
                        actor,
                        timestamp_ms: 2,
                        operation: FileMutationOperation::ReplacementRename {
                            source_parent: root_record,
                            source_name: "source.txt".into(),
                            destination_parent: root_record,
                            destination_name: "destination.txt".into(),
                            source_record,
                            replaced_record,
                        },
                        participants,
                    },
                    &device,
                )
                .unwrap();
                let before_delivery_probe = namespace
                    .lock()
                    .unwrap()
                    .clone()
                    .apply_replacement_transaction(&transaction);
                assert!(
                    before_delivery_probe.is_ok(),
                    "valid initial namespace must accept the transaction: {before_delivery_probe:?}"
                );

                let delivery_order = if source_first {
                    [source_record, replaced_record]
                } else {
                    [replaced_record, source_record]
                };
                let deliver = |repository: &RecordRepository, record_id| {
                    let envelopes = [
                        RecordEnvelopeV1::signed(
                            record_id,
                            actor,
                            4,
                            RecordOperationV1::PrepareTransaction(transaction.clone()),
                            &device,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            record_id,
                            actor,
                            5,
                            RecordOperationV1::CommitTransaction {
                                transaction_id: transaction.manifest.transaction_id,
                            },
                            &device,
                        )
                        .unwrap(),
                    ];
                    repository
                        .apply_batch(
                            RecordRepositoryKey {
                                workspace_id: workspace,
                                record_id,
                            },
                            &envelopes,
                            &authorizer,
                        )
                        .unwrap();
                };

                deliver(&repository, delivery_order[0]);
                let first_report = materializer.reconcile_repository(&repository).unwrap();
                assert_eq!(first_report.transactions_reconciled, 0);
                {
                    let mut namespace = namespace.lock().unwrap();
                    assert_eq!(
                        namespace.lookup(ROOT_INO, "source.txt").unwrap().ino.0,
                        source_ino
                    );
                    assert_eq!(
                        namespace.lookup(ROOT_INO, "destination.txt").unwrap().ino.0,
                        replaced_ino
                    );
                    drop(namespace);
                }
                assert_eq!(history.snapshot(source_record).unwrap().versions.len(), 1);
                assert_eq!(history.snapshot(replaced_record).unwrap().versions.len(), 1);

                // Simulate a process restart while exactly one participant is
                // durable. Both the record log and namespace are reloaded before
                // the other participant arrives.
                drop(materializer);
                drop(repository);
                drop(history);
                let restarted_namespace = Arc::new(Mutex::new(
                    Namespace::from_bytes(
                        "remote-transaction-after-restart",
                        &persisted_namespace_before_transaction,
                    )
                    .unwrap(),
                ));
                let restarted_history = Arc::new(FileHistoryStore::open(&history_path).unwrap());
                let restarted_repository = RecordRepository::open(&repository_path).unwrap();
                let blocked_namespace_path = temp.path().join("namespace-write-blocked");
                if source_first {
                    std::fs::create_dir(&blocked_namespace_path).unwrap();
                }
                let restarted_materializer = RecordMaterializer::new(
                    workspace,
                    Arc::clone(&restarted_namespace),
                    Some(Arc::clone(&restarted_history)),
                )
                .with_transaction_recovery(
                    Arc::new(Mutex::new(
                        crate::record_transaction::RecordTransactionJournal::open(&journal_path)
                            .unwrap(),
                    )),
                    if source_first {
                        blocked_namespace_path.clone()
                    } else {
                        namespace_path.clone()
                    },
                );
                {
                    let namespace = restarted_namespace.lock().unwrap();
                    assert_eq!(
                        namespace
                            .entry_at("source.txt")
                            .map(|entry| entry.record_id),
                        Some(source_record)
                    );
                    assert_eq!(
                        namespace
                            .entry_at("destination.txt")
                            .map(|entry| entry.record_id),
                        Some(replaced_record)
                    );
                    drop(namespace);
                }

                deliver(&restarted_repository, delivery_order[1]);
                let projection_probe = restarted_namespace
                    .lock()
                    .unwrap()
                    .clone()
                    .apply_replacement_transaction(&transaction);
                assert!(
                    projection_probe.is_ok(),
                    "complete remote transaction must project after restart: {projection_probe:?}"
                );
                let second_result =
                    restarted_materializer.reconcile_repository(&restarted_repository);
                let second_report = if source_first {
                    assert_eq!(
                        second_result,
                        Err(RecordMaterializerError::TransactionRecoveryUnavailable)
                    );
                    {
                        let mut namespace = restarted_namespace.lock().unwrap();
                        assert!(namespace.lookup(ROOT_INO, "source.txt").is_some());
                        assert_eq!(
                            namespace
                                .entry_at("destination.txt")
                                .map(|entry| entry.record_id),
                            Some(replaced_record)
                        );
                        drop(namespace);
                    }
                    let recovering_materializer = RecordMaterializer::new(
                        workspace,
                        Arc::clone(&restarted_namespace),
                        Some(Arc::clone(&restarted_history)),
                    )
                    .with_transaction_recovery(
                        Arc::new(Mutex::new(
                            crate::record_transaction::RecordTransactionJournal::open(
                                &journal_path,
                            )
                            .unwrap(),
                        )),
                        namespace_path.clone(),
                    );
                    recovering_materializer
                        .reconcile_repository(&restarted_repository)
                        .unwrap()
                } else {
                    second_result.unwrap()
                };
                assert_eq!(second_report.transactions_reconciled, 1);
                let mut namespace = restarted_namespace.lock().unwrap();
                assert!(namespace.lookup(ROOT_INO, "source.txt").is_none());
                assert_eq!(
                    namespace.lookup(ROOT_INO, "destination.txt").unwrap().ino.0,
                    namespace.ino_for_record_id(source_record).unwrap()
                );
                assert!(namespace.ino_for_record_id(replaced_record).is_none());
                drop(namespace);
                assert_eq!(
                    restarted_history
                        .snapshot(source_record)
                        .unwrap()
                        .versions
                        .len(),
                    2
                );
                assert_eq!(
                    restarted_history
                        .snapshot(replaced_record)
                        .unwrap()
                        .versions
                        .len(),
                    2
                );
                assert!(
                    crate::record_transaction::RecordTransactionJournal::open(&journal_path)
                        .unwrap()
                        .entries()
                        .next()
                        .is_none(),
                    "successful remote projection removes its fully committed journal intent"
                );
                let persisted_after = std::fs::read(&namespace_path).unwrap();
                let mut durable_namespace =
                    Namespace::from_bytes("remote-transaction-durable", &persisted_after).unwrap();
                assert!(durable_namespace.lookup(ROOT_INO, "source.txt").is_none());
                assert_eq!(
                    durable_namespace
                        .entry_at("destination.txt")
                        .map(|entry| entry.record_id),
                    Some(source_record)
                );
                assert!(
                    durable_namespace
                        .ino_for_record_id(replaced_record)
                        .is_none()
                );

                // Re-running startup reconciliation is an exact replay, not a
                // second history or namespace transition.
                restarted_materializer
                    .reconcile_repository(&restarted_repository)
                    .unwrap();
                assert_eq!(
                    restarted_history
                        .snapshot(source_record)
                        .unwrap()
                        .versions
                        .len(),
                    2
                );
                assert_eq!(
                    restarted_history
                        .snapshot(replaced_record)
                        .unwrap()
                        .versions
                        .len(),
                    2
                );
            }
        }
        // Phase `remote_completed_merge_materializes_exact_revision_and_replays_after_restart`.
        {
            for codec in [
                ConflictMergeCodecV1::PlainUtf8ThreeWayV1,
                ConflictMergeCodecV1::QdocAutomergeV1,
            ] {
                remote_completed_merge_case(codec);
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "The shared UTF-8 and Automerge merge fixture checks exact revision projection, history resolution, journal cleanup, and restart replay."
    )] // Shared end-to-end merge/restart scenario helper.
    fn remote_completed_merge_case(codec: ConflictMergeCodecV1) {
        use crate::record_replication::{RecordAction, RecordAuthorizer};
        use crate::record_repository::{RecordRepository, RecordRepositoryKey};

        #[derive(Clone, Copy)]
        struct AllowActor(FileActor);
        impl RecordAuthorizer for AllowActor {
            fn actor_is_certified(&self, actor: &FileActor) -> bool {
                *actor == self.0
            }
            fn allows(
                &self,
                actor: &FileActor,
                _record_id: FileRecordId,
                _action: RecordAction,
            ) -> bool {
                *actor == self.0
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let workspace = Uuid::new_v4();
        let device = SigningKey::from_bytes(&[51; 32]);
        let actor = FileActor {
            person: Ed25519PublicKey::from_bytes([52; 32]),
            device: Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
        };
        let authorizer = AllowActor(actor);
        let mut initial_namespace = Namespace::new("remote-merge").unwrap();
        let ino = initial_namespace
            .create_file(
                ROOT_INO,
                "merge.txt",
                Some(&iroh_blobs::Hash::from([2; 32])),
                9,
                1,
            )
            .unwrap();
        let record_id = initial_namespace.record_id_for(ino).unwrap();
        let root_record = initial_namespace.record_id_for(ROOT_INO).unwrap();
        let base = FileVersionV1 {
            id: FileVersionId::new(),
            record_id,
            parents: Vec::new(),
            actor,
            timestamp_ms: 1,
            content_hash: Some(ContentHash([1; 32])),
            metadata_hash: ContentHash([11; 32]),
            provenance: FileProvenance::Create,
        };
        let current = FileVersionV1 {
            id: FileVersionId::new(),
            record_id,
            parents: vec![base.id],
            actor,
            timestamp_ms: 2,
            content_hash: Some(ContentHash([2; 32])),
            metadata_hash: ContentHash([12; 32]),
            provenance: FileProvenance::Edit,
        };
        let other = FileVersionV1 {
            id: FileVersionId::new(),
            record_id,
            parents: vec![base.id],
            actor,
            timestamp_ms: 3,
            content_hash: Some(ContentHash([3; 32])),
            metadata_hash: ContentHash([13; 32]),
            provenance: FileProvenance::Edit,
        };
        let conflict = FileConflictV1 {
            id: Uuid::new_v4(),
            record_id,
            versions: vec![current.id, other.id],
            status: ConflictStatus::Open,
            resolution: None,
        };
        let current_revision = ContentRevisionV1 {
            schema_version: collab_types::CONTENT_REVISION_VERSION,
            record_id,
            version_id: current.id,
            content_hash: current.content_hash,
            byte_len: 9,
            metadata_hash: current.metadata_hash,
        };
        assert!(initial_namespace.set_content_revision(ino, &current_revision, 2));
        let namespace_path = temp.path().join("namespace.bin");
        tilo_files::native::atomic::write_creating_parent(
            &namespace_path,
            &initial_namespace.save(),
        )
        .unwrap();
        let namespace = Arc::new(Mutex::new(initial_namespace));
        let history_path = temp.path().join("history.json");
        let history = Arc::new(FileHistoryStore::open(&history_path).unwrap());
        let repository_path = temp.path().join("records");
        let repository = RecordRepository::open(&repository_path).unwrap();
        let key = RecordRepositoryKey {
            workspace_id: workspace,
            record_id,
        };
        let initial = [
            RecordEnvelopeV1::signed(
                record_id,
                actor,
                1,
                RecordOperationV1::SetBinding(FileBindingV1 {
                    schema_version: FILE_BINDING_VERSION,
                    record_id,
                    parent: Some(root_record),
                    name: "merge.txt".into(),
                    kind: FileNodeKind::File,
                    tombstoned: false,
                    actor,
                    observed_at_ms: 1,
                }),
                &device,
            )
            .unwrap(),
            RecordEnvelopeV1::signed(
                record_id,
                actor,
                2,
                RecordOperationV1::AppendVersion(base.clone()),
                &device,
            )
            .unwrap(),
            RecordEnvelopeV1::signed(
                record_id,
                actor,
                3,
                RecordOperationV1::AppendVersion(current.clone()),
                &device,
            )
            .unwrap(),
            RecordEnvelopeV1::signed(
                record_id,
                actor,
                4,
                RecordOperationV1::AppendVersion(other),
                &device,
            )
            .unwrap(),
            RecordEnvelopeV1::signed(
                record_id,
                actor,
                5,
                RecordOperationV1::SetContentRevision(current_revision),
                &device,
            )
            .unwrap(),
            RecordEnvelopeV1::signed(
                record_id,
                actor,
                6,
                RecordOperationV1::AppendConflict(conflict.clone()),
                &device,
            )
            .unwrap(),
        ];
        repository.apply_batch(key, &initial, &authorizer).unwrap();
        let journal_path = temp.path().join("journal");
        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(&journal_path).unwrap(),
        ));
        let materializer = RecordMaterializer::new(
            workspace,
            Arc::clone(&namespace),
            Some(Arc::clone(&history)),
        )
        .with_transaction_recovery(Arc::clone(&journal), namespace_path.clone());
        materializer.reconcile_repository(&repository).unwrap();

        let result = FileVersionV1 {
            id: FileVersionId::new(),
            record_id,
            parents: conflict.versions.clone(),
            actor,
            timestamp_ms: 4,
            content_hash: Some(ContentHash([4; 32])),
            metadata_hash: ContentHash([14; 32]),
            provenance: FileProvenance::ConflictResolution,
        };
        let result_revision = ContentRevisionV1 {
            schema_version: collab_types::CONTENT_REVISION_VERSION,
            record_id,
            version_id: result.id,
            content_hash: result.content_hash,
            byte_len: 15,
            metadata_hash: result.metadata_hash,
        };
        let mut participant = FileMutationParticipant {
            record_id,
            role: FileMutationParticipantRole::MergeSource,
            expected_version: current.id,
            proposed_binding: FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                record_id,
                parent: Some(root_record),
                name: "merge.txt".into(),
                kind: FileNodeKind::File,
                tombstoned: false,
                actor,
                observed_at_ms: 4,
            },
            proposed_version: result.clone(),
            proposed_content_revision: Some(result_revision),
            projection_hash: ContentHash([0; 32]),
        };
        participant.projection_hash = participant.computed_projection_hash().unwrap();
        let transaction = FileMutationTransaction::signed(
            FileMutationTransactionManifest {
                schema_version: collab_types::files::FILE_MUTATION_TRANSACTION_VERSION_V2,
                transaction_id: Uuid::new_v4(),
                workspace_id: workspace,
                actor,
                timestamp_ms: 4,
                operation: FileMutationOperation::MergeContentConflict {
                    source_record: record_id,
                    parent_record: root_record,
                    source_name: "merge.txt".into(),
                    conflict_id: conflict.id,
                    conflict_versions: conflict.versions,
                    current_version: current.id,
                    base_version: matches!(codec, ConflictMergeCodecV1::PlainUtf8ThreeWayV1)
                        .then_some(base.id),
                    codec,
                },
                participants: vec![participant],
            },
            &device,
        )
        .unwrap();
        repository
            .apply(
                key,
                &RecordEnvelopeV1::signed(
                    record_id,
                    actor,
                    7,
                    RecordOperationV1::PrepareTransaction(transaction.clone()),
                    &device,
                )
                .unwrap(),
                &authorizer,
            )
            .unwrap();
        assert_eq!(
            materializer
                .reconcile_repository(&repository)
                .unwrap()
                .transactions_reconciled,
            0
        );
        assert_eq!(
            namespace.lock().unwrap().content_revision_for(ino),
            Some(current_revision)
        );
        assert!(
            history.snapshot(record_id).unwrap().conflicts[0]
                .resolution
                .is_none()
        );

        repository
            .apply(
                key,
                &RecordEnvelopeV1::signed(
                    record_id,
                    actor,
                    8,
                    RecordOperationV1::CommitTransaction {
                        transaction_id: transaction.manifest.transaction_id,
                    },
                    &device,
                )
                .unwrap(),
                &authorizer,
            )
            .unwrap();
        assert_eq!(
            materializer
                .reconcile_repository(&repository)
                .unwrap()
                .transactions_reconciled,
            1
        );
        assert_eq!(
            namespace.lock().unwrap().content_revision_for(ino),
            Some(result_revision)
        );
        let state = history.snapshot(record_id).unwrap();
        assert_eq!(
            state
                .versions
                .iter()
                .filter(|value| **value == result)
                .count(),
            1
        );
        assert_eq!(
            state.conflicts[0].resolution.as_ref().unwrap().strategy,
            ConflictStrategy::Merge
        );
        assert!(journal.lock().unwrap().entries().next().is_none());

        drop(materializer);
        drop(namespace);
        drop(history);
        drop(repository);
        let restarted_namespace = Arc::new(Mutex::new(
            Namespace::from_bytes(
                "remote-merge-restart",
                &std::fs::read(&namespace_path).unwrap(),
            )
            .unwrap(),
        ));
        let restarted_history = Arc::new(FileHistoryStore::open(&history_path).unwrap());
        let restarted_repository = RecordRepository::open(&repository_path).unwrap();
        let restarted_journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(&journal_path).unwrap(),
        ));
        let restarted = RecordMaterializer::new(
            workspace,
            Arc::clone(&restarted_namespace),
            Some(Arc::clone(&restarted_history)),
        )
        .with_transaction_recovery(Arc::clone(&restarted_journal), namespace_path);
        restarted
            .reconcile_repository(&restarted_repository)
            .unwrap();
        assert_eq!(
            restarted_namespace
                .lock()
                .unwrap()
                .content_revision_for(ino),
            Some(result_revision)
        );
        assert_eq!(
            restarted_history
                .snapshot(record_id)
                .unwrap()
                .versions
                .iter()
                .filter(|value| **value == result)
                .count(),
            1
        );
        assert!(restarted_journal.lock().unwrap().entries().next().is_none());
    }
}
