//! Exact-record projection from accepted replication envelopes into the live
//! namespace and immutable history store.
//!
//! Materialization never resolves a path or creates a node. Stable record
//! identity is looked up only in the live namespace, so rename is transparent
//! and tombstoned or unknown records fail closed.

use std::sync::{Arc, Mutex};

use fs_protocol::{FileNodeKind, FileRecordId};
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
    /// Signed bindings that changed the live tree (created, moved, deleted or
    /// renamed a conflict sibling).
    pub bindings_projected: usize,
    /// Records whose signed parent is not live yet. They stay in the log and
    /// are retried once the namespace changes.
    pub deferred_records: usize,
    /// Records with no tree placement that are not explained by a valid
    /// tombstone: unknown identity, duplicate identity, or a malformed
    /// binding. They remain fail-closed.
    pub unknown_or_tombstoned_records: usize,
    pub transactions_reconciled: usize,
}

/// What projecting one record's converged signed binding found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BindingPlacement {
    /// The record's own binding is absent; any live node is legacy state.
    Absent,
    /// The record is live after projection (including an idempotent replay).
    Live,
    /// The record's latest signed binding tombstones it.
    Tombstoned,
    /// The signed parent record has not arrived (or is not live) yet.
    Deferred,
    /// The signed binding cannot be projected safely and stays out of the tree.
    Rejected,
    /// A completed transaction owns this record's structure; the binding
    /// phase may only create a missing pre-transaction node.
    TransactionOwned,
}

/// Dependency rounds for child-before-parent record arrival. Each round
/// resolves every record whose parent is already live, so a balanced tree
/// settles in O(log n) rounds and a worst-case chain in O(n).
const MAX_RECONCILE_ROUNDS: usize = 4_096;

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
        self.namespace
            .lock()
            .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?
            .begin_incremental_batch();
        let mut report = RecordReconciliationReport::default();
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
        let mut all = keys.to_vec();
        all.extend(more);
        let projected = self.reconcile_keys(repository, &all, &mut report);
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

    fn mark_unplaced(&self, key: crate::record_repository::RecordRepositoryKey) {
        if let Ok(mut unplaced) = self.unplaced.lock() {
            unplaced.insert(key);
        }
    }

    fn clear_unplaced(&self, key: crate::record_repository::RecordRepositoryKey) {
        if let Ok(mut unplaced) = self.unplaced.lock() {
            unplaced.remove(&key);
        }
    }

    /// Project the converged signed binding of one record. This is the only
    /// projection that may create, move, delete or conflict-rename a node.
    fn project_key_binding(
        &self,
        repository: &RecordRepository,
        key: crate::record_repository::RecordRepositoryKey,
        transaction_owned: &std::collections::BTreeSet<
            crate::record_repository::RecordRepositoryKey,
        >,
        report: &mut RecordReconciliationReport,
    ) -> Result<BindingPlacement, RecordMaterializerError> {
        if transaction_owned.contains(&key) {
            return Ok(BindingPlacement::TransactionOwned);
        }
        let Some(binding) = repository.converged_binding(key) else {
            return Ok(BindingPlacement::Absent);
        };
        let Some(binding) = binding else {
            return Ok(BindingPlacement::Absent);
        };
        if binding.kind == FileNodeKind::Directory && repository.record_has_file_content(key) {
            // A record whose signed history carries file content is never a
            // directory, even if its newest binding says so. Creating the node
            // from that malformed latest binding would turn a file record into
            // a directory and then strand its content operations.
            return Ok(BindingPlacement::Rejected);
        }
        let mut namespace = self
            .namespace
            .lock()
            .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?;
        let changed = match namespace.apply_signed_binding(&binding) {
            Ok(changed) => changed,
            Err(crate::namespace::BindingProjectionError::UnknownParent) => {
                return Ok(BindingPlacement::Deferred);
            }
            Err(_) => return Ok(BindingPlacement::Rejected),
        };
        if changed {
            report.bindings_projected = report.bindings_projected.saturating_add(1);
        }
        let live = namespace
            .ensure_ino_for_record_id(binding.record_id)
            .is_some();
        // A namespace batch (a pulled changeset) collects this delta and
        // broadcasts it once; a repository reconciliation persists it here.
        let delta = changed.then(|| namespace.save_incremental());
        drop(namespace);
        if let Some(delta) = delta
            && !delta.is_empty()
            && let Ok(sync) = self.sync.read()
            && let Some(sync) = sync.as_ref()
        {
            sync.broadcast_delta(delta);
        }
        if live {
            Ok(BindingPlacement::Live)
        } else if binding.tombstoned {
            Ok(BindingPlacement::Tombstoned)
        } else {
            Ok(BindingPlacement::Rejected)
        }
    }

    /// Project one record's immutable history and, when it has a live node,
    /// its converged content and metadata. History is record-scoped, so it is
    /// preserved for a tombstoned record and for one whose parent has not
    /// arrived yet; content and path metadata are not guessed into a tree.
    #[allow(
        clippy::too_many_lines,
        reason = "history, live content, metadata and conflict projection share the block that marks a record placed, deferred or immutably stored"
    )]
    fn reconcile_key_content(
        &self,
        repository: &RecordRepository,
        key: crate::record_repository::RecordRepositoryKey,
        report: &mut RecordReconciliationReport,
    ) -> Result<(), RecordMaterializerError> {
        let Some(batch) = repository.reconciliation_batch(key) else {
            self.clear_unplaced(key);
            return Ok(());
        };
        debug_assert_eq!(batch.key, key);
        let (live, namespace_revision) = {
            let mut namespace = self
                .namespace
                .lock()
                .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?;
            let ino = namespace.ensure_ino_for_record_id(key.record_id);
            let revision = ino.and_then(|ino| namespace.content_revision_for(ino));
            drop(namespace);
            (ino.is_some(), revision)
        };
        let transaction_revision = repository.transaction_owned_content_revision(key);
        let transaction_owns_content =
            transaction_revision.is_some() && namespace_revision == transaction_revision;
        let binding_present = batch.snapshot.binding.is_some();
        let tombstoned = batch
            .snapshot
            .binding
            .as_ref()
            .is_some_and(|binding| binding.tombstoned);
        if live {
            self.clear_unplaced(key);
        } else if !binding_present {
            report.unknown_or_tombstoned_records =
                report.unknown_or_tombstoned_records.saturating_add(1);
            self.mark_unplaced(key);
        } else if !tombstoned {
            // A known record whose parent is not live yet, or whose binding
            // was rejected. Both remain in the signed log.
            self.mark_unplaced(key);
        }

        for envelope in batch
            .envelopes
            .iter()
            .filter(|envelope| matches!(&envelope.operation, RecordOperationV1::AppendVersion(_)))
        {
            match self.apply_exact(key.record_id, envelope, &batch.snapshot, Some(repository)) {
                Ok(()) => {
                    report.operations_replayed = report.operations_replayed.saturating_add(1);
                }
                Err(RecordMaterializerError::DeferredPlacement) => {}
                Err(error) => return Err(error),
            }
        }
        if live && !transaction_owns_content {
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
                    Err(
                        RecordMaterializerError::UnknownOrTombstonedRecord(_)
                        | RecordMaterializerError::NotAFile,
                    ) => {
                        report.unknown_or_tombstoned_records =
                            report.unknown_or_tombstoned_records.saturating_add(1);
                        self.mark_unplaced(key);
                    }
                    Err(error) => return Err(error),
                }
            }
            // Project converged metadata/previews after the content revision
            // exists; losing metadata arrivals remain only in the signed log.
            if let Some(envelope) =
                batch.envelopes.iter().rev().find(|envelope| {
                    matches!(&envelope.operation, RecordOperationV1::SetMetadata(_))
                })
            {
                match self.apply_exact(key.record_id, envelope, &batch.snapshot, Some(repository)) {
                    Ok(()) => {
                        report.operations_replayed = report.operations_replayed.saturating_add(1);
                    }
                    Err(
                        RecordMaterializerError::UnknownOrTombstonedRecord(_)
                        | RecordMaterializerError::NotAFile,
                    ) => {
                        report.unknown_or_tombstoned_records =
                            report.unknown_or_tombstoned_records.saturating_add(1);
                        self.mark_unplaced(key);
                    }
                    Err(error) => return Err(error),
                }
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
                        self.mark_unplaced(key);
                        break;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        report.records_reconciled = report.records_reconciled.saturating_add(1);
        Ok(())
    }

    /// Project `keys` in dependency rounds: every record whose signed parent
    /// is already live is placed in a round, and records that arrived before
    /// their parent are retried in the next one. Content and immutable history
    /// are projected once after the tree has settled.
    fn reconcile_keys(
        &self,
        repository: &RecordRepository,
        keys: &[crate::record_repository::RecordRepositoryKey],
        report: &mut RecordReconciliationReport,
    ) -> Result<(), RecordMaterializerError> {
        let unique = keys
            .iter()
            .copied()
            .filter(|key| repository.converged_binding(*key).is_some())
            .collect::<std::collections::BTreeSet<_>>();
        let transaction_owned = repository.transaction_owned_bindings(self.workspace_id);
        let mut pending = unique.clone();
        let mut rounds = 0_usize;
        while !pending.is_empty() && rounds < MAX_RECONCILE_ROUNDS {
            rounds = rounds.saturating_add(1);
            let mut next = std::collections::BTreeSet::new();
            for key in &pending {
                match self.project_key_binding(repository, *key, &transaction_owned, report)? {
                    BindingPlacement::Deferred => {
                        next.insert(*key);
                    }
                    BindingPlacement::Rejected => {
                        report.unknown_or_tombstoned_records =
                            report.unknown_or_tombstoned_records.saturating_add(1);
                        self.mark_unplaced(*key);
                    }
                    BindingPlacement::Absent
                    | BindingPlacement::Live
                    | BindingPlacement::Tombstoned
                    | BindingPlacement::TransactionOwned => {}
                }
            }
            let progressed = next.len() < pending.len();
            pending = next;
            if !progressed {
                break;
            }
        }
        if !pending.is_empty() {
            report.deferred_records = report.deferred_records.saturating_add(pending.len());
            for key in &pending {
                self.mark_unplaced(*key);
            }
        }
        // A transaction participant may not exist yet in a log-only rebuild.
        // Create its pre-transaction node from its ordinary binding; the
        // transaction projector owns every later move or tombstone.
        for key in &transaction_owned {
            if !unique.contains(key) {
                continue;
            }
            self.ensure_transaction_participant_node(repository, *key, report)?;
        }
        for key in &unique {
            self.reconcile_key_content(repository, *key, report)?;
        }
        Ok(())
    }

    fn ensure_transaction_participant_node(
        &self,
        repository: &RecordRepository,
        key: crate::record_repository::RecordRepositoryKey,
        report: &mut RecordReconciliationReport,
    ) -> Result<(), RecordMaterializerError> {
        let Some(Some(binding)) = repository.converged_binding(key) else {
            return Ok(());
        };
        let mut namespace = self
            .namespace
            .lock()
            .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?;
        match namespace.apply_signed_binding_create_only(&binding) {
            Ok(false) => Ok(()),
            Ok(true) => {
                report.bindings_projected = report.bindings_projected.saturating_add(1);
                let delta = namespace.save_incremental();
                drop(namespace);
                if !delta.is_empty()
                    && let Ok(sync) = self.sync.read()
                    && let Some(sync) = sync.as_ref()
                {
                    sync.broadcast_delta(delta);
                }
                Ok(())
            }
            Err(crate::namespace::BindingProjectionError::UnknownParent) => {
                report.deferred_records = report.deferred_records.saturating_add(1);
                self.mark_unplaced(key);
                Ok(())
            }
            Err(_) => {
                report.unknown_or_tombstoned_records =
                    report.unknown_or_tombstoned_records.saturating_add(1);
                self.mark_unplaced(key);
                Ok(())
            }
        }
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
        // The signed repository is the durable history. Replace an empty or
        // legacy-only derived index with its complete projection before any
        // record-level replay; later passes are no-ops because subsequent
        // accepted operations fold into the index as they arrive.
        if let Some(history) = &self.history {
            history
                .rebuild_from_repository(repository, self.workspace_id)
                .map_err(|error| match error {
                    HistoryError::Io(_) | HistoryError::Repository(_) => {
                        RecordMaterializerError::HistoryUnavailable
                    }
                    _ => RecordMaterializerError::HistoryRejected,
                })?;
        }
        let mut report = RecordReconciliationReport::default();
        let keys = repository.reconciliation_keys(self.workspace_id);
        self.reconcile_keys(repository, &keys, &mut report)?;
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
            if repository.transaction_is_superseded(self.workspace_id, &transaction) {
                // A later ordinary binding moved every participant away from
                // this transaction. Keep its immutable versions, but never
                // replay its structure over the newer placement.
                history
                    .append_batch(&versions)
                    .map_err(|error| match error {
                        HistoryError::Io(_) => RecordMaterializerError::HistoryUnavailable,
                        _ => RecordMaterializerError::HistoryRejected,
                    })?;
                let delta = {
                    let mut namespace = self
                        .namespace
                        .lock()
                        .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?;
                    let mut changed = false;
                    for participant in &transaction.manifest.participants {
                        let Some(revision) = participant.proposed_content_revision else {
                            continue;
                        };
                        let Some(ino) = namespace.ensure_ino_for_record_id(participant.record_id)
                        else {
                            continue;
                        };
                        if namespace.content_revision_for(ino) != Some(revision) {
                            let _ = namespace.set_content_revision(
                                ino,
                                &revision,
                                transaction.manifest.timestamp_ms,
                            );
                            changed = true;
                        }
                    }
                    changed.then(|| namespace.save_incremental())
                };
                if let Some(delta) = delta
                    && !delta.is_empty()
                    && let Ok(sync) = self.sync.read()
                    && let Some(sync) = sync.as_ref()
                {
                    sync.broadcast_delta(delta);
                }
                report.transactions_reconciled = report.transactions_reconciled.saturating_add(1);
                continue;
            }
            if repository.transaction_is_partially_superseded(self.workspace_id, &transaction) {
                // The accepted transaction is atomic for the participants it
                // still owns. A later rename, move, restore or edit of one
                // participant must not force every other participant to be
                // superseded before this replay can run.
                self.reconcile_partially_superseded_transaction(
                    repository,
                    &transaction,
                    history,
                    report,
                )?;
                continue;
            }
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
                    fs_protocol::FileMutationOperation::RestoreContent { .. }
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
                    fs_protocol::FileMutationOperation::RestoreContent { .. }
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
                if let fs_protocol::FileMutationOperation::MergeContentConflict {
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
                            &fs_protocol::ConflictResolution {
                                strategy: fs_protocol::ConflictStrategy::Merge,
                                result: source.proposed_version.id,
                                actor: transaction.manifest.actor,
                                timestamp_ms: transaction.manifest.timestamp_ms,
                            },
                        )
                        .map_err(|_| RecordMaterializerError::HistoryRejected)?;
                }
                let delta = if matches!(
                    transaction.manifest.operation,
                    fs_protocol::FileMutationOperation::RestoreContent { .. }
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

    /// Project a completed transaction after an ordinary per-record edit,
    /// rename, move or restore superseded only some of its participants.
    ///
    /// Every participant's *current* signed projection is applied: the
    /// transaction's proposal where it still owns the structure, and the
    /// later ordinary binding where it does not. The transaction's immutable
    /// versions and any content it still owns stay in history and content.
    #[allow(
        clippy::too_many_lines,
        reason = "history, per-participant current-state bindings, content revisions and dependency retries stay together so partial supersession cannot rewind an unsuperseded participant"
    )]
    fn reconcile_partially_superseded_transaction(
        &self,
        repository: &RecordRepository,
        transaction: &fs_protocol::FileMutationTransaction,
        history: &FileHistoryStore,
        report: &mut RecordReconciliationReport,
    ) -> Result<(), RecordMaterializerError> {
        let versions = transaction
            .manifest
            .participants
            .iter()
            .map(|participant| participant.proposed_version.clone())
            .collect::<Vec<_>>();
        history
            .append_batch(&versions)
            .map_err(|error| match error {
                HistoryError::Io(_) => RecordMaterializerError::HistoryUnavailable,
                _ => RecordMaterializerError::HistoryRejected,
            })?;
        if let Some((source_record, conflict_id, strategy, role)) =
            match &transaction.manifest.operation {
                fs_protocol::FileMutationOperation::KeepBothContentConflict {
                    source_record,
                    conflict_id,
                    ..
                } => Some((
                    *source_record,
                    *conflict_id,
                    fs_protocol::ConflictStrategy::KeepBoth,
                    fs_protocol::FileMutationParticipantRole::KeepBothSource,
                )),
                fs_protocol::FileMutationOperation::MergeContentConflict {
                    source_record,
                    conflict_id,
                    ..
                } => Some((
                    *source_record,
                    *conflict_id,
                    fs_protocol::ConflictStrategy::Merge,
                    fs_protocol::FileMutationParticipantRole::MergeSource,
                )),
                fs_protocol::FileMutationOperation::ReplacementRename { .. }
                | fs_protocol::FileMutationOperation::AtomicSave { .. }
                | fs_protocol::FileMutationOperation::RestoreContent { .. } => None,
            }
        {
            let source = transaction
                .manifest
                .participants
                .iter()
                .find(|participant| participant.role == role)
                .ok_or(RecordMaterializerError::TransactionRejected)?;
            history
                .resolve(
                    source_record,
                    conflict_id,
                    &fs_protocol::ConflictResolution {
                        strategy,
                        result: source.proposed_version.id,
                        actor: transaction.manifest.actor,
                        timestamp_ms: transaction.manifest.timestamp_ms,
                    },
                )
                .map_err(|_| RecordMaterializerError::HistoryRejected)?;
        }

        let mut pending = transaction
            .manifest
            .participants
            .iter()
            .map(|participant| {
                repository
                    .projection_lease(crate::record_repository::RecordRepositoryKey {
                        workspace_id: self.workspace_id,
                        record_id: participant.record_id,
                    })
                    .map(|lease| lease.snapshot)
                    .map_err(|_| RecordMaterializerError::TransactionRejected)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut changed = false;
        let mut rounds = 0_usize;
        while !pending.is_empty() && rounds < MAX_RECONCILE_ROUNDS {
            rounds = rounds.saturating_add(1);
            let before = pending.len();
            let mut next = Vec::new();
            for snapshot in pending {
                let record_id = snapshot.record_id;
                let Some(binding) = snapshot.binding.clone() else {
                    continue;
                };
                let mut namespace = self
                    .namespace
                    .lock()
                    .map_err(|_| RecordMaterializerError::NamespaceUnavailable)?;
                match namespace.apply_signed_binding(&binding) {
                    Ok(applied) => {
                        changed |= applied;
                        if !binding.tombstoned
                            && let Some(ino) = namespace.ensure_ino_for_record_id(record_id)
                            && let Some(revision) = snapshot.content_revision.as_ref()
                            && namespace.content_revision_for(ino) != Some(*revision)
                            && namespace.set_content_revision(
                                ino,
                                revision,
                                transaction.manifest.timestamp_ms,
                            )
                        {
                            changed = true;
                        }
                    }
                    Err(crate::namespace::BindingProjectionError::UnknownParent) => {
                        next.push(snapshot);
                    }
                    Err(_) => return Err(RecordMaterializerError::TransactionRejected),
                }
            }
            if next.len() == before {
                report.deferred_records = report.deferred_records.saturating_add(next.len());
                for snapshot in &next {
                    self.mark_unplaced(crate::record_repository::RecordRepositoryKey {
                        workspace_id: self.workspace_id,
                        record_id: snapshot.record_id,
                    });
                }
                break;
            }
            pending = next;
        }
        if let Some(delta) = changed.then(|| {
            self.namespace
                .lock()
                .map(|mut namespace| namespace.save_incremental())
                .unwrap_or_default()
        }) && !delta.is_empty()
            && let Ok(sync) = self.sync.read()
            && let Some(sync) = sync.as_ref()
        {
            sync.broadcast_delta(delta);
        }
        report.transactions_reconciled = report.transactions_reconciled.saturating_add(1);
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
        // Who caused a version lives in the signed record alone: history
        // reads read it from there, so nothing projects into the namespace or
        // the history store, and a record deleted since still keeps it.
        if matches!(envelope.operation, RecordOperationV1::AttributeVersion(_)) {
            return Ok(());
        }

        // The node, if any, is resolved by the operation that needs one.
        // Immutable history is record-scoped and stays available after a
        // tombstone or while a child waits for its signed parent.
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
            let transaction_owned = repository.is_some_and(|repository| {
                repository.completed_transaction_owns_binding(
                    crate::record_repository::RecordRepositoryKey {
                        workspace_id: self.workspace_id,
                        record_id,
                    },
                )
            });
            let applied = if transaction_owned {
                namespace.apply_signed_binding_create_only(binding)
            } else {
                namespace.apply_signed_binding(binding)
            };
            let changed = applied.map_err(|error| match error {
                crate::namespace::BindingProjectionError::UnknownParent => {
                    RecordMaterializerError::DeferredPlacement
                }
                _ => RecordMaterializerError::BindingMismatch,
            })?;
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
        match &envelope.operation {
            RecordOperationV1::SetBinding(_) => {
                // Handled above from the converged snapshot.
            }
            RecordOperationV1::SetMetadata(_) => {
                let ino = namespace.ensure_ino_for_record_id(record_id).ok_or(
                    RecordMaterializerError::UnknownOrTombstonedRecord(record_id),
                )?;
                let kind = namespace.live_kind(ino).ok_or(
                    RecordMaterializerError::UnknownOrTombstonedRecord(record_id),
                )?;
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
                            .filter(|preview| preview.status == fs_protocol::PreviewStatus::Ready)
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
                let ino = namespace.ensure_ino_for_record_id(record_id).ok_or(
                    RecordMaterializerError::UnknownOrTombstonedRecord(record_id),
                )?;
                let kind = namespace.live_kind(ino).ok_or(
                    RecordMaterializerError::UnknownOrTombstonedRecord(record_id),
                )?;
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
                                .resolve(record_id, conflict_id, resolution)
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
                            .resolve(record_id, conflict_id, &resolution)
                            .map_err(history_projection_error)?;
                    }
                } else {
                    history
                        .record_conflict(conflict)
                        .map_err(history_projection_error)?;
                }
            }
            RecordOperationV1::PrepareTransaction(_)
            | RecordOperationV1::CommitTransaction { .. }
            | RecordOperationV1::AttributeVersion(_) => {
                // A record-local arrival is deliberately invisible. Only the
                // repository-wide complete-participant materializer may
                // project a transaction; an attribution never projects.
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
        match result {
            // The repository already authenticated and authorized this
            // envelope. A record whose parent has not arrived, or whose live
            // node is not there yet, is durable and queued for the next
            // namespace reconciliation instead of failing the writer.
            Ok(()) => Ok(()),
            Err(
                RecordMaterializerError::DeferredPlacement
                | RecordMaterializerError::UnknownOrTombstonedRecord(_),
            ) => {
                self.mark_unplaced(crate::record_repository::RecordRepositoryKey {
                    workspace_id: self.workspace_id,
                    record_id: scope.record_id,
                });
                Ok(())
            }
            Err(
                RecordMaterializerError::WrongWorkspace
                | RecordMaterializerError::BindingMismatch
                | RecordMaterializerError::InvalidEnvelope
                | RecordMaterializerError::MissingConvergedMetadata
                | RecordMaterializerError::MissingConvergedContentRevision
                | RecordMaterializerError::NotAFile
                | RecordMaterializerError::HistoryRejected
                | RecordMaterializerError::TransactionRejected,
            ) => Err(RecordMutationSinkError::Rejected),
            Err(
                RecordMaterializerError::NamespaceUnavailable
                | RecordMaterializerError::HistoryUnavailable
                | RecordMaterializerError::TransactionRecoveryUnavailable,
            ) => Err(RecordMutationSinkError::Unavailable),
        }
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
                | RecordMaterializerError::DeferredPlacement
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
    #[error("record placement waits for its signed parent directory")]
    DeferredPlacement,
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
mod tests;
