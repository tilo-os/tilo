//! Record-scoped, authenticated replication primitives.
//!
//! A [`RecordReplica`] owns exactly one stable [`FileRecordId`]. It never
//! contains a namespace or sibling records, which makes it a suitable unit
//! for capability-filtered replication. Network transport and persistence are
//! deliberately outside this module: callers authenticate an operation here
//! before translating it into namespace changes.

use std::collections::{HashMap, HashSet};

use collab_types::{
    CanonicalError, Signature, VerifyError, canonical_bytes, sign_event, verify_event,
};
use fs_protocol::{
    ConflictResolution, ConflictStatus, ContentRevisionV1, FileActor, FileAttributionV1,
    FileBindingV1, FileConflictV1, FileMetadataV1, FileMutationTransaction, FileProvenance,
    FileRecordError, FileRecordId, FileVersionAttributionV1, FileVersionId, FileVersionV1,
    prefer_conflict_resolution,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Wire version for record operation envelopes.
pub const RECORD_ENVELOPE_VERSION: u16 = 4;
/// The envelope version that carries [`RecordOperationV1::AttributeVersion`],
/// and only it: every other operation keeps signing at
/// [`RECORD_ENVELOPE_VERSION`], so a device that predates attribution still
/// reads everything else this one writes.
pub const RECORD_ENVELOPE_V5: u16 = 5;
pub const RECORD_ENVELOPE_V3: u16 = 3;
pub const RECORD_ENVELOPE_V2: u16 = 2;
pub const LEGACY_RECORD_ENVELOPE_VERSION: u16 = 1;

/// Domain separation included in every record-operation signature.
///
/// Keeping this value inside the canonical signed view prevents a signature
/// made for another Tilo protocol from being replayed as a file operation.
pub const RECORD_OPERATION_SIGNATURE_DOMAIN: &str = "tilo.collaborative-fs.record-operation.v4";
const RECORD_OPERATION_SIGNATURE_DOMAIN_V5: &str = "tilo.collaborative-fs.record-operation.v5";
const RECORD_OPERATION_SIGNATURE_DOMAIN_V3: &str = "tilo.collaborative-fs.record-operation.v3";
const RECORD_OPERATION_SIGNATURE_DOMAIN_V2: &str = "tilo.collaborative-fs.record-operation.v2";
const LEGACY_RECORD_OPERATION_SIGNATURE_DOMAIN: &str = "tilo.collaborative-fs.record-operation.v1";

/// The access being requested from the record authorizer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordAction {
    /// Return this record's metadata and version history.
    Read,
    /// Merge a mutation into this record.
    Write,
}

/// Supplies the live identity certification and grant decision for a record.
///
/// Implementations should resolve `actor.person` through the identity layer,
/// prove that `actor.device` is currently certified for that person, and then
/// evaluate the record-scoped grant. Both checks are repeated for every read
/// or merge so revocation takes effect without rebuilding replicas.
pub trait RecordAuthorizer {
    /// Whether the person currently certifies this exact device key.
    fn actor_is_certified(&self, actor: &FileActor) -> bool;

    /// Whether this actor may perform `action` on this exact record.
    fn allows(&self, actor: &FileActor, record_id: FileRecordId, action: RecordAction) -> bool;
}

/// A mutation whose payload is constrained to one stable file record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum RecordOperationV1 {
    /// Replace the bounded, fetch-free metadata projection.
    SetMetadata(FileMetadataV1),
    /// Add an immutable version-history node.
    AppendVersion(FileVersionV1),
    /// Select one immutable version as the current atomic namespace content
    /// tuple. The envelope signature authenticates byte length as well as the
    /// version/hash/metadata binding.
    SetContentRevision(ContentRevisionV1),
    /// Add one open first-class conflict after every referenced immutable
    /// version is present.
    AppendConflict(FileConflictV1),
    /// Resolve an existing conflict through a separately appended immutable
    /// conflict-resolution version.
    ResolveConflict {
        conflict_id: uuid::Uuid,
        resolution: ConflictResolution,
    },
    /// Authenticate the record's current parent/name/kind independently of
    /// the mutable namespace projection. Appended to preserve V1/V2 postcard
    /// enum discriminants in durable repositories.
    SetBinding(FileBindingV1),
    /// Stage the identical signed multi-record manifest in each participant.
    PrepareTransaction(FileMutationTransaction),
    /// Mark this participant committed. Projection remains invisible until
    /// the repository proves matching commits for the exact participant set.
    CommitTransaction { transaction_id: uuid::Uuid },
    /// Record which agent, acting for the signing person through their
    /// session, caused one of this same actor's versions of the record. The
    /// session authenticated the agent from its action context; the device
    /// signature covers this operation like any other. Appended to keep
    /// every earlier discriminant, and carried only at
    /// [`RECORD_ENVELOPE_V5`]. Presentation only: it grants nothing.
    AttributeVersion(FileVersionAttributionV1),
}

impl RecordOperationV1 {
    fn validate_for(
        &self,
        record_id: FileRecordId,
        actor: FileActor,
    ) -> Result<(), RecordReplicationError> {
        match self {
            Self::SetBinding(binding) => {
                binding.validate()?;
                if binding.record_id != record_id {
                    return Err(RecordReplicationError::PayloadRecordMismatch);
                }
                if binding.actor != actor {
                    return Err(RecordReplicationError::PayloadActorMismatch);
                }
            }
            Self::PrepareTransaction(transaction) => {
                transaction
                    .verify()
                    .map_err(|_| RecordReplicationError::InvalidTransaction)?;
                if transaction.manifest.actor != actor
                    || !transaction
                        .manifest
                        .participants
                        .iter()
                        .any(|participant| participant.record_id == record_id)
                {
                    return Err(RecordReplicationError::InvalidTransaction);
                }
            }
            Self::CommitTransaction { transaction_id } => {
                if transaction_id.is_nil() {
                    return Err(RecordReplicationError::InvalidTransaction);
                }
            }
            Self::SetMetadata(metadata) => {
                metadata.validate()?;
                if metadata.record_id != record_id {
                    return Err(RecordReplicationError::PayloadRecordMismatch);
                }
                if metadata.latest_actor != Some(actor) {
                    return Err(RecordReplicationError::PayloadActorMismatch);
                }
            }
            Self::AppendVersion(version) => {
                version.validate()?;
                if version.record_id != record_id {
                    return Err(RecordReplicationError::PayloadRecordMismatch);
                }
                if version.actor != actor {
                    return Err(RecordReplicationError::PayloadActorMismatch);
                }
            }
            Self::SetContentRevision(revision) => {
                revision.validate()?;
                if revision.record_id != record_id {
                    return Err(RecordReplicationError::PayloadRecordMismatch);
                }
            }
            Self::AppendConflict(conflict) => {
                conflict.validate()?;
                if conflict.record_id != record_id {
                    return Err(RecordReplicationError::PayloadRecordMismatch);
                }
                if conflict.status != ConflictStatus::Open || conflict.resolution.is_some() {
                    return Err(RecordReplicationError::InvalidConflictTransition);
                }
            }
            Self::ResolveConflict { resolution, .. } => {
                if resolution.actor != actor {
                    return Err(RecordReplicationError::PayloadActorMismatch);
                }
            }
            Self::AttributeVersion(attributed) => {
                attributed.attribution.validate()?;
                if attributed.version_id.0.is_nil() {
                    return Err(FileRecordError::NilVersionId.into());
                }
            }
        }
        Ok(())
    }

    /// The envelope version this operation is signed at.
    const fn envelope_version(&self) -> u16 {
        if matches!(self, Self::AttributeVersion(_)) {
            RECORD_ENVELOPE_V5
        } else {
            RECORD_ENVELOPE_VERSION
        }
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct RecordSigningView<'a> {
    domain: &'static str,
    protocol_version: u16,
    record_id: FileRecordId,
    actor: FileActor,
    sequence: u64,
    operation: &'a RecordOperationV1,
}

/// Signed, canonical wire envelope for one record-local operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordEnvelopeV1 {
    /// Envelope schema version.
    pub protocol_version: u16,
    /// Stable record scope. This is signed separately from the payload's copy.
    pub record_id: FileRecordId,
    /// Certified human identity and exact authoring device.
    pub actor: FileActor,
    /// Strictly increasing per-record, per-actor sequence number.
    pub sequence: u64,
    /// The record-local mutation.
    pub operation: RecordOperationV1,
    /// Ed25519 signature made by `actor.device`.
    pub signature: Signature,
}

impl RecordEnvelopeV1 {
    fn signing_view(&self) -> RecordSigningView<'_> {
        RecordSigningView {
            domain: match self.protocol_version {
                LEGACY_RECORD_ENVELOPE_VERSION => LEGACY_RECORD_OPERATION_SIGNATURE_DOMAIN,
                RECORD_ENVELOPE_V2 => RECORD_OPERATION_SIGNATURE_DOMAIN_V2,
                RECORD_ENVELOPE_V3 => RECORD_OPERATION_SIGNATURE_DOMAIN_V3,
                RECORD_ENVELOPE_V5 => RECORD_OPERATION_SIGNATURE_DOMAIN_V5,
                _ => RECORD_OPERATION_SIGNATURE_DOMAIN,
            },
            protocol_version: self.protocol_version,
            record_id: self.record_id,
            actor: self.actor,
            sequence: self.sequence,
            operation: &self.operation,
        }
    }

    /// Canonical bytes covered by the signature.
    ///
    /// # Errors
    ///
    /// Returns a canonical serialization error if the signing view cannot be
    /// encoded.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&self.signing_view())
    }

    /// Encode the complete envelope using the canonical wire format.
    ///
    /// # Errors
    ///
    /// Returns a canonical serialization error if the envelope cannot be
    /// encoded.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(self)
    }

    /// Decode an exact-length canonical envelope and verify its signature.
    ///
    /// This performs cryptographic and structural validation only. Call
    /// [`RecordReplica::merge`] to apply the live identity and grant checks.
    pub fn decode_and_verify(bytes: &[u8]) -> Result<Self, RecordReplicationError> {
        let envelope: Self = collab_types::canonical::from_canonical_bytes(bytes)?;
        envelope.verify()?;
        Ok(envelope)
    }

    /// Construct and sign an envelope with the actor's device key.
    ///
    /// # Errors
    ///
    /// Returns a canonical serialization error if the signed view cannot be
    /// encoded.
    pub fn signed(
        record_id: FileRecordId,
        actor: FileActor,
        sequence: u64,
        operation: RecordOperationV1,
        device_key: &ed25519_dalek::SigningKey,
    ) -> Result<Self, CanonicalError> {
        let mut envelope = Self {
            protocol_version: operation.envelope_version(),
            record_id,
            actor,
            sequence,
            operation,
            signature: Signature::from_bytes([0; 64]),
        };
        envelope.signature = sign_event(&envelope.signing_view(), device_key)?;
        Ok(envelope)
    }

    /// Verify schema, record/actor binding, and the device signature.
    pub fn verify(&self) -> Result<(), RecordReplicationError> {
        self.validate()?;
        verify_event(&self.signing_view(), &self.signature, &self.actor.device)?;
        Ok(())
    }

    /// Everything [`Self::verify`] checks except the signature.
    fn validate(&self) -> Result<(), RecordReplicationError> {
        if !matches!(
            self.protocol_version,
            LEGACY_RECORD_ENVELOPE_VERSION
                | RECORD_ENVELOPE_V2
                | RECORD_ENVELOPE_V3
                | RECORD_ENVELOPE_VERSION
                | RECORD_ENVELOPE_V5
        ) {
            return Err(RecordReplicationError::UnsupportedEnvelopeVersion(
                self.protocol_version,
            ));
        }
        if self.sequence == 0 {
            return Err(RecordReplicationError::InvalidSequence);
        }
        if self.protocol_version == LEGACY_RECORD_ENVELOPE_VERSION
            && !matches!(
                self.operation,
                RecordOperationV1::SetMetadata(_) | RecordOperationV1::AppendVersion(_)
            )
        {
            return Err(RecordReplicationError::OperationRequiresV2);
        }
        if self.protocol_version < RECORD_ENVELOPE_V3
            && matches!(self.operation, RecordOperationV1::SetBinding(_))
        {
            return Err(RecordReplicationError::OperationRequiresV3);
        }
        if self.protocol_version != RECORD_ENVELOPE_VERSION
            && matches!(
                self.operation,
                RecordOperationV1::PrepareTransaction(_)
                    | RecordOperationV1::CommitTransaction { .. }
            )
        {
            return Err(RecordReplicationError::OperationRequiresV4);
        }
        // An attribution travels only at V5, and V5 carries nothing else.
        if (self.protocol_version == RECORD_ENVELOPE_V5)
            != matches!(self.operation, RecordOperationV1::AttributeVersion(_))
        {
            return Err(RecordReplicationError::OperationRequiresV5);
        }
        self.operation.validate_for(self.record_id, self.actor)?;
        Ok(())
    }
}

/// The authorized, record-local projection returned to a reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordSnapshot {
    /// The only record represented by this snapshot.
    pub record_id: FileRecordId,
    /// Latest authenticated structural binding, if the record predates the
    /// binding schema and has not yet been migrated.
    pub binding: Option<FileBindingV1>,
    /// Latest accepted metadata projection, if any.
    pub metadata: Option<FileMetadataV1>,
    /// Deterministically selected, signed current content tuple.
    pub content_revision: Option<ContentRevisionV1>,
    pub versions: Vec<FileVersionV1>,
    /// First-class open and resolved conflicts accepted for this record.
    pub conflicts: Vec<FileConflictV1>,
    /// The agents behind those versions an agent caused, as each version's
    /// own signing device recorded it, by version id.
    pub attributions: Vec<FileVersionAttributionV1>,
}

/// In-memory merge state for exactly one file record.
#[derive(Clone, Debug)]
pub struct RecordReplica {
    record_id: FileRecordId,
    binding: Option<(BindingStamp, FileBindingV1)>,
    metadata: Option<(MetadataStamp, FileMetadataV1)>,
    content_revision: Option<(RevisionStamp, ContentRevisionV1)>,
    /// Keep every incomparable signed candidate, including candidates that
    /// lose the concurrent tie-break. A later descendant can supersede that
    /// winner without superseding the other branch.
    content_revision_frontier: HashMap<FileVersionId, (RevisionStamp, ContentRevisionV1)>,
    versions: HashMap<FileVersionId, FileVersionV1>,
    /// Signed versions proposed by locally committed transaction envelopes.
    /// They are validation dependencies only; repository-wide participant
    /// completeness decides whether projection may expose them.
    transaction_versions: HashMap<FileVersionId, FileVersionV1>,
    conflicts: HashMap<uuid::Uuid, FileConflictV1>,
    transactions: HashMap<uuid::Uuid, (FileMutationTransaction, bool)>,
    highest_sequence: HashMap<FileActor, u64>,
    /// Who caused each attributed version, by its own signing actor.
    attributions: HashMap<FileVersionId, FileAttributionV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MetadataStamp {
    observed_at_ms: u64,
    sequence: u64,
    person: [u8; 32],
    device: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct BindingStamp {
    observed_at_ms: u64,
    sequence: u64,
    person: [u8; 32],
    device: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct RevisionStamp {
    /// Signed version time orders incomparable causal tips. In particular this
    /// makes concurrent conflict-resolution result revisions choose the same
    /// winner as `prefer_conflict_resolution` (timestamp, actor, result),
    /// instead of letting unrelated per-device sequence counters disagree
    /// with the visible resolution record.
    timestamp_ms: u64,
    person: [u8; 32],
    device: [u8; 32],
    version: [u8; 16],
    sequence: u64,
}

impl BindingStamp {
    fn for_binding(binding: &FileBindingV1, sequence: u64) -> Self {
        Self {
            observed_at_ms: binding.observed_at_ms,
            sequence,
            person: *binding.actor.person.as_bytes(),
            device: *binding.actor.device.as_bytes(),
        }
    }
}

impl RevisionStamp {
    fn for_version(version: &FileVersionV1, actor: FileActor, sequence: u64) -> Self {
        Self {
            timestamp_ms: version.timestamp_ms,
            person: *actor.person.as_bytes(),
            device: *actor.device.as_bytes(),
            version: *version.id.0.as_bytes(),
            sequence,
        }
    }
}

impl RecordReplica {
    /// Apply a repository-verified complete participant using exactly the same
    /// signed field ordering as ordinary operations. This mutates a projection
    /// clone only; incomplete transactions never enter the live replica fields.
    pub(crate) fn project_completed_participant(
        &mut self,
        participant: &fs_protocol::FileMutationParticipant,
        commit_sequence: u64,
    ) {
        let binding = &participant.proposed_binding;
        let stamp = BindingStamp::for_binding(binding, commit_sequence);
        if participant.role != fs_protocol::FileMutationParticipantRole::RestoreContent
            && self
                .binding
                .as_ref()
                .is_none_or(|(current, _)| stamp > *current)
        {
            self.binding = Some((stamp, binding.clone()));
        }
        if let Some(revision) = participant.proposed_content_revision {
            let stamp = RevisionStamp::for_version(
                &participant.proposed_version,
                participant.proposed_version.actor,
                commit_sequence,
            );
            self.select_content_revision(stamp, revision);
        }
    }

    /// Start an empty replica scoped to `record_id`.
    #[must_use]
    pub fn new(record_id: FileRecordId) -> Self {
        Self {
            record_id,
            binding: None,
            metadata: None,
            content_revision: None,
            content_revision_frontier: HashMap::new(),
            versions: HashMap::new(),
            transaction_versions: HashMap::new(),
            conflicts: HashMap::new(),
            transactions: HashMap::new(),
            highest_sequence: HashMap::new(),
            attributions: HashMap::new(),
        }
    }

    /// The stable record represented by this replica.
    #[must_use]
    pub fn record_id(&self) -> FileRecordId {
        self.record_id
    }

    /// Authenticate, authorize, replay-check, and merge an operation.
    ///
    /// State is not mutated until every security check succeeds.
    pub fn merge<A: RecordAuthorizer + ?Sized>(
        &mut self,
        envelope: &RecordEnvelopeV1,
        authorizer: &A,
    ) -> Result<(), RecordReplicationError> {
        envelope.verify()?;
        self.merge_authenticated(envelope, authorizer)
    }

    /// [`Self::merge`] for an envelope this process just signed with the
    /// actor's own device key: the signature is not checked again (it is
    /// in debug builds); certification, grant and state checks all run.
    pub(crate) fn merge_own<A: RecordAuthorizer + ?Sized>(
        &mut self,
        envelope: &RecordEnvelopeV1,
        authorizer: &A,
    ) -> Result<(), RecordReplicationError> {
        debug_assert!(envelope.verify().is_ok(), "an own envelope verifies");
        self.merge_authenticated(envelope, authorizer)
    }

    /// [`Self::merge`] for an envelope whose exact bytes the actor's device
    /// already vouched for another way (a verified signed changeset names
    /// their digest): its own signature is not checked again; everything
    /// else is.
    pub(crate) fn merge_authenticated<A: RecordAuthorizer + ?Sized>(
        &mut self,
        envelope: &RecordEnvelopeV1,
        authorizer: &A,
    ) -> Result<(), RecordReplicationError> {
        envelope.validate()?;
        if envelope.record_id != self.record_id {
            return Err(RecordReplicationError::WrongRecord);
        }
        if !authorizer.actor_is_certified(&envelope.actor) {
            return Err(RecordReplicationError::UncertifiedActor);
        }
        if !authorizer.allows(&envelope.actor, self.record_id, RecordAction::Write) {
            return Err(RecordReplicationError::WriteDenied);
        }
        self.apply_verified(envelope)
    }

    /// Restore one envelope from the repository's operation log, which
    /// holds only envelopes verified when they were admitted: the signature
    /// is not checked again, every state-machine check still runs.
    pub(crate) fn restore_trusted(
        &mut self,
        envelope: &RecordEnvelopeV1,
    ) -> Result<(), RecordReplicationError> {
        if envelope.record_id != self.record_id {
            return Err(RecordReplicationError::WrongRecord);
        }
        self.apply_verified(envelope)
    }

    // Keep the exhaustive authenticated operation state machine together so a
    // newly added operation cannot bypass common sequence bookkeeping.
    #[allow(
        clippy::too_many_lines,
        reason = "the exhaustive authenticated operation state machine shares sequence validation and final sequence bookkeeping"
    )]
    fn apply_verified(
        &mut self,
        envelope: &RecordEnvelopeV1,
    ) -> Result<(), RecordReplicationError> {
        let highest = self
            .highest_sequence
            .get(&envelope.actor)
            .copied()
            .unwrap_or(0);
        if envelope.sequence <= highest {
            return Err(RecordReplicationError::Replay {
                sequence: envelope.sequence,
            });
        }
        if envelope.sequence != highest.saturating_add(1) {
            return Err(RecordReplicationError::SequenceGap {
                expected: highest.saturating_add(1),
                received: envelope.sequence,
            });
        }

        match &envelope.operation {
            RecordOperationV1::SetBinding(binding) => {
                let stamp = BindingStamp::for_binding(binding, envelope.sequence);
                if self
                    .binding
                    .as_ref()
                    .is_none_or(|(current, _)| stamp > *current)
                {
                    self.binding = Some((stamp, binding.clone()));
                }
            }
            RecordOperationV1::PrepareTransaction(transaction) => {
                let id = transaction.manifest.transaction_id;
                if let Some((existing, _)) = self.transactions.get(&id) {
                    if existing != transaction {
                        return Err(RecordReplicationError::TransactionIdCollision(id));
                    }
                } else {
                    self.transactions.insert(id, (transaction.clone(), false));
                }
            }
            RecordOperationV1::CommitTransaction { transaction_id } => {
                let Some((transaction, committed)) = self.transactions.get_mut(transaction_id)
                else {
                    return Err(RecordReplicationError::TransactionNotPrepared(
                        *transaction_id,
                    ));
                };
                *committed = true;
                if let Some(participant) = transaction
                    .manifest
                    .participants
                    .iter()
                    .find(|participant| participant.record_id == self.record_id)
                {
                    self.transaction_versions.insert(
                        participant.proposed_version.id,
                        participant.proposed_version.clone(),
                    );
                }
            }
            RecordOperationV1::SetMetadata(metadata) => {
                let stamp = MetadataStamp {
                    observed_at_ms: metadata.observed_at_ms,
                    sequence: envelope.sequence,
                    person: *envelope.actor.person.as_bytes(),
                    device: *envelope.actor.device.as_bytes(),
                };
                if self
                    .metadata
                    .as_ref()
                    .is_none_or(|(current, _)| stamp > *current)
                {
                    self.metadata = Some((stamp, metadata.clone()));
                }
            }
            RecordOperationV1::AppendVersion(version) => {
                if let Some(existing) = self.versions.get(&version.id) {
                    if existing != version {
                        return Err(RecordReplicationError::VersionIdCollision(version.id));
                    }
                } else {
                    if version.parents.iter().any(|parent| {
                        !self.versions.contains_key(parent)
                            && !self.transaction_versions.contains_key(parent)
                    }) {
                        return Err(RecordReplicationError::MissingVersionParent);
                    }
                    if !self.versions.is_empty() && version.parents.is_empty() {
                        return Err(RecordReplicationError::DisconnectedVersion(version.id));
                    }
                    self.versions.insert(version.id, version.clone());
                }
            }
            RecordOperationV1::SetContentRevision(revision) => {
                let version = self
                    .versions
                    .get(&revision.version_id)
                    .or_else(|| self.transaction_versions.get(&revision.version_id))
                    .ok_or(RecordReplicationError::UnknownContentRevisionVersion)?;
                if version.record_id != revision.record_id
                    || version.content_hash != revision.content_hash
                    || version.metadata_hash != revision.metadata_hash
                {
                    return Err(RecordReplicationError::ContentRevisionMismatch);
                }
                let stamp = RevisionStamp::for_version(version, envelope.actor, envelope.sequence);
                self.select_content_revision(stamp, *revision);
            }
            RecordOperationV1::AppendConflict(conflict) => {
                if conflict.versions.iter().any(|version| {
                    !self.versions.contains_key(version)
                        && !self.transaction_versions.contains_key(version)
                }) {
                    return Err(RecordReplicationError::UnknownConflictVersion);
                }
                if let Some(existing) = self.conflicts.get(&conflict.id) {
                    if existing != conflict {
                        return Err(RecordReplicationError::ConflictIdCollision(conflict.id));
                    }
                } else {
                    self.conflicts.insert(conflict.id, conflict.clone());
                }
            }
            RecordOperationV1::ResolveConflict {
                conflict_id,
                resolution,
            } => {
                let conflict = self
                    .conflicts
                    .get(conflict_id)
                    .ok_or(RecordReplicationError::UnknownConflict(*conflict_id))?;
                let result = self
                    .versions
                    .get(&resolution.result)
                    .or_else(|| self.transaction_versions.get(&resolution.result))
                    .ok_or(RecordReplicationError::UnknownConflictVersion)?;
                if result.provenance != FileProvenance::ConflictResolution
                    || result.actor != resolution.actor
                    || result.timestamp_ms != resolution.timestamp_ms
                    || !conflict
                        .versions
                        .iter()
                        .all(|parent| result.parents.contains(parent))
                {
                    return Err(RecordReplicationError::InvalidConflictTransition);
                }
                if conflict
                    .resolution
                    .as_ref()
                    .is_none_or(|current| prefer_conflict_resolution(resolution, current))
                {
                    let mut resolved = conflict.clone();
                    resolved.status = ConflictStatus::Resolved;
                    resolved.resolution = Some(resolution.clone());
                    resolved.validate()?;
                    self.conflicts.insert(*conflict_id, resolved);
                }
            }
            RecordOperationV1::AttributeVersion(attributed) => {
                // Only the device that signed a version says who caused it:
                // an attribution never reaches another actor's version.
                let version = self
                    .versions
                    .get(&attributed.version_id)
                    .or_else(|| self.transaction_versions.get(&attributed.version_id))
                    .ok_or(RecordReplicationError::UnknownAttributedVersion)?;
                if version.actor != envelope.actor {
                    return Err(RecordReplicationError::ForeignAttribution);
                }
                match self.attributions.get(&attributed.version_id) {
                    Some(existing) if *existing != attributed.attribution => {
                        return Err(RecordReplicationError::AttributionCollision(
                            attributed.version_id,
                        ));
                    }
                    Some(_) => {}
                    None => {
                        self.attributions
                            .insert(attributed.version_id, attributed.attribution.clone());
                    }
                }
            }
        }
        self.highest_sequence
            .insert(envelope.actor, envelope.sequence);
        Ok(())
    }

    fn select_content_revision(&mut self, stamp: RevisionStamp, revision: ContentRevisionV1) {
        if self
            .content_revision_frontier
            .get(&revision.version_id)
            .is_none_or(|(current, _)| stamp > *current)
        {
            self.content_revision_frontier
                .insert(revision.version_id, (stamp, revision));
        }
        // Causality precedes the concurrent stamp: a signed edit supersedes
        // its ancestors even when the clock has not advanced. Traverse the
        // union once, rather than comparing pairs with an order that can cycle.
        let mut ancestors = HashSet::new();
        let mut pending = Vec::new();
        for version_id in self.content_revision_frontier.keys() {
            if let Some(version) = self
                .versions
                .get(version_id)
                .or_else(|| self.transaction_versions.get(version_id))
            {
                pending.extend(version.parents.iter().copied());
            }
        }
        while let Some(version_id) = pending.pop() {
            if ancestors.insert(version_id)
                && let Some(version) = self
                    .versions
                    .get(&version_id)
                    .or_else(|| self.transaction_versions.get(&version_id))
            {
                pending.extend(version.parents.iter().copied());
            }
        }
        self.content_revision_frontier
            .retain(|version_id, _| !ancestors.contains(version_id));
        self.content_revision = self
            .content_revision_frontier
            .values()
            .max_by_key(|(stamp, _)| *stamp)
            .copied();
    }

    pub(crate) fn version_count(&self) -> usize {
        self.versions.len()
    }

    pub(crate) fn conflict_count(&self) -> usize {
        self.conflicts.len()
    }

    pub(crate) fn transaction_state(
        &self,
        transaction_id: uuid::Uuid,
    ) -> Option<(&FileMutationTransaction, bool)> {
        self.transactions
            .get(&transaction_id)
            .map(|(transaction, committed)| (transaction, *committed))
    }

    pub(crate) fn transactions(&self) -> impl Iterator<Item = (&FileMutationTransaction, bool)> {
        self.transactions
            .values()
            .map(|(transaction, committed)| (transaction, *committed))
    }

    pub(crate) fn highest_sequence_for(&self, actor: &FileActor) -> Option<u64> {
        self.highest_sequence.get(actor).copied()
    }

    /// Return this record's state only after a live read decision.
    pub fn snapshot<A: RecordAuthorizer + ?Sized>(
        &self,
        requester: &FileActor,
        authorizer: &A,
    ) -> Result<RecordSnapshot, RecordReplicationError> {
        if !authorizer.actor_is_certified(requester) {
            return Err(RecordReplicationError::UncertifiedActor);
        }
        if !authorizer.allows(requester, self.record_id, RecordAction::Read) {
            return Err(RecordReplicationError::ReadDenied);
        }
        Ok(self.restored_snapshot())
    }

    /// The selected content revision, without materialising a snapshot.
    pub(crate) fn current_content_revision(&self) -> Option<ContentRevisionV1> {
        self.content_revision.map(|(_, value)| value)
    }

    /// The converged signed structural binding, if the record has one.
    /// Reading it must not clone versions or conflicts: reconciliation asks
    /// every record for this once per dependency round.
    pub(crate) fn converged_binding(&self) -> Option<FileBindingV1> {
        self.binding.as_ref().map(|(_, binding)| binding.clone())
    }

    /// Whether this record's signed state carries file content anywhere:
    /// a converged revision or metadata, or a version with a content hash.
    /// A directory binding is malformed for such a record.
    pub(crate) fn has_file_content(&self) -> bool {
        self.content_revision.is_some()
            || self.metadata.is_some()
            || self
                .versions
                .values()
                .chain(self.transaction_versions.values())
                .any(|version| version.content_hash.is_some())
    }

    /// Whether a completed transaction's participant binding for this record
    /// is newer than the record's ordinary binding. Such a binding is owned by
    /// the transaction projector: replaying the older ordinary binding first
    /// would undo the transaction's structural effect.
    pub(crate) fn transaction_binding_is_newer(
        &self,
        transaction: &FileMutationTransaction,
        commit_sequence: u64,
    ) -> bool {
        let Some(participant) = transaction
            .manifest
            .participants
            .iter()
            .find(|participant| participant.record_id == self.record_id)
        else {
            return false;
        };
        if participant.role == fs_protocol::FileMutationParticipantRole::RestoreContent {
            return false;
        }
        let stamp = BindingStamp::for_binding(&participant.proposed_binding, commit_sequence);
        self.binding
            .as_ref()
            .is_none_or(|(current, _)| stamp > *current)
    }

    /// Whether an ordinary binding accepted for this record is newer than a
    /// completed transaction's participant binding, so the transaction must
    /// not move the record back.
    pub(crate) fn binding_supersedes_transaction(
        &self,
        transaction: &FileMutationTransaction,
        commit_sequence: u64,
    ) -> bool {
        let Some(participant) = transaction
            .manifest
            .participants
            .iter()
            .find(|participant| participant.record_id == self.record_id)
        else {
            return false;
        };
        if participant.role == fs_protocol::FileMutationParticipantRole::RestoreContent {
            return false;
        }
        let stamp = BindingStamp::for_binding(&participant.proposed_binding, commit_sequence);
        self.binding
            .as_ref()
            .is_some_and(|(current, _)| *current > stamp)
    }

    /// Materialized view of state already authenticated and durably restored
    /// by the repository. This never crosses a request boundary; it exists for
    /// crash-recovery reconciliation only.
    pub(crate) fn restored_snapshot(&self) -> RecordSnapshot {
        let mut versions: Vec<_> = self.versions.values().cloned().collect();
        versions.sort_by_key(|version| version.id.0);
        let mut conflicts: Vec<_> = self.conflicts.values().cloned().collect();
        conflicts.sort_by_key(|conflict| conflict.id);
        let mut attributions: Vec<_> = self
            .attributions
            .iter()
            .map(|(version_id, attribution)| FileVersionAttributionV1 {
                version_id: *version_id,
                attribution: attribution.clone(),
            })
            .collect();
        attributions.sort_by_key(|attributed| attributed.version_id.0);
        RecordSnapshot {
            record_id: self.record_id,
            binding: self.binding.as_ref().map(|(_, value)| value.clone()),
            metadata: self.metadata.as_ref().map(|(_, value)| value.clone()),
            content_revision: self.content_revision.map(|(_, value)| value),
            versions,
            conflicts,
            attributions,
        }
    }
}

/// Failures that reject a record operation or attempted read.
#[derive(Debug, Error)]
pub enum RecordReplicationError {
    /// The wire schema is not supported.
    #[error("unsupported record envelope version {0}")]
    UnsupportedEnvelopeVersion(u16),
    #[error("record operation requires envelope version 2")]
    OperationRequiresV2,
    #[error("record operation requires envelope version 3")]
    OperationRequiresV3,
    #[error("record operation requires envelope version 4")]
    OperationRequiresV4,
    #[error("a version attribution travels alone at envelope version 5")]
    OperationRequiresV5,
    /// Sequence zero is reserved and cannot be accepted.
    #[error("record sequence numbers start at one")]
    InvalidSequence,
    /// Envelope scope does not match the receiving replica.
    #[error("operation was addressed to a different record")]
    WrongRecord,
    /// A payload's embedded record does not match its signed envelope.
    #[error("payload record does not match signed envelope scope")]
    PayloadRecordMismatch,
    /// A payload's embedded actor does not match its signed envelope.
    #[error("payload actor does not match signed envelope actor")]
    PayloadActorMismatch,
    /// The human-to-device identity binding is absent or revoked.
    #[error("actor device is not currently certified for the person")]
    UncertifiedActor,
    #[error("record read denied")]
    ReadDenied,
    /// The author lacks the record-scoped write grant.
    #[error("record write denied")]
    WriteDenied,
    /// This sequence has already been consumed or superseded.
    #[error("record operation sequence {sequence} was replayed or is stale")]
    Replay {
        /// Rejected sequence number.
        sequence: u64,
    },
    #[error("record operation sequence gap: expected {expected}, received {received}")]
    SequenceGap { expected: u64, received: u64 },
    /// Two immutable nodes used the same identifier with different content.
    #[error("version id {0:?} names conflicting immutable nodes")]
    VersionIdCollision(FileVersionId),
    #[error("version references a parent that is not present")]
    MissingVersionParent,
    #[error("version {0:?} is disconnected from existing history")]
    DisconnectedVersion(FileVersionId),
    #[error("content revision references a version that is not present")]
    UnknownContentRevisionVersion,
    #[error("content revision does not match its immutable version")]
    ContentRevisionMismatch,
    #[error("conflict id {0} names different conflict records")]
    ConflictIdCollision(uuid::Uuid),
    #[error("conflict {0} does not exist")]
    UnknownConflict(uuid::Uuid),
    #[error("conflict references a version that is not present")]
    UnknownConflictVersion,
    #[error("conflict {0} was already resolved differently")]
    ConflictAlreadyResolved(uuid::Uuid),
    #[error("conflict operation is not a valid convergent transition")]
    InvalidConflictTransition,
    #[error("transaction manifest or participant binding is invalid")]
    InvalidTransaction,
    #[error("transaction id {0} names a different signed manifest")]
    TransactionIdCollision(uuid::Uuid),
    #[error("transaction {0} was committed before it was prepared")]
    TransactionNotPrepared(uuid::Uuid),
    #[error("attribution names a version that is not present")]
    UnknownAttributedVersion,
    #[error("attribution names a version another actor signed")]
    ForeignAttribution,
    #[error("version {0:?} was already attributed differently")]
    AttributionCollision(FileVersionId),
    /// Payload validation failed.
    #[error(transparent)]
    InvalidPayload(#[from] FileRecordError),
    /// Signature verification failed.
    #[error(transparent)]
    Signature(#[from] VerifyError),
    /// Canonical envelope encoding or decoding failed.
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
}

/// Authorizers for tests whose subject is not authorization itself.
#[cfg(test)]
pub(crate) mod test_authorizers {
    use super::{FileActor, FileRecordId, RecordAction, RecordAuthorizer};
    use std::collections::BTreeSet;

    /// Certifies every actor and allows every action.
    pub(crate) struct AllowAll;

    impl RecordAuthorizer for AllowAll {
        fn actor_is_certified(&self, _: &FileActor) -> bool {
            true
        }

        fn allows(&self, _: &FileActor, _: FileRecordId, _: RecordAction) -> bool {
            true
        }
    }

    /// Certifies one actor and allows it every action on every record.
    #[derive(Clone, Copy)]
    pub(crate) struct AllowActor(pub(crate) FileActor);

    impl RecordAuthorizer for AllowActor {
        fn actor_is_certified(&self, actor: &FileActor) -> bool {
            *actor == self.0
        }

        fn allows(&self, actor: &FileActor, _: FileRecordId, _: RecordAction) -> bool {
            *actor == self.0
        }
    }

    /// Certifies one actor and allows it every action on these records only.
    pub(crate) struct AllowActorOn {
        pub(crate) actor: FileActor,
        pub(crate) records: BTreeSet<FileRecordId>,
    }

    impl RecordAuthorizer for AllowActorOn {
        fn actor_is_certified(&self, actor: &FileActor) -> bool {
            *actor == self.actor
        }

        fn allows(&self, actor: &FileActor, record: FileRecordId, _: RecordAction) -> bool {
            *actor == self.actor && self.records.contains(&record)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use collab_types::generate_keypair;
    use fs_protocol::{
        CONTENT_REVISION_VERSION, ConflictResolution, ConflictStatus, ConflictStrategy,
        ContentHash, FILE_BINDING_VERSION, FILE_MUTATION_TRANSACTION_VERSION, FileConflictV1,
        FileMutationOperation, FileMutationParticipant, FileMutationParticipantRole,
        FileMutationTransactionManifest, FileNodeKind, FilePropertiesV1, FileProvenance,
        FileVersionId, IndexStatus,
    };

    #[derive(Default)]
    struct TestAuthorizer {
        certified: HashSet<FileActor>,
        readers: HashSet<(FileRecordId, FileActor)>,
        writers: HashSet<(FileRecordId, FileActor)>,
    }

    impl RecordAuthorizer for TestAuthorizer {
        fn actor_is_certified(&self, actor: &FileActor) -> bool {
            self.certified.contains(actor)
        }

        fn allows(&self, actor: &FileActor, id: FileRecordId, action: RecordAction) -> bool {
            match action {
                RecordAction::Read => self.readers.contains(&(id, *actor)),
                RecordAction::Write => self.writers.contains(&(id, *actor)),
            }
        }
    }

    fn fixture() -> (
        ed25519_dalek::SigningKey,
        FileActor,
        FileRecordId,
        TestAuthorizer,
    ) {
        let (device_key, device) = generate_keypair();
        let (_, person) = generate_keypair();
        let actor = FileActor { person, device };
        let record = FileRecordId::new();
        let mut auth = TestAuthorizer::default();
        auth.certified.insert(actor);
        auth.readers.insert((record, actor));
        auth.writers.insert((record, actor));
        (device_key, actor, record, auth)
    }

    fn metadata(record: FileRecordId, actor: FileActor, observed_at_ms: u64) -> FileMetadataV1 {
        FileMetadataV1 {
            schema_version: fs_protocol::files::FILE_METADATA_VERSION,
            record_id: record,
            content_hash: Some(ContentHash([7; 32])),
            byte_len: 12,
            mime: Some("text/plain".into()),
            previews: vec![],
            latest_actor: Some(actor),
            observed_at_ms,
            index_status: IndexStatus::Indexed,
            properties: Some(FilePropertiesV1 {
                schema_version: fs_protocol::files::FILE_PROPERTIES_VERSION,
                image_width: Some(16),
                image_height: Some(9),
                duration_ms: None,
                page_count: None,
                codec: Some("png".into()),
                container: Some("png".into()),
            }),
        }
    }

    fn envelope(
        key: &ed25519_dalek::SigningKey,
        actor: FileActor,
        record: FileRecordId,
        sequence: u64,
    ) -> RecordEnvelopeV1 {
        RecordEnvelopeV1::signed(
            record,
            actor,
            sequence,
            RecordOperationV1::SetMetadata(metadata(record, actor, sequence)),
            key,
        )
        .unwrap()
    }

    fn transaction(
        key: &ed25519_dalek::SigningKey,
        actor: FileActor,
        source: FileRecordId,
        replaced: FileRecordId,
    ) -> FileMutationTransaction {
        let destination_parent = FileRecordId::new();
        let participant = |record_id, role| {
            let expected = FileVersionId::new();
            let version = FileVersionV1 {
                id: FileVersionId::new(),
                record_id,
                parents: vec![expected],
                actor,
                timestamp_ms: 5,
                content_hash: None,
                metadata_hash: ContentHash([5; 32]),
                provenance: match role {
                    FileMutationParticipantRole::Source => FileProvenance::Rename,
                    FileMutationParticipantRole::Replaced => FileProvenance::Delete,
                    _ => unreachable!("replacement fixture role"),
                },
            };
            let mut value = FileMutationParticipant {
                record_id,
                role,
                expected_version: expected,
                proposed_binding: FileBindingV1 {
                    schema_version: FILE_BINDING_VERSION,
                    record_id,
                    parent: Some(destination_parent),
                    name: "destination.txt".into(),
                    kind: FileNodeKind::File,
                    tombstoned: role == FileMutationParticipantRole::Replaced,
                    actor,
                    observed_at_ms: 5,
                },
                proposed_content_revision: Some(ContentRevisionV1 {
                    schema_version: CONTENT_REVISION_VERSION,
                    record_id,
                    version_id: version.id,
                    content_hash: version.content_hash,
                    byte_len: 0,
                    metadata_hash: version.metadata_hash,
                }),
                proposed_version: version,
                projection_hash: ContentHash([0; 32]),
            };
            value.projection_hash = value.computed_projection_hash().unwrap();
            value
        };
        let mut participants = vec![
            participant(source, FileMutationParticipantRole::Source),
            participant(replaced, FileMutationParticipantRole::Replaced),
        ];
        participants.sort_by_key(|value| value.record_id);
        FileMutationTransaction::signed(
            FileMutationTransactionManifest {
                schema_version: FILE_MUTATION_TRANSACTION_VERSION,
                transaction_id: uuid::Uuid::new_v4(),
                workspace_id: uuid::Uuid::new_v4(),
                actor,
                timestamp_ms: 5,
                operation: FileMutationOperation::ReplacementRename {
                    source_parent: FileRecordId::new(),
                    source_name: "source.txt".into(),
                    destination_parent,
                    destination_name: "destination.txt".into(),
                    source_record: source,
                    replaced_record: replaced,
                },
                participants,
            },
            key,
        )
        .unwrap()
    }

    fn causal_revision_operations(
        actors: &[(ed25519_dalek::SigningKey, FileActor)],
        root: FileVersionV1,
        versions: &[FileVersionV1],
    ) -> (RecordReplica, Vec<RecordEnvelopeV1>, TestAuthorizer) {
        let record = root.record_id;
        let mut auth = TestAuthorizer::default();
        for (_, actor) in actors {
            auth.certified.insert(*actor);
            auth.readers.insert((record, *actor));
            auth.writers.insert((record, *actor));
        }
        let (key, actor) = actors.first().unwrap();
        let mut common = RecordReplica::new(record);
        common
            .merge(
                &RecordEnvelopeV1::signed(
                    record,
                    *actor,
                    1,
                    RecordOperationV1::AppendVersion(root),
                    key,
                )
                .unwrap(),
                &auth,
            )
            .unwrap();
        for index in [2, 0, 1] {
            let (key, actor) = actors.get(index).unwrap();
            common
                .merge(
                    &RecordEnvelopeV1::signed(
                        record,
                        *actor,
                        if index == 0 { 2 } else { 1 },
                        RecordOperationV1::AppendVersion(versions.get(index).unwrap().clone()),
                        key,
                    )
                    .unwrap(),
                    &auth,
                )
                .unwrap();
        }
        let proposals = actors
            .iter()
            .zip(versions)
            .enumerate()
            .map(|(index, ((key, actor), version))| {
                RecordEnvelopeV1::signed(
                    record,
                    *actor,
                    if index == 0 { 3 } else { 2 },
                    RecordOperationV1::SetContentRevision(ContentRevisionV1 {
                        schema_version: CONTENT_REVISION_VERSION,
                        record_id: record,
                        version_id: version.id,
                        content_hash: version.content_hash,
                        byte_len: 1,
                        metadata_hash: version.metadata_hash,
                    }),
                    key,
                )
                .unwrap()
            })
            .collect();
        (common, proposals, auth)
    }

    #[test]
    fn causal_revision_frontier_converges_across_all_publication_orders() {
        let mut actors: Vec<_> = (0..3)
            .map(|_| {
                let (key, actor, _, _) = fixture();
                (key, actor)
            })
            .collect();
        actors.sort_by_key(|(_, actor)| (*actor.person.as_bytes(), *actor.device.as_bytes()));
        let record = FileRecordId::new();
        let (_, first_actor) = actors.first().unwrap();
        let root = FileVersionV1 {
            id: FileVersionId(uuid::Uuid::from_u128(4)),
            record_id: record,
            parents: vec![],
            actor: *first_actor,
            timestamp_ms: 1,
            content_hash: Some(ContentHash([4; 32])),
            metadata_hash: ContentHash([14; 32]),
            provenance: FileProvenance::Create,
        };
        let versions: Vec<_> = actors
            .iter()
            .enumerate()
            .map(|(index, (_, actor))| FileVersionV1 {
                id: FileVersionId(uuid::Uuid::from_u128(index as u128 + 1)),
                record_id: record,
                parents: vec![if index == 0 {
                    FileVersionId(uuid::Uuid::from_u128(3))
                } else {
                    root.id
                }],
                actor: *actor,
                timestamp_ms: 10,
                content_hash: Some(ContentHash([u8::try_from(index).unwrap(); 32])),
                metadata_hash: ContentHash([u8::try_from(index).unwrap() + 10; 32]),
                provenance: FileProvenance::Edit,
            })
            .collect();
        let (common, proposals, auth) = causal_revision_operations(&actors, root, &versions);
        let expected = versions.get(1).unwrap().id;
        let mut converged = None;
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let mut replica = common.clone();
            for index in order {
                replica.merge(proposals.get(index).unwrap(), &auth).unwrap();
            }
            let snapshot = replica.snapshot(first_actor, &auth).unwrap();
            assert_eq!(
                snapshot.content_revision.unwrap().version_id,
                expected,
                "the ancestor loses, but its incomparable competitor remains eligible"
            );
            if let Some(previous) = &converged {
                assert_eq!(
                    &snapshot, previous,
                    "the same signed operations must converge"
                );
            } else {
                converged = Some(snapshot);
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn signed_transactions_revisions_and_conflicts_replicate_exactly() {
        {
            {
                let (key, actor, record, auth) = fixture();
                let signed = transaction(&key, actor, record, FileRecordId::new());
                let id = signed.manifest.transaction_id;
                let mut replica = RecordReplica::new(record);
                let commit = RecordEnvelopeV1::signed(
                    record,
                    actor,
                    1,
                    RecordOperationV1::CommitTransaction { transaction_id: id },
                    &key,
                )
                .unwrap();
                assert!(matches!(
                    replica.merge(&commit, &auth),
                    Err(RecordReplicationError::TransactionNotPrepared(value)) if value == id
                ));
                let prepare = RecordEnvelopeV1::signed(
                    record,
                    actor,
                    1,
                    RecordOperationV1::PrepareTransaction(signed.clone()),
                    &key,
                )
                .unwrap();
                replica.merge(&prepare, &auth).unwrap();
                let snapshot = replica.restored_snapshot();
                assert!(snapshot.binding.is_none());
                assert!(snapshot.versions.is_empty());
                assert_eq!(replica.transaction_state(id), Some((&signed, false)));
                let commit = RecordEnvelopeV1::signed(
                    record,
                    actor,
                    2,
                    RecordOperationV1::CommitTransaction { transaction_id: id },
                    &key,
                )
                .unwrap();
                replica.merge(&commit, &auth).unwrap();
                assert_eq!(replica.transaction_state(id), Some((&signed, true)));
            }
            {
                let (key, actor, record, auth) = fixture();
                let version = FileVersionV1 {
                    id: FileVersionId::new(),
                    record_id: record,
                    parents: vec![],
                    actor,
                    timestamp_ms: 8,
                    content_hash: Some(ContentHash([8; 32])),
                    metadata_hash: ContentHash([9; 32]),
                    provenance: FileProvenance::Create,
                };
                let revision = ContentRevisionV1 {
                    schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                    record_id: record,
                    version_id: version.id,
                    content_hash: version.content_hash,
                    byte_len: 123,
                    metadata_hash: version.metadata_hash,
                };
                let mut replica = RecordReplica::new(record);
                let early = RecordEnvelopeV1::signed(
                    record,
                    actor,
                    1,
                    RecordOperationV1::SetContentRevision(revision),
                    &key,
                )
                .unwrap();
                assert!(matches!(
                    replica.merge(&early, &auth),
                    Err(RecordReplicationError::UnknownContentRevisionVersion)
                ));

                let append = RecordEnvelopeV1::signed(
                    record,
                    actor,
                    1,
                    RecordOperationV1::AppendVersion(version),
                    &key,
                )
                .unwrap();
                replica.merge(&append, &auth).unwrap();
                let mut mismatched = revision;
                mismatched.content_hash = Some(ContentHash([7; 32]));
                let bad = RecordEnvelopeV1::signed(
                    record,
                    actor,
                    2,
                    RecordOperationV1::SetContentRevision(mismatched),
                    &key,
                )
                .unwrap();
                assert!(matches!(
                    replica.merge(&bad, &auth),
                    Err(RecordReplicationError::ContentRevisionMismatch)
                ));
                let accepted = RecordEnvelopeV1::signed(
                    record,
                    actor,
                    2,
                    RecordOperationV1::SetContentRevision(revision),
                    &key,
                )
                .unwrap();
                replica.merge(&accepted, &auth).unwrap();
                assert_eq!(
                    replica.snapshot(&actor, &auth).unwrap().content_revision,
                    Some(revision)
                );
            }
            {
                let (key, actor, record, auth) = fixture();
                let first = FileVersionV1 {
                    id: FileVersionId::new(),
                    record_id: record,
                    parents: vec![],
                    actor,
                    timestamp_ms: 1,
                    content_hash: Some(ContentHash([1; 32])),
                    metadata_hash: ContentHash([11; 32]),
                    provenance: FileProvenance::Edit,
                };
                let second = FileVersionV1 {
                    id: FileVersionId::new(),
                    parents: vec![first.id],
                    content_hash: Some(ContentHash([2; 32])),
                    metadata_hash: ContentHash([12; 32]),
                    timestamp_ms: 2,
                    ..first.clone()
                };
                let result = FileVersionV1 {
                    id: FileVersionId::new(),
                    record_id: first.record_id,
                    parents: vec![first.id, second.id],
                    actor: first.actor,
                    content_hash: Some(ContentHash([3; 32])),
                    metadata_hash: ContentHash([13; 32]),
                    timestamp_ms: 3,
                    provenance: FileProvenance::ConflictResolution,
                };
                let conflict = FileConflictV1 {
                    id: uuid::Uuid::new_v4(),
                    record_id: record,
                    versions: vec![first.id, second.id],
                    status: ConflictStatus::Open,
                    resolution: None,
                };
                let resolution = ConflictResolution {
                    strategy: ConflictStrategy::Merge,
                    result: result.id,
                    actor,
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
                let mut replica = RecordReplica::new(record);
                for (index, operation) in operations.into_iter().enumerate() {
                    let envelope =
                        RecordEnvelopeV1::signed(record, actor, index as u64 + 1, operation, &key)
                            .unwrap();
                    replica.merge(&envelope, &auth).unwrap();
                }
                let snapshot = replica.snapshot(&actor, &auth).unwrap();
                assert_eq!(snapshot.conflicts.len(), 1);
                assert_eq!(snapshot.conflicts[0].status, ConflictStatus::Resolved);
                assert_eq!(snapshot.conflicts[0].resolution, Some(resolution));
            }
        }
        {
            let (first_key, first_actor, record, mut auth) = fixture();
            let (second_key, second_device) = generate_keypair();
            let (_, second_person) = generate_keypair();
            let second_actor = FileActor {
                person: second_person,
                device: second_device,
            };
            auth.certified.insert(second_actor);
            auth.readers.insert((record, second_actor));
            auth.writers.insert((record, second_actor));

            let root = FileVersionV1 {
                id: FileVersionId::new(),
                record_id: record,
                parents: vec![],
                actor: first_actor,
                timestamp_ms: 1,
                content_hash: Some(ContentHash([1; 32])),
                metadata_hash: ContentHash([11; 32]),
                provenance: FileProvenance::Create,
            };
            let branch = FileVersionV1 {
                id: FileVersionId::new(),
                record_id: root.record_id,
                parents: vec![root.id],
                actor: root.actor,
                timestamp_ms: 2,
                content_hash: Some(ContentHash([2; 32])),
                metadata_hash: ContentHash([12; 32]),
                provenance: FileProvenance::Edit,
            };
            let conflict = FileConflictV1 {
                id: uuid::Uuid::new_v4(),
                record_id: record,
                versions: vec![root.id, branch.id],
                status: ConflictStatus::Open,
                resolution: None,
            };
            let first_result = FileVersionV1 {
                id: FileVersionId::new(),
                record_id: root.record_id,
                parents: conflict.versions.clone(),
                actor: root.actor,
                timestamp_ms: 5,
                content_hash: root.content_hash,
                metadata_hash: ContentHash([13; 32]),
                provenance: FileProvenance::ConflictResolution,
            };
            let second_result = FileVersionV1 {
                id: FileVersionId::new(),
                record_id: record,
                parents: conflict.versions.clone(),
                actor: second_actor,
                timestamp_ms: 6,
                content_hash: branch.content_hash,
                metadata_hash: ContentHash([14; 32]),
                provenance: FileProvenance::ConflictResolution,
            };
            let common = [
                RecordEnvelopeV1::signed(
                    record,
                    first_actor,
                    1,
                    RecordOperationV1::AppendVersion(root),
                    &first_key,
                )
                .unwrap(),
                RecordEnvelopeV1::signed(
                    record,
                    first_actor,
                    2,
                    RecordOperationV1::AppendVersion(branch),
                    &first_key,
                )
                .unwrap(),
                RecordEnvelopeV1::signed(
                    record,
                    first_actor,
                    3,
                    RecordOperationV1::AppendConflict(conflict.clone()),
                    &first_key,
                )
                .unwrap(),
                RecordEnvelopeV1::signed(
                    record,
                    first_actor,
                    4,
                    RecordOperationV1::AppendVersion(first_result.clone()),
                    &first_key,
                )
                .unwrap(),
                RecordEnvelopeV1::signed(
                    record,
                    first_actor,
                    5,
                    RecordOperationV1::SetContentRevision(ContentRevisionV1 {
                        schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                        record_id: record,
                        version_id: first_result.id,
                        content_hash: first_result.content_hash,
                        byte_len: 10,
                        metadata_hash: first_result.metadata_hash,
                    }),
                    &first_key,
                )
                .unwrap(),
                RecordEnvelopeV1::signed(
                    record,
                    second_actor,
                    1,
                    RecordOperationV1::AppendVersion(second_result.clone()),
                    &second_key,
                )
                .unwrap(),
                RecordEnvelopeV1::signed(
                    record,
                    second_actor,
                    2,
                    RecordOperationV1::SetContentRevision(ContentRevisionV1 {
                        schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                        record_id: record,
                        version_id: second_result.id,
                        content_hash: second_result.content_hash,
                        byte_len: 20,
                        metadata_hash: second_result.metadata_hash,
                    }),
                    &second_key,
                )
                .unwrap(),
            ];
            let first_resolution = ConflictResolution {
                strategy: ConflictStrategy::KeepCurrent,
                result: first_result.id,
                actor: first_actor,
                timestamp_ms: 5,
            };
            let second_resolution = ConflictResolution {
                strategy: ConflictStrategy::UseOther,
                result: second_result.id,
                actor: second_actor,
                timestamp_ms: 6,
            };
            let proposals = [
                RecordEnvelopeV1::signed(
                    record,
                    first_actor,
                    6,
                    RecordOperationV1::ResolveConflict {
                        conflict_id: conflict.id,
                        resolution: first_resolution,
                    },
                    &first_key,
                )
                .unwrap(),
                RecordEnvelopeV1::signed(
                    record,
                    second_actor,
                    3,
                    RecordOperationV1::ResolveConflict {
                        conflict_id: conflict.id,
                        resolution: second_resolution.clone(),
                    },
                    &second_key,
                )
                .unwrap(),
            ];
            let mut left = RecordReplica::new(record);
            for envelope in &common {
                left.merge(envelope, &auth).unwrap();
            }
            let mut right = left.clone();
            left.merge(&proposals[0], &auth).unwrap();
            left.merge(&proposals[1], &auth).unwrap();
            right.merge(&proposals[1], &auth).unwrap();
            right.merge(&proposals[0], &auth).unwrap();
            let left = left.snapshot(&first_actor, &auth).unwrap();
            let right = right.snapshot(&first_actor, &auth).unwrap();
            assert_eq!(left, right);
            assert_eq!(left.conflicts[0].resolution, Some(second_resolution));
            assert_eq!(
                left.content_revision.unwrap().version_id,
                second_result.id,
                "resolution decision and visible content choose the same proposal"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn envelope_signatures_bind_payload_record_actor_and_certified_device() {
        {
            let (key, actor, record, _) = fixture();
            let first = envelope(&key, actor, record, 1);
            let second = envelope(&key, actor, record, 1);
            assert_eq!(
                first.signing_bytes().unwrap(),
                second.signing_bytes().unwrap()
            );
            assert_eq!(first.signature, second.signature);
            let wire = first.canonical_bytes().unwrap();
            assert_eq!(RecordEnvelopeV1::decode_and_verify(&wire).unwrap(), first);
            let mut trailing = wire;
            trailing.push(0);
            assert!(matches!(
                RecordEnvelopeV1::decode_and_verify(&trailing),
                Err(RecordReplicationError::Canonical(_))
            ));
            assert!(
                first
                    .signing_bytes()
                    .unwrap()
                    .windows(RECORD_OPERATION_SIGNATURE_DOMAIN.len())
                    .any(|window| window == RECORD_OPERATION_SIGNATURE_DOMAIN.as_bytes())
            );
        }
        {
            let (key, actor, record, auth) = fixture();
            let mut op = envelope(&key, actor, record, 1);
            let RecordOperationV1::SetMetadata(value) = &mut op.operation else {
                unreachable!();
            };
            value.byte_len += 1;
            let mut replica = RecordReplica::new(record);
            assert!(matches!(
                replica.merge(&op, &auth),
                Err(RecordReplicationError::Signature(VerifyError::BadSignature))
            ));
            assert!(replica.metadata.is_none());
            assert!(replica.highest_sequence.is_empty());
        }
        {
            let (key, actor, record, mut auth) = fixture();
            auth.certified.clear();
            let op = envelope(&key, actor, record, 1);
            let mut replica = RecordReplica::new(record);
            assert!(matches!(
                replica.merge(&op, &auth),
                Err(RecordReplicationError::UncertifiedActor)
            ));
        }
        {
            let (_, actor, record, auth) = fixture();
            let (wrong_key, _wrong_public) = generate_keypair();
            let op = envelope(&wrong_key, actor, record, 1);
            let mut replica = RecordReplica::new(record);
            assert!(matches!(
                replica.merge(&op, &auth),
                Err(RecordReplicationError::Signature(VerifyError::BadSignature))
            ));
        }
        {
            let (key, actor, record, _auth) = fixture();
            let other_record = FileRecordId::new();
            let op = RecordEnvelopeV1::signed(
                record,
                actor,
                1,
                RecordOperationV1::SetMetadata(metadata(other_record, actor, 1)),
                &key,
            )
            .unwrap();
            assert!(matches!(
                op.verify(),
                Err(RecordReplicationError::PayloadRecordMismatch)
            ));
        }
        {
            let (key, actor, record, _) = fixture();
            let mut legacy = envelope(&key, actor, record, 1);
            legacy.protocol_version = LEGACY_RECORD_ENVELOPE_VERSION;
            legacy.signature = sign_event(&legacy.signing_view(), &key).unwrap();
            legacy.verify().unwrap();

            let first = FileVersionId::new();
            let second = FileVersionId::new();
            legacy.operation = RecordOperationV1::AppendConflict(FileConflictV1 {
                id: uuid::Uuid::new_v4(),
                record_id: record,
                versions: vec![first, second],
                status: ConflictStatus::Open,
                resolution: None,
            });
            legacy.signature = sign_event(&legacy.signing_view(), &key).unwrap();
            assert!(matches!(
                legacy.verify(),
                Err(RecordReplicationError::OperationRequiresV2)
            ));
        }
        {
            let (key, actor, record, _) = fixture();
            let parent = FileRecordId::new();
            let binding = FileBindingV1 {
                schema_version: fs_protocol::FILE_BINDING_VERSION,
                record_id: record,
                parent: Some(parent),
                name: "bound.txt".into(),
                kind: fs_protocol::FileNodeKind::File,
                tombstoned: false,
                actor,
                observed_at_ms: 1,
            };
            let mut v3 = RecordEnvelopeV1::signed(
                record,
                actor,
                1,
                RecordOperationV1::SetBinding(binding),
                &key,
            )
            .unwrap();
            v3.protocol_version = RECORD_ENVELOPE_V3;
            v3.signature = sign_event(&v3.signing_view(), &key).unwrap();
            v3.verify().unwrap();

            v3.operation = RecordOperationV1::PrepareTransaction(transaction(
                &key,
                actor,
                record,
                FileRecordId::new(),
            ));
            v3.signature = sign_event(&v3.signing_view(), &key).unwrap();
            assert!(matches!(
                v3.verify(),
                Err(RecordReplicationError::OperationRequiresV4)
            ));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn replication_is_record_local_role_checked_and_replay_safe() {
        {
            let (key, actor, record, auth) = fixture();
            let parent = FileRecordId::new();
            let binding = |name: &str, observed_at_ms: u64| FileBindingV1 {
                schema_version: fs_protocol::FILE_BINDING_VERSION,
                record_id: record,
                parent: Some(parent),
                name: name.into(),
                kind: fs_protocol::FileNodeKind::File,
                tombstoned: false,
                actor,
                observed_at_ms,
            };
            let newer = RecordEnvelopeV1::signed(
                record,
                actor,
                1,
                RecordOperationV1::SetBinding(binding("new.txt", 10)),
                &key,
            )
            .unwrap();
            let late_stale = RecordEnvelopeV1::signed(
                record,
                actor,
                2,
                RecordOperationV1::SetBinding(binding("old.txt", 5)),
                &key,
            )
            .unwrap();
            let mut replica = RecordReplica::new(record);
            replica.merge(&newer, &auth).unwrap();
            replica.merge(&late_stale, &auth).unwrap();
            assert_eq!(
                replica.snapshot(&actor, &auth).unwrap().binding,
                Some(binding("new.txt", 10))
            );

            let mut wrong = binding("wrong.txt", 11);
            wrong.record_id = FileRecordId::new();
            let wrong = RecordEnvelopeV1::signed(
                record,
                actor,
                3,
                RecordOperationV1::SetBinding(wrong),
                &key,
            )
            .unwrap();
            assert!(matches!(
                replica.merge(&wrong, &auth),
                Err(RecordReplicationError::PayloadRecordMismatch)
            ));
        }
        {
            let (key, actor, record, mut auth) = fixture();
            let sibling = FileRecordId::new();
            auth.readers.insert((sibling, actor));
            auth.writers.insert((sibling, actor));
            let sibling_op = envelope(&key, actor, sibling, 1);
            let mut replica = RecordReplica::new(record);
            assert!(matches!(
                replica.merge(&sibling_op, &auth),
                Err(RecordReplicationError::WrongRecord)
            ));
            auth.readers.remove(&(record, actor));
            assert!(matches!(
                replica.snapshot(&actor, &auth),
                Err(RecordReplicationError::ReadDenied)
            ));
        }
        {
            let (key, actor, record, mut auth) = fixture();
            auth.writers.clear();
            let op = envelope(&key, actor, record, 1);
            let mut replica = RecordReplica::new(record);
            assert!(matches!(
                replica.merge(&op, &auth),
                Err(RecordReplicationError::WriteDenied)
            ));
            assert!(replica.metadata.is_none());
        }
        {
            let (key, actor, record, auth) = fixture();
            let first = envelope(&key, actor, record, 1);
            let second = envelope(&key, actor, record, 2);
            let mut replica = RecordReplica::new(record);
            replica.merge(&first, &auth).unwrap();
            replica.merge(&second, &auth).unwrap();
            assert!(matches!(
                replica.merge(&first, &auth),
                Err(RecordReplicationError::Replay { sequence: 1 })
            ));
        }
        {
            let (key, actor, record, auth) = fixture();
            let version = FileVersionV1 {
                id: FileVersionId::new(),
                record_id: record,
                parents: vec![],
                actor,
                timestamp_ms: 8,
                content_hash: Some(ContentHash([8; 32])),
                metadata_hash: ContentHash([9; 32]),
                provenance: FileProvenance::Create,
            };
            let op = RecordEnvelopeV1::signed(
                record,
                actor,
                1,
                RecordOperationV1::AppendVersion(version.clone()),
                &key,
            )
            .unwrap();
            let mut replica = RecordReplica::new(record);
            replica.merge(&op, &auth).unwrap();
            assert_eq!(
                replica.snapshot(&actor, &auth).unwrap().versions,
                vec![version]
            );
        }
    }

    /// The device that signed a version records which agent caused it, at
    /// V5 and only there; it can never attribute another actor's version,
    /// name no agent, or change an attribution once made. Every other
    /// operation keeps signing at V4.
    #[test]
    fn a_device_attributes_only_its_own_versions_to_an_agent() {
        let (key, actor, record, mut auth) = fixture();
        let (other_key, other_device) = generate_keypair();
        let other = FileActor {
            person: actor.person,
            device: other_device,
        };
        auth.certified.insert(other);
        auth.writers.insert((record, other));
        let version = |actor: FileActor, parents: Vec<FileVersionId>| FileVersionV1 {
            id: FileVersionId::new(),
            record_id: record,
            parents,
            actor,
            timestamp_ms: 1,
            content_hash: None,
            metadata_hash: ContentHash([1; 32]),
            provenance: FileProvenance::Edit,
        };
        let own = version(actor, vec![]);
        let theirs = version(other, vec![own.id]);
        let scribe = FileAttributionV1 {
            actor: "agent:scribe".into(),
            label: "Scribe".into(),
            color: [10, 20, 30],
        };
        let attribute = |version_id, attribution: &FileAttributionV1| {
            RecordOperationV1::AttributeVersion(FileVersionAttributionV1 {
                version_id,
                attribution: attribution.clone(),
            })
        };
        // Each device signs its next operation with its own key.
        let signed = |signer: FileActor, sequence, operation| {
            let signing_key = if signer == actor { &key } else { &other_key };
            RecordEnvelopeV1::signed(record, signer, sequence, operation, signing_key).unwrap()
        };
        let mut replica = RecordReplica::new(record);
        let append = signed(actor, 1, RecordOperationV1::AppendVersion(own.clone()));
        assert_eq!(
            append.protocol_version, RECORD_ENVELOPE_VERSION,
            "ordinary operations keep their version"
        );
        replica.merge(&append, &auth).unwrap();
        let theirs_append = signed(other, 1, RecordOperationV1::AppendVersion(theirs.clone()));
        replica.merge(&theirs_append, &auth).unwrap();

        let attributed = signed(actor, 2, attribute(own.id, &scribe));
        assert_eq!(attributed.protocol_version, RECORD_ENVELOPE_V5);
        let decoded =
            RecordEnvelopeV1::decode_and_verify(&attributed.canonical_bytes().unwrap()).unwrap();
        replica.merge(&decoded, &auth).unwrap();
        let snapshot = replica.snapshot(&actor, &auth).unwrap();
        assert_eq!(
            snapshot.attributions,
            vec![FileVersionAttributionV1 {
                version_id: own.id,
                attribution: scribe.clone(),
            }]
        );

        // Another device of the same person cannot say who caused this
        // device's version, nor this device say who caused that one's; a
        // version nobody holds, a second agent and a person are refused.
        let courier = FileAttributionV1 {
            actor: "agent:courier".into(),
            ..scribe.clone()
        };
        let person = FileAttributionV1 {
            actor: "ada".into(),
            ..scribe.clone()
        };
        let mut refused = |signer: FileActor, version_id, attribution| {
            let sequence = if signer == actor { 3 } else { 2 };
            let envelope = signed(signer, sequence, attribute(version_id, attribution));
            replica.merge(&envelope, &auth).unwrap_err()
        };
        assert!(matches!(
            refused(other, own.id, &scribe),
            RecordReplicationError::ForeignAttribution
        ));
        assert!(matches!(
            refused(actor, theirs.id, &scribe),
            RecordReplicationError::ForeignAttribution
        ));
        assert!(matches!(
            refused(actor, FileVersionId::new(), &scribe),
            RecordReplicationError::UnknownAttributedVersion
        ));
        assert!(matches!(
            refused(actor, own.id, &courier),
            RecordReplicationError::AttributionCollision(_)
        ));
        assert!(matches!(
            refused(actor, own.id, &person),
            RecordReplicationError::InvalidPayload(FileRecordError::InvalidAttribution)
        ));
    }

    /// The envelope version gates an attribution both ways, and the
    /// signature covers it: relabelling an envelope breaks it.
    #[test]
    fn attributions_travel_alone_at_v5_under_the_signature() {
        let (key, actor, record, _) = fixture();
        let attributed = RecordEnvelopeV1::signed(
            record,
            actor,
            2,
            RecordOperationV1::AttributeVersion(FileVersionAttributionV1 {
                version_id: FileVersionId::new(),
                attribution: FileAttributionV1 {
                    actor: "agent:scribe".into(),
                    label: "Scribe".into(),
                    color: [10, 20, 30],
                },
            }),
            &key,
        )
        .unwrap();
        assert_eq!(attributed.protocol_version, RECORD_ENVELOPE_V5);
        // The version gates the operation both ways, and the signature
        // covers both: relabelling an envelope breaks it.
        let mut at_v4 = attributed.clone();
        at_v4.protocol_version = RECORD_ENVELOPE_VERSION;
        assert!(matches!(
            at_v4.verify(),
            Err(RecordReplicationError::OperationRequiresV5)
        ));
        let mut metadata_at_v5 = envelope(&key, actor, record, 3);
        metadata_at_v5.protocol_version = RECORD_ENVELOPE_V5;
        assert!(matches!(
            metadata_at_v5.verify(),
            Err(RecordReplicationError::OperationRequiresV5)
        ));
        let mut relabelled = attributed;
        if let RecordOperationV1::AttributeVersion(attributed) = &mut relabelled.operation {
            attributed.attribution.label = "Someone else".into();
        }
        assert!(matches!(
            relabelled.verify(),
            Err(RecordReplicationError::Signature(_))
        ));
    }
}
