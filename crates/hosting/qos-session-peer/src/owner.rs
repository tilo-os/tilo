//! Serialized native owner. Every returned mutation receipt follows the durable barrier.
use crate::{
    action_timing::Timing,
    identity::{self, Identity},
    store::SessionStore,
};
use anyhow::{Context, Result, bail};
use ed25519_dalek::{Signature, Signer, Verifier};
use qos_peer_state::transport::{Retention, retention};
use qos_peer_state::{DocumentRow, NeighbourMsg, StatePeer, TransportOrigin};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tilo_user_identity::{DeviceCert, RevocationList, WorkspaceDirectory};
use web_time::SystemTime;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOperation {
    pub invocation_id: String,
    pub encoded_request: Vec<u8>,
    /// Retained for reconciliation, never automatically executed by this owner.
    pub outcome_unknown: bool,
}
/// Only the native verifier can mint the exceptional security-only turn.
enum CommitMode {
    Mutation,
    DeliveryActivation,
    /// Snapshot + suffix retention: the certified checkpoint is the first
    /// entry of a new store incarnation (see `retention.rs`).
    DeliveryRotation,
    PolicyFenced(OwnerPolicyFence),
    VerifiedSecurityMetadataOnly {
        stored_peer: [u8; 32],
        proof: qos_peer_state::VerifiedSecurityMetadataOnly,
    },
}
mod checkpoint;
pub(crate) mod committed_echo;
mod creation;
#[path = "delivery.rs"]
pub(crate) mod delivery;
mod exact_startup_replay;
pub(crate) mod policy_fence;
pub(crate) use policy_fence::{OwnerPolicyFence, OwnerPolicyRefused};
mod media;
mod native_credit;
/// Test-only thread-local allocation meter (peak/net requested bytes).
#[cfg(test)]
pub(crate) use native_credit::{Stats as AllocationStats, allocations_only};
pub(crate) mod resources;
pub(crate) mod retention;
mod schema;
pub mod sender_administration;
mod transport;
use schema::*;
pub use schema::{OutgoingPublications, PublicationView};
const MAX_PUBLICATION_TAIL_ENTRIES: usize = 64;
const MAX_PUBLICATION_TAIL_BYTES: usize = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct SignedAggregate {
    payload: Vec<u8>,
    signature: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub room: String,
    pub authority: [u8; 32],
    pub records: Vec<DocumentRow>,
    pub replay_floors: Vec<(String, String, [u8; 32], u64)>,
    pub revoked: Vec<[u8; 32]>,
    pub generation: u64,
    pub outgoing: usize,
    pub pending_operations: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitReceipt {
    pub generation: u64,
    pub aggregate_hash: String,
    pub records: Vec<DocumentRow>,
    pub replay_floors: Vec<(String, String, [u8; 32], u64)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PublicationRevision {
    baseline_generation: u64,
    baseline_len: usize,
    outgoing_len: usize,
    outgoing_bytes: usize,
}
#[derive(Clone, Debug)]
pub(crate) struct LivePublicationTransition {
    room: String,
    before: PublicationRevision,
    before_generation: u64,
    after: PublicationRevision,
    generation: u64,
    messages: Arc<[(Vec<u8>, Retention)]>,
    _serialized_credit: Option<Arc<resources::RetainedSerializedBytes>>,
    /// Pays for `messages` (and room) for as long as any clone escapes.
    _backing: Option<Arc<resources::AccountedCredit>>,
}
impl LivePublicationTransition {
    pub(crate) fn before(&self) -> PublicationRevision {
        self.before
    }
    pub(crate) fn after(&self) -> PublicationRevision {
        self.after
    }
    pub(crate) fn before_generation(&self) -> u64 {
        self.before_generation
    }
    pub(crate) fn room(&self) -> &str {
        &self.room
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn messages(&self) -> &[(Vec<u8>, Retention)] {
        &self.messages
    }
}
struct PendingLivePublicationTransition {
    before: PublicationRevision,
    before_generation: u64,
    messages: Vec<(Vec<u8>, Retention)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublicationAdvance {
    None,
    SelectiveTail { entries: usize, bytes: usize },
    BaselineReplacement { entries: usize, bytes: usize },
}
pub struct SessionOwner {
    store: SessionStore,
    pub(crate) identity: Identity,
    aggregate: Aggregate,
    peer: StatePeer,
    workspace_id: u64,
    live_publication_transition: Option<Arc<LivePublicationTransition>>,
    resources: Option<resources::OwnerResources>,
}

#[cfg(test)]
std::thread_local! {
    static DOCUMENT_CAUSAL_PROOFS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TEST_PUBLICATION_TIME: std::cell::RefCell<Option<SystemTime>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn reset_document_causal_proofs() {
    DOCUMENT_CAUSAL_PROOFS.set(0);
}

#[cfg(test)]
fn document_causal_proofs() -> usize {
    DOCUMENT_CAUSAL_PROOFS.get()
}
fn publication_time() -> SystemTime {
    #[cfg(test)]
    {
        return TEST_PUBLICATION_TIME
            .with(|time| time.borrow().clone().unwrap_or_else(SystemTime::now));
    }
    #[cfg(not(test))]
    SystemTime::now()
}
#[cfg(test)]
pub(crate) fn with_test_publication_time<T>(time: SystemTime, operation: impl FnOnce() -> T) -> T {
    TEST_PUBLICATION_TIME.with(|current| {
        let prior = current.replace(Some(time));
        let result = operation();
        current.replace(prior);
        result
    })
}

#[cfg(test)]
pub(crate) fn mutate_owned_record_for_test(
    owner: &mut SessionOwner,
    app: &str,
    record: &str,
    mutation: impl FnOnce(&mut ui::automerge::AutoCommit),
) -> Result<()> {
    let mut candidate = owner.peer().clone();
    let mut document = ui::automerge::AutoCommit::load(
        &candidate
            .document(app, record)
            .ok_or_else(|| anyhow::anyhow!("missing test record {app}/{record}"))?,
    )?;
    mutation(&mut document);
    document.commit();
    candidate
        .publish_document(
            app,
            record,
            document.save(),
            &owner.identity.native,
            SystemTime::now(),
        )
        .map_err(anyhow::Error::msg)?;
    owner.commit_mutation_candidate(candidate, owner.directory().to_vec())?;
    Ok(())
}
fn encode(aggregate: &Aggregate, identity: &Identity) -> Result<Vec<u8>> {
    encode_with_timing(aggregate, identity, None)
}
fn encode_with_timing(
    aggregate: &Aggregate,
    identity: &Identity,
    timing: Option<&Timing>,
) -> Result<Vec<u8>> {
    encode_with_cap(
        aggregate,
        identity,
        timing,
        crate::store::MAX_AGGREGATE_BYTES,
    )
}
fn encode_with_cap(
    aggregate: &Aggregate,
    identity: &Identity,
    timing: Option<&Timing>,
    cap: usize,
) -> Result<Vec<u8>> {
    if let Some(timing) = timing {
        timing.mark("aggregate_encode_start");
    }
    let payload = aggregate.encode_payload_bounded(cap)?;
    if let Some(timing) = timing {
        timing.mark_count("aggregate_payload_bytes", payload.len());
        timing.mark("aggregate_encode_end");
        timing.mark("signature_digest_start");
    }
    let digest = blake3::Hasher::new_derive_key("qOS native session aggregate v1")
        .update(&payload)
        .finalize();
    if let Some(timing) = timing {
        timing.mark("signature_digest_end");
        timing.mark("aggregate_sign_start");
    }
    let signature = identity.native.sign(digest.as_bytes()).to_bytes().to_vec();
    if let Some(timing) = timing {
        timing.mark("aggregate_sign_end");
        timing.mark("signed_envelope_encode_start");
    }
    let bytes = schema::encode_bounded(
        &SignedAggregate { payload, signature },
        crate::store::MAX_AGGREGATE_BYTES,
    )?;
    if let Some(timing) = timing {
        timing.mark_count("signed_aggregate_bytes", bytes.len());
        timing.mark("signed_envelope_encode_end");
    }
    Ok(bytes)
}
fn decode(bytes: &[u8]) -> Result<(Aggregate, SignedAggregate)> {
    if bytes.len() > crate::store::MAX_AGGREGATE_BYTES {
        bail!("aggregate exceeds the local storage size limit");
    }
    let signed: SignedAggregate = postcard::from_bytes(bytes)
        .context("corrupt session aggregate; preserve it for recovery")?;
    let aggregate = Aggregate::decode_payload(&signed.payload)?;
    if aggregate.sender.is_some() || matches!(aggregate.publication, PublicationStorage::V5 { .. })
    {
        schema::canonical(&signed, bytes)?;
    }
    Ok((aggregate, signed))
}
fn directory_workspace_id(directory: &WorkspaceDirectory, room: &str) -> Result<u64> {
    let rows = directory.as_map();
    let matches = tilo_relay::room_workspace_ids(room)?
        .into_iter()
        .filter(|id| rows.contains_key(id))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [id] => Ok(*id),
        [] => bail!("authenticated directory has no canonical room binding"),
        _ => bail!("authenticated directory has ambiguous room namespace bindings"),
    }
}

fn restore(
    aggregate: &Aggregate,
    signed: &SignedAggregate,
    identity: &Identity,
) -> Result<(StatePeer, u64)> {
    if aggregate.authority != identity.native_cert {
        bail!("aggregate does not belong to this native identity");
    }
    identity.native.verifying_key().verify(
        blake3::Hasher::new_derive_key("qOS native session aggregate v1")
            .update(&signed.payload)
            .finalize()
            .as_bytes(),
        &Signature::from_slice(&signed.signature)?,
    )?;
    let directory = WorkspaceDirectory::load(
        identity.root.verifying_key().to_bytes(),
        identity.native_cert.device_public_key,
        &aggregate.directory,
    )?;
    let workspace_id = directory_workspace_id(&directory, &aggregate.room)?;
    let peer = StatePeer::restore(
        &aggregate.peer,
        &aggregate.room,
        aggregate.authority.clone(),
        vec![aggregate.authority.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .map_err(anyhow::Error::msg)?;
    peer.permissions(identity.native_cert.device_public_key, SystemTime::now())
        .map_err(anyhow::Error::msg)?;
    if !peer.has_governance()
        || (peer.rows().is_empty() && !crate::seed::recordless_desktop_is_initialized(&peer))
    {
        bail!("restored canonical session is incomplete");
    }
    aggregate.validate()?;
    delivery::validate_replay(aggregate, workspace_id, &peer)?;
    Ok((peer, workspace_id))
}
impl SessionOwner {
    /// Explicitly provisions a NEW room and authority. An existing directory is never reseeded.
    pub fn initialize(root: &Path) -> Result<Self> {
        Self::initialize_new(root, None)
    }

    /// Create a named, empty workspace with independent local authority.
    /// Validation happens before creating any state or identity directories.
    pub fn initialize_workspace(root: &Path, name: &str) -> Result<Self> {
        if name.chars().any(char::is_control) {
            bail!("workspace name must not contain control characters");
        }
        let name = name.trim();
        if !(1..=80).contains(&name.chars().count()) {
            bail!("workspace name must contain 1–80 characters");
        }
        Self::initialize_new(root, Some(name))
    }

    fn initialize_new(root: &Path, workspace_name: Option<&str>) -> Result<Self> {
        let mut store = SessionStore::create(root)?;
        let root = std::fs::canonicalize(root)?;
        let identity_root = root.join("identity");
        let host_identity_root = root.join("host-identity");
        let identity = identity::initialize(&identity_root, &host_identity_root)?;
        let room = uuid::Uuid::new_v4().to_string();
        let roster = vec![identity.native_cert.clone()];
        let now = SystemTime::now();
        let mut peer = StatePeer::new(
            &room,
            identity.native_cert.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )
        .map_err(anyhow::Error::msg)?;
        let seed = match workspace_name {
            Some(name) => crate::seed::empty_workspace(&room, &identity.native_cert, &roster, name),
            None => crate::seed::seed(&room, &identity.native_cert, &roster),
        }
        .map_err(anyhow::Error::msg)?;
        peer.install_seed(seed, &identity.native, now)
            .map_err(anyhow::Error::msg)?;
        crate::seed::bootstrap_desktop(&mut peer, &identity.native).map_err(anyhow::Error::msg)?;
        media::prepare_media_governance(&mut peer, &identity.native, now)?;
        let outgoing = peer
            .durable_bootstrap_messages(&identity.native, now, 1)
            .map_err(anyhow::Error::msg)?
            .iter()
            .map(postcard::to_stdvec)
            .collect::<Result<Vec<_>, _>>()?;
        let workspace = tilo_relay::room_workspace_id(&room)?;
        let mut directory = WorkspaceDirectory::new(
            identity.root.verifying_key().to_bytes(),
            identity.native_cert.device_public_key,
        );
        directory.add_workspace(workspace, workspace_name.unwrap_or("Shared qOS workspace"))?;
        directory.set_member_devices(workspace, &[identity.native_cert.device_public_key])?;
        directory.set_host_devices(workspace, &[identity.host_cert.device_public_key])?;
        let aggregate: Aggregate = AggregateV4 {
            version: 4,
            publication: PublicationState::checkpoint(outgoing.len(), 1),
            identity_root,
            host_identity_root,
            room,
            authority: identity.native_cert.clone(),
            directory: directory.save(),
            peer: peer.durable().map_err(anyhow::Error::msg)?,
            outgoing,
            pending: vec![],
            generation: 1,
            issuances: vec![],
            collaboration: crate::collaboration::CollaborationStateV4::default(),
        }
        .into();
        store.commit(&encode(&aggregate, &identity)?)?;
        Ok(Self {
            store,
            identity,
            aggregate,
            peer,
            workspace_id: workspace,
            live_publication_transition: None,
            resources: None,
        })
    }
    pub fn open(root: &Path) -> Result<Self> {
        let mut store = SessionStore::open(root)?;
        let bytes = store.read_for_owner()?;
        let (mut aggregate, signed) = decode(&bytes)?;
        if aggregate.sender.is_some() {
            bail!(
                "selected sender storage requires explicit administration; runtime installation is unavailable"
            );
        }
        let identity = identity::load(&aggregate.identity_root, &aggregate.host_identity_root)?;
        let (peer, workspace_id) = restore(&aggregate, &signed, &identity)?;
        // Consumer ACK progress recorded beside the signed aggregate. The
        // commit below folds it (and truncates the side log).
        let side_records = store.read_side_log_for_owner()?;
        let advanced =
            delivery::ack_log::apply_recovered(&mut aggregate, &identity, &side_records)?;
        if !side_records.is_empty() {
            tracing::info!(
                records = side_records.len(),
                advanced,
                "session ACK side log applied"
            );
        }
        // Re-establish the barrier even after an interrupted rename, before any publication.
        store.commit(&encode(&aggregate, &identity)?)?;
        let mut owner = Self {
            store,
            identity,
            aggregate,
            peer,
            workspace_id,
            live_publication_transition: None,
            resources: None,
        };
        if matches!(owner.aggregate.publication, PublicationStorage::V5 { .. }) {
            return Ok(owner);
        }
        let mut candidate = owner.peer.clone();
        let desktop_changed =
            crate::seed::bootstrap_desktop(&mut candidate, &owner.identity.native)
                .map_err(anyhow::Error::msg)?;
        // Older complete aggregates may have no publication queue. Rebuild that
        // recovery basis once at open; ordinary reconnects reuse exact evidence.
        if (desktop_changed || owner.outgoing().is_empty())
            && candidate.incomplete_reason().is_none()
        {
            let messages = owner.prepare_certified_bootstrap(
                &mut candidate,
                owner
                    .aggregate
                    .generation
                    .checked_add(1)
                    .context("session generation exhausted")?,
            )?;
            let mut next = owner.aggregate.clone();
            owner.replace_publication(&mut next, &messages)?;
            owner.commit(next, candidate)?;
        }
        Ok(owner)
    }
    /// Storage uncertainty is terminal for this owner instance; missing peer
    /// prerequisites are a separate, potentially recoverable authority denial.
    pub(crate) fn durability_healthy(&self) -> bool {
        !self.store.is_poisoned()
    }
    /// Reader-only V5 imports require native exact-admission selection before writer use.
    pub fn schema_read_only_reason(&self) -> Option<&'static str> {
        self.aggregate.is_read_only().then_some(V5_READ_ONLY_REASON)
    }
    pub fn permits_publication(&self) -> bool {
        !self.aggregate.is_read_only()
            && !self.store.is_poisoned()
            && self.peer.incomplete_reason().is_none()
    }
    pub fn root(&self) -> &Path {
        self.store.root()
    }
    pub fn peer(&self) -> &StatePeer {
        &self.peer
    }
    /// Reconstruct only the peer owned by the authenticated durable aggregate.
    /// Live prerequisite queues in `self.peer` are not committed authority.
    pub(crate) fn committed_peer(&self) -> Result<StatePeer> {
        StatePeer::restore(
            &self.aggregate.peer,
            &self.aggregate.room,
            self.aggregate.authority.clone(),
            vec![self.aggregate.authority.clone()],
            RevocationList::new(),
            SystemTime::now(),
        )
        .map_err(anyhow::Error::msg)
    }
    /// Literal authenticated storage identity, independent of restored map order.
    pub(crate) fn committed_peer_identity(&self) -> [u8; 32] {
        *blake3::hash(&self.aggregate.peer).as_bytes()
    }
    /// Advances only after the owner's aggregate durability barrier succeeds.
    pub(crate) fn committed_generation(&self) -> u64 {
        self.aggregate.generation
    }
    /// Exact authenticated directory binding, retained across restart and import.
    pub fn workspace_id(&self) -> u64 {
        self.workspace_id
    }
    pub fn directory(&self) -> &[u8] {
        &self.aggregate.directory
    }
    pub fn host_certificate(&self) -> &DeviceCert {
        &self.identity.host_cert
    }
    pub(crate) fn sign_workspace_host_hint(
        &self,
        hint: &tilo_relay::workspace_join::IrohWorkspaceHostHintV1,
    ) -> Result<qos_peer_state::join::TransportHint> {
        hint.transport_hint(&self.identity.host)
            .map_err(anyhow::Error::msg)
    }
    pub fn status(&self) -> Status {
        Status {
            room: self.peer.room().into(),
            authority: self.identity.native_cert.device_public_key,
            records: self.peer.rows(),
            replay_floors: self.peer.replay_floors(),
            revoked: self
                .peer
                .revocations()
                .iter()
                .map(|r| r.revoked_device_public_key)
                .collect(),
            generation: self.aggregate.generation,
            outgoing: self.outgoing().len(),
            pending_operations: self.aggregate.pending.len(),
        }
    }
    pub fn enrollment_count(&self) -> usize {
        self.aggregate
            .issuances
            .iter()
            .map(|entry| entry.device_public_key)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }
    pub(crate) fn issuances(&self) -> &[crate::enrollment::Issuance] {
        &self.aggregate.issuances
    }
    pub(crate) fn collaboration_state(&self) -> &crate::collaboration::CollaborationState {
        &self.aggregate.collaboration
    }
    pub(crate) fn sign_invitation(
        &self,
        grant: qos_peer_state::join::InviteGrant,
    ) -> Result<Vec<u8>, String> {
        self.require_owner_resource_scope()
            .map_err(|e| e.to_string())?;
        if grant.workspace_id != self.workspace_id
            || grant.room != self.peer.room()
            || grant.authority != *self.peer.authority()
            || grant.issuer != self.identity.host_cert
            || grant
                .delegation
                .as_ref()
                .is_none_or(|delegation| delegation.delegate != self.identity.host_cert)
        {
            return Err("native host lacks its room-scoped invitation delegation".into());
        }
        crate::collaboration::sign(grant, &self.identity.host)
    }
    pub(crate) fn prepare_workspace_host_delegation(
        &self,
        now: SystemTime,
    ) -> Result<(StatePeer, qos_peer_state::join::WorkspaceDelegation)> {
        self.require_owner_resource_scope()?;
        use qos_peer_state::join::{WorkspaceDelegationPermissions, sign_workspace_delegation};

        let mut candidate = self.peer.clone();
        let mut material = Vec::with_capacity(self.peer.room().len() + 8 + 32);
        material.extend_from_slice(self.peer.room().as_bytes());
        material.extend_from_slice(&self.workspace_id.to_le_bytes());
        material.extend_from_slice(&self.identity.host_cert.device_public_key);
        let digest = blake3::hash(&material);
        let mut nonce = [0; 16];
        nonce.copy_from_slice(&digest.as_bytes()[..16]);
        let delegation = sign_workspace_delegation(
            self.peer.room(),
            self.workspace_id,
            self.peer.authority(),
            self.identity.host_cert.clone(),
            WorkspaceDelegationPermissions {
                issue_invitations: true,
                admit_invited_members: true,
            },
            nonce,
            None,
            &self.identity.native,
        )
        .map_err(anyhow::Error::msg)?;
        candidate
            .admit_certificate_with_workspace_delegation(
                self.identity.host_cert.clone(),
                delegation.clone(),
                &self.identity.native,
                now,
            )
            .map_err(anyhow::Error::msg)?;
        Ok((candidate, delegation))
    }
    pub(crate) fn commit_collaboration_state(
        &mut self,
        state: crate::collaboration::CollaborationState,
    ) -> Result<CommitReceipt> {
        self.require_owner_resource_scope()?;
        let mut next = self.aggregate.clone();
        next.collaboration = state;
        self.commit(next, self.peer.clone())
    }
    pub(crate) fn prepare_invited_member(
        &self,
        redemption: &qos_peer_state::join::VerifiedRedemption,
        now: SystemTime,
    ) -> Result<StatePeer> {
        self.require_owner_resource_scope()?;
        let mut candidate = self.peer.clone();
        let issuer = &redemption.join().grant().issuer;
        let signer = if issuer == &self.identity.host_cert {
            &self.identity.host
        } else if issuer == self.peer.authority() {
            &self.identity.native
        } else {
            bail!("invitation issuer is not controlled by this native host");
        };
        candidate
            .admit_invited_member(redemption, signer, now)
            .map_err(anyhow::Error::msg)?;
        Ok(candidate)
    }
    pub(crate) fn prepare_member_role(
        &self,
        user_public_key: [u8; 32],
        role: qos_peer_state::join::MemberRole,
        now: SystemTime,
    ) -> Result<StatePeer> {
        self.require_owner_resource_scope()?;
        let mut candidate = self.peer.clone();
        candidate
            .set_member_role(user_public_key, role, &self.identity.native, now)
            .map_err(anyhow::Error::msg)?;
        Ok(candidate)
    }
    pub(crate) fn prepare_member_removal(
        &self,
        user_public_key: [u8; 32],
        now: SystemTime,
    ) -> Result<StatePeer> {
        self.require_owner_resource_scope()?;
        let mut candidate = self.peer.clone();
        candidate
            .remove_member(user_public_key, &self.identity.native, now)
            .map_err(anyhow::Error::msg)?;
        Ok(candidate)
    }
    pub(crate) fn commit_collaboration_candidate(
        &mut self,
        mut candidate: StatePeer,
        state: crate::collaboration::CollaborationState,
    ) -> Result<CommitReceipt> {
        self.require_owner_resource_scope()?;
        if candidate.room() != self.peer.room() || candidate.authority() != self.peer.authority() {
            bail!("collaboration cannot replace native namespace");
        }
        self.validate_admission(&candidate, self.directory())?;
        let mut next = self.aggregate.clone();
        let transition = self.append_mutation_publication(&mut next, &mut candidate)?;
        next.collaboration = state;
        let result = self.commit(next, candidate);
        if result.is_ok() {
            self.install_live_publication_transition(transition);
        }
        result
    }
    pub(crate) fn commit_join_redemption(
        &mut self,
        mut candidate: StatePeer,
        mut state: crate::collaboration::CollaborationState,
        remote_endpoint: [u8; 32],
        request_id: [u8; 32],
        proof_digest: [u8; 32],
        invite_id: [u8; 32],
        device_cert: DeviceCert,
        role: qos_peer_state::join::MemberRole,
    ) -> Result<qos_peer_state::join::JoinBootstrap> {
        self.require_owner_resource_scope()?;
        if candidate.room() != self.peer.room() || candidate.authority() != self.peer.authority() {
            bail!("collaboration cannot replace native namespace");
        }
        self.validate_admission(&candidate, self.directory())?;
        let mut next = self.aggregate.clone();
        let transition = self.append_mutation_publication(&mut next, &mut candidate)?;
        let generation = self
            .aggregate
            .generation
            .checked_add(1)
            .context("session generation exhausted")?;
        let bootstrap = qos_peer_state::join::JoinBootstrap {
            version: 1,
            invite_id,
            device_cert,
            role,
            room: candidate.room().to_owned(),
            workspace_id: self.workspace_id,
            authority: candidate.authority().clone(),
            certified_roster: candidate.certificates(),
            revocations: candidate.revocations(),
            records: candidate.rows(),
            governance_messages: next.outgoing().iter().map(|p| p.bytes.to_vec()).collect(),
            generation,
        };
        state
            .join_completions
            .push(crate::collaboration::JoinCompletionRecord {
                remote_endpoint,
                request_id,
                proof_digest,
                bootstrap: bootstrap.clone(),
            });
        next.collaboration = state;
        let result = self.commit(next, candidate);
        if result.is_ok() {
            self.install_live_publication_transition(transition);
        }
        result?;
        Ok(bootstrap)
    }
    pub(crate) fn current_join_bootstrap(
        &self,
        invite_id: [u8; 32],
        device_cert: DeviceCert,
        role: qos_peer_state::join::MemberRole,
    ) -> qos_peer_state::join::JoinBootstrap {
        qos_peer_state::join::JoinBootstrap {
            version: 1,
            invite_id,
            device_cert,
            role,
            room: self.peer.room().to_owned(),
            workspace_id: self.workspace_id,
            authority: self.peer.authority().clone(),
            certified_roster: self.peer.certificates(),
            revocations: self.peer.revocations(),
            records: self.peer.rows(),
            governance_messages: self.outgoing().iter().map(|p| p.bytes.to_vec()).collect(),
            generation: self.aggregate.generation,
        }
    }
    pub fn pending(&self) -> &[PendingOperation] {
        &self.aggregate.pending
    }
    /// Complete committed recovery baseline plus tail, not a disposable live queue.
    /// Consumers must use the paired retention kinds and a transient send cursor.
    pub fn outgoing(&self) -> OutgoingPublications<'_> {
        self.aggregate.outgoing()
    }
    /// (replacement generation, complete-prefix length). Sending is not a durable ACK.
    pub fn publication_baseline(&self) -> (u64, usize) {
        self.aggregate.baseline()
    }
    pub(crate) fn publication_revision(&self) -> PublicationRevision {
        let (baseline_generation, baseline_len) = self.publication_baseline();
        PublicationRevision {
            baseline_generation,
            baseline_len,
            outgoing_len: self.outgoing().len(),
            outgoing_bytes: self.outgoing().iter().map(|p| p.bytes.len()).sum(),
        }
    }
    pub(crate) fn live_publication_transition(&self) -> Option<&Arc<LivePublicationTransition>> {
        self.live_publication_transition.as_ref()
    }
    pub(crate) fn publication_advance(&self, before: PublicationRevision) -> PublicationAdvance {
        let after = self.publication_revision();
        if after == before {
            return PublicationAdvance::None;
        }
        if after.baseline_generation == before.baseline_generation
            && after.baseline_len == before.baseline_len
            && after.outgoing_len > before.outgoing_len
            && after.outgoing_bytes >= before.outgoing_bytes
        {
            PublicationAdvance::SelectiveTail {
                entries: after.outgoing_len - before.outgoing_len,
                bytes: after.outgoing_bytes - before.outgoing_bytes,
            }
        } else {
            PublicationAdvance::BaselineReplacement {
                entries: after.outgoing_len,
                bytes: after.outgoing_bytes,
            }
        }
    }
    fn encode_publication_messages(&self, messages: &[NeighbourMsg]) -> Result<Vec<Vec<u8>>> {
        let cap = self.serialized_commit_cap()?;
        let mut total = 0usize;
        for message in messages {
            total = total
                .checked_add(schema::counted(message, cap)?)
                .context("publication bytes overflow")?;
            if total > cap {
                return Err(delivery::error::failure(
                    delivery::error::OwnerDataFailureKind::Capacity(
                        delivery::error::OwnerCapacityKind::Bytes,
                    ),
                    anyhow::anyhow!("publication exceeds reserved serialized cap"),
                ));
            }
        }
        messages
            .iter()
            .map(|message| schema::encode_bounded(message, cap))
            .collect()
    }
    fn replace_publication(&self, next: &mut Aggregate, messages: &[NeighbourMsg]) -> Result<()> {
        let encoded = self.encode_publication_messages(messages)?;
        if let PublicationStorage::V5 { pool, roots, .. } = &mut next.publication {
            let baseline = encoded
                .iter()
                .map(|bytes| {
                    Ok(tilo_relay::delivery_v3::commit::PublicationRef {
                        payload: pool.intern(bytes)?,
                        kind: Retention::Checkpoint,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            roots.baseline = baseline;
            roots.tail.clear();
            roots.baseline_generation = self
                .aggregate
                .generation
                .checked_add(1)
                .context("session generation exhausted")?;
            return Ok(());
        }
        let (outgoing, publication) = next.legacy_mut()?;
        *outgoing = encoded;
        *publication = PublicationState::checkpoint(
            outgoing.len(),
            self.aggregate
                .generation
                .checked_add(1)
                .context("session generation exhausted")?,
        );
        Ok(())
    }
    fn prepare_certified_bootstrap(
        &self,
        candidate: &mut StatePeer,
        generation: u64,
    ) -> Result<Vec<NeighbourMsg>> {
        self.prepare_certified_bootstrap_at(candidate, generation, publication_time())
    }
    fn prepare_certified_bootstrap_at(
        &self,
        candidate: &mut StatePeer,
        generation: u64,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>> {
        media::prepare_media_governance(candidate, &self.identity.native, now)?;
        candidate
            .durable_bootstrap_messages(&self.identity.native, now, generation)
            .map_err(anyhow::Error::msg)
    }
    /// A failed promotion may persist only a native-proven monotone metadata delta.
    fn prepare_received_media_governance(
        &self,
        candidate: &mut StatePeer,
    ) -> Result<Option<qos_peer_state::VerifiedSecurityMetadataOnly>> {
        match self.prepare_media_governance_for_publication(candidate) {
            Ok(()) => Ok(None),
            Err(error) => self
                .peer
                .verify_security_metadata_only(candidate)
                .map(Some)
                .map_err(|_| error),
        }
    }
    fn commit_verified_security_metadata_only(
        &mut self,
        candidate: StatePeer,
        proof: qos_peer_state::VerifiedSecurityMetadataOnly,
    ) -> Result<CommitReceipt> {
        // First prove this receive turn changed only verified security metadata
        // in the live peer. Its pre-existing deferred queue remains volatile.
        proof
            .validate(&self.peer, &candidate)
            .map_err(anyhow::Error::msg)?;
        let stored_peer = self.committed_peer_identity();
        let committed = self.committed_peer()?;
        let (persisted, committed_proof) = committed
            .project_verified_security_metadata(&candidate)
            .map_err(anyhow::Error::msg)?;
        let receipt = self.commit_delivery(
            self.aggregate.clone(),
            persisted,
            None,
            CommitMode::VerifiedSecurityMetadataOnly {
                stored_peer,
                proof: committed_proof,
            },
        )?;
        // Nothing in this retained queue was accepted or acknowledged. Keep its
        // bounded in-process custody, separately from the persisted projection.
        self.peer = candidate;
        Ok(receipt)
    }
    fn append_mutation_publication(
        &self,
        next: &mut Aggregate,
        candidate: &mut StatePeer,
    ) -> Result<Option<PendingLivePublicationTransition>> {
        self.append_mutation_publication_at(next, candidate, publication_time())
    }
    fn append_mutation_publication_at(
        &self,
        next: &mut Aggregate,
        candidate: &mut StatePeer,
        now: SystemTime,
    ) -> Result<Option<PendingLivePublicationTransition>> {
        media::prepare_media_governance(candidate, &self.identity.native, now)?;
        let generation = self
            .aggregate
            .generation
            .checked_add(1)
            .context("session generation exhausted")?;
        // Keep the unprepared candidate for compaction: a second receipt at this
        // same generation must never replace one prepared by the selective path.
        let mut selective = candidate.clone();
        let messages = selective
            .durable_mutation_messages(&self.peer, &self.identity.native, now, generation)
            .map_err(anyhow::Error::msg)?;
        let encoded = self.encode_publication_messages(&messages)?;
        let tail_len = next.outgoing().len() - next.baseline().1;
        let tail_bytes = next
            .outgoing()
            .iter()
            .skip(next.baseline().1)
            .map(|p| p.bytes.len())
            .sum::<usize>();
        if tail_len.saturating_add(encoded.len()) >= MAX_PUBLICATION_TAIL_ENTRIES
            || tail_bytes.saturating_add(encoded.iter().map(Vec::len).sum::<usize>())
                >= MAX_PUBLICATION_TAIL_BYTES
        {
            drop(encoded);
            drop(messages);
            drop(selective);
            let before = self.publication_revision();
            let before_generation = self.aggregate.generation;
            let mut before_room = self
                .peer
                .admitted_room()
                .ok_or_else(|| anyhow::anyhow!("missing prior room governance"))?
                .clone();
            let before_room_heads = before_room.get_heads();
            let checkpoint = self.prepare_certified_bootstrap_at(candidate, generation, now)?;
            let mut room = candidate
                .admitted_room()
                .ok_or_else(|| anyhow::anyhow!("missing compacted room governance"))?
                .clone();
            let changes = room.get_changes(&before_room_heads);
            let cap = self
                .serialized_commit_cap()?
                .min(MAX_PUBLICATION_TAIL_BYTES);
            let raw_changes = changes.iter().try_fold(0usize, |sum, change| {
                sum.checked_add(change.raw_bytes().len())
                    .context("live change bytes overflow")
            })?;
            self.replace_publication(next, &checkpoint)?;
            drop(checkpoint);
            if raw_changes <= cap && next.outgoing().len() <= MAX_PUBLICATION_TAIL_ENTRIES {
                let message = NeighbourMsg::SyncResp {
                    changes: changes
                        .into_iter()
                        .map(|change| change.raw_bytes().to_vec())
                        .collect(),
                    room: candidate.room().to_owned(),
                };
                let delta_len = schema::counted(&message, crate::store::MAX_AGGREGATE_BYTES)?;
                let live_bytes = next
                    .outgoing()
                    .iter()
                    .skip(1)
                    .try_fold(delta_len, |sum, p| {
                        sum.checked_add(p.bytes.len())
                            .context("live publication bytes overflow")
                    })?;
                if live_bytes <= cap {
                    // Check and retain before output encoding/copying. Native
                    // change/save DTOs use the separate qps allocation budget.
                    let backing = live_bytes
                        .checked_add(candidate.room().len())
                        .and_then(|bytes| {
                            bytes.checked_add(next.outgoing().len().checked_mul(size_of::<(
                                Vec<u8>,
                                Retention,
                            )>(
                            ))?)
                        })
                        .context("live publication backing overflow")?;
                    self.retain_live_publication_bytes(backing)?;
                    let mut live = Vec::with_capacity(next.outgoing().len());
                    live.push((schema::encode_bounded(&message, cap)?, Retention::Delta));
                    live.extend(
                        next.outgoing()
                            .iter()
                            .skip(1)
                            .map(|p| (p.bytes.to_vec(), Retention::Checkpoint)),
                    );
                    return Ok(Some(PendingLivePublicationTransition {
                        before,
                        before_generation,
                        messages: live,
                    }));
                }
            }
        } else {
            match &mut next.publication {
                PublicationStorage::Legacy { outgoing, state } => {
                    for message in &messages {
                        let (_, kind) = retention(message, false)
                            .context("unsupported ordinary publication")?;
                        state.kinds.push(kind);
                    }
                    outgoing.extend(encoded);
                }
                PublicationStorage::V5 { pool, roots, .. } => {
                    for (message, bytes) in messages.iter().zip(&encoded) {
                        let (_, kind) = retention(message, false)
                            .context("unsupported ordinary publication")?;
                        roots
                            .tail
                            .push(tilo_relay::delivery_v3::commit::PublicationRef {
                                payload: pool.intern(bytes)?,
                                kind,
                            });
                    }
                }
            }
            *candidate = selective;
        }
        Ok(None)
    }
    fn commit(&mut self, aggregate: Aggregate, candidate: StatePeer) -> Result<CommitReceipt> {
        self.commit_delivery(aggregate, candidate, None, CommitMode::Mutation)
    }
    fn commit_delivery(
        &mut self,
        mut aggregate: Aggregate,
        mut candidate: StatePeer,
        binding: Option<delivery::DeliveryBinding>,
        mode: CommitMode,
    ) -> Result<CommitReceipt> {
        if aggregate.sender != self.aggregate.sender {
            bail!("ordinary commit cannot change administrative sender selection");
        }
        let serialized_cap = self.serialized_commit_cap()?;
        if !matches!(mode, CommitMode::DeliveryActivation) {
            self.aggregate.require_writable()?;
        }
        if self.store.is_poisoned() {
            bail!("reopen owner after uncertain durability");
        }
        aggregate.generation = self
            .aggregate
            .generation
            .checked_add(1)
            .context("session generation exhausted")?;
        match &mode {
            CommitMode::Mutation
            | CommitMode::DeliveryActivation
            | CommitMode::DeliveryRotation
            | CommitMode::PolicyFenced(_) => {
                let policy_at = match &mode {
                    CommitMode::PolicyFenced(fence) => Some(fence.sample()?),
                    _ => None,
                };
                let rotation = matches!(mode, CommitMode::DeliveryRotation);
                self.finalize_delivery(
                    &mut aggregate,
                    &mut candidate,
                    binding,
                    policy_at,
                    rotation,
                )?;
                self.validate_media_governance_before_commit(&candidate)?;
            }
            CommitMode::VerifiedSecurityMetadataOnly { stored_peer, proof } => {
                if binding.is_some() {
                    bail!("suspended security metadata cannot bind a delivery receipt");
                }
                if *stored_peer != self.committed_peer_identity() {
                    bail!("stored security baseline changed before commit");
                }
                let committed = self.committed_peer()?;
                proof
                    .validate(&committed, &candidate)
                    .map_err(anyhow::Error::msg)?;
                if !candidate.admissions_since(&committed)?.is_empty() {
                    bail!("suspended security metadata cannot admit native operations");
                }
                let mut unchanged = aggregate.clone();
                unchanged.generation = self.aggregate.generation;
                if unchanged.encode_payload_bounded(serialized_cap)?
                    != self.aggregate.encode_payload_bounded(serialized_cap)?
                {
                    bail!("suspended security turn changed owner aggregate metadata");
                }
                if let PublicationStorage::V5 { delivery, .. } = &mut aggregate.publication {
                    delivery.resolved_through = aggregate.generation;
                }
            }
        }
        // A failed durability barrier must not leave an old live-only artifact
        // reachable while the owner is suspended. The new artifact is installed
        // only by its caller after this method returns successfully.
        self.live_publication_transition = None;
        let timing = Timing::new("aggregate_persist");
        timing.mark("candidate_durable_start");
        aggregate.peer = candidate.durable().map_err(anyhow::Error::msg)?;
        timing.mark_count("durable_peer_bytes", aggregate.peer.len());
        timing.mark("candidate_durable_end");
        aggregate.generation = self
            .aggregate
            .generation
            .checked_add(1)
            .context("session generation exhausted")?;
        aggregate.require_writable()?;
        aggregate.validate()?;
        delivery::validate_replay(&aggregate, self.workspace_id, &candidate)?;
        self.retain_commit_bytes(&aggregate)?;
        let bytes = encode_with_cap(&aggregate, &self.identity, Some(&timing), serialized_cap)?;
        timing.mark("store_commit_start");
        #[cfg(test)]
        if matches!(&mode, CommitMode::PolicyFenced(_)) {
            policy_fence::before_check();
        }
        // Authorization is sampled after encoding and immediately before the
        // write. A later expiry never reclassifies an already durable effect.
        if let CommitMode::PolicyFenced(fence) = &mode {
            fence.revalidate(self)?;
        }
        delivery::early_forward::fire();
        self.store.commit_timed(&bytes, &timing)?;
        self.publish_retained_bytes(&aggregate);
        timing.mark("store_commit_end");
        timing.mark("receipt_hash_start");
        // The store already hashed exactly these acknowledged bytes.
        let aggregate_hash = blake3::Hash::from_bytes(self.store.acknowledged_digest()?)
            .to_hex()
            .to_string();
        timing.mark("receipt_hash_end");
        let receipt = CommitReceipt {
            generation: aggregate.generation,
            aggregate_hash,
            records: candidate.rows(),
            replay_floors: candidate.replay_floors(),
        };
        self.aggregate = aggregate;
        self.peer = candidate;
        Ok(receipt)
    }
    fn install_live_publication_transition(
        &mut self,
        pending: Option<PendingLivePublicationTransition>,
    ) {
        let Some(pending) = pending else {
            return;
        };
        self.live_publication_transition = Some(Arc::new(LivePublicationTransition {
            room: self.peer.room().to_owned(),
            before: pending.before,
            before_generation: pending.before_generation,
            after: self.publication_revision(),
            generation: self.aggregate.generation,
            messages: pending.messages.into(),
            _serialized_credit: self.retained_serialized_credit(),
            _backing: self.take_live_publication_credit(),
        }));
    }
    /// Simulate abrupt process death for crash-recovery tests: dropping this
    /// owner releases custody without folding the journal into the checkpoint.
    #[cfg(test)]
    pub(crate) fn crash_store_on_drop(&mut self) {
        self.store.crash_on_drop();
    }
    pub fn flush(&mut self) -> Result<CommitReceipt> {
        let cap = self.serialized_commit_cap()?;
        self.live_publication_transition = None;
        let bytes = encode_with_cap(&self.aggregate, &self.identity, None, cap)?;
        self.store.commit(&bytes)?;
        Ok(CommitReceipt {
            generation: self.aggregate.generation,
            aggregate_hash: blake3::hash(&bytes).to_hex().to_string(),
            records: self.peer.rows(),
            replay_floors: self.peer.replay_floors(),
        })
    }
    /// Pending effects are retained, not executed. Repeated invocation IDs are idempotent.
    pub fn queue_pending(
        &mut self,
        id: &str,
        request: &[u8],
        outcome_unknown: bool,
    ) -> Result<CommitReceipt> {
        self.require_owner_resource_scope()?;
        self.aggregate.require_writable()?;
        if let Some(prior) = self
            .aggregate
            .pending
            .iter()
            .find(|p| p.invocation_id == id)
        {
            if prior.encoded_request != request || prior.outcome_unknown != outcome_unknown {
                bail!("invocation ID already names a different pending operation");
            }
            return self.flush();
        }
        let mut next = self.aggregate.clone();
        next.pending.push(PendingOperation {
            invocation_id: id.into(),
            encoded_request: request.into(),
            outcome_unknown,
        });
        self.commit(next, self.peer.clone())
    }
    /// Refreshing authority checkpoints produces new signed issuer sequences over the SAME history.
    pub fn refresh_checkpoint(&mut self) -> Result<CommitReceipt> {
        self.require_owner_resource_scope()?;
        self.aggregate.require_writable()?;
        let mut candidate = self.peer.clone();
        let messages = self.prepare_certified_bootstrap(
            &mut candidate,
            self.aggregate
                .generation
                .checked_add(1)
                .context("session generation exhausted")?,
        )?;
        let mut next = self.aggregate.clone();
        self.replace_publication(&mut next, &messages)?;
        self.commit(next, candidate)
    }
    pub fn receive(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
    ) -> Result<Option<CommitReceipt>> {
        self.require_owner_resource_scope()?;
        self.aggregate.require_writable()?;
        let request =
            matches!(&message, NeighbourMsg::SyncReq { room, .. } if room == self.peer.room());
        let mut candidate = self.peer.clone();
        let outcome = candidate
            .receive_from(origin, message, SystemTime::now())
            .map_err(anyhow::Error::msg)?;
        if !(request || outcome.changed || outcome.metadata_changed) {
            if outcome.deferred {
                // This is still unadmitted transport input, not committed document state.
                // Keep its bounded prerequisite queue reachable for later governance.
                // No receipt/checkpoint covers it; the sender retains its durable outbox
                // until admission, including across a native restart.
                self.peer = candidate;
            }
            return Ok(None);
        }
        if let Some(proof) = self.prepare_received_media_governance(&mut candidate)? {
            return self
                .commit_verified_security_metadata_only(candidate, proof)
                .map(Some);
        }
        let mut next = self.aggregate.clone();
        if candidate.incomplete_reason().is_some() {
            // Authenticated policy metadata is durable even when decryption prerequisites
            // are unavailable. Never sign a plaintext checkpoint over blocked state.
            if matches!(next.publication, PublicationStorage::V5 { .. }) {
                self.append_incomplete_governance(&mut next, &mut candidate)?;
            } else {
                let (outgoing, publication) = next.legacy_mut()?;
                outgoing.clear();
                *publication = PublicationState::checkpoint(
                    0,
                    self.aggregate
                        .generation
                        .checked_add(1)
                        .context("session generation exhausted")?,
                );
            }
            return self.commit(next, candidate).map(Some);
        }
        let transition = if request {
            let messages = self.prepare_certified_bootstrap(
                &mut candidate,
                self.aggregate
                    .generation
                    .checked_add(1)
                    .context("session generation exhausted")?,
            )?;
            self.replace_publication(&mut next, &messages)?;
            None
        } else {
            self.append_mutation_publication(&mut next, &mut candidate)?
        };
        let receipt = self.commit(next, candidate)?;
        self.install_live_publication_transition(transition);
        Ok(Some(receipt))
    }
    /// Explicit recovery/admission checkpoint: caller supplies a validated candidate.
    /// Ordinary local changes use `commit_mutation_candidate` instead.
    /// The owner still checks namespace, directory decoding and canonical restore before committing.
    pub fn commit_admission(
        &mut self,
        mut candidate: StatePeer,
        directory: Vec<u8>,
    ) -> Result<CommitReceipt> {
        self.require_owner_resource_scope()?;
        if candidate.room() != self.peer.room() || candidate.authority() != self.peer.authority() {
            bail!("admission cannot replace the session namespace");
        }
        self.validate_admission(&candidate, &directory)?;
        let messages = self.prepare_certified_bootstrap(
            &mut candidate,
            self.aggregate
                .generation
                .checked_add(1)
                .context("session generation exhausted")?,
        )?;
        let mut next = self.aggregate.clone();
        next.directory = directory;
        self.replace_publication(&mut next, &messages)?;
        self.commit(next, candidate)
    }
    /// Ordinary local commit: retain the recovery baseline and append only changed routes.
    pub fn commit_mutation_candidate(
        &mut self,
        candidate: StatePeer,
        directory: Vec<u8>,
    ) -> Result<CommitReceipt> {
        self.commit_mutation_candidate_with_creation(candidate, directory, None, None)
    }
    pub(crate) fn commit_mutation_candidate_with_policy(
        &mut self,
        candidate: StatePeer,
        directory: Vec<u8>,
        fence: OwnerPolicyFence,
    ) -> Result<CommitReceipt> {
        self.commit_mutation_candidate_with_creation(candidate, directory, None, Some(fence))
    }
    fn commit_mutation_candidate_with_creation(
        &mut self,
        mut candidate: StatePeer,
        directory: Vec<u8>,
        creation: Option<crate::canonical_records::creation::CreationRowV1>,
        fence: Option<OwnerPolicyFence>,
    ) -> Result<CommitReceipt> {
        self.require_owner_resource_scope()?;
        let timing = Timing::new("mutation_candidate");
        if candidate.room() != self.peer.room() || candidate.authority() != self.peer.authority() {
            bail!("admission cannot replace the session namespace");
        }
        timing.mark("validation_start");
        self.validate_admission(&candidate, &directory)?;
        timing.mark("validation_end");
        timing.mark("aggregate_clone_start");
        let mut next = self.aggregate.clone();
        timing.mark("aggregate_clone_end");
        next.directory = directory;
        if let Some(row) = creation {
            next.creation
                .as_mut()
                .context("creation format is not selected")?
                .admit(row)
                .map_err(anyhow::Error::msg)?;
        }
        timing.mark("mutation_publication_start");
        let transition = if let Some(fence) = &fence {
            self.append_mutation_publication_at(&mut next, &mut candidate, fence.sample()?)?
        } else {
            self.append_mutation_publication(&mut next, &mut candidate)?
        };
        timing.mark("mutation_publication_end");
        timing.mark("aggregate_persist_start");
        let result = match fence {
            Some(fence) => {
                self.commit_delivery(next, candidate, None, CommitMode::PolicyFenced(fence))
            }
            None => self.commit(next, candidate),
        };
        if result.is_ok() {
            self.install_live_publication_transition(transition);
        }
        timing.mark("aggregate_persist_end");
        result
    }
    pub(crate) fn commit_enrollment(
        &mut self,
        mut candidate: StatePeer,
        directory: Vec<u8>,
        issuance: crate::enrollment::Issuance,
    ) -> Result<CommitReceipt> {
        self.require_owner_resource_scope()?;
        if candidate.room() != self.peer.room() || candidate.authority() != self.peer.authority() {
            bail!("enrollment cannot replace native namespace");
        }
        self.validate_admission(&candidate, &directory)?;
        let messages = self.prepare_certified_bootstrap(
            &mut candidate,
            self.aggregate
                .generation
                .checked_add(1)
                .context("session generation exhausted")?,
        )?;
        let mut next = self.aggregate.clone();
        next.directory = directory;
        self.replace_publication(&mut next, &messages)?;
        next.issuances.push(issuance);
        self.commit(next, candidate)
    }
    /// Persist a newly authorized request for an already admitted device without
    /// replacing the committed publication basis. The exact request remains
    /// auditable and idempotent across restart, while a current publisher cursor
    /// has no new state to send.
    pub(crate) fn commit_existing_issuance(
        &mut self,
        issuance: crate::enrollment::Issuance,
    ) -> Result<CommitReceipt> {
        self.require_owner_resource_scope()?;
        if issuance.device_public_key != issuance.certificate.device_public_key
            || !self
                .peer
                .certificates()
                .iter()
                .any(|certificate| certificate == &issuance.certificate)
        {
            bail!("existing issuance certificate is not admitted");
        }
        let directory = WorkspaceDirectory::load(
            issuance.certificate.user_public_key,
            self.peer.authority().device_public_key,
            self.directory(),
        )?;
        let workspace = tilo_relay::room_workspace_id(self.peer.room())?;
        if !directory
            .as_map()
            .get(&workspace)
            .is_some_and(|entry| entry.member_devices.contains(&issuance.device_public_key))
        {
            bail!("existing issuance device is absent from the canonical directory");
        }
        if let Some(prior) = self.aggregate.issuances.iter().find(|prior| {
            prior.device_public_key == issuance.device_public_key
                && prior.request_id == issuance.request_id
        }) {
            if prior.certificate != issuance.certificate {
                bail!("enrollment request ID names a different certificate");
            }
            return self.flush();
        }
        let mut next = self.aggregate.clone();
        next.issuances.push(issuance);
        self.commit(next, self.peer.clone())
    }
    fn validate_admission(&self, candidate: &StatePeer, directory: &[u8]) -> Result<()> {
        fn preserves_document(before: &[u8], after: &[u8]) -> Result<()> {
            if before == after {
                return Ok(());
            }
            #[cfg(test)]
            DOCUMENT_CAUSAL_PROOFS.set(DOCUMENT_CAUSAL_PROOFS.get() + 1);
            let mut before = ui::automerge::AutoCommit::load(before)?;
            let mut after = ui::automerge::AutoCommit::load(after)?;
            if before
                .get_heads()
                .iter()
                .any(|head| after.get_change_by_hash(head).is_none())
            {
                bail!("admission candidate discards canonical causal history");
            }
            Ok(())
        }
        for row in self.peer.rows() {
            preserves_document(
                &self
                    .peer
                    .document(&row.app_id, &row.record_id)
                    .context("existing record missing")?,
                &candidate
                    .document(&row.app_id, &row.record_id)
                    .context("admission removes existing record")?,
            )?;
        }
        if let (
            NeighbourMsg::SyncResp {
                changes: before, ..
            },
            NeighbourMsg::SyncResp { changes: after, .. },
        ) = (self.peer.room_message(), candidate.room_message())
        {
            if before.iter().any(|change| !after.contains(change)) {
                bail!("admission discards room governance history");
            }
        }
        let floors = candidate.replay_floors();
        if self
            .peer
            .replay_floors()
            .iter()
            .any(|(app, record, actor, seq)| {
                !floors
                    .iter()
                    .any(|(a, r, k, n)| a == app && r == record && k == actor && n >= seq)
            })
        {
            bail!("admission resets actor replay floors");
        }
        if self
            .peer
            .certificates()
            .iter()
            .any(|cert| !candidate.certificates().contains(cert))
            || self
                .peer
                .revocations()
                .iter()
                .any(|entry| !candidate.revocations().contains(entry))
        {
            bail!("admission discards certified identity history");
        }
        if self.peer.incomplete_reason().is_some() && candidate.incomplete_reason().is_none() {
            bail!("admission discards encryption prerequisites");
        }
        preserves_document(&self.aggregate.directory, directory)?;
        let directory = WorkspaceDirectory::load(
            self.identity.root.verifying_key().to_bytes(),
            self.identity.native_cert.device_public_key,
            directory,
        )?;
        if directory_workspace_id(&directory, self.peer.room())? != self.workspace_id {
            bail!("admission changes the canonical room namespace binding");
        }
        let workspace = directory
            .as_map()
            .remove(&self.workspace_id)
            .context("admission directory removes canonical workspace")?;
        if !workspace
            .member_devices
            .contains(&self.identity.native_cert.device_public_key)
            || !workspace
                .host_devices
                .contains(&self.identity.host_cert.device_public_key)
        {
            bail!("admission directory removes native owner or enrolled host");
        }
        for member in workspace.member_devices {
            candidate
                .permissions(member, SystemTime::now())
                .map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }
    pub fn export(&mut self, path: &Path) -> Result<()> {
        self.require_owner_resource_scope()?;
        self.flush()?;
        atomic_file::write_with_mode(path, &encode(&self.aggregate, &self.identity)?, 0o600)?;
        crate::store::sync_dir(path.parent().context("export needs a parent directory")?)
    }
    /// Full identity import requires custody of the ORIGINAL native and root identities.
    /// No private key is contained in the export or copied to the destination.
    pub fn import(
        destination: &Path,
        export: &Path,
        identity_root: &Path,
        host_identity_root: &Path,
    ) -> Result<Self> {
        if std::fs::metadata(export)?.len() > crate::store::MAX_AGGREGATE_BYTES as u64 {
            bail!("aggregate exceeds the local storage size limit");
        }
        let bytes = std::fs::read(export)?;
        let (mut aggregate, signed) = decode(&bytes)?;
        let identity = identity::load(identity_root, host_identity_root)?;
        let (peer, workspace_id) = restore(&aggregate, &signed, &identity)?;
        aggregate.identity_root = std::fs::canonicalize(identity_root)?;
        aggregate.host_identity_root = std::fs::canonicalize(host_identity_root)?;
        let mut store = SessionStore::create(destination)?;
        store.commit(&encode(&aggregate, &identity)?)?;
        Ok(Self {
            store,
            identity,
            aggregate,
            peer,
            workspace_id,
            live_publication_transition: None,
            resources: None,
        })
    }
}

#[cfg(test)]
mod admission_validation_tests {
    use super::*;
    use ui::automerge::{ROOT, transaction::Transactable};

    #[test]
    fn unchanged_documents_skip_causal_decode_while_changed_documents_keep_the_proof() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let app = "dev.qos.validation";
        let record = "record";
        let mut document = ui::automerge::AutoCommit::new();
        document.put(ROOT, "value", 1_u64).unwrap();
        document.commit();
        let mut initial = owner.peer().clone();
        initial
            .publish_document(
                app,
                record,
                document.save(),
                &owner.identity.native,
                SystemTime::now(),
            )
            .unwrap();
        owner
            .commit_mutation_candidate(initial, owner.directory().to_vec())
            .unwrap();

        reset_document_causal_proofs();
        owner
            .validate_admission(owner.peer(), owner.directory())
            .unwrap();
        assert_eq!(
            document_causal_proofs(),
            0,
            "byte-identical admitted documents must not be decoded again"
        );

        let mut changed_document =
            ui::automerge::AutoCommit::load(&owner.peer().document(app, record).unwrap()).unwrap();
        let heads = changed_document.get_heads();
        changed_document.put(ROOT, "value", 2_u64).unwrap();
        changed_document.commit();
        let mut changed = owner.peer().clone();
        changed
            .publish_incremental(
                app,
                record,
                changed_document.save_after(&heads),
                &owner.identity.native,
                SystemTime::now(),
            )
            .unwrap();
        reset_document_causal_proofs();
        owner
            .commit_mutation_candidate(changed, owner.directory().to_vec())
            .unwrap();
        assert_eq!(
            document_causal_proofs(),
            1,
            "a changed document must retain the causal-head proof"
        );

        let mut revocations = tilo_user_identity::RevocationList::new();
        for entry in owner.peer().revocations() {
            revocations.insert(entry).unwrap();
        }
        let missing = StatePeer::new(
            owner.peer().room(),
            owner.peer().authority().clone(),
            owner.peer().certificates(),
            revocations,
            SystemTime::now(),
        )
        .unwrap();
        assert!(
            owner
                .validate_admission(&missing, owner.directory())
                .unwrap_err()
                .to_string()
                .contains("admission removes existing record")
        );
    }
}

#[cfg(test)]
mod compaction_transition_tests {
    use super::*;
    use std::time::Duration;
    use ui::automerge::{AutoCommit, ROOT, transaction::Transactable};

    fn commit_small_changed_record(owner: &mut SessionOwner, value: u64) {
        let app = "dev.qos.word-processor";
        let record = "tab:0000000000000001";
        let mut candidate = owner.peer().clone();
        let mut document = AutoCommit::load(&candidate.document(app, record).unwrap()).unwrap();
        document.put(ROOT, "fresh-compaction-test", value).unwrap();
        document.commit();
        candidate
            .publish_document(
                app,
                record,
                document.save(),
                &owner.identity.native,
                SystemTime::now(),
            )
            .unwrap();
        owner
            .commit_mutation_candidate(candidate, owner.directory().to_vec())
            .unwrap();
    }

    fn consumer(owner: &SessionOwner, now: SystemTime) -> StatePeer {
        let mut revocations = RevocationList::new();
        for entry in owner.peer().revocations() {
            revocations.insert(entry).unwrap();
        }
        StatePeer::new(
            owner.peer().room(),
            owner.peer().authority().clone(),
            owner.peer().certificates(),
            revocations,
            now,
        )
        .unwrap()
    }

    fn receive_encoded(target: &mut StatePeer, source: [u8; 32], encoded: &[u8], now: SystemTime) {
        target
            .receive_from(
                TransportOrigin::RelayDelta(source),
                postcard::from_bytes(encoded).unwrap(),
                now,
            )
            .unwrap();
    }

    fn room_heads(peer: &StatePeer) -> Vec<ui::automerge::ChangeHash> {
        peer.admitted_room().unwrap().clone().get_heads()
    }

    #[test]
    fn threshold_compaction_replaces_expired_live_basis_with_fresh_equivalent_transition() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let initial_at = SystemTime::now();
        let source = owner.peer().authority().device_public_key;
        let initial = outgoing_bytes(&owner).to_vec();
        let stale_snapshot = initial[1].clone();
        let mut transitioned = consumer(&owner, initial_at);
        for encoded in &initial {
            receive_encoded(&mut transitioned, source, encoded, initial_at);
        }
        let mut sent = initial.len();
        for value in 0..31 {
            commit_small_changed_record(&mut owner, value);
            for encoded in &outgoing_bytes(&owner)[sent..] {
                receive_encoded(&mut transitioned, source, encoded, initial_at);
            }
            sent = outgoing_bytes(&owner).len();
        }

        let delayed = initial_at + Duration::from_secs(301);
        assert!(
            transitioned
                .receive_from(
                    TransportOrigin::RelayDelta(source),
                    postcard::from_bytes(&stale_snapshot).unwrap(),
                    delayed,
                )
                .unwrap_err()
                .contains("timestamp"),
            "the old persisted checkpoint is not live-valid after 300 seconds"
        );
        with_test_publication_time(delayed, || commit_small_changed_record(&mut owner, 31));
        let transition = owner
            .live_publication_transition()
            .expect("threshold compaction installs the fresh live artifact")
            .clone();
        assert_eq!(
            transition
                .messages()
                .iter()
                .skip(1)
                .map(|(bytes, _)| bytes)
                .collect::<Vec<_>>(),
            outgoing_bytes(&owner).iter().skip(1).collect::<Vec<_>>(),
            "the transition and durable basis carry the exact same fresh snapshot wire frames"
        );
        for (encoded, _) in transition.messages() {
            receive_encoded(&mut transitioned, source, encoded, delayed);
        }

        let mut full = consumer(&owner, delayed);
        for encoded in &outgoing_bytes(&owner) {
            receive_encoded(&mut full, source, encoded, delayed);
        }
        assert_eq!(transitioned.rows(), full.rows());
        assert_eq!(room_heads(&transitioned), room_heads(&full));
        assert_eq!(transitioned.replay_floors(), full.replay_floors());
        assert_eq!(transitioned.incomplete_reason(), full.incomplete_reason());
        for row in full.rows() {
            assert_eq!(
                transitioned.document_heads(&row.app_id, &row.record_id),
                full.document_heads(&row.app_id, &row.record_id)
            );
            assert_eq!(
                transitioned
                    .durable_coverage(&row.app_id, &row.record_id)
                    .unwrap(),
                full.durable_coverage(&row.app_id, &row.record_id).unwrap()
            );
            assert_eq!(
                transitioned
                    .durable_covers_current(&row.app_id, &row.record_id)
                    .unwrap(),
                full.durable_covers_current(&row.app_id, &row.record_id)
                    .unwrap()
            );
        }
    }
}

#[cfg(test)]
mod enrollment_migration_tests {
    use super::*;

    fn retain_qos_v1_route(owner: &mut SessionOwner, app: &str, record: &str) {
        let now = SystemTime::now();
        let generation = owner.aggregate.generation + 1;
        let mut source = owner.peer.clone();
        let mut messages = source
            .durable_bootstrap_messages(&owner.identity.native, now, generation)
            .unwrap();
        let target = messages
            .iter_mut()
            .find_map(|message| match message {
                NeighbourMsg::AppDocument {
                    app_id,
                    record_id,
                    envelope,
                    ..
                } if app_id == app && record_id == record => Some(envelope),
                _ => None,
            })
            .expect("target checkpoint");
        let mutation = collab_types::DocumentMutation::decode_versioned(&target.payload)
            .unwrap()
            .mutation;
        let version = collab_types::DocumentWireVersion::QosV1;
        target.payload = mutation.encode_for(version).unwrap();
        target.artifact =
            collab_types::document_artifact_id_for_version(version, app, owner.peer.room(), record);
        target.attestation_id = collab_types::document_attestation_id_for_version(
            version,
            app,
            owner.peer.room(),
            record,
        );
        collab_types::sign_op(target, &owner.identity.native).unwrap();
        let payload_hash = *blake3::hash(&target.payload).as_bytes();

        let NeighbourMsg::SyncResp { changes, .. } = &messages[0] else {
            panic!("room checkpoint first")
        };
        let mut room = qos_peer_state::room::candidate(None, changes).unwrap();
        let route_key = qos_peer_state::coverage::route_key(app, record);
        let mut coverage = qos_peer_state::coverage::entries(&room)
            .unwrap()
            .remove(&route_key)
            .expect("target coverage");
        coverage.payload_hash = payload_hash;
        qos_peer_state::coverage::put(&mut room, coverage.clone()).unwrap();
        let mut receipts = qos_peer_state::durability::entries(&room).unwrap();
        receipts.insert(
            route_key,
            qos_peer_state::durability::DurableReceipt::new(coverage, generation).unwrap(),
        );
        qos_peer_state::durability::put(&mut room, receipts).unwrap();
        messages[0] = NeighbourMsg::SyncResp {
            changes: room
                .get_changes(&[])
                .into_iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect(),
            room: owner.peer.room().into(),
        };

        let mut revocations = RevocationList::new();
        for revocation in owner.peer.revocations() {
            revocations.insert(revocation).unwrap();
        }
        let mut legacy = StatePeer::new(
            owner.peer.room(),
            owner.peer.authority().clone(),
            owner.peer.certificates(),
            revocations,
            now,
        )
        .unwrap();
        for message in &messages {
            legacy
                .receive_from(
                    TransportOrigin::RetainedRelayDelta(owner.peer.authority().device_public_key),
                    message.clone(),
                    now,
                )
                .unwrap();
        }
        owner.aggregate.peer = legacy.durable().unwrap();
        owner.aggregate.generation = generation;
        *owner.aggregate.legacy_mut().unwrap().0 = messages
            .iter()
            .map(postcard::to_stdvec)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        *owner.aggregate.legacy_mut().unwrap().1 =
            PublicationState::checkpoint(owner.outgoing().len(), generation);
        owner.peer = legacy;
        owner
            .store
            .commit(&encode(&owner.aggregate, &owner.identity).unwrap())
            .unwrap();
    }

    #[tokio::test]
    async fn retained_qos_v1_precommit_and_owner_receipt_preserve_exact_wire() {
        use qos_peer_state::owner_commit::{
            CommitOperation, OwnerCommitRequest, OwnerCommitService,
        };
        use ui::automerge::transaction::Transactable;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("legacy-owner");
        let mut owner = SessionOwner::initialize(&path).unwrap();
        let app = "dev.qos.word-processor";
        let record = "tab:0000000000000001";
        retain_qos_v1_route(&mut owner, app, record);
        drop(owner);

        let mut owner = SessionOwner::open(&path).unwrap();
        let mut document =
            ui::automerge::AutoCommit::load(&owner.peer().document(app, record).unwrap()).unwrap();
        let heads = document.get_heads();
        document
            .put(ui::automerge::ROOT, "legacy_receipt", true)
            .unwrap();
        document.commit();
        let mut precommitted = owner.peer().clone();
        let message = precommitted
            .publish_incremental(
                app,
                record,
                document.save_after(&heads),
                &owner.identity.native,
                SystemTime::now(),
            )
            .unwrap();
        let NeighbourMsg::AppDocument { envelope, .. } = message else {
            panic!("precommit emits document")
        };
        assert!(matches!(
            collab_types::DocumentMutation::decode_versioned(&envelope.payload),
            Ok(collab_types::DecodedDocumentMutation {
                version: collab_types::DocumentWireVersion::QosV1,
                mutation: collab_types::DocumentMutation::Incremental(_),
            })
        ));
        let request = OwnerCommitRequest {
            room: owner.peer().room().into(),
            operation: CommitOperation::Document {
                app: app.into(),
                record: record.into(),
                envelope,
            },
        };
        let (service, mut mailbox) = crate::owner_commit::OwnerCommitMailbox::bounded(2);
        let expected = request.clone();
        let waiting = tokio::spawn(async move { service.commit(request).await });
        let receipt = mailbox.recv().await.unwrap().apply(&mut owner).unwrap();
        receipt.validate_for(&expected).unwrap();
        let returned = waiting.await.unwrap().unwrap();
        returned.validate_for(&expected).unwrap();
        assert_eq!(returned.binding, receipt.binding);
        assert_eq!(returned.generation, receipt.generation);
        assert_eq!(returned.aggregate_hash, receipt.aggregate_hash);
        assert!(owner.status().generation > 1);
    }
    #[test]
    fn empty_publication_rebuilds_once_and_reconnects_from_the_committed_basis() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owner");
        let mut owner = SessionOwner::initialize(&path).unwrap();
        let generation = owner.status().generation;
        owner.aggregate.legacy_mut().unwrap().0.clear();
        *owner.aggregate.legacy_mut().unwrap().1 = PublicationState::checkpoint(0, generation);
        owner
            .store
            .commit(&encode(&owner.aggregate, &owner.identity).unwrap())
            .unwrap();
        drop(owner);

        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.status().generation, generation + 1);
        assert!(!outgoing_bytes(&owner).is_empty());
        assert!(
            outgoing_kinds(&owner)
                .iter()
                .all(|kind| *kind == Retention::Checkpoint)
        );
        let outgoing = outgoing_bytes(&owner).to_vec();
        let kinds = outgoing_kinds(&owner).to_vec();
        let mut delivered = Vec::new();
        crate::publication::PublicationCursor::default()
            .send(&owner, |bytes, kind| {
                delivered.push((bytes.to_vec(), kind));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            delivered,
            outgoing
                .iter()
                .cloned()
                .zip(kinds.iter().copied())
                .collect::<Vec<_>>()
        );
        drop(owner);

        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.status().generation, generation + 1);
        assert_eq!(outgoing_bytes(&owner), outgoing);
        assert_eq!(outgoing_kinds(&owner), kinds);
    }

    #[test]
    fn incomplete_encryption_reopens_without_publishing_a_plaintext_checkpoint() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owner");
        let mut owner = SessionOwner::initialize(&path).unwrap();
        let author = owner.identity.native.verifying_key().to_bytes();
        let mut encrypted = owner
            .peer
            .signed_snapshots(&owner.identity.native, SystemTime::now())
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            record_id,
            envelope,
            ..
        } = &mut encrypted
        else {
            panic!("signed snapshot must be an application document");
        };
        envelope.seq += 1;
        envelope.encryption = Some(collab_types::EncryptionMetadata {
            epoch: 1,
            channel: collab_types::document_channel_id(app_id, cluster_id, record_id),
        });
        envelope.payload = vec![1, 2, 3];
        collab_types::sign_op(envelope, &owner.identity.native).unwrap();
        owner
            .receive(TransportOrigin::RelayDelta(author), encrypted)
            .unwrap()
            .unwrap();
        assert!(!owner.permits_publication());
        assert!(outgoing_bytes(&owner).is_empty());
        let generation = owner.status().generation;
        drop(owner);

        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.status().generation, generation);
        assert!(owner.peer().incomplete_reason().is_some());
        assert!(!owner.permits_publication());
        assert!(outgoing_bytes(&owner).is_empty());
    }

    #[test]
    fn legacy_room_binding_survives_restart_and_enrollment() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("legacy-room");
        let mut owner = SessionOwner::initialize(&path).unwrap();
        let current = tilo_relay::room_workspace_id(owner.peer.room()).unwrap();
        let legacy = tilo_relay::legacy_room_workspace_id(owner.peer.room()).unwrap();
        let mut directory = WorkspaceDirectory::load(
            owner.identity.root.verifying_key().to_bytes(),
            owner.identity.native_cert.device_public_key,
            &owner.aggregate.directory,
        )
        .unwrap();
        directory.remove_workspace(current).unwrap();
        directory
            .add_workspace(legacy, "Existing shared room")
            .unwrap();
        directory
            .set_member_devices(legacy, &[owner.identity.native_cert.device_public_key])
            .unwrap();
        directory
            .set_host_devices(legacy, &[owner.identity.host_cert.device_public_key])
            .unwrap();
        owner.aggregate.directory = directory.save();
        owner
            .store
            .commit(&encode(&owner.aggregate, &owner.identity).unwrap())
            .unwrap();
        drop(owner);
        let mut owner = SessionOwner::open(&path).unwrap();
        let device = ed25519_dalek::SigningKey::from_bytes(&[37; 32]);
        let mut enrollment = crate::enrollment::EnrollmentAuthority::default();
        let now = tilo_relay::clock::now_ns();
        let response = enrollment
            .challenge(
                &owner,
                qos_peer_state::enrollment::ChallengeRequest {
                    device_public_key: hex::encode(device.verifying_key().to_bytes()),
                    request_id: hex::encode([38; 32]),
                },
                now,
            )
            .unwrap();
        let challenge = qos_peer_state::enrollment::decode(&response.challenge).unwrap();
        assert_eq!(challenge.workspace_id, legacy);
        assert_ne!(challenge.workspace_id, current);
        assert_eq!(owner.workspace_id(), legacy);
        let proof = qos_peer_state::enrollment::sign(&challenge, &device).unwrap();
        let certificate = enrollment.complete(&mut owner, proof, now).unwrap();
        assert_eq!(
            certificate.device_public_key,
            device.verifying_key().to_bytes()
        );
        drop(owner);
        let mut owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.workspace_id(), legacy);
        let admitted = WorkspaceDirectory::load(
            owner.identity.root.verifying_key().to_bytes(),
            owner.identity.native_cert.device_public_key,
            &owner.aggregate.directory,
        )
        .unwrap();
        assert!(
            admitted.as_map()[&legacy]
                .member_devices
                .contains(&certificate.device_public_key)
        );
        assert!(!admitted.as_map().contains_key(&current));
        let mut ambiguous = WorkspaceDirectory::load(
            owner.identity.root.verifying_key().to_bytes(),
            owner.identity.native_cert.device_public_key,
            &owner.aggregate.directory,
        )
        .unwrap();
        ambiguous
            .add_workspace(current, "Competing namespace")
            .unwrap();
        owner.aggregate.directory = ambiguous.save();
        owner
            .store
            .commit(&encode(&owner.aggregate, &owner.identity).unwrap())
            .unwrap();
        drop(owner);
        assert!(
            SessionOwner::open(&path).is_err(),
            "ambiguous durable room bindings must be rejected"
        );
    }

    #[test]
    fn signed_v2_migration_preserves_exact_legacy_bytes_and_rejects_invalid_signature() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owner");
        let mut owner = SessionOwner::initialize(&path).unwrap();
        owner
            .queue_pending("original-id", &[1, 2, 3], true)
            .unwrap();
        let before = owner.status();
        let old = &owner.aggregate;
        let legacy = AggregateV2 {
            version: 2,
            identity_root: old.identity_root.clone(),
            host_identity_root: old.host_identity_root.clone(),
            room: old.room.clone(),
            authority: old.authority.clone(),
            directory: old.directory.clone(),
            peer: old.peer.clone(),
            outgoing: old.outgoing().iter().map(|p| p.bytes.to_vec()).collect(),
            pending: old.pending.clone(),
            generation: old.generation,
            issuances: old.issuances.clone(),
        };
        let payload = postcard::to_stdvec(&legacy).unwrap();
        let signature = owner
            .identity
            .native
            .sign(
                blake3::Hasher::new_derive_key("qOS native session aggregate v1")
                    .update(&payload)
                    .finalize()
                    .as_bytes(),
            )
            .to_bytes()
            .to_vec();
        let signed = SignedAggregate { payload, signature };
        let bytes = postcard::to_stdvec(&signed).unwrap();
        owner.store.commit(&bytes).unwrap();
        drop(owner);
        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.status(), before);
        assert_eq!(outgoing_bytes(&owner), legacy.outgoing);
        assert_eq!(owner.peer().durable().unwrap(), legacy.peer);
        assert_eq!(owner.pending(), legacy.pending);
        assert!(
            outgoing_kinds(&owner)
                .iter()
                .all(|kind| *kind == Retention::Checkpoint)
        );
        let migrated: SignedAggregate =
            postcard::from_bytes(&std::fs::read(path.join("session.bin")).unwrap()).unwrap();
        assert_eq!(
            postcard::take_from_bytes::<u32>(&migrated.payload)
                .unwrap()
                .0,
            4
        );
        drop(owner);
        let mut invalid = signed;
        invalid.signature[0] ^= 1;
        let invalid_bytes = postcard::to_stdvec(&invalid).unwrap();
        std::fs::write(path.join("session.bin"), &invalid_bytes).unwrap();
        assert!(SessionOwner::open(&path).is_err());
        assert_eq!(
            std::fs::read(path.join("session.bin")).unwrap(),
            invalid_bytes,
            "migration must verify the original signed payload before writing"
        );
    }
    #[test]
    fn signed_v1_state_migrates_without_replacing_identity_or_canonical_history() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owner");
        let mut owner = SessionOwner::initialize(&path).unwrap();
        let before = owner.status();
        let old = &owner.aggregate;
        let legacy = LegacyAggregate {
            version: 1,
            identity_root: old.identity_root.clone(),
            host_identity_root: old.host_identity_root.clone(),
            room: old.room.clone(),
            authority: old.authority.clone(),
            directory: old.directory.clone(),
            peer: old.peer.clone(),
            outgoing: old.outgoing().iter().map(|p| p.bytes.to_vec()).collect(),
            pending: old.pending.clone(),
            generation: old.generation,
        };
        let payload = postcard::to_stdvec(&legacy).unwrap();
        let hash = blake3::Hasher::new_derive_key("qOS native session aggregate v1")
            .update(&payload)
            .finalize();
        let signature = owner
            .identity
            .native
            .sign(hash.as_bytes())
            .to_bytes()
            .to_vec();
        owner
            .store
            .commit(&postcard::to_stdvec(&SignedAggregate { payload, signature }).unwrap())
            .unwrap();
        drop(owner);
        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.status(), before);
        assert_eq!(owner.enrollment_count(), 0);
        let signed: SignedAggregate =
            postcard::from_bytes(&std::fs::read(path.join("session.bin")).unwrap()).unwrap();
        let (version, _) = postcard::take_from_bytes::<u32>(&signed.payload).unwrap();
        assert_eq!(version, 4);
    }
}

#[cfg(test)]
mod desktop_migration_tests {
    use super::*;

    fn app_id(tab: &qos_desktop::projection::ReplicatedTab) -> Option<&str> {
        match tab.tile.as_ref()? {
            qos_desktop::TileContent::AppPlaceholder {
                kind: qos_desktop::AppKind::Other(app),
                ..
            } => Some(app),
            _ => None,
        }
    }

    fn topology(owner: &SessionOwner) -> Vec<qos_desktop::projection::ReplicatedStrip> {
        qos_desktop::projection::RoomProjection::contents_of(owner.peer().admitted_room().unwrap())
    }

    fn persist_legacy_shared_writer_notes(owner: &mut SessionOwner) {
        use qos_desktop::projection::RoomProjection;
        use ui::automerge::{ROOT, transaction::Transactable};
        let mut doc = owner.peer().admitted_room().unwrap().clone();
        doc.delete(ROOT, "writer_notes_split_v1").unwrap();
        // Real workspaces can have a high durable allocator even when the
        // corresponding closed topology ids are no longer visible.
        doc.put(ROOT, "desktop_counter_v1", 50_000_u64).unwrap();
        let routes = RoomProjection::record_routes(&doc);
        let mut strips = RoomProjection::contents_of(&doc)
            .into_iter()
            .map(RoomProjection::hydrate_strip)
            .collect::<Vec<_>>();
        let strip = strips.first_mut().unwrap();
        let location = |app: &str| {
            strip
                .window_groups
                .iter()
                .enumerate()
                .find_map(|(group_index, group)| {
                    group
                        .tabs
                        .iter()
                        .position(|tab| match &tab.tile {
                            qos_desktop::TileContent::AppPlaceholder {
                                kind: qos_desktop::AppKind::Other(id),
                                ..
                            } => id == app,
                            _ => false,
                        })
                        .map(|tab_index| (group_index, tab_index))
                })
                .unwrap()
        };
        let (writer_group, _) = location("dev.qos.word-processor");
        let (notes_group, notes_tab) = location("dev.qos.notes");
        if writer_group != notes_group {
            let mut group = strip.window_groups.remove(notes_group);
            let notes = group.tabs.remove(notes_tab);
            let writer_group = if notes_group < writer_group {
                writer_group - 1
            } else {
                writer_group
            };
            strip.window_groups[writer_group].tabs.push(notes);
        }
        let salt = qos_desktop::identity::peer_salt_of(strip.id.0);
        RoomProjection::project_owned_contents(&mut doc, &strips, salt, None);
        for (tab, record) in routes {
            let (strip, group) = strips
                .iter()
                .find_map(|strip| {
                    strip.window_groups.iter().find_map(|group| {
                        group
                            .tabs
                            .iter()
                            .any(|candidate| candidate.id == tab)
                            .then_some((strip.id.0, group.id))
                    })
                })
                .unwrap();
            RoomProjection::bind_record(&mut doc, strip, group, tab, &record).unwrap();
        }
        doc.commit();
        owner
            .peer
            .install_seed(
                qos_peer_state::SeedState {
                    room_changes: doc
                        .get_changes(&[])
                        .iter()
                        .map(|change| change.raw_bytes().to_vec())
                        .collect(),
                    documents: vec![],
                },
                &owner.identity.native,
                SystemTime::now(),
            )
            .unwrap();
        owner.aggregate.peer = owner.peer.durable().unwrap();
        owner
            .store
            .commit(&encode(&owner.aggregate, &owner.identity).unwrap())
            .unwrap();
    }

    #[test]
    fn new_session_places_writer_notes_and_chat_in_three_visible_groups() {
        let dir = tempfile::tempdir().unwrap();
        let owner = SessionOwner::initialize(&dir.path().join("separate-groups")).unwrap();
        let strips = topology(&owner);
        assert_eq!(strips.len(), 1);
        assert_eq!(strips[0].groups.len(), 3);
        let apps = strips[0]
            .groups
            .iter()
            .map(|group| {
                assert_eq!(group.tabs.len(), 1);
                app_id(&group.tabs[0]).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            apps,
            ["dev.qos.word-processor", "dev.qos.notes", "dev.qos.chat"]
        );
    }

    #[test]
    fn reopening_bound_legacy_notes_splits_once_without_rebinding_its_record() {
        use qos_desktop::projection::RoomProjection;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy-shared-documents");
        let mut owner = SessionOwner::initialize(&path).unwrap();
        let initial = topology(&owner);
        let tab = |app| {
            initial[0]
                .groups
                .iter()
                .flat_map(|group| &group.tabs)
                .find(|candidate| app_id(candidate) == Some(app))
                .unwrap()
                .id
        };
        let writer_tab = tab("dev.qos.word-processor");
        let notes_tab = tab("dev.qos.notes");
        let chat_tab = tab("dev.qos.chat");
        let writer_group = initial[0]
            .groups
            .iter()
            .find(|group| app_id(&group.tabs[0]) == Some("dev.qos.word-processor"))
            .unwrap()
            .id;
        let chat_group = initial[0]
            .groups
            .iter()
            .find(|group| app_id(&group.tabs[0]) == Some("dev.qos.chat"))
            .unwrap()
            .id;
        let routes = RoomProjection::record_routes(owner.peer().admitted_room().unwrap());
        let notes_record = routes
            .get(&notes_tab)
            .expect("legacy Notes tab must carry an explicit durable route")
            .clone();
        let notes_bytes = owner
            .peer()
            .document("dev.qos.notes", &notes_record)
            .unwrap();
        persist_legacy_shared_writer_notes(&mut owner);
        assert_eq!(topology(&owner)[0].groups.len(), 2);
        let generation = owner.status().generation;
        drop(owner);

        let owner = SessionOwner::open(&path).unwrap();
        let migrated = topology(&owner);
        assert_eq!(migrated[0].groups.len(), 3);
        assert!(migrated[0].groups.iter().all(|group| group.tabs.len() == 1));
        let migrated_tab = |app| {
            migrated[0]
                .groups
                .iter()
                .flat_map(|group| &group.tabs)
                .find(|candidate| app_id(candidate) == Some(app))
                .unwrap()
                .id
        };
        assert_eq!(migrated_tab("dev.qos.word-processor"), writer_tab);
        assert_eq!(migrated_tab("dev.qos.chat"), chat_tab);
        let migrated_notes_tab = migrated_tab("dev.qos.notes");
        assert_ne!(migrated_notes_tab, notes_tab);
        assert!(qos_desktop::identity::counter_of(migrated_notes_tab) >= 50_000);
        assert_eq!(
            migrated[0]
                .groups
                .iter()
                .find(|group| app_id(&group.tabs[0]) == Some("dev.qos.word-processor"))
                .unwrap()
                .id,
            writer_group
        );
        assert_eq!(
            migrated[0]
                .groups
                .iter()
                .find(|group| app_id(&group.tabs[0]) == Some("dev.qos.chat"))
                .unwrap()
                .id,
            chat_group
        );
        let migrated_notes_group = migrated[0]
            .groups
            .iter()
            .find(|group| app_id(&group.tabs[0]) == Some("dev.qos.notes"))
            .unwrap()
            .id;
        assert!(qos_desktop::identity::counter_of(migrated_notes_group) >= 50_000);
        let migrated_routes = RoomProjection::record_routes(owner.peer().admitted_room().unwrap());
        assert_eq!(migrated_routes.get(&writer_tab), routes.get(&writer_tab));
        assert_eq!(migrated_routes.get(&chat_tab), routes.get(&chat_tab));
        assert!(!migrated_routes.contains_key(&notes_tab));
        assert_eq!(
            migrated_routes.get(&migrated_notes_tab),
            Some(&notes_record)
        );
        assert_eq!(
            owner
                .peer()
                .document("dev.qos.notes", &notes_record)
                .unwrap(),
            notes_bytes
        );
        assert_eq!(owner.status().generation, generation + 1);
        let durable = owner.peer().durable().unwrap();
        drop(owner);

        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.status().generation, generation + 1);
        assert_eq!(owner.peer().durable().unwrap(), durable);
    }

    #[test]
    fn existing_multi_namespace_topology_survives_native_restart_unchanged() {
        use ui::automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multi-owner");
        let mut owner = SessionOwner::initialize(&path).unwrap();
        let mut doc = owner.peer.admitted_room().unwrap().clone();
        let contents = doc.get(ROOT, "contents").unwrap().unwrap().1;
        for salt in [7_u16, 8] {
            let strip = doc
                .put_object(
                    &contents,
                    format!("{:016x}", (u64::from(salt) << 48) | 1),
                    ObjType::Map,
                )
                .unwrap();
            doc.put(&strip, "name", "Existing peer strip").unwrap();
            doc.put(&strip, "order", u64::from(salt)).unwrap();
            doc.put_object(strip, "groups", ObjType::Map).unwrap();
        }
        doc.commit();
        owner
            .peer
            .install_seed(
                qos_peer_state::SeedState {
                    room_changes: doc
                        .get_changes(&[])
                        .iter()
                        .map(|c| c.raw_bytes().to_vec())
                        .collect(),
                    documents: vec![],
                },
                &owner.identity.native,
                SystemTime::now(),
            )
            .unwrap();
        owner.aggregate.peer = owner.peer.durable().unwrap();
        owner
            .store
            .commit(&encode(&owner.aggregate, &owner.identity).unwrap())
            .unwrap();
        let expected = owner.peer.durable().unwrap();
        let generation = owner.status().generation;
        drop(owner);
        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.peer().durable().unwrap(), expected);
        assert_eq!(owner.status().generation, generation);
    }
    #[test]
    fn legacy_room_migration_preserves_authority_app_bytes_pending_and_outgoing_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy");
        let mut owner = SessionOwner::initialize(&path).unwrap();
        let room = owner.peer.room().to_owned();
        let cert = owner.identity.native_cert.clone();
        let now = SystemTime::now();
        let mut legacy = StatePeer::new(
            &room,
            cert.clone(),
            vec![cert.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        legacy
            .install_seed(
                crate::seed::seed(&room, &cert, &[cert.clone()]).unwrap(),
                &owner.identity.native,
                now,
            )
            .unwrap();
        // A topology-free room may already reserve a certified owner's namespace.
        // Migration must extend that registry rather than replacing it.
        {
            use ui::automerge::{ObjType, ROOT, transaction::Transactable};
            let mut doc = legacy.admitted_room().unwrap().clone();
            let owners = doc
                .put_object(ROOT, "contents_owners_v1", ObjType::Map)
                .unwrap();
            doc.put(owners, "0007", hex::encode(cert.device_public_key))
                .unwrap();
            doc.commit();
            legacy
                .install_seed(
                    qos_peer_state::SeedState {
                        room_changes: doc
                            .get_changes(&[])
                            .iter()
                            .map(|c| c.raw_bytes().to_vec())
                            .collect(),
                        documents: vec![],
                    },
                    &owner.identity.native,
                    now,
                )
                .unwrap();
        }
        let writer = legacy
            .document("dev.qos.word-processor", "tab:0000000000000001")
            .unwrap();
        let chat = legacy.document("dev.qos.chat", "").unwrap();
        owner.aggregate.pending.push(PendingOperation {
            invocation_id: "retained-intent".into(),
            encoded_request: vec![1, 2, 3],
            outcome_unknown: true,
        });
        *owner.aggregate.legacy_mut().unwrap().0 = legacy
            .durable_bootstrap_messages(&owner.identity.native, now, owner.aggregate.generation)
            .unwrap()
            .iter()
            .map(|m| postcard::to_stdvec(m).unwrap())
            .collect();
        *owner.aggregate.legacy_mut().unwrap().1 =
            PublicationState::checkpoint(owner.outgoing().len(), owner.aggregate.generation);
        let old_delta = postcard::to_stdvec(&NeighbourMsg::SyncResp {
            changes: Vec::new(),
            room: room.clone(),
        })
        .unwrap();
        owner
            .aggregate
            .legacy_mut()
            .unwrap()
            .0
            .push(old_delta.clone());
        owner
            .aggregate
            .legacy_mut()
            .unwrap()
            .1
            .kinds
            .push(Retention::Delta);
        owner.aggregate.peer = legacy.durable().unwrap();
        let pending = owner.aggregate.pending.clone();
        let generation = owner.aggregate.generation;
        let authority = owner.aggregate.authority.clone();
        let directory = owner.aggregate.directory.clone();
        owner
            .store
            .commit(&encode(&owner.aggregate, &owner.identity).unwrap())
            .unwrap();
        drop(owner);
        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.status().room, room);
        assert_eq!(owner.aggregate.authority, authority);
        assert_eq!(owner.directory(), directory);
        assert_eq!(owner.pending(), pending);
        assert!(!outgoing_bytes(&owner).contains(&old_delta));
        assert!(
            outgoing_kinds(&owner)
                .iter()
                .all(|kind| *kind == Retention::Checkpoint)
        );
        let mut delivered = Vec::new();
        crate::publication::PublicationCursor::default()
            .send(&owner, |bytes, kind| {
                delivered.push((bytes.to_vec(), kind));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            delivered,
            outgoing_bytes(&owner)
                .iter()
                .cloned()
                .zip(outgoing_kinds(&owner).iter().copied())
                .collect::<Vec<_>>()
        );
        assert_eq!(owner.status().generation, generation + 1);
        assert_eq!(
            owner
                .peer()
                .document("dev.qos.word-processor", "tab:0000000000000001")
                .unwrap(),
            writer
        );
        assert_eq!(owner.peer().document("dev.qos.chat", "").unwrap(), chat);
        assert!(
            owner
                .status()
                .records
                .iter()
                .any(|r| r.app_id == "dev.qos.notes")
        );
        let state = owner.peer().durable().unwrap();
        drop(owner);
        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.peer().durable().unwrap(), state);
        assert_eq!(owner.status().generation, generation + 1);
    }
}

#[cfg(test)]
fn outgoing_bytes(owner: &crate::SessionOwner) -> Vec<Vec<u8>> {
    owner.outgoing().iter().map(|p| p.bytes.to_vec()).collect()
}
#[cfg(test)]
fn outgoing_kinds(owner: &crate::SessionOwner) -> Vec<qos_peer_state::transport::Retention> {
    owner.outgoing().iter().map(|p| p.kind).collect()
}
