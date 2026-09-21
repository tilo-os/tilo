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
#[derive(Clone, Serialize, Deserialize)]
struct LegacyAggregate {
    version: u32,
    identity_root: PathBuf,
    host_identity_root: PathBuf,
    room: String,
    authority: DeviceCert,
    directory: Vec<u8>,
    /// Versioned canonical StatePeer export owns roster, revocations and replay floors.
    peer: Vec<u8>,
    outgoing: Vec<Vec<u8>>,
    pending: Vec<PendingOperation>,
    generation: u64,
}
#[derive(Clone, Serialize, Deserialize)]
struct AggregateV2 {
    version: u32,
    identity_root: PathBuf,
    host_identity_root: PathBuf,
    room: String,
    authority: DeviceCert,
    directory: Vec<u8>,
    /// Versioned canonical StatePeer export owns roster, revocations and replay floors.
    peer: Vec<u8>,
    outgoing: Vec<Vec<u8>>,
    pending: Vec<PendingOperation>,
    generation: u64,
    issuances: Vec<crate::enrollment::Issuance>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Aggregate {
    version: u32,
    identity_root: PathBuf,
    host_identity_root: PathBuf,
    room: String,
    authority: DeviceCert,
    directory: Vec<u8>,
    /// Versioned canonical StatePeer export owns roster, revocations and replay floors.
    peer: Vec<u8>,
    outgoing: Vec<Vec<u8>>,
    pending: Vec<PendingOperation>,
    generation: u64,
    issuances: Vec<crate::enrollment::Issuance>,
    publication: PublicationState,
}
impl From<LegacyAggregate> for AggregateV2 {
    fn from(old: LegacyAggregate) -> Self {
        Self {
            version: 2,
            identity_root: old.identity_root,
            host_identity_root: old.host_identity_root,
            room: old.room,
            authority: old.authority,
            directory: old.directory,
            peer: old.peer,
            outgoing: old.outgoing,
            pending: old.pending,
            generation: old.generation,
            issuances: vec![],
        }
    }
}
/// Bounds redundant publication tail, not canonical Automerge history or baseline size.
const MAX_PUBLICATION_TAIL_ENTRIES: usize = 64;
const MAX_PUBLICATION_TAIL_BYTES: usize = 4 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
struct PublicationState {
    kinds: Vec<Retention>,
    baseline_len: usize,
    baseline_generation: u64,
}
impl PublicationState {
    fn checkpoint(len: usize, generation: u64) -> Self {
        Self {
            kinds: vec![Retention::Checkpoint; len],
            baseline_len: len,
            baseline_generation: generation,
        }
    }
}
impl From<AggregateV2> for Aggregate {
    fn from(old: AggregateV2) -> Self {
        let publication = PublicationState::checkpoint(old.outgoing.len(), old.generation);
        Self {
            version: 3,
            identity_root: old.identity_root,
            host_identity_root: old.host_identity_root,
            room: old.room,
            authority: old.authority,
            directory: old.directory,
            peer: old.peer,
            outgoing: old.outgoing,
            pending: old.pending,
            generation: old.generation,
            issuances: old.issuances,
            publication,
        }
    }
}
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
fn with_test_publication_time<T>(time: SystemTime, operation: impl FnOnce() -> T) -> T {
    TEST_PUBLICATION_TIME.with(|current| {
        let prior = current.replace(Some(time));
        let result = operation();
        current.replace(prior);
        result
    })
}
fn encode(aggregate: &Aggregate, identity: &Identity) -> Result<Vec<u8>> {
    encode_with_timing(aggregate, identity, None)
}
fn encode_with_timing(
    aggregate: &Aggregate,
    identity: &Identity,
    timing: Option<&Timing>,
) -> Result<Vec<u8>> {
    if let Some(timing) = timing {
        timing.mark("aggregate_encode_start");
    }
    let payload = postcard::to_stdvec(aggregate)?;
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
    let bytes = postcard::to_stdvec(&SignedAggregate { payload, signature })?;
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
    let (version, _) = postcard::take_from_bytes::<u32>(&signed.payload)?;
    let aggregate = match version {
        1 => Aggregate::from(AggregateV2::from(postcard::from_bytes::<LegacyAggregate>(
            &signed.payload,
        )?)),
        2 => Aggregate::from(postcard::from_bytes::<AggregateV2>(&signed.payload)?),
        3 => postcard::from_bytes::<Aggregate>(&signed.payload)?,
        _ => bail!("unsupported aggregate version; explicit migration required"),
    };
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
    if !peer.has_governance() || peer.rows().is_empty() {
        bail!("restored canonical session is incomplete");
    }
    if aggregate.publication.kinds.len() != aggregate.outgoing.len()
        || aggregate.publication.baseline_len > aggregate.outgoing.len()
        || aggregate.publication.baseline_generation == 0
        || aggregate.publication.baseline_generation > aggregate.generation
        || (!aggregate.outgoing.is_empty() && aggregate.publication.baseline_len == 0)
    {
        bail!("invalid durable publication baseline");
    }
    for (bytes, kind) in aggregate.outgoing.iter().zip(&aggregate.publication.kinds) {
        let message: NeighbourMsg = postcard::from_bytes(bytes)?;
        if retention(&message, *kind == Retention::Checkpoint).map(|(_, actual)| actual)
            != Some(*kind)
        {
            bail!("invalid durable publication retention kind");
        }
    }
    Ok((peer, workspace_id))
}
impl SessionOwner {
    /// Explicitly provisions a NEW room and authority. An existing directory is never reseeded.
    pub fn initialize(root: &Path) -> Result<Self> {
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
        peer.install_seed(
            crate::seed::seed(&room, &identity.native_cert, &roster).map_err(anyhow::Error::msg)?,
            &identity.native,
            now,
        )
        .map_err(anyhow::Error::msg)?;
        crate::seed::bootstrap_desktop(&mut peer, &identity.native).map_err(anyhow::Error::msg)?;
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
        directory.add_workspace(workspace, "Shared qOS workspace")?;
        directory.set_member_devices(workspace, &[identity.native_cert.device_public_key])?;
        directory.set_host_devices(workspace, &[identity.host_cert.device_public_key])?;
        let aggregate = Aggregate {
            version: 3,
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
        };
        store.commit(&encode(&aggregate, &identity)?)?;
        Ok(Self {
            store,
            identity,
            aggregate,
            peer,
            workspace_id: workspace,
            live_publication_transition: None,
        })
    }
    pub fn open(root: &Path) -> Result<Self> {
        let mut store = SessionStore::open(root)?;
        let bytes = store.read()?;
        let (aggregate, signed) = decode(&bytes)?;
        let identity = identity::load(&aggregate.identity_root, &aggregate.host_identity_root)?;
        let (peer, workspace_id) = restore(&aggregate, &signed, &identity)?;
        // Re-establish the barrier even after an interrupted rename, before any publication.
        store.commit(&encode(&aggregate, &identity)?)?;
        let mut owner = Self {
            store,
            identity,
            aggregate,
            peer,
            workspace_id,
            live_publication_transition: None,
        };
        let mut candidate = owner.peer.clone();
        let desktop_changed =
            crate::seed::bootstrap_desktop(&mut candidate, &owner.identity.native)
                .map_err(anyhow::Error::msg)?;
        // Older complete aggregates may have no publication queue. Rebuild that
        // recovery basis once at open; ordinary reconnects reuse exact evidence.
        if (desktop_changed || owner.aggregate.outgoing.is_empty())
            && candidate.incomplete_reason().is_none()
        {
            let messages = candidate
                .durable_bootstrap_messages(
                    &owner.identity.native,
                    SystemTime::now(),
                    owner
                        .aggregate
                        .generation
                        .checked_add(1)
                        .context("session generation exhausted")?,
                )
                .map_err(anyhow::Error::msg)?;
            let mut next = owner.aggregate.clone();
            owner.replace_publication(&mut next, &messages)?;
            owner.commit(next, candidate)?;
        }
        Ok(owner)
    }
    pub fn permits_publication(&self) -> bool {
        !self.store.is_poisoned() && self.peer.incomplete_reason().is_none()
    }
    pub fn root(&self) -> &Path {
        self.store.root()
    }
    pub fn peer(&self) -> &StatePeer {
        &self.peer
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
            outgoing: self.aggregate.outgoing.len(),
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
    pub fn pending(&self) -> &[PendingOperation] {
        &self.aggregate.pending
    }
    /// Complete committed recovery baseline plus tail, not a disposable live queue.
    /// Consumers must use the paired retention kinds and a transient send cursor.
    pub fn outgoing(&self) -> &[Vec<u8>] {
        &self.aggregate.outgoing
    }
    /// Exact retention metadata paired with `outgoing()`. The complete prefix is
    /// followed by ordinary room deltas and touched-record snapshots.
    pub fn outgoing_kinds(&self) -> &[Retention] {
        &self.aggregate.publication.kinds
    }
    /// (replacement generation, complete-prefix length). Replacement resets a
    /// consumer cursor; neither this value nor enqueue is a durable acknowledgement.
    pub fn publication_baseline(&self) -> (u64, usize) {
        (
            self.aggregate.publication.baseline_generation,
            self.aggregate.publication.baseline_len,
        )
    }
    pub(crate) fn publication_revision(&self) -> PublicationRevision {
        PublicationRevision {
            baseline_generation: self.aggregate.publication.baseline_generation,
            baseline_len: self.aggregate.publication.baseline_len,
            outgoing_len: self.aggregate.outgoing.len(),
            outgoing_bytes: self.aggregate.outgoing.iter().map(Vec::len).sum(),
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
    fn replace_publication(&self, next: &mut Aggregate, messages: &[NeighbourMsg]) -> Result<()> {
        next.outgoing = messages
            .iter()
            .map(postcard::to_stdvec)
            .collect::<Result<Vec<_>, _>>()?;
        next.publication = PublicationState::checkpoint(
            next.outgoing.len(),
            self.aggregate
                .generation
                .checked_add(1)
                .context("session generation exhausted")?,
        );
        Ok(())
    }
    fn append_mutation_publication(
        &self,
        next: &mut Aggregate,
        candidate: &mut StatePeer,
    ) -> Result<Option<PendingLivePublicationTransition>> {
        let generation = self
            .aggregate
            .generation
            .checked_add(1)
            .context("session generation exhausted")?;
        // Keep the unprepared candidate for compaction: a second receipt at this
        // same generation must never replace one prepared by the selective path.
        let mut selective = candidate.clone();
        let messages = selective
            .durable_mutation_messages(
                &self.peer,
                &self.identity.native,
                publication_time(),
                generation,
            )
            .map_err(anyhow::Error::msg)?;
        let encoded = messages
            .iter()
            .map(postcard::to_stdvec)
            .collect::<Result<Vec<_>, _>>()?;
        let tail = &next.outgoing[next.publication.baseline_len..];
        if tail.len().saturating_add(encoded.len()) >= MAX_PUBLICATION_TAIL_ENTRIES
            || tail.iter().chain(&encoded).map(Vec::len).sum::<usize>()
                >= MAX_PUBLICATION_TAIL_BYTES
        {
            let before = self.publication_revision();
            let before_generation = self.aggregate.generation;
            let mut before_room = self
                .peer
                .admitted_room()
                .ok_or_else(|| anyhow::anyhow!("missing prior room governance"))?
                .clone();
            let before_room_heads = before_room.get_heads();
            let checkpoint = candidate
                .durable_bootstrap_messages(&self.identity.native, publication_time(), generation)
                .map_err(anyhow::Error::msg)?;
            let mut room = candidate
                .admitted_room()
                .ok_or_else(|| anyhow::anyhow!("missing compacted room governance"))?
                .clone();
            let changes = room
                .get_changes(&before_room_heads)
                .into_iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect::<Vec<_>>();
            let mut live = Vec::with_capacity(checkpoint.len());
            live.push((
                postcard::to_stdvec(&NeighbourMsg::SyncResp {
                    changes,
                    room: candidate.room().to_owned(),
                })?,
                Retention::Delta,
            ));
            self.replace_publication(next, &checkpoint)?;
            live.extend(
                next.outgoing
                    .iter()
                    .skip(1)
                    .cloned()
                    .map(|bytes| (bytes, Retention::Checkpoint)),
            );
            let live_bytes = live.iter().map(|(bytes, _)| bytes.len()).sum::<usize>();
            if live.len() <= MAX_PUBLICATION_TAIL_ENTRIES
                && live_bytes <= MAX_PUBLICATION_TAIL_BYTES
            {
                return Ok(Some(PendingLivePublicationTransition {
                    before,
                    before_generation,
                    messages: live,
                }));
            }
        } else {
            for message in &messages {
                let (_, kind) =
                    retention(message, false).context("unsupported ordinary publication")?;
                next.publication.kinds.push(kind);
            }
            next.outgoing.extend(encoded);
            *candidate = selective;
        }
        Ok(None)
    }
    fn commit(&mut self, mut aggregate: Aggregate, candidate: StatePeer) -> Result<CommitReceipt> {
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
        let bytes = encode_with_timing(&aggregate, &self.identity, Some(&timing))?;
        timing.mark("store_commit_start");
        self.store.commit_timed(&bytes, &timing)?;
        timing.mark("store_commit_end");
        timing.mark("receipt_hash_start");
        let aggregate_hash = blake3::hash(&bytes).to_hex().to_string();
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
        }));
    }
    pub fn flush(&mut self) -> Result<CommitReceipt> {
        self.live_publication_transition = None;
        let bytes = encode(&self.aggregate, &self.identity)?;
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
        let mut candidate = self.peer.clone();
        let messages = candidate
            .durable_bootstrap_messages(
                &self.identity.native,
                SystemTime::now(),
                self.aggregate
                    .generation
                    .checked_add(1)
                    .context("session generation exhausted")?,
            )
            .map_err(anyhow::Error::msg)?;
        let mut next = self.aggregate.clone();
        self.replace_publication(&mut next, &messages)?;
        self.commit(next, candidate)
    }
    pub fn receive(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
    ) -> Result<Option<CommitReceipt>> {
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
        let mut next = self.aggregate.clone();
        if candidate.incomplete_reason().is_some() {
            // Authenticated policy metadata is durable even when decryption prerequisites
            // are unavailable. Never sign a plaintext checkpoint over blocked state.
            next.outgoing.clear();
            next.publication = PublicationState::checkpoint(
                0,
                self.aggregate
                    .generation
                    .checked_add(1)
                    .context("session generation exhausted")?,
            );
            return self.commit(next, candidate).map(Some);
        }
        let transition = if request {
            let messages = candidate
                .durable_bootstrap_messages(
                    &self.identity.native,
                    SystemTime::now(),
                    self.aggregate
                        .generation
                        .checked_add(1)
                        .context("session generation exhausted")?,
                )
                .map_err(anyhow::Error::msg)?;
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
        if candidate.room() != self.peer.room() || candidate.authority() != self.peer.authority() {
            bail!("admission cannot replace the session namespace");
        }
        self.validate_admission(&candidate, &directory)?;
        let messages = candidate
            .durable_bootstrap_messages(
                &self.identity.native,
                SystemTime::now(),
                self.aggregate
                    .generation
                    .checked_add(1)
                    .context("session generation exhausted")?,
            )
            .map_err(anyhow::Error::msg)?;
        let mut next = self.aggregate.clone();
        next.directory = directory;
        self.replace_publication(&mut next, &messages)?;
        self.commit(next, candidate)
    }
    /// Ordinary local commit: retain the recovery baseline and append only changed routes.
    pub fn commit_mutation_candidate(
        &mut self,
        mut candidate: StatePeer,
        directory: Vec<u8>,
    ) -> Result<CommitReceipt> {
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
        timing.mark("mutation_publication_start");
        let transition = self.append_mutation_publication(&mut next, &mut candidate)?;
        timing.mark("mutation_publication_end");
        timing.mark("aggregate_persist_start");
        let result = self.commit(next, candidate);
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
        if candidate.room() != self.peer.room() || candidate.authority() != self.peer.authority() {
            bail!("enrollment cannot replace native namespace");
        }
        self.validate_admission(&candidate, &directory)?;
        let messages = candidate
            .durable_bootstrap_messages(
                &self.identity.native,
                SystemTime::now(),
                self.aggregate
                    .generation
                    .checked_add(1)
                    .context("session generation exhausted")?,
            )
            .map_err(anyhow::Error::msg)?;
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
        let initial = owner.outgoing().to_vec();
        let stale_snapshot = initial[1].clone();
        let mut transitioned = consumer(&owner, initial_at);
        for encoded in &initial {
            receive_encoded(&mut transitioned, source, encoded, initial_at);
        }
        let mut sent = initial.len();
        for value in 0..31 {
            commit_small_changed_record(&mut owner, value);
            for encoded in &owner.outgoing()[sent..] {
                receive_encoded(&mut transitioned, source, encoded, initial_at);
            }
            sent = owner.outgoing().len();
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
            owner.outgoing().iter().skip(1).collect::<Vec<_>>(),
            "the transition and durable basis carry the exact same fresh snapshot wire frames"
        );
        for (encoded, _) in transition.messages() {
            receive_encoded(&mut transitioned, source, encoded, delayed);
        }

        let mut full = consumer(&owner, delayed);
        for encoded in owner.outgoing() {
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
        owner.aggregate.outgoing = messages
            .iter()
            .map(postcard::to_stdvec)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        owner.aggregate.publication =
            PublicationState::checkpoint(owner.aggregate.outgoing.len(), generation);
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
        owner.aggregate.outgoing.clear();
        owner.aggregate.publication = PublicationState::checkpoint(0, generation);
        owner
            .store
            .commit(&encode(&owner.aggregate, &owner.identity).unwrap())
            .unwrap();
        drop(owner);

        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.status().generation, generation + 1);
        assert!(!owner.outgoing().is_empty());
        assert!(
            owner
                .outgoing_kinds()
                .iter()
                .all(|kind| *kind == Retention::Checkpoint)
        );
        let outgoing = owner.outgoing().to_vec();
        let kinds = owner.outgoing_kinds().to_vec();
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
        assert_eq!(owner.outgoing(), outgoing);
        assert_eq!(owner.outgoing_kinds(), kinds);
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
        assert!(owner.outgoing().is_empty());
        let generation = owner.status().generation;
        drop(owner);

        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.status().generation, generation);
        assert!(owner.peer().incomplete_reason().is_some());
        assert!(!owner.permits_publication());
        assert!(owner.outgoing().is_empty());
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
            outgoing: old.outgoing.clone(),
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
        assert_eq!(owner.outgoing(), legacy.outgoing);
        assert_eq!(owner.peer().durable().unwrap(), legacy.peer);
        assert_eq!(owner.pending(), legacy.pending);
        assert!(
            owner
                .outgoing_kinds()
                .iter()
                .all(|kind| *kind == Retention::Checkpoint)
        );
        let migrated: SignedAggregate =
            postcard::from_bytes(&std::fs::read(path.join("session.bin")).unwrap()).unwrap();
        assert_eq!(
            postcard::take_from_bytes::<u32>(&migrated.payload)
                .unwrap()
                .0,
            3
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
            outgoing: old.outgoing.clone(),
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
        assert_eq!(version, 3);
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
        owner.aggregate.outgoing = legacy
            .durable_bootstrap_messages(&owner.identity.native, now, owner.aggregate.generation)
            .unwrap()
            .iter()
            .map(|m| postcard::to_stdvec(m).unwrap())
            .collect();
        owner.aggregate.publication = PublicationState::checkpoint(
            owner.aggregate.outgoing.len(),
            owner.aggregate.generation,
        );
        let old_delta = postcard::to_stdvec(&NeighbourMsg::SyncResp {
            changes: Vec::new(),
            room: room.clone(),
        })
        .unwrap();
        owner.aggregate.outgoing.push(old_delta.clone());
        owner.aggregate.publication.kinds.push(Retention::Delta);
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
        assert!(!owner.outgoing().contains(&old_delta));
        assert!(
            owner
                .outgoing_kinds()
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
            owner
                .outgoing()
                .iter()
                .cloned()
                .zip(owner.outgoing_kinds().iter().copied())
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
