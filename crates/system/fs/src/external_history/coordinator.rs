//! Same-daemon external native replacement coordination. All positive tokens
//! below are private and retain actual admitted source/capture capabilities.
//! Native metadata is never an immutable content archive.
use super::pending_source::AuthenticatedPendingSource;
use super::{ExternalSourceAdmission, refused};
use crate::{
    history::FileHistoryStore,
    namespace::Namespace,
    record_log::{ExternalCanonicalIds, ExternalReservationInput, PreviousCanonicalIds},
    record_transaction::{
        ExternalNativeEffectBinding, RecordTransactionJournal, RecordTransactionJournalEntry,
        RecordTransactionPhase,
    },
};
use ed25519_dalek::Signer;
use fs_protocol::files::FILE_METADATA_VERSION;
use fs_protocol::{
    CONTENT_REVISION_VERSION, ContentHash, ContentRevisionV1, FILE_BINDING_VERSION, FileActor,
    FileBindingV1, FileMetadataV1, FileMutationOperation, FileMutationParticipant,
    FileMutationParticipantRole, FileMutationTransactionManifest, FileNodeKind, FileProvenance,
    FileRecordId, FileVersionId, FileVersionV1, IndexStatus,
};
use runtime_fs::{
    directory_backend::ReplacementObservation,
    directory_client::ReplacementPinClient,
    external_history::{CommittedReceipt, PreparedReceipt},
};
use std::{
    cell::RefCell,
    io,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};

/// These are the existing mount's actual stores, never a caller-selected path
/// or a second publisher/history backend.
pub struct ExternalHistoryRuntime {
    pub(super) history: Arc<FileHistoryStore>,
    pub(super) journal: Arc<Mutex<RecordTransactionJournal>>,
    pub(super) namespace: Arc<Mutex<Namespace>>,
    pub(super) namespace_path: PathBuf,
}
impl ExternalHistoryRuntime {
    pub fn from_mount(
        history: Arc<FileHistoryStore>,
        journal: Arc<Mutex<RecordTransactionJournal>>,
        namespace: Arc<Mutex<Namespace>>,
        namespace_path: PathBuf,
    ) -> Self {
        Self {
            history,
            journal,
            namespace,
            namespace_path,
        }
    }
}
/// Carries live source and current same-enrollment daemon authority as well as
/// actual signed repository seed publications. No public or DTO constructor.
pub(crate) struct SeededExternalRecords {
    source: Rc<AuthenticatedPendingSource>,
    admission: Rc<RefCell<ExternalSourceAdmission>>,
    bindings: Vec<FileBindingV1>,
    versions: Vec<FileVersionV1>,
    ids: ExternalCanonicalIds,
}
impl SeededExternalRecords {
    pub(crate) fn check(&self) -> io::Result<()> {
        self.source.check_peer()?;
        let admission = self.admission.try_borrow().map_err(|_| refused())?;
        admission.factory.check_current()?;
        admission.helper.check_current()?;
        if admission.revoked || !admission.proof.shell.current() {
            return Err(refused());
        }
        Ok(())
    }
    pub(crate) fn bindings(&self) -> &[FileBindingV1] {
        &self.bindings
    }
    pub(crate) fn versions(&self) -> &[FileVersionV1] {
        &self.versions
    }
}
/// A completed native pin remains held throughout canonical publication and
/// source commit acknowledgement. The wire observation cannot construct this.
pub(crate) struct ObservedExternalReplacement {
    seed: Rc<SeededExternalRecords>,
    pin: ReplacementPinClient,
    observed: ReplacementObservation,
    prepared: PreparedReceipt,
    entry: RecordTransactionJournalEntry,
}
impl ObservedExternalReplacement {
    pub(crate) fn check_entry(&self, entry: &RecordTransactionJournalEntry) -> io::Result<()> {
        self.seed.check()?;
        if entry.transaction != self.entry.transaction
            || entry.participant_batches != self.entry.participant_batches
            || entry.external_native_effect != self.entry.external_native_effect
            || entry.external_native_effect.as_ref().is_none_or(|e| {
                e.operation_id.as_bytes() != &self.observed.operation
                    || e.source_owner != self.observed.source_owner
                    || e.source_identity != self.observed.source
                    || e.victim_identity != self.observed.victim
            })
        {
            return Err(refused());
        }
        entry.validate().map_err(io::Error::other)
    }
}
/// Kept only after the actual same source pin acknowledged signed history.
/// Later Saves use these canonical IDs plus current held native lineage; they
/// never restore identity from owner/dev/inode lookup.
pub(super) struct AcknowledgedReplacement {
    pub(super) receipt: CommittedReceipt,
    observed: Rc<ObservedExternalReplacement>,
}
impl AcknowledgedReplacement {
    /// Completed, signed-and-acknowledged owners may leave discovery only
    /// after their genuine source channel/kernel helper has retired. Unknown
    /// submitted effects stay in the owning lane and never enter this table.
    pub(super) fn discovery_current(&self) -> bool {
        self.observed.seed.source.check_peer().is_ok()
            && self
                .observed
                .seed
                .admission
                .try_borrow()
                .is_ok_and(|a| a.helper.check_current().is_ok())
    }

    // Only actual same-pin signed ownership acknowledgement can derive this.
    pub(super) fn read_lineage<'a>(
        &'a self,
        cancel: &'a std::sync::atomic::AtomicBool,
        deadline: std::time::Instant,
    ) -> super::LocalFuture<'a, io::Result<super::read_handle::ReadLineage>> {
        Box::pin(async move {
            ExternalSourceAdmission::refresh_shared(
                &self.observed.seed.admission,
                cancel,
                deadline,
            )
            .await?;
            self.observed.check_entry(&self.observed.entry)?;
            super::pending_source::within(cancel, deadline, async {
                self.observed.pin.check().await.map_err(io::Error::other)?;
                if self
                    .observed
                    .pin
                    .observe()
                    .await
                    .map_err(io::Error::other)?
                    != self.observed.observed
                {
                    return Err(refused());
                }
                Ok(())
            })
            .await?;
            self.observed.check_entry(&self.observed.entry)?;
            let admission = self
                .observed
                .seed
                .admission
                .try_borrow()
                .map_err(|_| refused())?;
            self.receipt
                .verify(&self.observed.prepared, &admission.context.verifier)?;
            let participant = self
                .observed
                .entry
                .transaction
                .manifest
                .participants
                .iter()
                .find(|p| {
                    matches!(
                        p.role,
                        FileMutationParticipantRole::Source
                            | FileMutationParticipantRole::AtomicSaveDocument
                    )
                })
                .ok_or_else(refused)?;
            let description = &self.observed.seed.source.description;
            Ok(super::read_handle::ReadLineage {
                observation: runtime_fs::directory_backend::HistoryReadObservation {
                    source_owner: self.receipt.source_owner,
                    root: description.destination().root_identity(),
                    parent: description.destination_parent_identity(),
                    parents: description.destination().parents().to_vec(),
                    name: description.destination().name().to_vec(),
                    identity: self.receipt.destination_identity,
                },
                binding: participant.proposed_binding.clone(),
                version: participant.proposed_version.id,
                person: admission.context.verifier.person,
                room: admission.context.verifier.personal_room.clone(),
            })
        })
    }
}
fn metadata(
    record_id: FileRecordId,
    facts: &runtime_fs::external_history::NativeReservationFacts,
    actor: FileActor,
    at: u64,
) -> io::Result<(FileMetadataV1, ContentHash)> {
    let metadata = FileMetadataV1 {
        schema_version: FILE_METADATA_VERSION,
        record_id,
        content_hash: None,
        byte_len: facts.byte_len,
        mime: None,
        previews: Vec::new(),
        latest_actor: Some(actor),
        observed_at_ms: at,
        index_status: IndexStatus::Pending,
        properties: None,
    };
    let hash = ContentHash(
        *blake3::hash(&postcard::to_allocvec(&metadata).map_err(io::Error::other)?).as_bytes(),
    );
    Ok((metadata, hash))
}
fn binding(
    id: FileRecordId,
    parent: Option<FileRecordId>,
    name: Vec<u8>,
    kind: FileNodeKind,
    actor: FileActor,
    at: u64,
) -> FileBindingV1 {
    FileBindingV1 {
        schema_version: FILE_BINDING_VERSION,
        record_id: id,
        parent,
        name: name.into(),
        kind,
        tombstoned: false,
        actor,
        observed_at_ms: at,
    }
}
impl SeededExternalRecords {
    /// Called only after receiving the real same-provider Pending capability
    /// under sealed expectation + worker QPE and refreshing this enrollment.
    #[expect(
        clippy::too_many_lines,
        reason = "one authenticated reservation seeds exact signed parent, temporary and victim lineage without inode-derived identity"
    )]
    pub(super) fn reserve(
        source: Rc<AuthenticatedPendingSource>,
        admission: Rc<RefCell<ExternalSourceAdmission>>,
        previous: Option<&AcknowledgedReplacement>,
    ) -> io::Result<Rc<Self>> {
        source.check_peer()?;
        let a = admission.try_borrow().map_err(|_| refused())?;
        a.factory.check_current()?;
        a.helper.check_current()?;
        if a.factory.runtime.is_none() {
            return Err(refused());
        }
        if a.revoked || !a.proof.shell.current() {
            return Err(refused());
        }
        let d = &source.description;
        if d.purpose() != runtime_fs::operations::NativeReplacementPurpose::AtomicSave {
            return Err(refused());
        }
        let operation = uuid::Uuid::from_bytes(source.observation.operation);
        if d.source_parent_identity() != d.destination_parent_identity()
            || d.source().root_identity() != d.destination().root_identity()
            || d.source().parents() != d.destination().parents()
            || d.victim().root_identity() != d.destination().root_identity()
            || d.victim().parents() != d.destination().parents()
            || d.victim().name() != d.destination().name()
        {
            return Err(refused());
        }
        let previous_ids = if let Some(previous) = previous {
            previous.observed.seed.check()?;
            previous
                .receipt
                .verify(&previous.observed.prepared, &a.context.verifier)?;
            let old = &previous.observed.seed.source.description;
            if previous.receipt.source_owner != source.observation.source_owner
                || previous.receipt.destination_identity != source.observation.expected_victim
                || old.destination_parent_identity() != d.destination_parent_identity()
                || old.destination().root_identity() != d.destination().root_identity()
                || old.destination().parents() != d.destination().parents()
                || old.destination().name() != d.destination().name()
            {
                return Err(refused());
            }
            Some(PreviousCanonicalIds {
                parent: previous.observed.seed.ids.parent,
                selected: previous.observed.seed.ids.victim,
                version: previous.observed.seed.ids.victim_proposed,
            })
        } else {
            None
        };
        let ids = a
            .factory
            .publisher
            .repository()
            .reserve_external_operation(&ExternalReservationInput {
                workspace: a.factory.publisher.workspace_id(),
                person: a.context.verifier.person,
                source_owner: source.observation.source_owner,
                operation,
                intent: source.observation.intent_digest,
                archive_plan: source.observation.archive_plan_digest,
                previous: previous_ids,
            })
            .map_err(io::Error::other)?;
        let actor = a.factory.publisher.actor();
        let at = ids.observed_at_ms;
        let parent_binding = if let Some(previous) = previous {
            previous
                .observed
                .seed
                .bindings
                .first()
                .ok_or_else(refused)?
                .clone()
        } else {
            binding(
                ids.parent,
                None,
                Vec::new(),
                FileNodeKind::Directory,
                actor,
                at,
            )
        };
        let source_binding = binding(
            ids.source,
            Some(ids.parent),
            d.source().name().to_vec(),
            FileNodeKind::File,
            actor,
            at,
        );
        let victim_binding = if let Some(previous) = previous {
            previous
                .observed
                .entry
                .transaction
                .manifest
                .participants
                .iter()
                .find(|p| {
                    matches!(
                        p.role,
                        FileMutationParticipantRole::Source
                            | FileMutationParticipantRole::AtomicSaveDocument
                    )
                })
                .ok_or_else(refused)?
                .proposed_binding
                .clone()
        } else {
            binding(
                ids.victim,
                Some(ids.parent),
                d.victim().name().to_vec(),
                FileNodeKind::File,
                actor,
                at,
            )
        };
        let (source_metadata, source_hash) = metadata(ids.source, &d.source_facts(), actor, at)?;
        let source_version = FileVersionV1 {
            id: ids.source_initial,
            record_id: ids.source,
            parents: Vec::new(),
            actor,
            timestamp_ms: at,
            content_hash: None,
            metadata_hash: source_hash,
            provenance: FileProvenance::Create,
        };
        let victim_version = if let Some(previous) = previous {
            previous
                .observed
                .entry
                .transaction
                .manifest
                .participants
                .iter()
                .find(|p| {
                    matches!(
                        p.role,
                        FileMutationParticipantRole::Source
                            | FileMutationParticipantRole::AtomicSaveDocument
                    )
                })
                .ok_or_else(refused)?
                .proposed_version
                .clone()
        } else {
            let (_, hash) = metadata(ids.victim, &d.victim_facts(), actor, at)?;
            FileVersionV1 {
                id: ids.victim_initial,
                record_id: ids.victim,
                parents: Vec::new(),
                actor,
                timestamp_ms: at,
                content_hash: None,
                metadata_hash: hash,
                provenance: FileProvenance::Create,
            }
        };
        for b in [&parent_binding, &source_binding, &victim_binding] {
            a.factory
                .publisher
                .publish_binding(b)
                .map_err(io::Error::other)?;
        }
        a.factory
            .publisher
            .publish(&source_metadata, std::slice::from_ref(&source_version))
            .map_err(io::Error::other)?;
        if previous.is_none() {
            let (m, _) = metadata(ids.victim, &d.victim_facts(), actor, at)?;
            a.factory
                .publisher
                .publish(&m, std::slice::from_ref(&victim_version))
                .map_err(io::Error::other)?;
        }
        let runtime = a.factory.runtime.as_ref().ok_or_else(refused)?;
        runtime
            .history
            .append_batch(&[source_version.clone(), victim_version.clone()])
            .map_err(io::Error::other)?;
        drop(a);
        let seed = Rc::new(Self {
            source,
            admission,
            bindings: vec![parent_binding, source_binding, victim_binding],
            versions: vec![source_version, victim_version],
            ids,
        });
        seed.check()?;
        let a = seed.admission.try_borrow().map_err(|_| refused())?;
        let runtime = a.factory.runtime.as_ref().ok_or_else(refused)?;
        let mut namespace = runtime.namespace.lock().map_err(|_| refused())?;
        let mut candidate = namespace.clone();
        let batch =
            crate::namespace::external_projection::ExternalProjectionBatch::from_seed(&seed)
                .map_err(io::Error::other)?;
        candidate
            .apply_external_projection(&batch)
            .map_err(io::Error::other)?;
        crate::durable_backup::write(&runtime.namespace_path, &candidate.save(), |bytes| {
            Namespace::from_bytes("external-seed-validator", bytes).is_ok()
        })?;
        seed.check()?;
        *namespace = candidate;
        drop(namespace);
        drop(a);
        Ok(seed)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "construct and journal the exact two-record signed transaction before signing the native prepare binding"
    )]
    pub(super) fn prepare(&self) -> io::Result<(RecordTransactionJournalEntry, PreparedReceipt)> {
        self.check()?;
        let a = self.admission.try_borrow().map_err(|_| refused())?;
        let actor = a.factory.publisher.actor();
        let at = self.ids.observed_at_ms;
        let mut participants = Vec::new();
        for (initial, proposed_id, role, facts) in [
            (
                self.versions.first().ok_or_else(refused)?,
                self.ids.source_proposed,
                FileMutationParticipantRole::AtomicSaveTemporary,
                self.source.description.source_facts(),
            ),
            (
                self.versions.get(1).ok_or_else(refused)?,
                self.ids.victim_proposed,
                FileMutationParticipantRole::AtomicSaveDocument,
                self.source.description.source_facts(),
            ),
        ] {
            let (current_metadata, current_metadata_hash) =
                metadata(initial.record_id, &facts, actor, at)?;
            // Publish the exact currently observed metadata payload; prior
            // acknowledged versions establish lineage, not unchanged facts.
            a.factory
                .publisher
                .publish(&current_metadata, &[])
                .map_err(io::Error::other)?;
            let proposed_version = FileVersionV1 {
                id: proposed_id,
                record_id: initial.record_id,
                parents: vec![initial.id],
                actor,
                timestamp_ms: at,
                content_hash: None,
                metadata_hash: current_metadata_hash,
                provenance: if role == FileMutationParticipantRole::AtomicSaveDocument {
                    FileProvenance::Edit
                } else {
                    FileProvenance::Delete
                },
            };
            let mut proposed_binding = binding(
                initial.record_id,
                Some(self.ids.parent),
                if role == FileMutationParticipantRole::AtomicSaveDocument {
                    self.source.description.destination().name().to_vec()
                } else {
                    self.source.description.source().name().to_vec()
                },
                FileNodeKind::File,
                actor,
                at,
            );
            proposed_binding.tombstoned = role == FileMutationParticipantRole::AtomicSaveTemporary;
            let mut participant = FileMutationParticipant {
                record_id: initial.record_id,
                role,
                expected_version: initial.id,
                proposed_binding,
                proposed_content_revision: Some(ContentRevisionV1 {
                    schema_version: CONTENT_REVISION_VERSION,
                    record_id: initial.record_id,
                    version_id: proposed_id,
                    content_hash: None,
                    byte_len: facts.byte_len,
                    metadata_hash: current_metadata_hash,
                }),
                proposed_version,
                projection_hash: ContentHash([0; 32]),
            };
            participant.projection_hash = participant
                .computed_projection_hash()
                .map_err(io::Error::other)?;
            participants.push(participant);
        }
        participants.sort_by_key(|p| p.record_id);
        let manifest = FileMutationTransactionManifest {
            schema_version: fs_protocol::files::FILE_MUTATION_TRANSACTION_VERSION_V4,
            transaction_id: uuid::Uuid::from_bytes(self.source.observation.operation),
            workspace_id: a.factory.publisher.workspace_id(),
            actor,
            timestamp_ms: at,
            operation: FileMutationOperation::AtomicSave {
                parent_record: self.ids.parent,
                document_name: self.source.description.destination().name().to_vec().into(),
                temporary_name: self.source.description.source().name().to_vec().into(),
                document_record: self.ids.victim,
                temporary_record: self.ids.source,
                prior_document_version: self.versions.get(1).ok_or_else(refused)?.id,
            },
            participants,
        };
        let _lease = a
            .factory
            .publisher
            .publication_lease()
            .map_err(io::Error::other)?;
        let mut entry = a
            .factory
            .publisher
            .prepare_replacement_transaction(manifest)
            .map_err(io::Error::other)?;
        entry.external_native_effect = Some(ExternalNativeEffectBinding {
            operation_id: uuid::Uuid::from_bytes(self.source.observation.operation),
            source_owner: self.source.observation.source_owner,
            source_identity: self.source.observation.source,
            victim_identity: self.source.observation.expected_victim,
        });
        entry.validate().map_err(io::Error::other)?;
        let runtime = a.factory.runtime.as_ref().ok_or_else(refused)?;
        runtime
            .journal
            .lock()
            .map_err(|_| refused())?
            .insert_prepared(entry.clone())
            .map_err(io::Error::other)?;
        let transaction_digest =
            *blake3::hash(&postcard::to_allocvec(&entry.transaction).map_err(io::Error::other)?)
                .as_bytes();
        let mut receipt = PreparedReceipt {
            schema: 1,
            operation_id: entry.transaction_id().to_string(),
            source_owner: self.source.observation.source_owner,
            intent_digest: self.source.observation.intent_digest,
            personal_room: a.context.verifier.personal_room.clone(),
            person: a.context.verifier.person,
            device: a.context.verifier.device,
            transaction_digest,
            archive_binding_digest: self.source.observation.archive_plan_digest,
            signature: Vec::new(),
        };
        receipt.signature = a
            .factory
            .credentials
            .signing_key()
            .sign(&receipt.signing_bytes()?)
            .to_bytes()
            .to_vec();
        receipt.verify(&a.context.verifier)?;
        self.check()?;
        Ok((entry, receipt))
    }
}

/// Pure traversal is used only after actual signed repository/current-source
/// admission above. It grants nothing from a supplied version graph.
fn acknowledged_version_ancestor(
    versions: &[FileVersionV1],
    ancestor: FileVersionId,
    current: FileVersionId,
) -> bool {
    if ancestor == current {
        return false;
    }
    let graph: std::collections::BTreeMap<_, _> = versions
        .iter()
        .map(|version| (version.id, version))
        .collect();
    let mut pending = vec![current];
    let mut visited = std::collections::BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        let Some(version) = graph.get(&id) else {
            return false;
        };
        for parent in &version.parents {
            if *parent == ancestor {
                return graph.contains_key(parent);
            }
            pending.push(*parent);
        }
    }
    false
}

/// The registry owns this before the first start request is sent. A lost reply
/// never drops the admitted source or permits another start for this request.
struct RepositoryRetirementCheck {
    current: FileVersionId,
    task: tokio::task::JoinHandle<io::Result<bool>>,
}

pub(super) struct OwnedReplacement {
    request_id: String,
    seed: Rc<SeededExternalRecords>,
    entry: RecordTransactionJournalEntry,
    prepared: PreparedReceipt,
    job: Option<runtime_fs::directory_client::NativeJobClient>,
    // The source poll drains updates. Own the exact in-flight future across
    // bounded turns so a timeout cannot discard its sole Finished update.
    poll_task: Option<
        tokio::task::JoinHandle<(
            runtime_fs::directory_client::NativeJobClient,
            io::Result<runtime_fs::directory_backend::JobPoll>,
        )>,
    >,
    observed: Option<Rc<ObservedExternalReplacement>>,
    signed_commit: Option<CommittedReceipt>,
    acknowledged: Option<Rc<AcknowledgedReplacement>>,
    attempted_start: bool,
    completed_job: bool,
    refused: bool,
    // An async-aware guard keeps the exact task in this owner across awaits,
    // including dropped turns; competing cleanup checks still fail closed.
    retirement_check: tokio::sync::Mutex<Option<RepositoryRetirementCheck>>,
}
impl OwnedReplacement {
    pub(super) fn reserve(
        request_id: String,
        source: Rc<AuthenticatedPendingSource>,
        admission: Rc<RefCell<ExternalSourceAdmission>>,
        previous: Option<&AcknowledgedReplacement>,
    ) -> io::Result<Self> {
        let seed = SeededExternalRecords::reserve(source, admission, previous)?;
        let (entry, prepared) = seed.prepare()?;
        Ok(Self {
            request_id,
            seed,
            entry,
            prepared,
            job: None,
            poll_task: None,
            observed: None,
            signed_commit: None,
            acknowledged: None,
            attempted_start: false,
            completed_job: false,
            refused: false,
            retirement_check: tokio::sync::Mutex::new(None),
        })
    }
    /// Only a later actual signed/native acknowledged save permits releasing
    /// prior source RPC custody. No caller selector creates this evidence.
    #[expect(
        clippy::too_many_lines,
        reason = "actual old/native ACK, fresh successor and owned signed repository ancestry form one cleanup gate with no authority gap"
    )]
    pub(super) fn check_retirement_successor<'a>(
        &'a self,
        successor: &'a AcknowledgedReplacement,
        operation_id: &'a str,
        transaction_digest: [u8; 32],
        cancel: &'a std::sync::atomic::AtomicBool,
        deadline: std::time::Instant,
    ) -> super::LocalFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let old = self.acknowledged.as_ref().ok_or_else(refused)?;
            if self.unresolved_effect()
                || !self.completed_job
                || self.poll_task.is_some()
                || old.receipt.operation_id != operation_id
                || old.receipt.transaction_digest != transaction_digest
                || old.receipt.operation_id == successor.receipt.operation_id
            {
                return Err(refused());
            }
            // Fresh successor's own kernel/person/source/pin proof is required.
            // Never check the old named destination pin: a later EXCHANGE moved it.
            let current_lineage = successor.read_lineage(cancel, deadline).await?;
            {
                let admission = self.seed.admission.try_borrow().map_err(|_| refused())?;
                old.receipt
                    .verify(&self.prepared, &admission.context.verifier)?;
                successor
                    .receipt
                    .verify(&successor.observed.prepared, &admission.context.verifier)?;
            }
            let old_locus = &self.seed.source.description;
            let new_locus = &successor.observed.seed.source.description;
            let document =
                uuid::Uuid::parse_str(&old.receipt.victim_record_id).map_err(io::Error::other)?;
            let prior =
                uuid::Uuid::parse_str(&old.receipt.victim_version_id).map_err(io::Error::other)?;
            let participant = successor
                .observed
                .entry
                .transaction
                .manifest
                .participants
                .iter()
                .find(|participant| participant.record_id.0 == document)
                .ok_or_else(refused)?;
            if old.receipt.source_owner != successor.receipt.source_owner
                || old.receipt.person != successor.receipt.person
                || old.receipt.personal_room != successor.receipt.personal_room
                || old.receipt.victim_record_id != successor.receipt.victim_record_id
                || old_locus.destination_parent_identity()
                    != new_locus.destination_parent_identity()
                || old_locus.destination().root_identity()
                    != new_locus.destination().root_identity()
                || old_locus.destination().parents() != new_locus.destination().parents()
                || old_locus.destination().name() != new_locus.destination().name()
                || participant.proposed_binding.tombstoned
                || participant.proposed_version.provenance != FileProvenance::Edit
            {
                return Err(refused());
            }
            let latest = fs_protocol::FileVersionId(
                uuid::Uuid::parse_str(&successor.receipt.victim_version_id)
                    .map_err(io::Error::other)?,
            );
            if current_lineage.binding.record_id.0 != document || current_lineage.version != latest
            {
                return Err(refused());
            }
            // The lookup is derived from the genuine owned signed ACK, not caller
            // record IDs or native inode hints. Later signed saves may intervene
            // while an older retirement was transiently blocked by a Reader.
            let (key, publisher) = {
                let admission = self.seed.admission.try_borrow().map_err(|_| refused())?;
                let key = crate::record_repository::RecordRepositoryKey {
                    workspace_id: admission.factory.authority.workspace(),
                    record_id: fs_protocol::FileRecordId(document),
                };
                let publisher = Arc::clone(&admission.factory.publisher);
                (key, publisher)
            };
            let binding = current_lineage.binding.clone();
            let ancestor = fs_protocol::FileVersionId(prior);
            let mut slot = self.retirement_check.try_lock().map_err(|_| refused())?;
            if slot.is_none() {
                *slot = Some(RepositoryRetirementCheck {
                    current: latest,
                    task: tokio::task::spawn_blocking(move || {
                        let repository = publisher
                            .repository()
                            .projection_lease(key)
                            .map_err(io::Error::other)?;
                        let snapshot = &repository.snapshot;
                        let matches = snapshot.binding.as_ref() == Some(&binding)
                            && snapshot
                                .content_revision
                                .as_ref()
                                .is_some_and(|revision| revision.version_id == latest)
                            && acknowledged_version_ancestor(&snapshot.versions, ancestor, latest);
                        drop(repository);
                        Ok(matches)
                    }),
                });
            }
            let owned = slot.as_mut().ok_or_else(refused)?;
            let checked = owned.current;
            let result =
                super::pending_source::within_owned_task(cancel, deadline, &mut owned.task).await;
            if let Err(error) = &result
                && matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
                )
            {
                return Err(io::Error::new(
                    error.kind(),
                    "Canonical ancestry check remains owned",
                ));
            }
            // All other outcomes came from actually polling this exact task to
            // completion (including panic). A later retry may create a fresh check.
            slot.take();
            drop(slot);
            if checked != latest || !result.map_err(io::Error::other)?? {
                return Err(refused());
            }
            // Current source/person/pin proof brackets the actual owned repository
            // task. A completed old snapshot cannot authorize cleanup after revocation.
            let refreshed = successor.read_lineage(cancel, deadline).await?;
            if refreshed.binding != current_lineage.binding || refreshed.version != latest {
                return Err(refused());
            }
            Ok(())
        })
    }
    /// A lane cannot discard a still-running repository worker. It has no
    /// native effects, but still owns actual canonical publisher/repository custody.
    pub(super) fn retire_repository_check(&self) -> super::LocalFuture<'_, io::Result<()>> {
        Box::pin(async move {
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let mut slot = self.retirement_check.try_lock().map_err(|_| refused())?;
            while let Some(owned) = slot.as_mut() {
                let deadline = std::time::Instant::now()
                    .checked_add(std::time::Duration::from_secs(10))
                    .ok_or_else(refused)?;
                match super::pending_source::within_owned_task(&cancel, deadline, &mut owned.task)
                    .await
                {
                    Ok(_) => {
                        slot.take();
                    }
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                    Err(_) => {
                        // A joined worker panic is observed stopped. This cleanup
                        // does not mint a retirement permission or a Save result.
                        slot.take();
                    }
                }
            }
            drop(slot);
            Ok(())
        })
    }
    pub(super) fn retire_acknowledged_rpc(&self) -> super::LocalFuture<'_, io::Result<()>> {
        Box::pin(async move {
            if self.unresolved_effect()
                || !self.completed_job
                || self.acknowledged.is_none()
                || self.poll_task.is_some()
            {
                return Err(refused());
            }
            self.seed.source.retire_rpc().await
        })
    }
    pub(super) fn operation_id(&self) -> String {
        self.prepared.operation_id.clone()
    }
    pub(super) fn acknowledged(&self) -> Option<Rc<AcknowledgedReplacement>> {
        self.acknowledged.clone()
    }
    pub(super) fn unresolved_effect(&self) -> bool {
        self.attempted_start && self.acknowledged.is_none()
    }
    pub(super) fn cancel_owned<'a>(
        &'a mut self,
        cancel: &'a std::sync::atomic::AtomicBool,
        deadline: std::time::Instant,
    ) -> super::LocalFuture<'a, ()> {
        Box::pin(async move {
            if let Some(job) = &self.job {
                let _ = super::pending_source::within(cancel, deadline, async {
                    job.cancel().await.map_err(io::Error::other)
                })
                .await;
            } else if !self.attempted_start {
                let _ = super::pending_source::within(cancel, deadline, async {
                    self.seed
                        .source
                        .pending
                        .cancel()
                        .await
                        .map_err(io::Error::other)
                })
                .await;
            }
            // A cancellation ACK is not proof of native stop or preserved history.
            self.refused = true;
        })
    }
    pub(super) fn record_turn_error(&mut self, error: &io::Error) {
        if matches!(
            error.kind(),
            io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound
        ) {
            self.refused = true;
        }
    }

    pub(super) fn status(&self) -> super::replacement_control::ReplacementReply {
        if let Some(done) = &self.acknowledged {
            return super::replacement_control::ReplacementReply::Committed {
                request_id: self.request_id.clone(),
                prepared: Box::new(self.prepared.clone()),
                receipt: Box::new(done.receipt.clone()),
            };
        }
        if self.refused {
            super::replacement_control::ReplacementReply::Refused {
                request_id: self.request_id.clone(),
                operation_id: Some(self.operation_id()),
            }
        } else {
            super::replacement_control::ReplacementReply::Pending {
                request_id: self.request_id.clone(),
                operation_id: self.operation_id(),
            }
        }
    }
    #[expect(
        clippy::too_many_lines,
        reason = "the same owned native job and pin span submission, completion, canonical commit and acknowledgement without a custody gap"
    )]
    pub(super) fn advance<'a>(
        &'a mut self,
        cancel: &'a std::sync::atomic::AtomicBool,
        deadline: std::time::Instant,
    ) -> super::LocalFuture<'a, io::Result<()>> {
        Box::pin(async move {
            use super::pending_source::within;
            ExternalSourceAdmission::refresh_shared(&self.seed.admission, cancel, deadline).await?;
            self.seed.check()?;

            if self.refused {
                return Err(refused());
            }
            if self.acknowledged.is_some() {
                return Ok(());
            }
            if !self.attempted_start {
                within(cancel, deadline, async {
                    self.seed
                        .source
                        .pending
                        .check_scope()
                        .await
                        .map_err(io::Error::other)
                })
                .await?;
                let bytes = serde_json::to_vec(&self.prepared).map_err(io::Error::other)?;
                self.prepared.verify(
                    &self
                        .seed
                        .admission
                        .try_borrow()
                        .map_err(|_| refused())?
                        .context
                        .verifier,
                )?;
                // Mark before sending: neither a missing reply nor a revoked scope
                // can establish that the actual source performed no replacement.
                self.attempted_start = true;
                self.job = Some(
                    within(cancel, deadline, async {
                        self.seed
                            .source
                            .pending
                            .start_with_history(&bytes)
                            .await
                            .map_err(io::Error::other)
                    })
                    .await?,
                );
            }
            if self.observed.is_none() {
                if !self.completed_job {
                    if self.poll_task.is_none() {
                        let mut job = self.job.take().ok_or_else(refused)?;
                        self.poll_task = Some(tokio::task::spawn_local(async move {
                            let result = async {
                                job.check_scope().await.map_err(io::Error::other)?;
                                job.poll().await.map_err(io::Error::other)
                            }
                            .await;
                            (job, result)
                        }));
                    }
                    // Timeout/cancellation leaves the task in this owner. Never
                    // wrap this consuming result in a post-completion deadline test.
                    let task = self.poll_task.as_mut().ok_or_else(refused)?;
                    let (job, updates) =
                        super::pending_source::within_owned_task(cancel, deadline, task).await?;
                    self.poll_task = None;
                    self.job = Some(job);
                    let updates = updates?;
                    if updates.operation_id != self.seed.source.observation.operation {
                        return Err(refused());
                    }
                    let mut finished = false;
                    for update in updates.updates {
                        if let runtime_fs::directory_backend::JobUpdate::Finished {
                            completed,
                            failed,
                            cancelled,
                        } = update
                        {
                            if completed != 1 || failed != 0 || cancelled != 0 {
                                return Err(refused());
                            }
                            finished = true;
                        }
                    }
                    if !finished {
                        return Ok(());
                    }
                    self.completed_job = true;
                }
                let job = self.job.as_mut().ok_or_else(refused)?;
                let pin = within(cancel, deadline, async {
                    job.pin_replacement().await.map_err(io::Error::other)
                })
                .await?;
                let observed = within(cancel, deadline, async {
                    pin.check().await.map_err(io::Error::other)?;
                    pin.observe().await.map_err(io::Error::other)
                })
                .await?;
                if observed.operation != self.seed.source.observation.operation
                    || observed.source_owner != self.seed.source.observation.source_owner
                    || observed.source != self.seed.source.observation.source
                    || observed.victim != self.seed.source.observation.expected_victim
                    || observed.destination != observed.source
                {
                    return Err(refused());
                }
                ExternalSourceAdmission::refresh_shared(&self.seed.admission, cancel, deadline)
                    .await?;
                let token = Rc::new(ObservedExternalReplacement {
                    seed: Rc::clone(&self.seed),
                    pin,
                    observed,
                    prepared: self.prepared.clone(),
                    entry: self.entry.clone(),
                });
                token.check_entry(&self.entry)?;
                self.observed = Some(token);
            }
            let observed = self.observed.as_ref().ok_or_else(refused)?;
            within(cancel, deadline, async {
                observed.pin.check().await.map_err(io::Error::other)?;
                let current = observed.pin.observe().await.map_err(io::Error::other)?;
                if current.operation != observed.observed.operation
                    || current.capture != observed.observed.capture
                    || current.source_owner != observed.observed.source_owner
                    || current.source != observed.observed.source
                    || current.victim != observed.observed.victim
                    || current.destination != observed.observed.destination
                {
                    return Err(refused());
                }
                Ok(())
            })
            .await?;
            ExternalSourceAdmission::refresh_shared(&self.seed.admission, cancel, deadline).await?;
            if self.signed_commit.is_none() {
                self.signed_commit = Some(observed.commit_canonical()?);
            }
            let receipt = self.signed_commit.as_ref().ok_or_else(refused)?;
            let bytes = serde_json::to_vec(receipt).map_err(io::Error::other)?;
            // Retry is only the exact durable signed commit on the same owned pin.
            // It can acknowledge an earlier lost reply, never restart native work.
            within(cancel, deadline, async {
                observed.pin.check().await.map_err(io::Error::other)?;
                observed
                    .pin
                    .commit_with_history(&bytes)
                    .await
                    .map_err(io::Error::other)?;
                observed.pin.check().await.map_err(io::Error::other)?;
                let current = observed.pin.observe().await.map_err(io::Error::other)?;
                if current.operation != observed.observed.operation
                    || current.capture != observed.observed.capture
                    || current.source_owner != observed.observed.source_owner
                    || current.source != observed.observed.source
                    || current.victim != observed.observed.victim
                    || current.destination != observed.observed.destination
                {
                    return Err(refused());
                }
                Ok(())
            })
            .await?;
            ExternalSourceAdmission::refresh_shared(&self.seed.admission, cancel, deadline).await?;
            observed.check_entry(&self.entry)?;
            self.acknowledged = Some(Rc::new(AcknowledgedReplacement {
                receipt: receipt.clone(),
                observed: Rc::clone(observed),
            }));
            Ok(())
        })
    }
}
impl ObservedExternalReplacement {
    #[expect(
        clippy::too_many_lines,
        reason = "repository, history, namespace and journal phases share one checked completed native pin"
    )]
    fn commit_canonical(&self) -> io::Result<CommittedReceipt> {
        use crate::namespace::external_projection::ExternalProjectionBatch;
        struct Authorizer {
            actor: FileActor,
            records: [FileRecordId; 2],
        }
        impl crate::record_replication::RecordAuthorizer for Authorizer {
            fn actor_is_certified(&self, actor: &FileActor) -> bool {
                *actor == self.actor
            }
            fn allows(
                &self,
                actor: &FileActor,
                record: FileRecordId,
                _action: crate::record_replication::RecordAction,
            ) -> bool {
                *actor == self.actor && self.records.contains(&record)
            }
        }

        self.check_entry(&self.entry)?;
        let a = self.seed.admission.try_borrow().map_err(|_| refused())?;
        let runtime = a.factory.runtime.as_ref().ok_or_else(refused)?;
        let publisher = &a.factory.publisher;
        let _publish = publisher.publication_lease().map_err(io::Error::other)?;
        let mut journal = runtime.journal.lock().map_err(|_| refused())?;
        let mut repository = publisher.repository().lease().map_err(io::Error::other)?;
        let authorizer = Authorizer {
            actor: publisher.actor(),
            records: [self.seed.ids.source, self.seed.ids.victim],
        };
        let prepared = repository
            .prepare_external(&journal, &self.entry, &authorizer, self)
            .map_err(io::Error::other)?;
        repository
            .commit(&mut journal, prepared, |_| Ok(()))
            .map_err(io::Error::other)?;
        drop(repository);
        let id = self.entry.transaction_id();
        let phase = journal.get(id).ok_or_else(refused)?.phase;
        if phase == RecordTransactionPhase::Prepared {
            journal
                .advance(
                    id,
                    RecordTransactionPhase::ParticipantRecordsCommitted,
                    None,
                )
                .map_err(io::Error::other)?;
        }
        if journal.get(id).ok_or_else(refused)?.phase
            == RecordTransactionPhase::ParticipantRecordsCommitted
        {
            runtime
                .history
                .append_batch(
                    &self
                        .entry
                        .transaction
                        .manifest
                        .participants
                        .iter()
                        .map(|p| p.proposed_version.clone())
                        .collect::<Vec<_>>(),
                )
                .map_err(io::Error::other)?;
        }
        if journal.get(id).ok_or_else(refused)?.phase
            == RecordTransactionPhase::ParticipantRecordsCommitted
        {
            let mut namespace = runtime.namespace.lock().map_err(|_| refused())?;
            let mut candidate = namespace.clone();
            let batch = ExternalProjectionBatch::from_observed(self, &self.entry)
                .map_err(io::Error::other)?;
            let actual = candidate
                .apply_external_projection(&batch)
                .map_err(io::Error::other)?;
            let stored = journal
                .get(id)
                .and_then(|entry| entry.namespace_delta.clone());
            if let (Some(actual), Some(stored)) = (&actual, &stored)
                && actual != stored
            {
                return Err(refused());
            }
            let delta = stored.or(actual).unwrap_or_default();
            journal
                .stage_namespace_delta(id, delta.clone())
                .map_err(io::Error::other)?;
            crate::durable_backup::write(&runtime.namespace_path, &candidate.save(), |bytes| {
                Namespace::from_bytes("external-history-validator", bytes).is_ok()
            })?;
            self.check_entry(&self.entry)?;
            *namespace = candidate;
            journal
                .advance(id, RecordTransactionPhase::NamespaceCommitted, Some(delta))
                .map_err(io::Error::other)?;
            // Keep namespace serialization through the durable journal transition.
            drop(namespace);
        }
        let saved = journal.get(id).ok_or_else(refused)?;
        if saved.phase != RecordTransactionPhase::NamespaceCommitted {
            return Err(refused());
        }
        // NamespaceCommitted is the actual local durable journal phase. Do not
        // pretend a replication broadcast happened or remove recovery evidence.
        let journal_commit_digest =
            *blake3::hash(&serde_json::to_vec(saved).map_err(io::Error::other)?).as_bytes();
        let mut receipt = CommittedReceipt {
            schema: 1,
            operation_id: self.prepared.operation_id.clone(),
            source_owner: self.prepared.source_owner,
            intent_digest: self.prepared.intent_digest,
            personal_room: self.prepared.personal_room.clone(),
            person: self.prepared.person,
            device: self.prepared.device,
            transaction_digest: self.prepared.transaction_digest,
            archive_binding_digest: self.prepared.archive_binding_digest,
            source_identity: self.observed.source,
            victim_identity: self.observed.victim,
            destination_identity: self.observed.destination,
            capture_id: uuid::Uuid::from_bytes(self.observed.capture).to_string(),
            journal_commit_digest,
            source_record_id: self.seed.ids.source.0.to_string(),
            source_version_id: self.seed.ids.source_proposed.0.to_string(),
            victim_record_id: self.seed.ids.victim.0.to_string(),
            victim_version_id: self.seed.ids.victim_proposed.0.to_string(),
            signature: Vec::new(),
        };
        receipt.signature = a
            .factory
            .credentials
            .signing_key()
            .sign(&receipt.signing_bytes()?)
            .to_bytes()
            .to_vec();
        receipt.verify(&self.prepared, &a.context.verifier)?;
        self.check_entry(&self.entry)?;
        // The journal remains serialized through signing and the final authority check.
        drop(journal);
        Ok(receipt)
    }
}

#[cfg(test)]
mod retirement_ancestry_tests {
    use super::*;
    use fs_protocol::Ed25519PublicKey;

    #[test]
    fn actual_ancestry_traversal_has_no_total_version_quota() {
        let record = FileRecordId::new();
        let actor = FileActor {
            person: Ed25519PublicKey([1; 32]),
            device: Ed25519PublicKey([2; 32]),
        };
        let mut versions = Vec::new();
        for timestamp in 0..192_u64 {
            let parents = versions
                .last()
                .map_or_else(Vec::new, |version: &FileVersionV1| vec![version.id]);
            versions.push(FileVersionV1 {
                id: FileVersionId::new(),
                record_id: record,
                parents,
                actor,
                timestamp_ms: timestamp,
                content_hash: None,
                metadata_hash: ContentHash([3; 32]),
                provenance: if timestamp == 0 {
                    FileProvenance::Create
                } else {
                    FileProvenance::Edit
                },
            });
        }
        let first = versions[0].id;
        let last = versions.last().unwrap().id;
        assert!(acknowledged_version_ancestor(&versions, first, last));
        assert!(!acknowledged_version_ancestor(&versions, last, first));
        assert!(!acknowledged_version_ancestor(&versions, first, first));
        assert!(!acknowledged_version_ancestor(
            &versions,
            FileVersionId::new(),
            last
        ));
        versions[100].parents = vec![FileVersionId::new()];
        assert!(!acknowledged_version_ancestor(&versions, first, last));
    }

    #[test]
    fn cancelled_repository_turn_retains_exact_worker_until_observed_completion() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (release, wait) = std::sync::mpsc::sync_channel(1);
            let (started, ready) = tokio::sync::oneshot::channel();
            let mut owned = RepositoryRetirementCheck {
                current: FileVersionId::new(),
                task: tokio::task::spawn_blocking(move || {
                    started.send(()).unwrap();
                    wait.recv().map_err(io::Error::other)?;
                    Ok(true)
                }),
            };
            ready.await.unwrap();
            let cancelled = std::sync::atomic::AtomicBool::new(true);
            let error = super::super::pending_source::within_owned_task(
                &cancelled,
                std::time::Instant::now() + std::time::Duration::from_secs(1),
                &mut owned.task,
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            assert!(!owned.task.is_finished());
            release.send(()).unwrap();
            let active = std::sync::atomic::AtomicBool::new(false);
            assert!(
                super::super::pending_source::within_owned_task(
                    &active,
                    tilo_test_env::hang_deadline(),
                    &mut owned.task,
                )
                .await
                .unwrap()
                .unwrap()
            );
            assert!(owned.task.is_finished());
        });
    }
}
