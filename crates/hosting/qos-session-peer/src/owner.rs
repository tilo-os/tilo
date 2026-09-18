//! Serialized native owner. Every returned mutation receipt follows the durable barrier.
use crate::{
    identity::{self, Identity},
    store::SessionStore,
};
use anyhow::{Context, Result, bail};
use ed25519_dalek::{Signature, Signer, Verifier};
use qos_peer_state::{DocumentRow, NeighbourMsg, StatePeer, TransportOrigin};
use qos_user_identity::{DeviceCert, RevocationList, WorkspaceDirectory};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use web_time::SystemTime;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOperation {
    pub invocation_id: String,
    pub encoded_request: Vec<u8>,
    /// Retained for reconciliation, never automatically executed by this owner.
    pub outcome_unknown: bool,
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
pub struct SessionOwner {
    store: SessionStore,
    pub(crate) identity: Identity,
    aggregate: Aggregate,
    peer: StatePeer,
}
fn encode(aggregate: &Aggregate, identity: &Identity) -> Result<Vec<u8>> {
    let payload = postcard::to_stdvec(aggregate)?;
    let digest = blake3::Hasher::new_derive_key("qOS native session aggregate v1")
        .update(&payload)
        .finalize();
    let signature = identity.native.sign(digest.as_bytes()).to_bytes().to_vec();
    Ok(postcard::to_stdvec(&SignedAggregate {
        payload,
        signature,
    })?)
}
fn decode(bytes: &[u8]) -> Result<(Aggregate, SignedAggregate)> {
    if bytes.len() > crate::store::MAX_AGGREGATE_BYTES {
        bail!("aggregate exceeds the local storage size limit");
    }
    let signed: SignedAggregate = postcard::from_bytes(bytes)
        .context("corrupt session aggregate; preserve it for recovery")?;
    let aggregate: Aggregate = postcard::from_bytes(&signed.payload)?;
    if aggregate.version != 1 {
        bail!("unsupported aggregate version; explicit migration required");
    }
    Ok((aggregate, signed))
}
fn restore(
    aggregate: &Aggregate,
    signed: &SignedAggregate,
    identity: &Identity,
) -> Result<StatePeer> {
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
    WorkspaceDirectory::load(
        identity.root.verifying_key().to_bytes(),
        identity.native_cert.device_public_key,
        &aggregate.directory,
    )?;
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
    for bytes in &aggregate.outgoing {
        let _: NeighbourMsg = postcard::from_bytes(bytes)?;
    }
    Ok(peer)
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
        let outgoing = peer
            .bootstrap_messages(&identity.native, now)
            .map_err(anyhow::Error::msg)?
            .iter()
            .map(postcard::to_stdvec)
            .collect::<Result<Vec<_>, _>>()?;
        let workspace = qos_relay::room_workspace_id(&room)?;
        let mut directory = WorkspaceDirectory::new(
            identity.root.verifying_key().to_bytes(),
            identity.native_cert.device_public_key,
        );
        directory.add_workspace(workspace, "Shared qOS workspace")?;
        directory.set_member_devices(workspace, &[identity.native_cert.device_public_key])?;
        directory.set_host_devices(workspace, &[identity.host_cert.device_public_key])?;
        let aggregate = Aggregate {
            version: 1,
            identity_root,
            host_identity_root,
            room,
            authority: identity.native_cert.clone(),
            directory: directory.save(),
            peer: peer.durable().map_err(anyhow::Error::msg)?,
            outgoing,
            pending: vec![],
            generation: 1,
        };
        store.commit(&encode(&aggregate, &identity)?)?;
        Ok(Self {
            store,
            identity,
            aggregate,
            peer,
        })
    }
    pub fn open(root: &Path) -> Result<Self> {
        let mut store = SessionStore::open(root)?;
        let bytes = store.read()?;
        let (aggregate, signed) = decode(&bytes)?;
        let identity = identity::load(&aggregate.identity_root, &aggregate.host_identity_root)?;
        let peer = restore(&aggregate, &signed, &identity)?;
        // Re-establish the barrier even after an interrupted rename, before any publication.
        store.commit(&bytes)?;
        Ok(Self {
            store,
            identity,
            aggregate,
            peer,
        })
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
    pub fn pending(&self) -> &[PendingOperation] {
        &self.aggregate.pending
    }
    pub fn outgoing(&self) -> &[Vec<u8>] {
        &self.aggregate.outgoing
    }
    fn commit(&mut self, mut aggregate: Aggregate, candidate: StatePeer) -> Result<CommitReceipt> {
        aggregate.peer = candidate.durable().map_err(anyhow::Error::msg)?;
        aggregate.generation = self
            .aggregate
            .generation
            .checked_add(1)
            .context("session generation exhausted")?;
        let bytes = encode(&aggregate, &self.identity)?;
        self.store.commit(&bytes)?;
        let receipt = CommitReceipt {
            generation: aggregate.generation,
            aggregate_hash: blake3::hash(&bytes).to_hex().to_string(),
            records: candidate.rows(),
            replay_floors: candidate.replay_floors(),
        };
        self.aggregate = aggregate;
        self.peer = candidate;
        Ok(receipt)
    }
    pub fn flush(&mut self) -> Result<CommitReceipt> {
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
            .bootstrap_messages(&self.identity.native, SystemTime::now())
            .map_err(anyhow::Error::msg)?;
        let mut next = self.aggregate.clone();
        next.outgoing = messages
            .iter()
            .map(postcard::to_stdvec)
            .collect::<Result<Vec<_>, _>>()?;
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
            return Ok(None);
        }
        let mut next = self.aggregate.clone();
        if candidate.incomplete_reason().is_some() {
            // Authenticated policy metadata is durable even when decryption prerequisites
            // are unavailable. Never sign a plaintext checkpoint over blocked state.
            next.outgoing.clear();
            return self.commit(next, candidate).map(Some);
        }
        let messages = candidate
            .bootstrap_messages(&self.identity.native, SystemTime::now())
            .map_err(anyhow::Error::msg)?;
        // All entries here are authoritative complete checkpoints; newer committed checkpoints supersede older ones.
        next.outgoing = messages
            .iter()
            .map(postcard::to_stdvec)
            .collect::<Result<Vec<_>, _>>()?;
        self.commit(next, candidate).map(Some)
    }
    /// Local in-process admission integration: caller supplies a canonically validated candidate.
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
            .bootstrap_messages(&self.identity.native, SystemTime::now())
            .map_err(anyhow::Error::msg)?;
        let mut next = self.aggregate.clone();
        next.directory = directory;
        next.outgoing = messages
            .iter()
            .map(postcard::to_stdvec)
            .collect::<Result<Vec<_>, _>>()?;
        self.commit(next, candidate)
    }
    fn validate_admission(&self, candidate: &StatePeer, directory: &[u8]) -> Result<()> {
        fn preserves_document(before: &[u8], after: &[u8]) -> Result<()> {
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
        let workspace = directory
            .as_map()
            .remove(&qos_relay::room_workspace_id(self.peer.room())?)
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
        let peer = restore(&aggregate, &signed, &identity)?;
        aggregate.identity_root = std::fs::canonicalize(identity_root)?;
        aggregate.host_identity_root = std::fs::canonicalize(host_identity_root)?;
        let mut store = SessionStore::create(destination)?;
        store.commit(&encode(&aggregate, &identity)?)?;
        Ok(Self {
            store,
            identity,
            aggregate,
            peer,
        })
    }
}
