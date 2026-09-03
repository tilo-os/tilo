//! The sync plane: namespace replication plus content hydration.
//!
//! Unenforced workspaces use iroh-gossip for live Automerge deltas and
//! `qos-fs-sync/0` for bootstrap/anti-entropy. Enforced workspaces use only
//! the authenticated sync ALPN: gossip does not preserve the original author
//! needed for a write-authorisation decision. Iroh-blobs hydrates content on
//! read, guarded by the live namespace and ACL.
//!
//! A joiner must *load the host's document* so both share the same
//! Automerge object ids — independently-created docs would fork on
//! merge. The bootstrap exchange guarantees that shared history.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use futures_lite::StreamExt;
use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::{Connection, Endpoint, RecvStream, SendStream, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh_base::EndpointAddr;
use iroh_blobs::Hash;
use iroh_gossip::api::{Event, GossipReceiver, GossipSender};
use iroh_gossip::net::{GOSSIP_ALPN, Gossip};
use iroh_gossip::proto::TopicId;
use qos_user_identity::RevocationList;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::acl::{AclHandle, IdentityKey};
use crate::authenticated_transport::{AuthenticatedTransportConfig, authenticate_fs_peer_parts_at};
use crate::hydration::Hydrator;
use crate::identity::PeerCredentials;
use crate::namespace::Namespace;
use crate::peer_auth::AuthenticatedFsPeer;
use crate::record_access::RecordAccessRegistry;
use crate::record_mesh::{
    MAX_RECORD_MESH_INVENTORY_PAGE_STREAMS, MAX_RECORD_MESH_PULL_BATCH_ENVELOPES,
    QOS_FS_RECORD_MESH_ALPN, RECORD_MESH_PROTOCOL_VERSION, RecordMeshEndV1, RecordMeshOperationV1,
    RecordMeshRequestV1, RecordMeshResultV1, RecordMeshStreamV1, read_record_mesh_request,
    read_record_mesh_response, serve_authorized_request, write_record_mesh_request,
    write_record_mesh_response,
};
use crate::record_protocol::{QOS_FS_RECORD_ALPN, RecordProtocol};
use crate::record_replication::{RecordAction, RecordAuthorizer};
use crate::record_repository::{RecordRepository, RecordRepositoryKey};
use crate::replica_ack::{
    QOS_FS_REPLICA_ACK_ALPN, REPLICA_ACK_TIMEOUT, ReplicaAckRequestV1, ReplicaAckResponseV1,
    ReplicaAckV1, read_frame as read_replica_ack_frame, write_frame as write_replica_ack_frame,
};
use crate::share_authority::ShareAuthority;
use crate::share_session::LiveActorCertification;
use crate::store::ContentStore;
use crate::workspace_authority::{
    AuthorityRequest, AuthorityResponse, QOS_FS_AUTHORITY_ALPN, WorkspaceAuthority,
    read_frame as read_authority_frame, write_frame as write_authority_frame,
};

/// Custom ALPN for the document exchange (bootstrap + incremental sync).
pub const QOS_FS_SYNC_ALPN: &[u8] = b"qos-fs-sync/0";
/// Interactive peers always retain small previews when metadata changes, but
/// never fetch a file manifest/chunks merely to populate a directory view.
const MAX_INTERACTIVE_THUMBNAIL_BYTES: u64 = 32 * 1024 * 1024;

/// Certified local identity and live revocation state required by every
/// workspace-sync connection.
#[derive(Clone)]
pub struct FsTransportIdentity {
    credentials: PeerCredentials,
    revocations: Arc<RwLock<RevocationList>>,
    gate: crate::transport_gate::TransportGate,
    certified_peers: Arc<RwLock<std::collections::HashMap<IdentityKey, AuthenticatedFsPeer>>>,
    authenticated_peer_addrs: Arc<RwLock<std::collections::HashMap<IdentityKey, EndpointAddr>>>,
}

impl FsTransportIdentity {
    /// Create the authentication context installed on both outbound and
    /// inbound workspace transports.
    pub fn new(credentials: PeerCredentials, revocations: Arc<RwLock<RevocationList>>) -> Self {
        Self {
            credentials,
            revocations,
            gate: crate::transport_gate::TransportGate::default(),
            certified_peers: Arc::new(RwLock::new(std::collections::HashMap::new())),
            authenticated_peer_addrs: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    #[must_use]
    pub fn with_transport_gate(mut self, gate: crate::transport_gate::TransportGate) -> Self {
        self.gate = gate;
        self
    }

    fn partitioned(&self) -> bool {
        self.gate.partitioned()
    }

    fn remember_peer(&self, peer: &AuthenticatedFsPeer, addr: EndpointAddr) {
        if self.certification_is_current(peer, unix_now_ns()) {
            if let Ok(mut peers) = self.certified_peers.write() {
                peers.insert(peer.device_public_key, peer.clone());
            }
            if let Ok(mut addrs) = self.authenticated_peer_addrs.write() {
                addrs.insert(peer.device_public_key, addr);
            }
        }
    }

    fn authenticated_peer_addrs(&self) -> Vec<EndpointAddr> {
        self.authenticated_peer_addrs
            .read()
            .map(|addrs| addrs.values().cloned().collect())
            .unwrap_or_default()
    }

    fn certified_peer_for_device(&self, device: IdentityKey) -> Option<AuthenticatedFsPeer> {
        let peer = self.certified_peers.read().ok()?.get(&device)?.clone();
        self.certification_is_current(&peer, unix_now_ns())
            .then_some(peer)
    }

    fn certified_person_for_device(&self, device: IdentityKey) -> Option<IdentityKey> {
        let peer = self.certified_peers.read().ok()?.get(&device)?.clone();
        self.certification_is_current(&peer, unix_now_ns())
            .then_some(peer.user_public_key)
    }

    /// Certified human root represented by this mount.
    pub fn user_public_key(&self) -> IdentityKey {
        self.credentials.user_public_key()
    }

    /// Certified device key, also used as the Iroh endpoint identity.
    pub fn device_public_key(&self) -> IdentityKey {
        self.credentials.device_public_key()
    }

    pub(crate) fn credentials(&self) -> PeerCredentials {
        self.credentials.clone()
    }

    pub(crate) fn revocations(&self) -> Arc<RwLock<RevocationList>> {
        Arc::clone(&self.revocations)
    }

    fn revocation_snapshot(&self) -> Result<RevocationList> {
        self.revocations
            .read()
            .map(|list| list.clone())
            .map_err(|_| anyhow::anyhow!("peer revocation state is poisoned"))
    }
}

impl crate::share_session::LiveActorCertification for FsTransportIdentity {
    #[allow(clippy::suspicious_operation_groupings)]
    fn certification_is_current(&self, peer: &AuthenticatedFsPeer, now_ns: u64) -> bool {
        if peer.user_public_key != peer.cert.user_public_key
            || peer.device_public_key != peer.cert.device_public_key
            || peer.cert.issued_at_ns > now_ns
            || qos_user_identity::verify_device_cert_at(&peer.cert, now_ns).is_err()
        {
            return false;
        }
        let Ok(revocations) = self.revocations.read() else {
            return false;
        };
        !revocations.iter().any(|entry| {
            (entry.user_public_key == peer.user_public_key)
                && (entry.revoked_device_public_key == peer.device_public_key)
                && entry.revoked_at_ns <= now_ns
        })
    }
}

fn unix_now_ns() -> u64 {
    crate::clock::unix_now_ns()
}

async fn authenticate_connection(
    endpoint: &Endpoint,
    connection: &Connection,
    recv: &mut RecvStream,
    send: &mut SendStream,
    identity: &FsTransportIdentity,
) -> Result<AuthenticatedFsPeer> {
    let remote_id = connection.remote_id();
    let remote = *remote_id.as_bytes();
    let revocations = identity.revocation_snapshot()?;
    let peer = authenticate_fs_peer_parts_at(
        recv,
        send,
        &identity.credentials,
        *endpoint.id().as_bytes(),
        remote,
        &revocations,
        unix_now_ns(),
        AuthenticatedTransportConfig::default(),
    )
    .await?;
    identity.remember_peer(&peer, EndpointAddr::new(remote_id));
    Ok(peer)
}

/// Mode byte: transfer the full document (first-join bootstrap).
const MODE_FULL: u8 = b'F';
/// Mode byte: incremental Automerge sync-protocol exchange (anti-entropy).
const MODE_SYNC: u8 = b'S';
/// Hard cap on a single sync frame (`DoS` guard).
const MAX_FRAME: usize = 64 * 1024 * 1024;

async fn write_frame(send: &mut SendStream, data: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(data.len())
        .map_err(|_| std::io::Error::other("sync frame too large to length-prefix"))?;
    send.write_all(&len.to_le_bytes())
        .await
        .map_err(std::io::Error::other)?;
    if !data.is_empty() {
        send.write_all(data).await.map_err(std::io::Error::other)?;
    }
    Ok(())
}

async fn read_frame(recv: &mut RecvStream) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len)
        .await
        .map_err(std::io::Error::other)?;
    let n = u32::from_le_bytes(len) as usize;
    if n == 0 {
        return Ok(Vec::new());
    }
    if n > MAX_FRAME {
        return Err(std::io::Error::other(format!("sync frame too large: {n}")));
    }
    let mut buf = vec![0u8; n];
    recv.read_exact(&mut buf)
        .await
        .map_err(std::io::Error::other)?;
    Ok(buf)
}

/// A shareable invitation to join a workspace: how to reach a peer + which
/// gossip topic the workspace uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub node: EndpointAddr,
    pub workspace: [u8; 32],
}

impl Ticket {
    /// # Errors
    ///
    /// Returns an error if postcard serialization fails.
    pub fn encode(&self) -> Result<String> {
        Ok(hex::encode(postcard::to_allocvec(self)?))
    }

    /// # Errors
    ///
    /// Returns an error if `s` is not valid hex, or the decoded bytes are not a valid postcard-encoded [`Ticket`].
    pub fn decode(s: &str) -> Result<Self> {
        let raw = hex::decode(s.trim())?;
        Ok(postcard::from_bytes(&raw)?)
    }

    /// Stable public identifier used by record-scoped grants and repositories.
    ///
    /// Older workspace tickets predate record sharing and carry only the
    /// random 256-bit topic. Deriving the UUID keeps those tickets compatible
    /// while ensuring every participant computes the same record namespace.
    pub fn workspace_id(&self) -> uuid::Uuid {
        Self::workspace_id_for_topic(self.workspace)
    }

    pub fn workspace_id_for_topic(topic: [u8; 32]) -> uuid::Uuid {
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&topic[..16]);
        uuid::Uuid::from_bytes(bytes)
    }
}

/// Bind an endpoint under this device's own key, with an in-memory
/// address lookup (so gossip can dial peers we learn about by id).
/// Returns both so callers can register peer addresses.
///
/// The `NodeId` this produces is the device's Ed25519 public key —
/// iroh's secret key and the device key are the same 32 bytes, which is
/// what makes the node id a *name* that still means the same thing after
/// a restart.
///
/// # Errors
///
/// Returns an error if the endpoint fails to bind.
pub async fn bind_endpoint_with_key(secret: iroh::SecretKey) -> Result<(Endpoint, MemoryLookup)> {
    let lookup = MemoryLookup::new();
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(secret)
        .address_lookup(lookup.clone())
        .bind()
        .await?;
    Ok((endpoint, lookup))
}

/// Bind an endpoint under a freshly-generated key. **The node id differs
/// on every call**, so nothing durable may be keyed on it — for tests
/// and throwaway probes only; a real mount uses
/// [`bind_endpoint_with_key`] so its identity is the device's.
///
/// # Errors
///
/// Returns an error if the endpoint fails to bind.
pub async fn bind_ephemeral_endpoint() -> Result<(Endpoint, MemoryLookup)> {
    bind_endpoint_with_key(iroh::SecretKey::generate()).await
}

/// Fetch the full namespace document from a peer over the bootstrap
/// ALPN, bounded by a timeout so a stalled host can't hang the caller.
///
/// # Errors
///
/// Returns an error when the partition gate is closed or connection,
/// authentication, framing, or transport fails.
pub async fn fetch_remote_doc(
    endpoint: &Endpoint,
    host: EndpointAddr,
    identity: &FsTransportIdentity,
) -> Result<Vec<u8>> {
    if identity.partitioned() {
        anyhow::bail!("transport partition gate is closed");
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        let conn = endpoint.connect(host, QOS_FS_SYNC_ALPN).await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        let _peer =
            authenticate_connection(endpoint, &conn, &mut recv, &mut send, identity).await?;
        send.write_all(&[MODE_FULL]).await?;
        send.finish()?;
        // Documents are small (metadata only); 64 MiB is a generous ceiling.
        let doc = recv.read_to_end(MAX_FRAME).await?;
        if doc.is_empty() {
            anyhow::bail!("peer did not serve a workspace document");
        }
        Ok::<_, anyhow::Error>(doc)
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out fetching workspace document"))?
}

/// Run the incremental Automerge sync protocol against a peer, applying
/// missing changes into `namespace` — periodic anti-entropy exchanging
/// only the delta, not the whole document.
///
/// # Errors
///
/// Returns an error if the connection, exchange, or a frame read/write fails, or the peer doesn't respond within the timeout.
pub async fn sync_with_peer(
    endpoint: &Endpoint,
    peer: EndpointAddr,
    namespace: &Arc<Mutex<Namespace>>,
    identity: &FsTransportIdentity,
) -> Result<bool> {
    if identity.partitioned() {
        anyhow::bail!("transport partition gate is closed");
    }
    tokio::time::timeout(Duration::from_secs(15), async {
        let conn = endpoint.connect(peer, QOS_FS_SYNC_ALPN).await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        let _peer =
            authenticate_connection(endpoint, &conn, &mut recv, &mut send, identity).await?;
        send.write_all(&[MODE_SYNC]).await?;
        let mut state = automerge::sync::State::new();
        // Initiator: write-then-read each round; the responder mirrors.
        let mut received_change = false;
        for _ in 0..200 {
            let msg = {
                namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .sync_generate(&mut state)
            };
            write_frame(&mut send, msg.as_deref().unwrap_or(&[])).await?;
            let peer_msg = read_frame(&mut recv).await?;
            if !peer_msg.is_empty() {
                received_change = true;
                let _ = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .sync_receive(&mut state, &peer_msg);
            }
            if msg.is_none() && peer_msg.is_empty() {
                break;
            }
        }
        let _ = send.finish();
        Ok::<bool, anyhow::Error>(received_change)
    })
    .await
    .map_err(|_| anyhow::anyhow!("sync exchange timed out"))?
}

/// Fetch only preview blobs newly referenced by the namespace. This makes a
/// normal desktop immediately useful offline for browsing while keeping
/// manifests and file bytes strictly on-demand. Callers invoke it only after
/// a successful namespace/record change, never on the reconciliation timer.
async fn prefetch_namespace_thumbnails(
    namespace: &Arc<Mutex<Namespace>>,
    hydrator: &Hydrator,
    pending: &mut HashMap<Hash, (u8, tokio::time::Instant)>,
    refresh_roots: bool,
) -> Result<()> {
    if refresh_roots {
        let thumbnails = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_record_blob_roots(crate::namespace::MAX_LIVE_RECORD_BLOB_SCAN)?
            .into_iter()
            .filter_map(|roots| roots.thumbnail)
            .collect::<HashSet<Hash>>();
        pending.retain(|hash, _| thumbnails.contains(hash));
        let now = tokio::time::Instant::now();
        for thumbnail in thumbnails {
            pending.entry(thumbnail).or_insert((0, now));
        }
    }
    let now = tokio::time::Instant::now();
    let due = pending
        .iter()
        .filter_map(|(hash, (_, retry_at))| (*retry_at <= now).then_some(*hash))
        .collect::<Vec<_>>();
    for thumbnail in due {
        if hydrator.store.has(thumbnail).await {
            pending.remove(&thumbnail);
            continue;
        }
        let result = hydrator
            .read_blob(
                thumbnail,
                0,
                MAX_INTERACTIVE_THUMBNAIL_BYTES.saturating_add(1),
            )
            .await
            .and_then(|bytes| {
                anyhow::ensure!(
                    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
                        <= MAX_INTERACTIVE_THUMBNAIL_BYTES,
                    "interactive thumbnail exceeds byte budget"
                );
                Ok(())
            });
        if result.is_ok() {
            pending.remove(&thumbnail);
            continue;
        }
        // A certificate/authority refresh or a temporarily offline owner must
        // not make the rest of the preview queue fail. Retry each pending
        // thumbnail independently with a capped 3/6/12/24/30 second backoff.
        if let Some((attempts, retry_at)) = pending.get_mut(&thumbnail) {
            *attempts = attempts.saturating_add(1).min(4);
            let delay = 3_u64.saturating_mul(1_u64 << u32::from(*attempts));
            *retry_at = now + Duration::from_secs(delay.min(30));
        }
    }
    Ok(())
}

fn namespace_generation(namespace: &Arc<Mutex<Namespace>>) -> [u8; 32] {
    let bytes = {
        let mut namespace = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        namespace.save()
    };
    *blake3::hash(&bytes).as_bytes()
}

/// The policy the network planes serve through, or `None` for an
/// unenforced mount — same opt-out as [`crate::QuickFS`]: a mount
/// without `--acl` states no policy, and inventing one here would deny a
/// single-user workspace its own devices. The handle is live, so a
/// revocation lands on the next connection, not the next remount.
pub type PeerPolicy = Option<AclHandle>;

/// Does `peer` hold a read grant on *anything* here? `None` policy ⇒
/// unenforced ⇒ yes; otherwise the ACL decides, `false` for anyone it
/// has never heard of.
fn peer_may_read_anything(policy: &PeerPolicy, peer: &IdentityKey) -> bool {
    match policy {
        None => true,
        Some(acl) => acl.reads_anything(peer),
    }
}

/// The monolithic namespace document names every live path, so it may only be
/// served to a peer that reads the workspace root. A subtree grant is useful
/// to FUSE and the blob plane, but cannot safely receive a filtered Automerge
/// change graph; per-share projection documents are required for that.
fn peer_may_read_whole_workspace(policy: &PeerPolicy, peer: &IdentityKey) -> bool {
    match policy {
        None => true,
        Some(acl) => acl.permits_peer(peer, "/", crate::acl::Access::Read),
    }
}

/// Receiving an Automerge sync message lets the caller contribute arbitrary
/// namespace changes. Until changes are individually authorised, only a peer
/// with write authority at the workspace root may use the bidirectional mode.
fn peer_may_write_whole_workspace(policy: &PeerPolicy, peer: &IdentityKey) -> bool {
    match policy {
        None => true,
        Some(acl) => {
            acl.permits_peer(peer, "/", crate::acl::Access::Read)
                && acl.permits_peer(peer, "/", crate::acl::Access::Write)
        }
    }
}

/// A named human grant applies to all currently certified devices; a
/// device-specific grant remains possible by naming the device key. The
/// transport has proved both identities, so authorization may safely accept
/// either without treating an endpoint key as if it were a person.
fn authenticated_peer_may_read_whole_workspace(
    policy: &PeerPolicy,
    peer: &AuthenticatedFsPeer,
) -> bool {
    peer_may_read_whole_workspace(policy, &peer.user_public_key)
        || peer_may_read_whole_workspace(policy, &peer.device_public_key)
}

fn authenticated_peer_may_write_whole_workspace(
    policy: &PeerPolicy,
    peer: &AuthenticatedFsPeer,
) -> bool {
    peer_may_write_whole_workspace(policy, &peer.user_public_key)
        || peer_may_write_whole_workspace(policy, &peer.device_public_key)
}

/// An iroh endpoint id read as a qOS identity — the same 32 bytes (see
/// [`bind_endpoint_with_key`]), letting a capability written against a
/// device key decide who may open a connection.
fn identity_of(id: iroh::EndpointId) -> IdentityKey {
    *id.as_bytes()
}

/// Serves the current namespace document to bootstrapping peers. Who may
/// fetch or contribute to it is an access-control decision, not a transport
/// detail. Because this is one monolithic Automerge document, subtree readers
/// cannot safely receive it and subtree writers cannot safely author it.
#[derive(Clone)]
struct SyncProtocol {
    endpoint: Endpoint,
    identity: FsTransportIdentity,
    namespace: Arc<Mutex<Namespace>>,
    policy: PeerPolicy,
}

impl std::fmt::Debug for SyncProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyncProtocol")
    }
}

impl ProtocolHandler for SyncProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if self.identity.partitioned() {
            connection.close(1u32.into(), b"partitioned");
            return Ok(());
        }
        let (mut send, mut recv) = connection.accept_bi().await?;
        // Bounded so a client that opens a stream but never sends/closes
        // can't leak an accept task or connection forever.
        let exchange = async {
            // This is deliberately the first application-layer exchange. No
            // mode byte or namespace data is accepted until the transport key
            // has been bound to a certified human/device and checked against
            // the current revocation snapshot.
            let peer = authenticate_connection(
                &self.endpoint,
                &connection,
                &mut recv,
                &mut send,
                &self.identity,
            )
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
            let mut mode = [0u8; 1];
            recv.read_exact(&mut mode)
                .await
                .map_err(AcceptError::from_err)?;
            match mode[0] {
                MODE_FULL => {
                    if !authenticated_peer_may_read_whole_workspace(&self.policy, &peer) {
                        let _ = send.finish();
                        return Ok(());
                    }
                    let _ = recv.read_to_end(64).await;
                    let doc = self
                        .namespace
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .save();
                    send.write_all(&doc).await.map_err(AcceptError::from_err)?;
                    let _ = send.finish();
                }
                MODE_SYNC => {
                    if !authenticated_peer_may_write_whole_workspace(&self.policy, &peer) {
                        let _ = send.finish();
                        return Ok(());
                    }
                    // Responder: read-then-write each round (mirrors the initiator).
                    let mut state = automerge::sync::State::new();
                    for _ in 0..200 {
                        // The authority handle is live. A newer signed
                        // tombstone/revocation cuts an already-connected
                        // writer off at the next protocol round.
                        if !authenticated_peer_may_write_whole_workspace(&self.policy, &peer) {
                            let _ = send.finish();
                            break;
                        }
                        let peer_msg =
                            read_frame(&mut recv).await.map_err(AcceptError::from_err)?;
                        if !peer_msg.is_empty() {
                            let _ = self
                                .namespace
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .sync_receive(&mut state, &peer_msg);
                        }
                        let msg = {
                            self.namespace
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .sync_generate(&mut state)
                        };
                        write_frame(&mut send, msg.as_deref().unwrap_or(&[]))
                            .await
                            .map_err(AcceptError::from_err)?;
                        if peer_msg.is_empty() && msg.is_none() {
                            break;
                        }
                    }
                }
                _ => {
                    let _ = send.finish();
                }
            }
            Ok::<(), AcceptError>(())
        };
        match tokio::time::timeout(Duration::from_secs(30), exchange).await {
            Ok(res) => res?,
            Err(_) => return Ok(()), // timed-out client; just drop the connection
        }
        connection.closed().await;
        Ok(())
    }
}

/// The connect-time verdict for a blob-plane caller, and the identity to
/// remember for requests riding the connection. `None` endpoint id means
/// the transport couldn't name the caller: refuse, since an unnamed
/// caller can't be checked.
#[cfg(test)]
fn admit_blob_caller(
    policy: &PeerPolicy,
    endpoint_id: Option<iroh::EndpointId>,
) -> Result<IdentityKey, iroh_blobs::provider::events::AbortReason> {
    admit_blob_caller_with_records(policy, endpoint_id, None)
}

fn admit_blob_caller_with_records(
    policy: &PeerPolicy,
    endpoint_id: Option<iroh::EndpointId>,
    records: Option<&RecordPlane>,
) -> Result<IdentityKey, iroh_blobs::provider::events::AbortReason> {
    use iroh_blobs::provider::events::AbortReason;

    let Some(id) = endpoint_id else {
        return Err(AbortReason::Permission);
    };
    let peer = identity_of(id);
    let has_record_session = records.is_some_and(|records| {
        !records
            .access_registry
            .readable_record_ids_for_device(
                records.workspace_id,
                collab_types::Ed25519PublicKey(peer),
            )
            .is_empty()
    });
    if peer_may_read_anything(policy, &peer) || has_record_session {
        Ok(peer)
    } else {
        Err(AbortReason::Permission)
    }
}

/// Does `peer` currently read at least one live path naming `hash` as file
/// content or thumbnail content? Hash knowledge alone is never authority.
#[cfg(test)]
async fn peer_may_read_blob(
    policy: &PeerPolicy,
    namespace: &Arc<Mutex<Namespace>>,
    store: &ContentStore,
    peer: &IdentityKey,
    hash: iroh_blobs::Hash,
) -> bool {
    peer_may_read_blob_with_records(policy, namespace, store, peer, None, hash, None).await
}

#[allow(clippy::significant_drop_tightening)]
async fn peer_may_read_blob_with_records(
    policy: &PeerPolicy,
    namespace: &Arc<Mutex<Namespace>>,
    store: &ContentStore,
    peer: &IdentityKey,
    certified_person: Option<IdentityKey>,
    hash: iroh_blobs::Hash,
    records: Option<&RecordPlane>,
) -> bool {
    if policy.is_none() && records.is_none() {
        return true;
    }

    let (directly_authorized, readable_manifests) = policy.as_ref().map_or_else(
        || (false, Vec::new()),
        |acl| {
            let ns = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let permits = |path: &str| {
                acl.permits_peer(peer, path, crate::acl::Access::Read)
                    || certified_person.is_some_and(|person| {
                        acl.permits_peer(&person, path, crate::acl::Access::Read)
                    })
            };
            let direct = ns
                .live_paths_for_blob(hash)
                .iter()
                .any(|path| permits(path));
            let manifests = ns
                .live_node_ids()
                .into_iter()
                .filter_map(|id| Some((ns.path_for_id(&id)?, ns.node_hash(&id)?)))
                .filter(|(path, _)| permits(path))
                .map(|(_, manifest)| manifest)
                .collect::<Vec<_>>();
            (direct, manifests)
        },
    );

    if directly_authorized {
        return true;
    }

    // File hashes address manifests; range hydration requests their chunks
    // individually. A chunk is readable only while a live, readable file's
    // manifest names it. A learned chunk hash is not a bearer capability.
    for manifest_hash in readable_manifests {
        let Some(bytes) = store.local(manifest_hash).await else {
            continue;
        };
        let Ok(manifest) = crate::content::Manifest::decode(&bytes) else {
            continue;
        };
        if manifest
            .chunks
            .iter()
            .any(|chunk| iroh_blobs::Hash::from(chunk.hash) == hash)
        {
            return true;
        }
    }

    let Some(records) = records else {
        return false;
    };
    let readable = records
        .access_registry
        .readable_record_ids_for_device(records.workspace_id, collab_types::Ed25519PublicKey(*peer))
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    if readable.is_empty() {
        return false;
    }
    let limits = crate::store::BlobRecordLookupLimits::default();
    let roots = {
        namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_record_blob_roots(limits.max_live_records)
    };
    let reachable = match roots {
        Ok(roots) => {
            store
                .live_record_ids_for_blob_roots(roots, hash, limits)
                .await
        }
        Err(error) => Err(error.into()),
    };
    reachable.is_ok_and(|records| records.into_iter().any(|record| readable.contains(&record)))
}

/// Build the request interceptor iroh-blobs consults before serving
/// anything, or `None` for an unenforced mount. `BlobsProtocol::new(store,
/// None)` serves every blob to anyone who knows its hash, and
/// [`EventSender`](iroh_blobs::provider::events::EventSender) is the
/// protocol's only interception point, so the identity check goes here —
/// nothing later still knows who is asking. Only the connect event names
/// the peer (a request event carries just a connection id), so identity
/// is recorded at connect and looked up per request. Pushes are refused
/// outright: this is a *read* plane, content arrives via the FUSE write
/// path where the write-side capability model is enforced.
#[allow(clippy::too_many_lines)]
fn blob_guard(
    policy: PeerPolicy,
    namespace: Arc<Mutex<Namespace>>,
    store: Arc<ContentStore>,
    records: Option<RecordPlane>,
    identity: Option<FsTransportIdentity>,
) -> Option<iroh_blobs::provider::events::EventSender> {
    use iroh_blobs::provider::events::{
        AbortReason, ConnectMode, EventMask, EventSender, ObserveMode, ProviderMessage,
        RequestMode, ThrottleMode,
    };

    if policy.is_none() && records.is_none() {
        return None;
    }
    let mask = EventMask {
        connected: ConnectMode::Intercept,
        get: RequestMode::Intercept,
        get_many: RequestMode::Intercept,
        push: RequestMode::Disabled,
        observe: ObserveMode::Intercept,
        throttle: ThrottleMode::None,
    };
    let (sender, mut rx) = EventSender::channel(64, mask);
    tokio::spawn(async move {
        let mut callers: std::collections::HashMap<u64, (IdentityKey, Option<IdentityKey>)> =
            std::collections::HashMap::new();
        // Re-resolve both the live namespace and live ACL for every request:
        // revocation, rename, and deleting the last readable reference all
        // take effect without ending the connection.
        while let Some(msg) = rx.recv().await {
            match msg {
                ProviderMessage::ClientConnected(msg) => {
                    let device = msg.inner.endpoint_id.map(identity_of);
                    let person = device.and_then(|device| {
                        identity
                            .as_ref()
                            .and_then(|identity| identity.certified_person_for_device(device))
                    });
                    let mut verdict = admit_blob_caller_with_records(
                        &policy,
                        msg.inner.endpoint_id,
                        records.as_ref(),
                    );
                    if verdict.is_err()
                        && person.is_some_and(|person| peer_may_read_anything(&policy, &person))
                    {
                        verdict = device.ok_or(AbortReason::Permission);
                    }
                    if verdict.is_err() {
                        eprintln!(
                            "iroh-blobs: refusing {} — it reads nothing in this workspace",
                            msg.inner
                                .endpoint_id
                                .map_or_else(|| "unnamed peer".into(), |id| id.to_string())
                        );
                    }
                    if let Ok(peer) = verdict {
                        callers.insert(msg.inner.connection_id, (peer, person));
                    }
                    msg.tx.send(verdict.map(|_| ())).await.ok();
                }
                ProviderMessage::ConnectionClosed(msg) => {
                    callers.remove(&msg.inner.connection_id);
                }
                ProviderMessage::GetRequestReceived(msg) => {
                    let ok = match callers.get(&msg.inner.connection_id) {
                        Some((peer, person)) => {
                            peer_may_read_blob_with_records(
                                &policy,
                                &namespace,
                                &store,
                                peer,
                                *person,
                                msg.inner.request.hash,
                                records.as_ref(),
                            )
                            .await
                        }
                        None => false,
                    };
                    if !ok {
                        eprintln!(
                            "iroh-blobs: refusing unreadable blob {} on connection {}",
                            msg.inner.request.hash, msg.inner.connection_id
                        );
                    }
                    msg.tx
                        .send(if ok {
                            Ok(())
                        } else {
                            Err(AbortReason::Permission)
                        })
                        .await
                        .ok();
                }
                ProviderMessage::GetManyRequestReceived(msg) => {
                    let mut ok = callers.contains_key(&msg.inner.connection_id);
                    if let Some((peer, person)) = callers.get(&msg.inner.connection_id) {
                        for hash in &msg.inner.request.hashes {
                            if !peer_may_read_blob_with_records(
                                &policy,
                                &namespace,
                                &store,
                                peer,
                                *person,
                                *hash,
                                records.as_ref(),
                            )
                            .await
                            {
                                ok = false;
                                break;
                            }
                        }
                    }
                    msg.tx
                        .send(if ok {
                            Ok(())
                        } else {
                            Err(AbortReason::Permission)
                        })
                        .await
                        .ok();
                }
                ProviderMessage::ObserveRequestReceived(msg) => {
                    let ok = match callers.get(&msg.inner.connection_id) {
                        Some((peer, person)) => {
                            peer_may_read_blob_with_records(
                                &policy,
                                &namespace,
                                &store,
                                peer,
                                *person,
                                msg.inner.request.hash,
                                records.as_ref(),
                            )
                            .await
                        }
                        None => false,
                    };
                    msg.tx
                        .send(if ok {
                            Ok(())
                        } else {
                            Err(AbortReason::Permission)
                        })
                        .await
                        .ok();
                }
                ProviderMessage::PushRequestReceived(msg) => {
                    msg.tx.send(Err(AbortReason::Permission)).await.ok();
                }
                // Notify-shaped variants carry no reply channel; the mask never
                // asks for them.
                _ => {}
            }
        }
    });
    Some(sender)
}

/// Enqueues local Automerge deltas for broadcast over gossip. Cheap to clone.
#[derive(Clone)]
pub struct SyncHandle {
    local_tx: mpsc::UnboundedSender<Bytes>,
}

impl SyncHandle {
    /// Queue a freshly-produced local delta for broadcast (no-op if empty).
    pub fn broadcast_delta(&self, delta: Vec<u8>) {
        if !delta.is_empty() {
            let _ = self.local_tx.send(Bytes::from(delta));
        }
    }
}

/// Aborts the pump task when the workspace is dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A live, replicated workspace: a CRDT namespace kept in sync with peers.
pub struct Workspace {
    pub namespace: Arc<Mutex<Namespace>>,
    pub sync: Arc<SyncHandle>,
    _router: Router,
    _tasks: Vec<AbortOnDrop>,
}

/// Durable state installed when this workspace serves record-scoped shares.
/// Keeping this separate preserves the legacy whole-workspace constructors
/// while letting production mounts opt into the stricter record ALPN.
#[derive(Clone)]
pub struct RecordServices {
    pub authority: Arc<ShareAuthority>,
    pub repository: Arc<RecordRepository>,
    pub access_registry: Arc<RecordAccessRegistry>,
    pub mutation_sink: Option<Arc<dyn crate::record_protocol::RecordMutationSink>>,
    pub materializer: Option<Arc<crate::record_materializer::RecordMaterializer>>,
    /// Durable verified remote-replica receipts. `None` preserves ephemeral
    /// test/workspace construction but cannot ever produce eviction proof.
    pub availability: Option<Arc<Mutex<crate::availability::AvailabilityRegistry>>>,
    /// Signed workspace ACL state shared through the authority ALPN.
    pub workspace_authority: Option<Arc<WorkspaceAuthority>>,
}

#[derive(Clone)]
struct RecordPlane {
    protocol: RecordProtocol,
    mesh_protocol: RecordMeshProtocol,
    access_registry: Arc<RecordAccessRegistry>,
    repository: Arc<RecordRepository>,
    materializer: Option<Arc<crate::record_materializer::RecordMaterializer>>,
    workspace_id: uuid::Uuid,
    availability: Option<Arc<Mutex<crate::availability::AvailabilityRegistry>>>,
    workspace_authority: Option<Arc<WorkspaceAuthority>>,
}

#[derive(Clone)]
struct AuthorityProtocol {
    endpoint: Endpoint,
    identity: FsTransportIdentity,
    policy: PeerPolicy,
    authority: Arc<WorkspaceAuthority>,
    workspace_id: uuid::Uuid,
}

impl std::fmt::Debug for AuthorityProtocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AuthorityProtocol")
    }
}

impl ProtocolHandler for AuthorityProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        let peer = authenticate_connection(
            &self.endpoint,
            &connection,
            &mut recv,
            &mut send,
            &self.identity,
        )
        .await
        .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        let request: AuthorityRequest = read_authority_frame(&mut recv)
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        let authority = if request.workspace == self.workspace_id {
            let governor = peer.user_public_key == self.authority.governor();
            if let Some(published) = request.publish
                && governor
            {
                let _ = self.authority.apply(published);
            }
            (governor || authenticated_peer_may_read_whole_workspace(&self.policy, &peer))
                .then(|| self.authority.current())
                .flatten()
        } else {
            None
        };
        write_authority_frame(&mut send, &AuthorityResponse { authority })
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        send.finish().map_err(AcceptError::from_err)?;
        // Keep the protocol handler alive until the client has consumed the
        // final authority frame; dropping immediately can race QUIC stream
        // delivery and turn a valid publish into `connection lost`.
        connection.closed().await;
        Ok(())
    }
}

#[derive(Clone)]
struct ReplicaAckProtocol {
    endpoint: Endpoint,
    identity: FsTransportIdentity,
    policy: PeerPolicy,
    workspace_id: uuid::Uuid,
    repository: Arc<RecordRepository>,
    store: Arc<ContentStore>,
}

impl std::fmt::Debug for ReplicaAckProtocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ReplicaAckProtocol")
    }
}

impl ProtocolHandler for ReplicaAckProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if self.identity.partitioned() {
            connection.close(1u32.into(), b"partitioned");
            return Ok(());
        }
        let (mut send, mut recv) = connection.accept_bi().await?;
        let exchange = async {
            let peer = authenticate_connection(
                &self.endpoint,
                &connection,
                &mut recv,
                &mut send,
                &self.identity,
            )
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
            let request: ReplicaAckRequestV1 = read_replica_ack_frame(&mut recv)
                .await
                .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
            let response = self.response(&peer, &request).await;
            write_replica_ack_frame(&mut send, &response)
                .await
                .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
            send.finish().map_err(AcceptError::from_err)?;
            Ok::<(), AcceptError>(())
        };
        tokio::time::timeout(REPLICA_ACK_TIMEOUT, exchange)
            .await
            .map_err(|_| {
                AcceptError::from_err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "replica acknowledgement request timed out",
                ))
            })??;
        connection.closed().await;
        Ok(())
    }
}

impl ReplicaAckProtocol {
    async fn response(
        &self,
        peer: &AuthenticatedFsPeer,
        request: &ReplicaAckRequestV1,
    ) -> ReplicaAckResponseV1 {
        if request.validate().is_err() || request.workspace_id != self.workspace_id {
            return ReplicaAckResponseV1::Denied;
        }
        let requester = collab_types::FileActor {
            person: collab_types::Ed25519PublicKey(peer.user_public_key),
            device: collab_types::Ed25519PublicKey(peer.device_public_key),
        };
        let key = RecordRepositoryKey {
            workspace_id: self.workspace_id,
            record_id: request.revision.record_id,
        };
        let Ok(snapshot) = self.repository.snapshot(
            key,
            &requester,
            &DirectMeshAuthorizer {
                peer,
                identity: &self.identity,
                policy: &self.policy,
            },
        ) else {
            return ReplicaAckResponseV1::Denied;
        };
        let version_matches = snapshot.versions.iter().any(|version| {
            version.id == request.revision.version_id
                && version.record_id == request.revision.record_id
                && version.content_hash == request.revision.content_hash
                && version.metadata_hash == request.revision.metadata_hash
        });
        if snapshot.content_revision != Some(request.revision) || !version_matches {
            return ReplicaAckResponseV1::Unavailable;
        }
        if self
            .store
            .verify_durable_revision(&request.revision)
            .await
            .is_err()
        {
            return ReplicaAckResponseV1::Unavailable;
        }
        let actor = collab_types::FileActor {
            person: collab_types::Ed25519PublicKey(self.identity.user_public_key()),
            device: collab_types::Ed25519PublicKey(self.identity.device_public_key()),
        };
        match ReplicaAckV1::signed(
            request,
            actor,
            now_ms(),
            self.identity.credentials().signing_key(),
        ) {
            Ok(receipt) => ReplicaAckResponseV1::Acknowledged(receipt),
            Err(_) => ReplicaAckResponseV1::Unavailable,
        }
    }
}

#[derive(Clone)]
struct RecordMeshProtocol {
    endpoint: Endpoint,
    identity: FsTransportIdentity,
    policy: PeerPolicy,
    workspace_id: uuid::Uuid,
    repository: Arc<RecordRepository>,
}

impl std::fmt::Debug for RecordMeshProtocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RecordMeshProtocol")
    }
}

impl ProtocolHandler for RecordMeshProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if self.identity.partitioned() {
            connection.close(1u32.into(), b"partitioned");
            return Ok(());
        }
        let (mut send, mut recv) = connection.accept_bi().await?;
        let exchange = async {
            let peer = authenticate_connection(
                &self.endpoint,
                &connection,
                &mut recv,
                &mut send,
                &self.identity,
            )
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
            let request = read_record_mesh_request(&mut recv)
                .await
                .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
            let response = if authenticated_peer_may_read_whole_workspace(&self.policy, &peer) {
                serve_authorized_request(&self.repository, self.workspace_id, &request)
            } else {
                crate::record_mesh::RecordMeshResponseV1 {
                    protocol_version: crate::record_mesh::RECORD_MESH_PROTOCOL_VERSION,
                    result: crate::record_mesh::RecordMeshResultV1::Denied,
                }
            };
            write_record_mesh_response(&mut send, &response)
                .await
                .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
            send.finish().map_err(AcceptError::from_err)?;
            Ok::<(), AcceptError>(())
        };
        tokio::time::timeout(Duration::from_secs(15), exchange)
            .await
            .map_err(|_| {
                AcceptError::from_err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "record mesh request timed out",
                ))
            })??;
        // Returning drops the router-owned connection. Wait until the client
        // has consumed the finished response stream, otherwise QUIC may tear
        // down buffered response bytes as soon as this handler completes.
        connection.closed().await;
        Ok(())
    }
}

fn build_record_plane(
    endpoint: &Endpoint,
    identity: &FsTransportIdentity,
    policy: &PeerPolicy,
    store: Arc<ContentStore>,
    workspace_id: uuid::Uuid,
    records: Option<RecordServices>,
) -> Result<Option<RecordPlane>> {
    let Some(records) = records else {
        return Ok(None);
    };
    let mut protocol = RecordProtocol::new(
        endpoint.clone(),
        identity.credentials(),
        identity.revocations(),
        workspace_id,
        Arc::clone(&records.authority),
        Arc::new(identity.clone()),
        Arc::clone(&records.access_registry),
        Arc::clone(&records.repository),
    )?
    .with_replica_store(store);
    if let Some(sink) = records.mutation_sink.clone() {
        protocol = protocol.with_mutation_sink(sink);
    }
    if let Some(availability) = records.availability.clone() {
        protocol = protocol.with_availability(availability);
    }
    let mesh_protocol = RecordMeshProtocol {
        endpoint: endpoint.clone(),
        identity: identity.clone(),
        policy: policy.clone(),
        workspace_id,
        repository: Arc::clone(&records.repository),
    };
    Ok(Some(RecordPlane {
        protocol,
        mesh_protocol,
        access_registry: records.access_registry,
        repository: records.repository,
        materializer: records.materializer,
        workspace_id,
        availability: records.availability,
        workspace_authority: records.workspace_authority,
    }))
}

impl Workspace {
    /// Begin hosting a new workspace, returning a ticket to share with
    /// peers. `policy` is what the sync and content planes serve *through*
    /// (see [`PeerPolicy`]); `None` hosts unenforced.
    ///
    /// # Errors
    ///
    /// Returns an error if the gossip topic or protocol router fails to start.
    /// Host a workspace and its authenticated synchronization protocols.
    ///
    /// # Errors
    ///
    /// Returns an error when the gossip topic or protocol router fails to start.
    pub async fn host(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        store: Arc<ContentStore>,
        hydrator: Arc<Hydrator>,
        namespace: Arc<Mutex<Namespace>>,
        identity: FsTransportIdentity,
        policy: PeerPolicy,
    ) -> Result<(Workspace, Ticket)> {
        Self::host_inner(
            endpoint, namespace, lookup, store, hydrator, identity, policy, None,
        )
        .await
    }

    /// Host a workspace with the exact-record sharing protocol enabled.
    ///
    /// # Errors
    ///
    /// Returns an error when the gossip topic or protocol router fails to start.
    #[allow(clippy::too_many_arguments)]
    pub async fn host_with_records(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        store: Arc<ContentStore>,
        hydrator: Arc<Hydrator>,
        namespace: Arc<Mutex<Namespace>>,
        identity: FsTransportIdentity,
        policy: PeerPolicy,
        records: RecordServices,
    ) -> Result<(Workspace, Ticket)> {
        Self::host_inner(
            endpoint,
            namespace,
            lookup,
            store,
            hydrator,
            identity,
            policy,
            Some(records),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn host_inner(
        endpoint: Endpoint,
        namespace: Arc<Mutex<Namespace>>,
        lookup: MemoryLookup,
        store: Arc<ContentStore>,
        hydrator: Arc<Hydrator>,
        identity: FsTransportIdentity,
        policy: PeerPolicy,
        records: Option<RecordServices>,
    ) -> Result<(Workspace, Ticket)> {
        let topic = *endpoint.id().as_bytes();
        let ticket = Ticket {
            node: endpoint.addr(),
            workspace: topic,
        };
        let record_plane = build_record_plane(
            &endpoint,
            &identity,
            &policy,
            Arc::clone(&store),
            ticket.workspace_id(),
            records,
        )?;
        let ws = Self::start(
            endpoint,
            lookup,
            store,
            hydrator,
            namespace,
            identity,
            policy,
            topic,
            vec![],
            record_plane,
        )
        .await?;
        Ok((ws, ticket))
    }

    /// Join an existing workspace described by `ticket`. The caller must
    /// already have bootstrapped `namespace` from the host (see
    /// [`fetch_remote_doc`]) — a joiner serves too (anti-entropy is
    /// symmetric), so it carries the same `policy` a host does.
    ///
    /// # Errors
    ///
    /// Returns an error if the gossip topic or protocol router fails to start.
    #[allow(clippy::too_many_arguments)] // Mirrors host inputs plus the join ticket.
    pub async fn join(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        store: Arc<ContentStore>,
        hydrator: Arc<Hydrator>,
        namespace: Arc<Mutex<Namespace>>,
        ticket: Ticket,
        identity: FsTransportIdentity,
        policy: PeerPolicy,
    ) -> Result<Workspace> {
        Self::join_inner(
            endpoint, lookup, store, hydrator, namespace, ticket, identity, policy, None,
        )
        .await
    }

    /// Join a workspace and serve/consume its exact-record sharing protocol.
    ///
    /// # Errors
    ///
    /// Returns an error if the gossip topic or protocol router fails to start.
    #[allow(clippy::too_many_arguments)]
    pub async fn join_with_records(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        store: Arc<ContentStore>,
        hydrator: Arc<Hydrator>,
        namespace: Arc<Mutex<Namespace>>,
        ticket: Ticket,
        identity: FsTransportIdentity,
        policy: PeerPolicy,
        records: RecordServices,
    ) -> Result<Workspace> {
        Self::join_inner(
            endpoint,
            lookup,
            store,
            hydrator,
            namespace,
            ticket,
            identity,
            policy,
            Some(records),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn join_inner(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        store: Arc<ContentStore>,
        hydrator: Arc<Hydrator>,
        namespace: Arc<Mutex<Namespace>>,
        ticket: Ticket,
        identity: FsTransportIdentity,
        policy: PeerPolicy,
        records: Option<RecordServices>,
    ) -> Result<Workspace> {
        let topic = ticket.workspace;
        let record_plane = build_record_plane(
            &endpoint,
            &identity,
            &policy,
            Arc::clone(&store),
            ticket.workspace_id(),
            records,
        )?;
        // A full namespace bootstrap contains the structural tree, but signed
        // file hashes and byte lengths live in the authenticated record mesh.
        // Pull that projection before exposing the mount: otherwise a fresh
        // collaborator can briefly observe real files as zero-byte entries,
        // and an eager application may cache that false empty state.
        if let Some(records) = &record_plane
            && let Err(error) = pull_record_mesh_from_peer(
                &endpoint,
                ticket.node.clone(),
                &identity,
                &policy,
                records,
            )
            .await
            && !identity.partitioned()
        {
            // Persisted/offline mounts remain available. The periodic mesh
            // worker below keeps retrying a temporarily unavailable or older
            // peer after startup.
            eprintln!("initial record mesh reconciliation failed: {error}");
        }
        Self::start(
            endpoint,
            lookup,
            store,
            hydrator,
            namespace,
            identity,
            policy,
            topic,
            vec![ticket.node],
            record_plane,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn start(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        store: Arc<ContentStore>,
        hydrator: Arc<Hydrator>,
        namespace: Arc<Mutex<Namespace>>,
        identity: FsTransportIdentity,
        policy: PeerPolicy,
        topic_bytes: [u8; 32],
        bootstrap: Vec<EndpointAddr>,
        record_plane: Option<RecordPlane>,
    ) -> Result<Workspace> {
        // After bootstrap/seed, set the delta watermark so future local
        // writes produce minimal deltas (and the seed isn't re-broadcast).
        namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .prime_baseline();

        // Register bootstrap peers so gossip can dial them and we can hydrate.
        let mut boot_ids = Vec::new();
        for addr in &bootstrap {
            lookup.add_endpoint_info(addr.clone());
            hydrator.add_peer(addr.clone());
            boot_ids.push(addr.id);
        }

        let sync_proto = SyncProtocol {
            endpoint: endpoint.clone(),
            identity: identity.clone(),
            namespace: namespace.clone(),
            policy: policy.clone(),
        };
        let replica_ack_protocol = record_plane.as_ref().map(|records| ReplicaAckProtocol {
            endpoint: endpoint.clone(),
            identity: identity.clone(),
            policy: policy.clone(),
            workspace_id: records.workspace_id,
            repository: Arc::clone(&records.repository),
            store: Arc::clone(&store),
        });
        let gossip = Gossip::builder().spawn(endpoint.clone());
        let builder = Router::builder(endpoint.clone()).accept(
            iroh_blobs::ALPN,
            store.blobs_protocol(blob_guard(
                policy.clone(),
                namespace.clone(),
                store.clone(),
                record_plane.clone(),
                Some(identity.clone()),
            )),
        );
        let builder = match (&record_plane, replica_ack_protocol) {
            (Some(records), Some(replica_ack)) => builder
                .accept(QOS_FS_RECORD_ALPN, records.protocol.clone())
                .accept(QOS_FS_RECORD_MESH_ALPN, records.mesh_protocol.clone())
                .accept(QOS_FS_REPLICA_ACK_ALPN, replica_ack),
            _ => builder,
        };
        let builder = if let Some(records) = &record_plane {
            if let Some(authority) = &records.workspace_authority {
                builder.accept(
                    QOS_FS_AUTHORITY_ALPN,
                    AuthorityProtocol {
                        endpoint: endpoint.clone(),
                        identity: identity.clone(),
                        policy: policy.clone(),
                        authority: Arc::clone(authority),
                        workspace_id: records.workspace_id,
                    },
                )
            } else {
                builder
            }
        } else {
            builder
        };
        // A gossip message records only the peer that relayed it, not the
        // original author, so a receiver cannot enforce a writer capability.
        // Enforced workspaces use the authenticated sync ALPN exclusively.
        let router = if policy.is_none() {
            builder
                .accept(GOSSIP_ALPN, gossip.clone())
                .accept(QOS_FS_SYNC_ALPN, sync_proto)
                .spawn()
        } else {
            builder.accept(QOS_FS_SYNC_ALPN, sync_proto).spawn()
        };

        let (local_tx, local_rx) = mpsc::unbounded_channel();
        let mut tasks = Vec::new();
        let gossip_sender = if policy.is_none() {
            let topic = TopicId::from_bytes(topic_bytes);
            let gtopic = if boot_ids.is_empty() {
                gossip.subscribe(topic, vec![]).await?
            } else {
                gossip.subscribe_and_join(topic, boot_ids).await?
            };
            let (sender, receiver) = gtopic.split();
            tasks.push(AbortOnDrop(tokio::spawn(pump_loop(
                receiver,
                sender.clone(),
                local_rx,
                namespace.clone(),
                hydrator.clone(),
            ))));
            Some(sender)
        } else {
            // Dropping the receiver makes `broadcast_delta` a cheap no-op.
            // Authenticated anti-entropy reads the namespace itself and does
            // not need a second change queue.
            drop(local_rx);
            None
        };
        // Anti-entropy backstop: periodically pull+merge peers' full docs and
        // resolve conflicts, so a missed gossip delta or a mid-join window
        // can't leave a peer permanently diverged.
        tasks.push(AbortOnDrop(tokio::spawn(reconcile_loop(
            endpoint,
            namespace.clone(),
            hydrator.clone(),
            gossip_sender,
            identity,
            policy,
            record_plane,
        ))));

        Ok(Workspace {
            namespace,
            sync: Arc::new(SyncHandle { local_tx }),
            _router: router,
            _tasks: tasks,
        })
    }
}

fn now_ms() -> u64 {
    crate::clock::unix_now_ms()
}

/// The bidirectional pump: broadcast local deltas, apply remote deltas,
/// and learn peer addresses for hydration as neighbors come up.
async fn pump_loop(
    mut receiver: GossipReceiver,
    sender: GossipSender,
    mut local_rx: mpsc::UnboundedReceiver<Bytes>,
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
) {
    loop {
        tokio::select! {
            maybe = local_rx.recv() => {
                match maybe {
                    Some(delta) => { let _ = sender.broadcast(delta).await; }
                    None => break, // all SyncHandles dropped
                }
            }
            ev = receiver.next() => {
                match ev {
                    Some(Ok(Event::Received(msg))) => {
                        // Apply the delta + break any move-cycle it introduced (cheap,
                        // pure-CPU). Content-conflict resolution and anti-entropy run in
                        // reconcile_loop so a slow hydration never blocks local delta propagation.
                        let resolve_delta = {
                            let mut ns = namespace
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            ns.apply_remote_and_resolve(msg.content.as_ref())
                                .unwrap_or_default()
                        };
                        if !resolve_delta.is_empty() {
                            let _ = sender.broadcast(Bytes::from(resolve_delta)).await;
                        }
                    }
                    Some(Ok(Event::NeighborUp(id))) => {
                        hydrator.add_peer(EndpointAddr::new(id));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => {
                        // Transient gossip error: don't kill the pump (that would silently
                        // dead-sync the workspace) — back off briefly and keep going.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    None => break, // stream genuinely ended
                }
            }
        }
    }
}

/// Periodic anti-entropy + conflict resolution: pulls each known peer's
/// changes to recover from dropped gossip or a mid-join window, then
/// materializes conflict copies for concurrent content edits.
struct DirectMeshAuthorizer<'a> {
    peer: &'a AuthenticatedFsPeer,
    identity: &'a FsTransportIdentity,
    policy: &'a PeerPolicy,
}

impl RecordAuthorizer for DirectMeshAuthorizer<'_> {
    fn actor_is_certified(&self, actor: &collab_types::FileActor) -> bool {
        actor.person.as_bytes() == &self.peer.user_public_key
            && actor.device.as_bytes() == &self.peer.device_public_key
            && crate::share_session::LiveActorCertification::certification_is_current(
                self.identity,
                self.peer,
                unix_now_ns(),
            )
    }

    fn allows(
        &self,
        actor: &collab_types::FileActor,
        _record_id: collab_types::FileRecordId,
        action: RecordAction,
    ) -> bool {
        self.actor_is_certified(actor)
            && match action {
                RecordAction::Read => {
                    authenticated_peer_may_read_whole_workspace(self.policy, self.peer)
                }
                RecordAction::Write => {
                    authenticated_peer_may_write_whole_workspace(self.policy, self.peer)
                }
            }
    }
}

async fn record_mesh_request(
    endpoint: &Endpoint,
    remote: EndpointAddr,
    identity: &FsTransportIdentity,
    request: &RecordMeshRequestV1,
) -> Result<(
    AuthenticatedFsPeer,
    crate::record_mesh::RecordMeshResponseV1,
)> {
    if identity.partitioned() {
        anyhow::bail!("transport partition gate is closed");
    }
    let connection = endpoint.connect(remote, QOS_FS_RECORD_MESH_ALPN).await?;
    let (mut send, mut recv) = connection.open_bi().await?;
    let peer =
        authenticate_connection(endpoint, &connection, &mut recv, &mut send, identity).await?;
    write_record_mesh_request(&mut send, request).await?;
    send.finish()?;
    let response = read_record_mesh_response(&mut recv).await?;
    connection.close(0u32.into(), b"");
    Ok((peer, response))
}

async fn pull_workspace_authority_from_peer(
    endpoint: &Endpoint,
    remote: EndpointAddr,
    identity: &FsTransportIdentity,
    authority: &WorkspaceAuthority,
    workspace_id: uuid::Uuid,
) -> Result<()> {
    let connection = endpoint.connect(remote, QOS_FS_AUTHORITY_ALPN).await?;
    let (mut send, mut recv) = connection.open_bi().await?;
    let _peer =
        authenticate_connection(endpoint, &connection, &mut recv, &mut send, identity).await?;
    write_authority_frame(
        &mut send,
        &AuthorityRequest {
            workspace: workspace_id,
            publish: None,
        },
    )
    .await?;
    send.finish()?;
    let response: AuthorityResponse = read_authority_frame(&mut recv).await?;
    if let Some(envelope) = response.authority {
        authority.apply(envelope)?;
    }
    connection.close(0u32.into(), b"");
    Ok(())
}

pub async fn submit_workspace_authority(
    endpoint: &Endpoint,
    remote: EndpointAddr,
    identity: &FsTransportIdentity,
    workspace: uuid::Uuid,
    publish: Option<crate::workspace_authority::WorkspaceAuthorityEnvelope>,
) -> Result<Option<crate::workspace_authority::WorkspaceAuthorityEnvelope>> {
    let connection = endpoint.connect(remote, QOS_FS_AUTHORITY_ALPN).await?;
    let (mut send, mut recv) = connection.open_bi().await?;
    let _ = authenticate_connection(endpoint, &connection, &mut recv, &mut send, identity).await?;
    write_authority_frame(&mut send, &AuthorityRequest { workspace, publish }).await?;
    send.finish()?;
    let response: AuthorityResponse = read_authority_frame(&mut recv).await?;
    connection.close(0u32.into(), b"");
    Ok(response.authority)
}

#[allow(clippy::too_many_lines)]
async fn pull_record_mesh_from_peer(
    endpoint: &Endpoint,
    remote: EndpointAddr,
    identity: &FsTransportIdentity,
    policy: &PeerPolicy,
    records: &RecordPlane,
) -> Result<bool> {
    let mut changed = false;
    let mut after_inventory = None;
    // One pass is deliberately finite even for a maliciously fragmented
    // repository. Subsequent anti-entropy passes resume from the beginning;
    // already-equal prefix cursors are cheap and no state is trusted merely
    // because it appeared in inventory.
    for _ in 0..256 {
        let request = RecordMeshRequestV1 {
            protocol_version: RECORD_MESH_PROTOCOL_VERSION,
            workspace_id: records.workspace_id,
            operation: RecordMeshOperationV1::Inventory {
                after: after_inventory,
                limit: u16::try_from(MAX_RECORD_MESH_INVENTORY_PAGE_STREAMS).unwrap_or(u16::MAX),
            },
        };
        let (inventory_peer, response) =
            record_mesh_request(endpoint, remote.clone(), identity, &request).await?;
        let (cursors, next) = match response.result {
            RecordMeshResultV1::End {
                workspace_id,
                operation: RecordMeshEndV1::Inventory,
            } if workspace_id == records.workspace_id => break,
            RecordMeshResultV1::InventoryPage {
                workspace_id,
                cursors,
                next,
            } if workspace_id == records.workspace_id => (cursors, next),
            RecordMeshResultV1::Denied => return Ok(false),
            _ => anyhow::bail!("record mesh returned an invalid inventory response"),
        };
        let direct_actor = collab_types::FileActor {
            person: collab_types::Ed25519PublicKey(inventory_peer.user_public_key),
            device: collab_types::Ed25519PublicKey(inventory_peer.device_public_key),
        };
        for remote_cursor in cursors
            .into_iter()
            .filter(|cursor| cursor.actor == direct_actor)
        {
            let key = RecordRepositoryKey {
                workspace_id: records.workspace_id,
                record_id: remote_cursor.record_id,
            };
            let mut local_sequence = records
                .repository
                .highest_sequence(key, &remote_cursor.actor)
                .unwrap_or(0);
            if local_sequence > remote_cursor.contiguous_sequence {
                continue;
            }
            if local_sequence == remote_cursor.contiguous_sequence {
                if local_sequence > 0
                    && records.repository.stream_cursor(
                        key,
                        &remote_cursor.actor,
                        local_sequence,
                    )? != remote_cursor
                {
                    anyhow::bail!("record mesh prefix digest diverged");
                }
                continue;
            }
            while local_sequence < remote_cursor.contiguous_sequence {
                let after = if local_sequence == 0 {
                    None
                } else {
                    Some(records.repository.stream_cursor(
                        key,
                        &remote_cursor.actor,
                        local_sequence,
                    )?)
                };
                let pull = RecordMeshRequestV1 {
                    protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                    workspace_id: records.workspace_id,
                    operation: RecordMeshOperationV1::Pull {
                        stream: RecordMeshStreamV1 {
                            record_id: remote_cursor.record_id,
                            actor: remote_cursor.actor,
                        },
                        after,
                        limit: u16::try_from(MAX_RECORD_MESH_PULL_BATCH_ENVELOPES)
                            .unwrap_or(u16::MAX),
                    },
                };
                let (peer, response) =
                    record_mesh_request(endpoint, remote.clone(), identity, &pull).await?;
                if peer.user_public_key != inventory_peer.user_public_key
                    || peer.device_public_key != inventory_peer.device_public_key
                {
                    anyhow::bail!("record mesh peer identity changed during pull");
                }
                let envelopes = match response.result {
                    RecordMeshResultV1::EnvelopeBatch {
                        workspace_id,
                        stream,
                        after_sequence,
                        envelopes,
                    } if workspace_id == records.workspace_id
                        && stream.record_id == remote_cursor.record_id
                        && stream.actor == remote_cursor.actor
                        && after_sequence == local_sequence =>
                    {
                        envelopes
                    }
                    _ => anyhow::bail!("record mesh returned an invalid envelope batch"),
                };
                records.repository.apply_batch(
                    key,
                    &envelopes,
                    &DirectMeshAuthorizer {
                        peer: &peer,
                        identity,
                        policy,
                    },
                )?;
                changed |= !envelopes.is_empty();
                local_sequence = records
                    .repository
                    .highest_sequence(key, &remote_cursor.actor)
                    .unwrap_or(local_sequence);
                if let Some(materializer) = &records.materializer {
                    materializer.reconcile_repository(&records.repository)?;
                }
            }
        }
        after_inventory = Some(next);
    }
    // Also heal mesh-first / namespace-later ordering and restart gaps when
    // every remote cursor was already equal and no new batch was applied.
    if let Some(materializer) = &records.materializer {
        materializer.reconcile_repository(&records.repository)?;
    }
    Ok(changed)
}

async fn request_replica_ack(
    endpoint: &Endpoint,
    remote: EndpointAddr,
    identity: &FsTransportIdentity,
    policy: &PeerPolicy,
    request: &ReplicaAckRequestV1,
) -> Result<crate::availability::ReplicaObservation> {
    if identity.partitioned() {
        anyhow::bail!("transport partition gate is closed");
    }
    tokio::time::timeout(REPLICA_ACK_TIMEOUT, async {
        let connection = endpoint.connect(remote, QOS_FS_REPLICA_ACK_ALPN).await?;
        let (mut send, mut recv) = connection.open_bi().await?;
        let peer =
            authenticate_connection(endpoint, &connection, &mut recv, &mut send, identity).await?;
        if !crate::share_session::LiveActorCertification::certification_is_current(
            identity,
            &peer,
            unix_now_ns(),
        ) || !authenticated_peer_may_read_whole_workspace(policy, &peer)
        {
            anyhow::bail!("replica peer lacks live workspace read authorization");
        }
        write_replica_ack_frame(&mut send, request).await?;
        send.finish()?;
        let response: ReplicaAckResponseV1 = read_replica_ack_frame(&mut recv).await?;
        let ReplicaAckResponseV1::Acknowledged(receipt) = response else {
            anyhow::bail!("peer did not acknowledge a durable exact replica");
        };
        let observed = receipt.verify_for(
            request,
            &peer,
            collab_types::Ed25519PublicKey(identity.device_public_key()),
            now_ms(),
        )?;
        connection.close(0u32.into(), b"");
        Ok::<_, anyhow::Error>(observed)
    })
    .await
    .map_err(|_| anyhow::anyhow!("replica acknowledgement exchange timed out"))?
}

async fn collect_replica_acks_from_peer(
    endpoint: &Endpoint,
    remote: EndpointAddr,
    identity: &FsTransportIdentity,
    policy: &PeerPolicy,
    records: &RecordPlane,
) {
    let Some(registry) = &records.availability else {
        return;
    };
    for key in records.repository.reconciliation_keys(records.workspace_id) {
        let Some(batch) = records.repository.reconciliation_batch(key) else {
            continue;
        };
        let Some(revision) = batch.snapshot.content_revision else {
            continue;
        };
        let Ok(request) = ReplicaAckRequestV1::new(records.workspace_id, revision) else {
            continue;
        };
        let Ok(observation) =
            request_replica_ack(endpoint, remote.clone(), identity, policy, &request).await
        else {
            continue;
        };
        let observed_at = observation.observed_at_ms;
        let _ = persist_workspace_replica_observation(
            registry,
            revision,
            collab_types::Ed25519PublicKey(identity.device_public_key()),
            observation,
            observed_at,
        );
    }
}

#[allow(clippy::significant_drop_tightening)]
fn persist_workspace_replica_observation(
    registry: &Arc<Mutex<crate::availability::AvailabilityRegistry>>,
    revision: collab_types::ContentRevisionV1,
    local_device: collab_types::Ed25519PublicKey,
    observation: crate::availability::ReplicaObservation,
    observed_at: u64,
) -> Result<(), crate::availability::AvailabilityError> {
    let mut registry = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Whole-workspace records historically never created availability rows,
    // so valid NAS receipts were silently rejected as `UnknownRecord`.
    let parent = registry
        .get(revision.record_id)
        .and_then(|record| record.parent);
    let metadata_observed = registry
        .get(revision.record_id)
        .and_then(|record| record.freshness.metadata_observed_at_ms)
        .unwrap_or(observed_at);
    registry.track_current_revision(
        revision,
        crate::availability::RecordKind::File,
        parent,
        metadata_observed,
    )?;
    registry.observe_replica(revision.record_id, local_device, observation, observed_at)
}

#[allow(clippy::too_many_lines)]
async fn reconcile_loop(
    endpoint: Endpoint,
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    sender: Option<GossipSender>,
    identity: FsTransportIdentity,
    policy: PeerPolicy,
    records: Option<RecordPlane>,
) {
    let may_write = peer_may_write_whole_workspace(&policy, &identity.user_public_key())
        || peer_may_write_whole_workspace(&policy, &identity.device_public_key());
    let mut pending_thumbnails = HashMap::new();
    // `mount` bootstraps a namespace before this task starts, so schedule its
    // previews once even if the first anti-entropy exchange is clean.
    let mut refresh_thumbnails = true;
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;

        for peer in identity.authenticated_peer_addrs() {
            hydrator.add_peer(peer);
        }
        let known_peers = hydrator.known_peers();
        let mut namespace_changed = false;
        for peer in known_peers {
            let remote_may_write = policy.is_none()
                || identity
                    .certified_peer_for_device(*peer.id.as_bytes())
                    .is_some_and(|remote| {
                        authenticated_peer_may_write_whole_workspace(&policy, &remote)
                    });
            if may_write && remote_may_write {
                // Automerge sync is symmetric: even the responder sends its
                // local changes. Never exchange with a read-only peer, or its
                // unauthorised local document could hitch a ride on an
                // authorised writer's reconciliation request.
                // Only a workspace-wide writer may send Automerge sync
                // messages. The responder independently checks the same fact.
                if let Ok(received_change) =
                    sync_with_peer(&endpoint, peer.clone(), &namespace, &identity).await
                {
                    namespace_changed |= received_change;
                    let resolve_delta = {
                        namespace
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .post_sync_resolve()
                    };
                    if !resolve_delta.is_empty()
                        && let Some(sender) = &sender
                    {
                        let _ = sender.broadcast(Bytes::from(resolve_delta)).await;
                    }
                }
            } else if !may_write
                && let Ok(doc) = fetch_remote_doc(&endpoint, peer.clone(), &identity).await
            {
                // Read-only peers pull a full snapshot and never upload local
                // changes. Full snapshots are safe here because the remote
                // serves them only to workspace-root readers.
                let before_merge = namespace_generation(&namespace);
                let resolve_delta = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .merge_remote_and_resolve(&doc)
                    .unwrap_or_default();
                // A clean merge commonly needs no conflict-resolution delta;
                // compare the document generation so its previews are still
                // scheduled on a fresh collaborator.
                namespace_changed |= before_merge != namespace_generation(&namespace);
                if !resolve_delta.is_empty()
                    && let Some(sender) = &sender
                {
                    let _ = sender.broadcast(Bytes::from(resolve_delta)).await;
                }
            }
            if let Some(records) = &records {
                if let Some(authority) = &records.workspace_authority
                    && let Err(error) = pull_workspace_authority_from_peer(
                        &endpoint,
                        peer.clone(),
                        &identity,
                        authority,
                        records.workspace_id,
                    )
                    .await
                    && !identity.partitioned()
                {
                    eprintln!("workspace authority reconciliation failed: {error}");
                }
                match pull_record_mesh_from_peer(
                    &endpoint,
                    peer.clone(),
                    &identity,
                    &policy,
                    records,
                )
                .await
                {
                    Ok(changed) => namespace_changed |= changed,
                    Err(error) if !identity.partitioned() => {
                        eprintln!("record mesh reconciliation failed: {error}");
                    }
                    Err(_) => {}
                }
                collect_replica_acks_from_peer(
                    &endpoint,
                    peer.clone(),
                    &identity,
                    &policy,
                    records,
                )
                .await;
            }
        }

        if let Err(error) = prefetch_namespace_thumbnails(
            &namespace,
            &hydrator,
            &mut pending_thumbnails,
            namespace_changed || std::mem::take(&mut refresh_thumbnails),
        )
        .await
            && !identity.partitioned()
        {
            eprintln!("thumbnail metadata prefetch failed: {error}");
        }

        if !may_write {
            continue;
        }
        if let Some(records) = &records {
            let actor = collab_types::FileActor {
                person: collab_types::Ed25519PublicKey(identity.user_public_key()),
                device: collab_types::Ed25519PublicKey(identity.device_public_key()),
            };
            match crate::record_publisher::RecordPublisher::new(
                Arc::clone(&records.repository),
                records.workspace_id,
                actor,
                identity.credentials().signing_key().clone(),
            ) {
                Ok(publisher) => {
                    let mut conflict_published = false;
                    for key in records.repository.reconciliation_keys(records.workspace_id) {
                        match publisher.publish_concurrent_head_conflict(key.record_id) {
                            Ok(changed) => conflict_published |= changed,
                            Err(error) => {
                                eprintln!("record conflict publication failed: {error}");
                            }
                        }
                    }
                    if conflict_published
                        && let Some(materializer) = &records.materializer
                        && let Err(error) = materializer.reconcile_repository(&records.repository)
                    {
                        eprintln!("record conflict materialization failed: {error}");
                    }
                }
                Err(error) => eprintln!("record conflict publisher unavailable: {error}"),
            }
        }
        let conflicts = crate::conflict::detect(
            &namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for c in conflicts {
            // Fetch just the loser's manifest for its length (chunks hydrate
            // lazily on read). Skipped if no peer can serve it yet; retried next pass.
            if let Ok(len) = hydrator.content_len(c.loser).await {
                let delta = {
                    let mut ns = namespace
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    crate::conflict::materialize(&mut ns, &c, len, now_ms())
                };
                if !delta.is_empty()
                    && let Some(sender) = &sender
                {
                    let _ = sender.broadcast(Bytes::from(delta)).await;
                }
            }
        }
    }
}

#[cfg(test)]
static NEXT_TEST_IDENTITY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Build a real certified endpoint for cross-module transport integration
/// tests. Production never synthesizes certificates.
#[cfg(test)]
pub(crate) async fn bind_certified_test_endpoint() -> (Endpoint, MemoryLookup, FsTransportIdentity)
{
    let (endpoint, lookup, identity, _root, _device) =
        bind_certified_test_endpoint_with_root().await;
    (endpoint, lookup, identity)
}

#[cfg(test)]
async fn bind_certified_test_endpoint_with_root() -> (
    Endpoint,
    MemoryLookup,
    FsTransportIdentity,
    ed25519_dalek::SigningKey,
    ed25519_dalek::SigningKey,
) {
    use ed25519_dalek::SigningKey;
    use qos_user_identity::{
        DeviceCapabilities, DeviceCert, IdentityPaths, adopt_device_key,
        persist::write_device_cert, sign_device_cert,
    };
    use std::sync::atomic::Ordering;

    let sequence = NEXT_TEST_IDENTITY.fetch_add(1, Ordering::Relaxed);
    let device_seed = *blake3::hash(&sequence.to_le_bytes()).as_bytes();
    let root_seed = *blake3::hash(&sequence.wrapping_add(u64::MAX / 2).to_le_bytes()).as_bytes();
    let device = SigningKey::from_bytes(&device_seed);
    let root = SigningKey::from_bytes(&root_seed);
    let now = unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: format!("sync-test-device-{sequence}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now.saturating_sub(1),
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let paths = IdentityPaths::under(temp.path());
    adopt_device_key(&paths, &device_seed).unwrap();
    write_device_cert(&paths.device_cert, &cert).unwrap();
    let certified = crate::identity::load_certified_mount_identity(&paths, now).unwrap();
    let (mount, credentials) = certified.into_parts();
    let transport =
        FsTransportIdentity::new(credentials, Arc::new(RwLock::new(RevocationList::new())));
    let (endpoint, lookup) = bind_endpoint_with_key(mount.secret).await.unwrap();
    (endpoint, lookup, transport, root, device)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acl::WorkspaceAcl;
    use crate::history::FileHistoryStore;
    use crate::namespace::ROOT_INO;
    use crate::record_publisher::RecordPublisher;
    use crate::record_replication::{
        RecordAction, RecordAuthorizer, RecordEnvelopeV1, RecordOperationV1,
    };
    use collab_types::{
        ContentHash, ContentRevisionV1, Ed25519PublicKey, FILE_BINDING_VERSION,
        FILE_MUTATION_TRANSACTION_VERSION, FileActor, FileBindingV1, FileMetadataV1,
        FileMutationOperationV1, FileMutationParticipantRoleV1, FileMutationParticipantV1,
        FileMutationTransactionManifestV1, FileMutationTransactionV1, FileNodeKind, FileProvenance,
        FileRecordId, FileVersionId, FileVersionV1, IndexStatus,
    };
    use std::time::Duration;

    async fn bind_certified_endpoint() -> (Endpoint, MemoryLookup, FsTransportIdentity) {
        bind_certified_test_endpoint().await
    }

    #[tokio::test]
    async fn thumbnail_prefetch_isolated_retries_without_another_namespace_change() {
        let source_endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let source_store = ContentStore::memory();
        let ready_thumbnail = source_store.ingest("ready preview").await.unwrap();
        let source_router = Router::builder(source_endpoint.clone())
            .accept(iroh_blobs::ALPN, source_store.blobs_protocol(None))
            .spawn();

        // This hash is real but its serving endpoint is intentionally absent
        // during the first pass, modelling an auth warm-up/offline source.
        let late_store = ContentStore::memory();
        let late_thumbnail = late_store.ingest("late preview").await.unwrap();
        let client_endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let local_store = Arc::new(ContentStore::memory());
        let hydrator = Hydrator::new(client_endpoint.clone(), Arc::clone(&local_store));
        hydrator.add_peer(source_endpoint.addr());
        let mut namespace = Namespace::new("preview-retry").unwrap();
        let ready = namespace
            .create_file(ROOT_INO, "ready.png", None, 0, 0)
            .unwrap();
        let late = namespace
            .create_file(ROOT_INO, "late.png", None, 0, 0)
            .unwrap();
        assert!(namespace.set_thumb(ready, &ready_thumbnail));
        assert!(namespace.set_thumb(late, &late_thumbnail));
        let namespace = Arc::new(Mutex::new(namespace));
        let mut pending = HashMap::new();

        prefetch_namespace_thumbnails(&namespace, &hydrator, &mut pending, true)
            .await
            .unwrap();
        assert!(local_store.has(ready_thumbnail).await);
        assert!(pending.contains_key(&late_thumbnail));

        let late_endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let late_router = Router::builder(late_endpoint.clone())
            .accept(iroh_blobs::ALPN, late_store.blobs_protocol(None))
            .spawn();
        hydrator.add_peer(late_endpoint.addr());
        pending.insert(late_thumbnail, (0, tokio::time::Instant::now()));
        prefetch_namespace_thumbnails(&namespace, &hydrator, &mut pending, false)
            .await
            .unwrap();
        assert!(local_store.has(late_thumbnail).await);
        assert!(pending.is_empty());

        late_router.shutdown().await.unwrap();
        source_router.shutdown().await.unwrap();
        late_endpoint.close().await;
        source_endpoint.close().await;
        client_endpoint.close().await;
    }

    #[test]
    fn clean_remote_merge_changes_generation_even_without_resolution_delta() {
        let mut owner = Namespace::new("owner-preview").unwrap();
        owner
            .create_file(ROOT_INO, "preview.png", None, 0, 1)
            .unwrap();
        let document = owner.save();
        let joined = Arc::new(Mutex::new(Namespace::new("joined-preview").unwrap()));
        let before = namespace_generation(&joined);
        let resolve = joined
            .lock()
            .unwrap()
            .merge_remote_and_resolve(&document)
            .unwrap();
        assert!(resolve.is_empty(), "ordinary merge needs no repair delta");
        assert_ne!(before, namespace_generation(&joined));
    }

    /// The workspace's policy: `entitled` get the run of the tree, nothing
    /// said about anybody else. Direct entries rather than capabilities so
    /// the test states its grant outright; `reads_anything` is asked the
    /// same question either way (see `acl::tests` for the capability route).
    fn policy_granting(entitled: &[IdentityKey]) -> AclHandle {
        let mut acl = WorkspaceAcl::new();
        for id in entitled {
            acl.grant_read(*id, "/");
        }
        AclHandle::new(acl)
    }

    fn policy_read_write(writers: &[IdentityKey], readers: &[IdentityKey]) -> AclHandle {
        let mut acl = WorkspaceAcl::new();
        for id in writers {
            acl.grant_read_write(*id, "/");
        }
        for id in readers {
            acl.grant_read(*id, "/");
        }
        AclHandle::new(acl)
    }

    #[tokio::test]
    async fn signed_authority_alpn_updates_nas_and_targeted_revoke_keeps_owner() {
        use crate::workspace_authority::{WorkspaceAuthority, WorkspaceAuthorityEnvelope};
        use collab_types::{CapOverride, Effect, Scope, Subject, Verb};

        let (nas_endpoint, _lookup, nas_identity) = bind_certified_endpoint().await;
        let (owner_endpoint, _lookup, owner_identity, owner_root, _owner_device) =
            bind_certified_test_endpoint_with_root().await;
        let (_collab_endpoint, _lookup, collab_identity, collab_root, _collab_device) =
            bind_certified_test_endpoint_with_root().await;
        let workspace = uuid::Uuid::new_v4();
        let temp = tempfile::tempdir().unwrap();
        let nas_authority = Arc::new(
            WorkspaceAuthority::open(
                temp.path().join("nas-authority.bin"),
                workspace,
                owner_root.verifying_key().to_bytes(),
                collab_root.verifying_key().to_bytes(),
                Vec::new(),
            )
            .unwrap(),
        );
        let router = Router::builder(nas_endpoint.clone())
            .accept(
                QOS_FS_AUTHORITY_ALPN,
                AuthorityProtocol {
                    endpoint: nas_endpoint.clone(),
                    identity: nas_identity,
                    policy: None,
                    authority: nas_authority.clone(),
                    workspace_id: workspace,
                },
            )
            .spawn();
        let owner = owner_root.verifying_key().to_bytes();
        let collaborator = collab_root.verifying_key().to_bytes();
        let grant = WorkspaceAuthorityEnvelope::sign(
            workspace,
            1,
            vec![
                CapOverride::new(
                    Subject::User(collab_types::Ed25519PublicKey(owner)),
                    Scope::FsPath("/".into()),
                    Effect::Grant,
                    Verb::Read,
                ),
                CapOverride::new(
                    Subject::User(collab_types::Ed25519PublicKey(collaborator)),
                    Scope::FsPath("/".into()),
                    Effect::Grant,
                    Verb::Read,
                ),
            ],
            false,
            &owner_root,
        )
        .unwrap();
        let accepted = submit_workspace_authority(
            &owner_endpoint,
            nas_endpoint.addr(),
            &owner_identity,
            workspace,
            Some(grant),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(accepted.revision, 1);
        assert!(
            nas_authority
                .handle()
                .permits(&collaborator, "/shared", crate::Access::Read)
        );

        let revoked = WorkspaceAuthorityEnvelope::sign(
            workspace,
            2,
            vec![
                CapOverride::new(
                    Subject::User(collab_types::Ed25519PublicKey(owner)),
                    Scope::FsPath("/".into()),
                    Effect::Grant,
                    Verb::Read,
                ),
                CapOverride::new(
                    Subject::User(collab_types::Ed25519PublicKey(collaborator)),
                    Scope::FsPath("/".into()),
                    Effect::Restrict,
                    Verb::Read,
                ),
            ],
            false,
            &owner_root,
        )
        .unwrap();
        submit_workspace_authority(
            &owner_endpoint,
            nas_endpoint.addr(),
            &owner_identity,
            workspace,
            Some(revoked.clone()),
        )
        .await
        .unwrap();
        assert!(
            !nas_authority
                .handle()
                .permits(&collaborator, "/shared", crate::Access::Read)
        );
        let owner_view = WorkspaceAuthority::open(
            temp.path().join("owner-view.bin"),
            workspace,
            owner,
            owner,
            Vec::new(),
        )
        .unwrap();
        owner_view.apply(revoked).unwrap();
        assert!(
            owner_view
                .handle()
                .permits(&owner, "/shared", crate::Access::Read)
        );
        router.shutdown().await.unwrap();
        nas_endpoint.close().await;
        owner_endpoint.close().await;
        drop(collab_identity);
    }

    /// A content request is not trusted merely because its hash is known.
    /// The collaborator first bootstraps through the authenticated
    /// filesystem protocol; its subsequent blob request is served by that
    /// workspace's guarded iroh-blobs handler. Replacing the signed
    /// authority while that certified relationship is live must deny the
    /// collaborator's *next* request while preserving governor access.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn signed_targeted_revoke_denies_an_authenticated_collaborator_blob_request() {
        use crate::workspace_authority::{WorkspaceAuthority, WorkspaceAuthorityEnvelope};
        use collab_types::{CapOverride, Effect, Scope, Subject, Verb};

        let temp = tempfile::tempdir().unwrap();
        let (nas_endpoint, nas_lookup, nas_identity) = bind_certified_endpoint().await;
        let (owner_endpoint, _owner_lookup, owner_identity, owner_root, _owner_device) =
            bind_certified_test_endpoint_with_root().await;
        let (collab_endpoint, _collab_lookup, collab_identity, collab_root, _collab_device) =
            bind_certified_test_endpoint_with_root().await;
        let workspace_id = Ticket::workspace_id_for_topic(*nas_endpoint.id().as_bytes());
        let owner = owner_root.verifying_key().to_bytes();
        let collaborator = collab_root.verifying_key().to_bytes();

        let authority = Arc::new(
            WorkspaceAuthority::open(
                temp.path().join("nas-authority.bin"),
                workspace_id,
                owner,
                collaborator,
                Vec::new(),
            )
            .unwrap(),
        );
        let mut services = record_services(temp.path());
        services.workspace_authority = Some(authority.clone());
        let nas_store = Arc::new(ContentStore::memory());
        let nas_hydrator = Arc::new(Hydrator::new(nas_endpoint.clone(), nas_store.clone()));
        let nas_namespace = Arc::new(Mutex::new(Namespace::new("nas").unwrap()));
        let first = b"collaborator is initially entitled";
        let first_hash = nas_store.write_content(first).await.unwrap().unwrap();
        nas_namespace
            .lock()
            .unwrap()
            .create_file(
                ROOT_INO,
                "before-revoke.txt",
                Some(&first_hash),
                first.len() as u64,
                1,
            )
            .unwrap();
        let (_nas_workspace, ticket) = Workspace::host_with_records(
            nas_endpoint.clone(),
            nas_lookup,
            nas_store.clone(),
            nas_hydrator,
            nas_namespace.clone(),
            nas_identity,
            Some(authority.handle()),
            services,
        )
        .await
        .unwrap();

        let grant = WorkspaceAuthorityEnvelope::sign(
            workspace_id,
            1,
            vec![
                CapOverride::new(
                    Subject::User(collab_types::Ed25519PublicKey(owner)),
                    Scope::FsPath("/".into()),
                    Effect::Grant,
                    Verb::Read,
                ),
                CapOverride::new(
                    Subject::User(collab_types::Ed25519PublicKey(collaborator)),
                    Scope::FsPath("/".into()),
                    Effect::Grant,
                    Verb::Read,
                ),
            ],
            false,
            &owner_root,
        )
        .unwrap();
        assert_eq!(
            submit_workspace_authority(
                &owner_endpoint,
                nas_endpoint.addr(),
                &owner_identity,
                workspace_id,
                Some(grant.clone()),
            )
            .await
            .unwrap(),
            Some(grant),
        );

        // Bootstrap over QOS_FS_SYNC_ALPN before the content request. This
        // gives the server the certificate-to-device association that its
        // guarded blob protocol requires; a bare hash-only endpoint cannot
        // obtain that association.
        let document = tokio::time::timeout(
            Duration::from_secs(12),
            fetch_remote_doc(&collab_endpoint, ticket.node.clone(), &collab_identity),
        )
        .await
        .expect("authenticated collaborator bootstrap timed out")
        .expect("signed collaborator bootstraps through the filesystem protocol");
        let collab_namespace = Namespace::from_bytes("collab", &document).unwrap();
        assert!(collab_namespace.entry_at("before-revoke.txt").is_some());
        let collab_store = Arc::new(ContentStore::memory());
        let collab_hydrator = Hydrator::new(collab_endpoint.clone(), collab_store.clone());
        collab_hydrator.add_peer(ticket.node.clone());
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(12),
                collab_hydrator.read(first_hash, 0, first.len() as u64),
            )
            .await
            .expect("authenticated collaborator blob request timed out")
            .unwrap()
            .as_slice(),
            first,
        );

        let revoke = WorkspaceAuthorityEnvelope::sign(
            workspace_id,
            2,
            vec![
                CapOverride::new(
                    Subject::User(collab_types::Ed25519PublicKey(owner)),
                    Scope::FsPath("/".into()),
                    Effect::Grant,
                    Verb::Read,
                ),
                CapOverride::new(
                    Subject::User(collab_types::Ed25519PublicKey(collaborator)),
                    Scope::FsPath("/".into()),
                    Effect::Restrict,
                    Verb::Read,
                ),
            ],
            false,
            &owner_root,
        )
        .unwrap();
        assert_eq!(
            submit_workspace_authority(
                &owner_endpoint,
                nas_endpoint.addr(),
                &owner_identity,
                workspace_id,
                Some(revoke.clone()),
            )
            .await
            .unwrap(),
            Some(revoke),
        );

        let after = b"this remains available only to the owner";
        let after_hash = nas_store.write_content(after).await.unwrap().unwrap();
        {
            let mut namespace = nas_namespace.lock().unwrap();
            namespace
                .create_file(
                    ROOT_INO,
                    "after-revoke.txt",
                    Some(&after_hash),
                    after.len() as u64,
                    2,
                )
                .unwrap();
            namespace.save_incremental();
        }
        assert!(
            tokio::time::timeout(
                Duration::from_secs(12),
                collab_hydrator.read(after_hash, 0, after.len() as u64),
            )
            .await
            .expect("revoked collaborator blob request timed out")
            .is_err(),
            "an already-authenticated collaborator is denied by the revised authority"
        );
        assert!(
            tokio::time::timeout(
                Duration::from_secs(12),
                fetch_remote_doc(&collab_endpoint, ticket.node.clone(), &collab_identity),
            )
            .await
            .expect("revoked collaborator namespace request timed out")
            .is_err(),
            "the same authenticated collaborator loses its next namespace request too"
        );

        let owner_store = Arc::new(ContentStore::memory());
        let owner_hydrator = Hydrator::new(owner_endpoint.clone(), owner_store);
        owner_hydrator.add_peer(nas_endpoint.addr());
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(12),
                owner_hydrator.read(after_hash, 0, after.len() as u64),
            )
            .await
            .expect("owner blob request timed out")
            .unwrap()
            .as_slice(),
            after,
            "targeted revocation leaves the governor's authenticated content access intact"
        );
    }

    /// The identity an endpoint authenticates as — the same 32 bytes an ACL
    /// grant names.
    fn identity(endpoint: &Endpoint) -> IdentityKey {
        *endpoint.id().as_bytes()
    }

    async fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
        for _ in 0..100 {
            if cond() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        cond()
    }

    fn record_services(root: &std::path::Path) -> RecordServices {
        RecordServices {
            authority: Arc::new(ShareAuthority::open(root.join("share-authority.json")).unwrap()),
            repository: Arc::new(RecordRepository::open(root.join("records")).unwrap()),
            access_registry: Arc::new(RecordAccessRegistry::new(32, 60_000).unwrap()),
            mutation_sink: None,
            materializer: None,
            availability: None,
            workspace_authority: None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::too_many_lines)] // Durable acknowledgement lifecycle scenario.
    async fn certified_peer_acknowledges_only_a_durable_exact_version_and_replay_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let (server_ep, _server_lookup, server_identity, _server_root, server_device) =
            bind_certified_test_endpoint_with_root().await;
        let (client_ep, client_lookup, client_identity, _client_root, _client_device) =
            bind_certified_test_endpoint_with_root().await;
        client_lookup.add_endpoint_info(server_ep.addr());

        let workspace_id = uuid::Uuid::new_v4();
        let record_id = FileRecordId::new();
        let version_id = FileVersionId::new();
        let store = Arc::new(
            ContentStore::open(temp.path().join("durable-blobs"))
                .await
                .unwrap(),
        );
        let bytes = b"durably replicated bytes";
        let manifest = store.write_content(bytes).await.unwrap().unwrap();
        let content_hash = Some(ContentHash(*manifest.as_bytes()));
        let metadata_hash = ContentHash([9; 32]);
        let actor = FileActor {
            person: Ed25519PublicKey(server_identity.user_public_key()),
            device: Ed25519PublicKey(server_identity.device_public_key()),
        };
        let repository =
            Arc::new(RecordRepository::open(temp.path().join("durable-records")).unwrap());
        RecordPublisher::new(Arc::clone(&repository), workspace_id, actor, server_device)
            .unwrap()
            .publish(
                &FileMetadataV1 {
                    schema_version: 1,
                    record_id,
                    content_hash,
                    byte_len: bytes.len() as u64,
                    mime: Some("application/octet-stream".into()),
                    previews: Vec::new(),
                    latest_actor: Some(actor),
                    observed_at_ms: now_ms(),
                    index_status: IndexStatus::Pending,
                    properties: None,
                },
                &[FileVersionV1 {
                    id: version_id,
                    record_id,
                    parents: Vec::new(),
                    actor,
                    timestamp_ms: now_ms(),
                    content_hash,
                    metadata_hash,
                    provenance: FileProvenance::Create,
                }],
            )
            .unwrap();
        let revision = repository
            .reconciliation_batch(RecordRepositoryKey {
                workspace_id,
                record_id,
            })
            .unwrap()
            .snapshot
            .content_revision
            .unwrap();

        let protocol = ReplicaAckProtocol {
            endpoint: server_ep.clone(),
            identity: server_identity,
            policy: None,
            workspace_id,
            repository,
            store,
        };
        let _router = Router::builder(server_ep.clone())
            .accept(QOS_FS_REPLICA_ACK_ALPN, protocol)
            .spawn();

        let request = ReplicaAckRequestV1::new(workspace_id, revision).unwrap();
        let observation = request_replica_ack(
            &client_ep,
            server_ep.addr(),
            &client_identity,
            &None,
            &request,
        )
        .await
        .unwrap();

        let path = temp.path().join("availability.json");
        let registry = Arc::new(Mutex::new(
            crate::availability::AvailabilityRegistry::open(&path).unwrap(),
        ));
        persist_workspace_replica_observation(
            &registry,
            revision,
            Ed25519PublicKey(client_identity.device_public_key()),
            observation.clone(),
            observation.observed_at_ms,
        )
        .unwrap();
        let mut registry = registry.lock().unwrap();
        assert_eq!(
            registry.get(record_id).unwrap().current_version,
            Some(version_id),
            "the shipped whole-workspace ACK path creates the ordinary record row first"
        );
        assert!(matches!(
            registry.observe_replica(
                record_id,
                Ed25519PublicKey(client_identity.device_public_key()),
                observation.clone(),
                observation.observed_at_ms,
            ),
            Err(crate::availability::AvailabilityError::ReplicaReplay)
        ));
        drop(registry);
        let reopened = crate::availability::AvailabilityRegistry::open(path).unwrap();
        assert_eq!(
            reopened.get(record_id).unwrap().replica_observations.len(),
            1
        );

        let mut wrong_revision = revision;
        wrong_revision.version_id = FileVersionId::new();
        let wrong = ReplicaAckRequestV1::new(workspace_id, wrong_revision).unwrap();
        assert!(
            request_replica_ack(
                &client_ep,
                server_ep.addr(),
                &client_identity,
                &None,
                &wrong,
            )
            .await
            .is_err()
        );
    }

    fn publish_record_metadata(
        repository: Arc<RecordRepository>,
        workspace_id: uuid::Uuid,
        record_id: FileRecordId,
        actor: FileActor,
        device: ed25519_dalek::SigningKey,
        byte_len: u64,
    ) {
        RecordPublisher::new(repository, workspace_id, actor, device)
            .unwrap()
            .publish(
                &FileMetadataV1 {
                    schema_version: 1,
                    record_id,
                    content_hash: None,
                    byte_len,
                    mime: Some("application/octet-stream".into()),
                    previews: vec![],
                    latest_actor: Some(actor),
                    observed_at_ms: byte_len,
                    index_status: IndexStatus::Pending,
                    properties: None,
                },
                &[],
            )
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)] // Explicit inputs expose fixture authority boundaries.
    async fn publish_durable_test_revision(
        repository: Arc<RecordRepository>,
        store: &ContentStore,
        workspace_id: uuid::Uuid,
        record_id: FileRecordId,
        version_id: FileVersionId,
        actor: FileActor,
        device: ed25519_dalek::SigningKey,
        bytes: &[u8],
        metadata_byte: u8,
    ) -> ContentRevisionV1 {
        let manifest = store.write_content(bytes).await.unwrap().unwrap();
        let metadata_hash = ContentHash([metadata_byte; 32]);
        RecordPublisher::new(Arc::clone(&repository), workspace_id, actor, device)
            .unwrap()
            .publish(
                &FileMetadataV1 {
                    schema_version: collab_types::files::FILE_METADATA_VERSION,
                    record_id,
                    content_hash: Some(ContentHash(*manifest.as_bytes())),
                    byte_len: bytes.len() as u64,
                    mime: Some("application/octet-stream".into()),
                    previews: vec![],
                    latest_actor: Some(actor),
                    observed_at_ms: 1,
                    index_status: IndexStatus::Pending,
                    properties: None,
                },
                &[FileVersionV1 {
                    id: version_id,
                    record_id,
                    parents: vec![],
                    actor,
                    timestamp_ms: 1,
                    content_hash: Some(ContentHash(*manifest.as_bytes())),
                    metadata_hash,
                    provenance: FileProvenance::Create,
                }],
            )
            .unwrap();
        repository
            .reconciliation_batch(RecordRepositoryKey {
                workspace_id,
                record_id,
            })
            .unwrap()
            .snapshot
            .content_revision
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    #[allow(clippy::too_many_lines)] // Three-peer eviction/rehydration scenario.
    async fn three_live_peers_require_a_fresh_exact_replica_before_eviction_and_rehydrate_after_gc()
    {
        use crate::availability::{
            AvailabilityError, AvailabilityRegistry, EvictionBlocker, EvictionDecision,
            EvictionRequirements, RecordAvailability, RecordKind, Residency,
        };
        use crate::eviction::{
            EvictionCatalog, EvictionQueue, EvictionRequestState, PlanError, plan_safe_eviction,
        };

        let temp = tempfile::tempdir().unwrap();
        let (owner_ep, _owner_lookup, owner_identity, _owner_root, _owner_device) =
            bind_certified_test_endpoint_with_root().await;
        let (durable_ep, _durable_lookup, durable_identity, durable_root, durable_device) =
            bind_certified_test_endpoint_with_root().await;
        let (stale_ep, _stale_lookup, stale_identity, stale_root, stale_device) =
            bind_certified_test_endpoint_with_root().await;
        let roots = [
            owner_identity.user_public_key(),
            durable_identity.user_public_key(),
            stale_identity.user_public_key(),
        ];
        assert!(roots[0] != roots[1] && roots[0] != roots[2] && roots[1] != roots[2]);

        let workspace_id = uuid::Uuid::new_v4();
        let mut durable_namespace = Namespace::new("three-peer-durable-replica").unwrap();
        let durable_ino = durable_namespace
            .create_file(ROOT_INO, "replicated.bin", None, 1, 1)
            .unwrap();
        let record_id = durable_namespace.record_id_for(durable_ino).unwrap();
        let current_version = FileVersionId::new();
        let stale_version = FileVersionId::new();
        let current_bytes = b"current durable collaborative payload";
        let stale_bytes = b"obsolete payload";
        let owner_store_path = temp.path().join("owner-blobs");
        let owner_store = Arc::new(
            ContentStore::open_with_gc_interval(&owner_store_path, Duration::from_millis(10))
                .await
                .unwrap(),
        );
        let durable_store = Arc::new(
            ContentStore::open(temp.path().join("durable-blobs"))
                .await
                .unwrap(),
        );
        let stale_store = Arc::new(
            ContentStore::open(temp.path().join("stale-blobs"))
                .await
                .unwrap(),
        );
        assert!(!Arc::ptr_eq(&owner_store, &durable_store));
        assert!(!Arc::ptr_eq(&owner_store, &stale_store));

        let durable_repository =
            Arc::new(RecordRepository::open(temp.path().join("durable-records")).unwrap());
        let stale_repository =
            Arc::new(RecordRepository::open(temp.path().join("stale-records")).unwrap());
        let durable_actor = FileActor {
            person: Ed25519PublicKey(durable_root.verifying_key().to_bytes()),
            device: Ed25519PublicKey(durable_identity.device_public_key()),
        };
        let stale_actor = FileActor {
            person: Ed25519PublicKey(stale_root.verifying_key().to_bytes()),
            device: Ed25519PublicKey(stale_identity.device_public_key()),
        };
        let current_revision = publish_durable_test_revision(
            Arc::clone(&durable_repository),
            &durable_store,
            workspace_id,
            record_id,
            current_version,
            durable_actor,
            durable_device,
            current_bytes,
            71,
        )
        .await;
        assert!(durable_namespace.set_content_revision(durable_ino, &current_revision, 1,));
        let durable_namespace = Arc::new(Mutex::new(durable_namespace));
        let stale_revision = publish_durable_test_revision(
            Arc::clone(&stale_repository),
            &stale_store,
            workspace_id,
            record_id,
            stale_version,
            stale_actor,
            stale_device,
            stale_bytes,
            72,
        )
        .await;
        let owner_manifest = owner_store
            .write_content(current_bytes)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            current_revision.content_hash,
            Some(ContentHash(*owner_manifest.as_bytes()))
        );

        let durable_policy = policy_granting(&[identity(&owner_ep)]);
        let stale_policy = policy_granting(&[identity(&owner_ep)]);
        let durable_ack = ReplicaAckProtocol {
            endpoint: durable_ep.clone(),
            identity: durable_identity.clone(),
            policy: Some(durable_policy.clone()),
            workspace_id,
            repository: Arc::clone(&durable_repository),
            store: Arc::clone(&durable_store),
        };
        let stale_ack = ReplicaAckProtocol {
            endpoint: stale_ep.clone(),
            identity: stale_identity.clone(),
            policy: Some(stale_policy),
            workspace_id,
            repository: Arc::clone(&stale_repository),
            store: Arc::clone(&stale_store),
        };
        let _durable_router = Router::builder(durable_ep.clone())
            .accept(
                iroh_blobs::ALPN,
                durable_store.blobs_protocol(blob_guard(
                    Some(durable_policy),
                    Arc::clone(&durable_namespace),
                    Arc::clone(&durable_store),
                    None,
                    None,
                )),
            )
            .accept(QOS_FS_REPLICA_ACK_ALPN, durable_ack)
            .spawn();
        let _stale_router = Router::builder(stale_ep.clone())
            .accept(iroh_blobs::ALPN, stale_store.blobs_protocol(None))
            .accept(QOS_FS_REPLICA_ACK_ALPN, stale_ack)
            .spawn();
        let owner_policy = Some(policy_read_write(
            &[identity(&durable_ep), identity(&stale_ep)],
            &[],
        ));

        let current_request = ReplicaAckRequestV1::new(workspace_id, current_revision).unwrap();
        assert!(
            request_replica_ack(
                &owner_ep,
                stale_ep.addr(),
                &owner_identity,
                &owner_policy,
                &current_request,
            )
            .await
            .is_err(),
            "a peer holding only an obsolete version must not acknowledge the current revision"
        );
        let stale_request = ReplicaAckRequestV1::new(workspace_id, stale_revision).unwrap();
        let stale_observation = request_replica_ack(
            &owner_ep,
            stale_ep.addr(),
            &owner_identity,
            &owner_policy,
            &stale_request,
        )
        .await
        .unwrap();

        let availability_path = temp.path().join("owner-availability.json");
        let mut availability = AvailabilityRegistry::open(&availability_path).unwrap();
        let mut record = RecordAvailability::new(record_id, RecordKind::File);
        record.current_version = Some(current_version);
        record.residency = Residency::Resident {
            bytes: current_bytes.len() as u64,
        };
        availability.put(record).unwrap();
        assert!(matches!(
            availability.observe_replica(
                record_id,
                Ed25519PublicKey(owner_identity.device_public_key()),
                stale_observation.clone(),
                stale_observation.observed_at_ms,
            ),
            Err(AvailabilityError::ReplicaVersionMismatch)
        ));

        let payload = owner_store
            .inspect_version_payload(record_id, current_version, owner_manifest)
            .await
            .unwrap();
        let catalog = EvictionCatalog::new([payload]).unwrap();
        let requirements = EvictionRequirements {
            minimum_reachable: 1,
            minimum_durable: 1,
            maximum_replica_age_ms: 60_000,
        };
        assert!(matches!(
            plan_safe_eviction(
                &availability,
                &catalog,
                record_id,
                stale_observation.observed_at_ms,
                requirements,
            ),
            Err(PlanError::Denied(_))
        ));

        let fresh_observation = request_replica_ack(
            &owner_ep,
            durable_ep.addr(),
            &owner_identity,
            &owner_policy,
            &current_request,
        )
        .await
        .unwrap();
        availability
            .observe_replica(
                record_id,
                Ed25519PublicKey(owner_identity.device_public_key()),
                fresh_observation.clone(),
                fresh_observation.observed_at_ms,
            )
            .unwrap();
        assert_eq!(
            availability.eviction_decision(
                record_id,
                fresh_observation.observed_at_ms,
                requirements,
                false,
            ),
            EvictionDecision::Allowed
        );
        assert_eq!(
            availability.eviction_decision(
                record_id,
                fresh_observation
                    .observed_at_ms
                    .saturating_add(requirements.maximum_replica_age_ms + 1),
                requirements,
                false,
            ),
            EvictionDecision::Denied(EvictionBlocker::ReplicaAcknowledgementStale)
        );

        let plan = plan_safe_eviction(
            &availability,
            &catalog,
            record_id,
            fresh_observation.observed_at_ms,
            requirements,
        )
        .unwrap();
        assert!(!plan.reclaimable.is_empty());
        let mut queue = EvictionQueue::open(temp.path().join("owner-evictions.json")).unwrap();
        let request_id = queue
            .enqueue(&plan, fresh_observation.observed_at_ms)
            .unwrap()
            .id;
        owner_store
            .release_eviction_to_backend_gc(
                &queue,
                request_id,
                &availability,
                &catalog,
                fresh_observation.observed_at_ms,
                requirements,
            )
            .await
            .unwrap();

        // A clean store restart clears the process cache and lets configured
        // backend GC finish the durable release. Completion is gated by exact
        // backend observation rather than a fixed delay.
        owner_store.store().shutdown().await.unwrap();
        drop(owner_store);
        let owner_store = Arc::new(
            ContentStore::open_with_gc_interval(&owner_store_path, Duration::from_millis(10))
                .await
                .unwrap(),
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let observation = owner_store
                    .observe_eviction_request(
                        &mut queue,
                        request_id,
                        fresh_observation.observed_at_ms.saturating_add(1),
                    )
                    .await
                    .unwrap();
                if matches!(observation.state, EvictionRequestState::Completed { .. }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("backend GC did not emit an exact absence observation");
        assert!(owner_store.local(owner_manifest).await.is_none());

        let owner_hydrator = Hydrator::new(owner_ep.clone(), Arc::clone(&owner_store));
        owner_hydrator.add_peer(durable_ep.addr());
        let recovered = owner_hydrator
            .read(owner_manifest, 0, current_bytes.len() as u64)
            .await
            .unwrap();
        assert_eq!(recovered, current_bytes);
        owner_store
            .verify_durable_revision(&current_revision)
            .await
            .unwrap();
    }

    #[derive(Clone, Copy)]
    struct AllowRecordActor(FileActor);

    impl RecordAuthorizer for AllowRecordActor {
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

    /// Exercise replacement transactions through two real authenticated Iroh
    /// endpoints. The receiver never reads or mutates the sender repository
    /// directly: each asserted boundary follows a completed inventory/pull
    /// exchange over the record-mesh ALPN.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::too_many_lines)] // Both staging orders and restart recovery.
    async fn live_record_mesh_replacement_transaction_survives_staging_restart_in_both_orders() {
        for source_first in [true, false] {
            let server_temp = tempfile::tempdir().unwrap();
            let client_temp = tempfile::tempdir().unwrap();
            let (server_ep, server_lookup, server_identity, server_root, server_device) =
                bind_certified_test_endpoint_with_root().await;
            let (client_ep, _client_lookup, client_identity, _client_root, _client_device) =
                bind_certified_test_endpoint_with_root().await;
            assert_ne!(
                server_identity.user_public_key(),
                client_identity.user_public_key(),
                "the live peers must authenticate under separate identity roots"
            );
            let workspace_id = Ticket::workspace_id_for_topic(*server_ep.id().as_bytes());
            let actor = FileActor {
                person: Ed25519PublicKey(server_root.verifying_key().to_bytes()),
                device: Ed25519PublicKey(server_identity.device_public_key()),
            };
            let authorizer = AllowRecordActor(actor);

            let mut server_namespace = Namespace::new("live-record-mesh-server").unwrap();
            let source_ino = server_namespace
                .create_file(ROOT_INO, "source.txt", None, 1, 1)
                .unwrap();
            let replaced_ino = server_namespace
                .create_file(ROOT_INO, "destination.txt", None, 1, 1)
                .unwrap();
            let source_record = server_namespace.record_id_for(source_ino).unwrap();
            let replaced_record = server_namespace.record_id_for(replaced_ino).unwrap();
            let root_record = server_namespace.record_id_for(ROOT_INO).unwrap();
            let source_expected = FileVersionId::new();
            let replaced_expected = FileVersionId::new();
            for (ino, record_id, version_id, hash) in [
                (
                    source_ino,
                    source_record,
                    source_expected,
                    ContentHash([61; 32]),
                ),
                (
                    replaced_ino,
                    replaced_record,
                    replaced_expected,
                    ContentHash([62; 32]),
                ),
            ] {
                assert!(server_namespace.set_content_revision(
                    ino,
                    &ContentRevisionV1 {
                        schema_version: collab_types::CONTENT_REVISION_VERSION,
                        record_id,
                        version_id,
                        content_hash: Some(hash),
                        byte_len: 1,
                        metadata_hash: ContentHash([63; 32]),
                    },
                    1,
                ));
            }
            let namespace_seed = server_namespace.save();
            let server_namespace = Arc::new(Mutex::new(server_namespace));

            let server_services = record_services(server_temp.path());
            let publisher = RecordPublisher::new(
                Arc::clone(&server_services.repository),
                workspace_id,
                actor,
                server_device.clone(),
            )
            .unwrap();
            for (record_id, version_id, name, hash) in [
                (
                    source_record,
                    source_expected,
                    "source.txt",
                    ContentHash([61; 32]),
                ),
                (
                    replaced_record,
                    replaced_expected,
                    "destination.txt",
                    ContentHash([62; 32]),
                ),
            ] {
                publisher
                    .publish_binding(&FileBindingV1 {
                        schema_version: FILE_BINDING_VERSION,
                        record_id,
                        parent: Some(root_record),
                        name: name.into(),
                        kind: FileNodeKind::File,
                        tombstoned: false,
                        actor,
                        observed_at_ms: 1,
                    })
                    .unwrap();
                publisher
                    .publish_structural_version(&FileVersionV1 {
                        id: version_id,
                        record_id,
                        parents: vec![],
                        actor,
                        timestamp_ms: 1,
                        content_hash: Some(hash),
                        metadata_hash: ContentHash([63; 32]),
                        provenance: FileProvenance::Create,
                    })
                    .unwrap();
            }

            let participant = |record_id, expected_version, role, hash| {
                let version = FileVersionV1 {
                    id: FileVersionId::new(),
                    record_id,
                    parents: vec![expected_version],
                    actor,
                    timestamp_ms: 2,
                    content_hash: Some(hash),
                    metadata_hash: ContentHash([63; 32]),
                    provenance: match role {
                        FileMutationParticipantRoleV1::Source => FileProvenance::Rename,
                        FileMutationParticipantRoleV1::Replaced => FileProvenance::Delete,
                        _ => unreachable!("replacement fixture role"),
                    },
                };
                let mut participant = FileMutationParticipantV1 {
                    record_id,
                    role,
                    expected_version,
                    proposed_binding: FileBindingV1 {
                        schema_version: FILE_BINDING_VERSION,
                        record_id,
                        parent: Some(root_record),
                        name: "destination.txt".into(),
                        kind: FileNodeKind::File,
                        tombstoned: role == FileMutationParticipantRoleV1::Replaced,
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
                    FileMutationParticipantRoleV1::Source,
                    ContentHash([61; 32]),
                ),
                participant(
                    replaced_record,
                    replaced_expected,
                    FileMutationParticipantRoleV1::Replaced,
                    ContentHash([62; 32]),
                ),
            ];
            participants.sort_by_key(|participant| participant.record_id);
            let transaction = FileMutationTransactionV1::signed(
                FileMutationTransactionManifestV1 {
                    schema_version: FILE_MUTATION_TRANSACTION_VERSION,
                    transaction_id: uuid::Uuid::new_v4(),
                    workspace_id,
                    actor,
                    timestamp_ms: 2,
                    operation: FileMutationOperationV1::ReplacementRename {
                        source_parent: root_record,
                        source_name: "source.txt".into(),
                        destination_parent: root_record,
                        destination_name: "destination.txt".into(),
                        source_record,
                        replaced_record,
                    },
                    participants,
                },
                &server_device,
            )
            .unwrap();

            let mut server_acl = WorkspaceAcl::new();
            server_acl.grant_read(identity(&client_ep), "/");
            let server_policy = Some(AclHandle::new(server_acl));
            let server_store = Arc::new(ContentStore::memory());
            let client_store = Arc::new(ContentStore::memory());
            assert!(!Arc::ptr_eq(&server_store, &client_store));
            let server_hydrator =
                Arc::new(Hydrator::new(server_ep.clone(), Arc::clone(&server_store)));
            let (_server_workspace, ticket) = Workspace::host_with_records(
                server_ep.clone(),
                server_lookup,
                Arc::clone(&server_store),
                server_hydrator,
                server_namespace,
                server_identity.clone(),
                server_policy,
                server_services.clone(),
            )
            .await
            .unwrap();
            assert_eq!(ticket.workspace_id(), workspace_id);

            let client_policy = Some(policy_read_write(&[identity(&server_ep)], &[]));
            let client_repository_path = client_temp.path().join("records");
            let client_history_path = client_temp.path().join("history.json");
            let client_transaction_journal_path = client_temp.path().join("record-transactions");
            let client_namespace_path = client_temp.path().join("namespace.bin");
            let client_repository =
                Arc::new(RecordRepository::open(&client_repository_path).unwrap());
            let client_history = Arc::new(FileHistoryStore::open(&client_history_path).unwrap());
            let client_namespace = Arc::new(Mutex::new(
                Namespace::from_bytes("live-record-mesh-client", &namespace_seed).unwrap(),
            ));
            let client_materializer = Arc::new(
                crate::record_materializer::RecordMaterializer::new(
                    workspace_id,
                    Arc::clone(&client_namespace),
                    Some(Arc::clone(&client_history)),
                )
                .with_transaction_recovery(
                    Arc::new(Mutex::new(
                        crate::record_transaction::RecordTransactionJournal::open(
                            &client_transaction_journal_path,
                        )
                        .unwrap(),
                    )),
                    client_namespace_path.clone(),
                ),
            );
            let client_services = RecordServices {
                authority: Arc::new(
                    ShareAuthority::open(client_temp.path().join("share-authority.json")).unwrap(),
                ),
                repository: Arc::clone(&client_repository),
                access_registry: Arc::new(RecordAccessRegistry::new(32, 60_000).unwrap()),
                mutation_sink: None,
                materializer: Some(Arc::clone(&client_materializer)),
                availability: None,
                workspace_authority: None,
            };
            let client_records = build_record_plane(
                &client_ep,
                &client_identity,
                &client_policy,
                Arc::clone(&client_store),
                workspace_id,
                Some(client_services.clone()),
            )
            .unwrap()
            .unwrap();

            // Awaited pull completion is the baseline barrier; no timing loop
            // or arbitrary sleep decides whether both initial streams arrived.
            pull_record_mesh_from_peer(
                &client_ep,
                server_ep.addr(),
                &client_identity,
                &client_policy,
                &client_records,
            )
            .await
            .unwrap();
            for record_id in [source_record, replaced_record] {
                assert_eq!(
                    client_repository.highest_sequence(
                        RecordRepositoryKey {
                            workspace_id,
                            record_id,
                        },
                        &actor,
                    ),
                    Some(2)
                );
                assert_eq!(
                    client_history.snapshot(record_id).unwrap().versions.len(),
                    1
                );
            }

            let delivery_order = if source_first {
                [source_record, replaced_record]
            } else {
                [replaced_record, source_record]
            };
            let publish_participant = |record_id| {
                let envelopes = [
                    RecordEnvelopeV1::signed(
                        record_id,
                        actor,
                        3,
                        RecordOperationV1::PrepareTransaction(transaction.clone()),
                        &server_device,
                    )
                    .unwrap(),
                    RecordEnvelopeV1::signed(
                        record_id,
                        actor,
                        4,
                        RecordOperationV1::CommitTransaction {
                            transaction_id: transaction.manifest.transaction_id,
                        },
                        &server_device,
                    )
                    .unwrap(),
                ];
                server_services
                    .repository
                    .apply_batch(
                        RecordRepositoryKey {
                            workspace_id,
                            record_id,
                        },
                        &envelopes,
                        &authorizer,
                    )
                    .unwrap();
            };

            publish_participant(delivery_order[0]);
            pull_record_mesh_from_peer(
                &client_ep,
                server_ep.addr(),
                &client_identity,
                &client_policy,
                &client_records,
            )
            .await
            .unwrap();
            assert_eq!(
                client_repository.highest_sequence(
                    RecordRepositoryKey {
                        workspace_id,
                        record_id: delivery_order[0],
                    },
                    &actor,
                ),
                Some(4)
            );
            assert!(
                !client_repository
                    .transaction_status(workspace_id, transaction.manifest.transaction_id)
                    .unwrap()
                    .unwrap()
                    .complete
            );
            {
                let mut namespace = client_namespace.lock().unwrap();
                assert_eq!(
                    namespace
                        .lookup(ROOT_INO, "source.txt")
                        .and_then(|attr| namespace.record_id_for(attr.ino.0)),
                    Some(source_record)
                );
                assert_eq!(
                    namespace
                        .lookup(ROOT_INO, "destination.txt")
                        .and_then(|attr| namespace.record_id_for(attr.ino.0)),
                    Some(replaced_record)
                );
                drop(namespace);
            }

            // Restart every durable receiver-side projection while the first
            // participant is staged. The endpoint/identity remains distinct
            // and the next state change still arrives only over Iroh.
            let restarted_namespace_bytes = client_namespace.lock().unwrap().save();
            drop(client_records);
            drop(client_services);
            drop(client_materializer);
            drop(client_repository);
            drop(client_history);
            let restarted_repository =
                Arc::new(RecordRepository::open(&client_repository_path).unwrap());
            let restarted_history = Arc::new(FileHistoryStore::open(&client_history_path).unwrap());
            let restarted_namespace = Arc::new(Mutex::new(
                Namespace::from_bytes(
                    "live-record-mesh-client-restarted",
                    &restarted_namespace_bytes,
                )
                .unwrap(),
            ));
            let restarted_journal = Arc::new(Mutex::new(
                crate::record_transaction::RecordTransactionJournal::open(
                    &client_transaction_journal_path,
                )
                .unwrap(),
            ));
            let restarted_materializer = Arc::new(
                crate::record_materializer::RecordMaterializer::new(
                    workspace_id,
                    Arc::clone(&restarted_namespace),
                    Some(Arc::clone(&restarted_history)),
                )
                .with_transaction_recovery(
                    Arc::clone(&restarted_journal),
                    client_namespace_path.clone(),
                ),
            );
            let restarted_services = RecordServices {
                authority: Arc::new(
                    ShareAuthority::open(client_temp.path().join("share-authority.json")).unwrap(),
                ),
                repository: Arc::clone(&restarted_repository),
                access_registry: Arc::new(RecordAccessRegistry::new(32, 60_000).unwrap()),
                mutation_sink: None,
                materializer: Some(Arc::clone(&restarted_materializer)),
                availability: None,
                workspace_authority: None,
            };
            let restarted_records = build_record_plane(
                &client_ep,
                &client_identity,
                &client_policy,
                Arc::clone(&client_store),
                workspace_id,
                Some(restarted_services),
            )
            .unwrap()
            .unwrap();

            publish_participant(delivery_order[1]);
            pull_record_mesh_from_peer(
                &client_ep,
                server_ep.addr(),
                &client_identity,
                &client_policy,
                &restarted_records,
            )
            .await
            .unwrap();
            assert!(
                restarted_repository
                    .transaction_status(workspace_id, transaction.manifest.transaction_id)
                    .unwrap()
                    .unwrap()
                    .complete
            );
            let mut namespace = restarted_namespace.lock().unwrap();
            assert!(namespace.lookup(ROOT_INO, "source.txt").is_none());
            assert_eq!(
                namespace
                    .lookup(ROOT_INO, "destination.txt")
                    .and_then(|attr| namespace.record_id_for(attr.ino.0)),
                Some(source_record)
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
            assert!(client_namespace_path.is_file());
            assert!(restarted_journal.lock().unwrap().entries().next().is_none());
        }
    }

    #[test]
    fn ticket_roundtrips() {
        let id = iroh::SecretKey::from_bytes(&[7u8; 32]).public();
        let ticket = Ticket {
            node: EndpointAddr::new(id),
            workspace: [3u8; 32],
        };
        let decoded = Ticket::decode(&ticket.encode().unwrap()).unwrap();
        assert_eq!(decoded.node.id, id);
        assert_eq!(decoded.workspace, [3u8; 32]);
        assert_eq!(decoded.workspace_id(), ticket.workspace_id());
        assert_eq!(ticket.workspace_id(), uuid::Uuid::from_bytes([3; 16]));
    }

    /// Two real nodes bootstrap, then structural changes propagate live over
    /// gossip in BOTH directions.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn structural_changes_propagate_both_ways() {
        // --- host A: seed a dir, then start hosting ---
        let (ep_a, look_a, auth_a) = bind_certified_endpoint().await;
        let store_a = Arc::new(ContentStore::memory());
        let hyd_a = Arc::new(Hydrator::new(ep_a.clone(), store_a.clone()));
        let ns_a = Arc::new(Mutex::new(Namespace::new("A").unwrap()));
        ns_a.lock()
            .unwrap()
            .create_dir(ROOT_INO, "shared", 1)
            .unwrap();
        let (ws_a, ticket) = Workspace::host(
            ep_a.clone(),
            look_a,
            store_a.clone(),
            hyd_a.clone(),
            ns_a.clone(),
            auth_a,
            None,
        )
        .await
        .unwrap();

        // --- joiner B: bootstrap the doc, then join ---
        let (ep_b, look_b, auth_b) = bind_certified_endpoint().await;
        let store_b = Arc::new(ContentStore::memory());
        let hyd_b = Arc::new(Hydrator::new(ep_b.clone(), store_b.clone()));
        let doc = fetch_remote_doc(&ep_b, ticket.node.clone(), &auth_b)
            .await
            .unwrap();
        let ns_b = Arc::new(Mutex::new(Namespace::from_bytes("B", &doc).unwrap()));
        let ws_b = Workspace::join(
            ep_b.clone(),
            look_b,
            store_b.clone(),
            hyd_b.clone(),
            ns_b.clone(),
            ticket,
            auth_b,
            None,
        )
        .await
        .unwrap();

        // Bootstrap projection: B sees the seeded dir immediately.
        assert!(
            ns_b.lock().unwrap().entry_at("shared").is_some(),
            "joiner must see the host's seeded tree via bootstrap"
        );

        // A -> B: host creates a file post-sync (simulating a FUSE write).
        {
            let delta = {
                let mut n = ns_a.lock().unwrap();
                n.create_file(ROOT_INO, "from_a.txt", None, 0, 2).unwrap();
                n.save_incremental()
            };
            ws_a.sync.broadcast_delta(delta);
        }
        assert!(
            wait_until(|| ns_b.lock().unwrap().entry_at("from_a.txt").is_some()).await,
            "A's change must propagate to B over gossip"
        );

        // B -> A: joiner creates a file; host must see it.
        {
            let delta = {
                let mut n = ns_b.lock().unwrap();
                n.create_file(ROOT_INO, "from_b.txt", None, 0, 3).unwrap();
                n.save_incremental()
            };
            ws_b.sync.broadcast_delta(delta);
        }
        assert!(
            wait_until(|| ns_a.lock().unwrap().entry_at("from_b.txt").is_some()).await,
            "B's change must propagate back to A (bidirectional)"
        );
    }

    /// The full collaborative read-write loop: A writes a file with real
    /// content; B's namespace syncs the entry over gossip; B then hydrates
    /// the content over iroh-blobs and reads A's actual bytes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn content_written_on_one_peer_hydrates_on_another() {
        // host A
        let (ep_a, look_a, auth_a) = bind_certified_endpoint().await;
        let store_a = Arc::new(ContentStore::memory());
        let hyd_a = Arc::new(Hydrator::new(ep_a.clone(), store_a.clone()));
        let ns_a = Arc::new(Mutex::new(Namespace::new("A").unwrap()));
        let (ws_a, ticket) = Workspace::host(
            ep_a.clone(),
            look_a,
            store_a.clone(),
            hyd_a.clone(),
            ns_a.clone(),
            auth_a,
            None,
        )
        .await
        .unwrap();

        // joiner B
        let (ep_b, look_b, auth_b) = bind_certified_endpoint().await;
        let store_b = Arc::new(ContentStore::memory());
        let hyd_b = Arc::new(Hydrator::new(ep_b.clone(), store_b.clone()));
        let doc = fetch_remote_doc(&ep_b, ticket.node.clone(), &auth_b)
            .await
            .unwrap();
        let ns_b = Arc::new(Mutex::new(Namespace::from_bytes("B", &doc).unwrap()));
        let _ws_b = Workspace::join(
            ep_b.clone(),
            look_b,
            store_b.clone(),
            hyd_b.clone(),
            ns_b.clone(),
            ticket,
            auth_b,
            None,
        )
        .await
        .unwrap();

        // A writes a file: chunk+ingest content into A's store, set the entry, broadcast.
        let content = b"a shared collaborative document body";
        let hash = store_a.write_content(content).await.unwrap().unwrap();
        {
            let delta = {
                let mut n = ns_a.lock().unwrap();
                n.create_file(ROOT_INO, "doc.txt", Some(&hash), content.len() as u64, 7)
                    .unwrap();
                n.save_incremental()
            };
            ws_a.sync.broadcast_delta(delta);
        }

        // B sees the entry via gossip...
        assert!(
            wait_until(|| ns_b.lock().unwrap().entry_at("doc.txt").is_some()).await,
            "doc entry must sync to B"
        );

        // ...and hydrates the real bytes from A over iroh-blobs.
        let bhash = {
            let mut n = ns_b.lock().unwrap();
            let attr = n.lookup(ROOT_INO, "doc.txt").unwrap();
            n.hash_for(attr.ino.0).unwrap()
        };
        assert!(
            !store_b.has(bhash).await,
            "precondition: B lacks the content"
        );
        let got = hyd_b
            .read(bhash, 0, content.len() as u64)
            .await
            .expect("B hydrates content from A");
        assert_eq!(got.as_slice(), content, "B must read A's real bytes");
    }

    /// A caller the transport could not name is refused — an endpoint
    /// arriving without an id isn't served on the strength of arriving.
    #[test]
    fn an_unnamed_blob_caller_is_refused() {
        use iroh_blobs::provider::events::AbortReason;

        let invited = iroh::SecretKey::generate().public();
        let policy: PeerPolicy = Some(policy_granting(&[identity_of(invited)]));

        assert_eq!(
            admit_blob_caller(&policy, None),
            Err(AbortReason::Permission),
            "an unnamed caller cannot be checked and must not be served"
        );
        assert_eq!(
            admit_blob_caller(&policy, Some(invited)),
            Ok(identity_of(invited)),
            "and a named, entitled caller is admitted as itself"
        );
        assert_eq!(
            admit_blob_caller(&policy, Some(iroh::SecretKey::generate().public())),
            Err(AbortReason::Permission),
            "while a named stranger is refused"
        );
    }

    /// Revocation reaches the blob plane through the live handle: the same
    /// caller is admitted, then refused, with no remount in between.
    #[test]
    fn revoking_a_grant_refuses_the_next_blob_connection() {
        use iroh_blobs::provider::events::AbortReason;

        let invited = iroh::SecretKey::generate().public();
        let mut acl = WorkspaceAcl::new();
        acl.grant_read(identity_of(invited), "/");
        let handle = AclHandle::new(acl);
        let policy: PeerPolicy = Some(handle.clone());

        assert!(admit_blob_caller(&policy, Some(invited)).is_ok());
        handle.replace(WorkspaceAcl::new());
        assert_eq!(
            admit_blob_caller(&policy, Some(invited)),
            Err(AbortReason::Permission),
            "the next connection must be decided by the ACL bound now"
        );
    }

    #[tokio::test]
    async fn blob_authority_is_scoped_to_live_readable_paths() {
        let peer = identity_of(iroh::SecretKey::generate().public());
        let mut acl = WorkspaceAcl::new();
        acl.grant_read(peer, "/shared");
        let handle = AclHandle::new(acl);
        let policy = Some(handle.clone());

        let mut ns = Namespace::new("host").unwrap();
        let shared = ns.create_dir(ROOT_INO, "shared", 1).unwrap();
        let private = ns.create_dir(ROOT_INO, "private", 1).unwrap();
        let store = ContentStore::memory();
        let allowed = store
            .write_content(b"readable bytes")
            .await
            .unwrap()
            .unwrap();
        let allowed_manifest = crate::content::Manifest::decode(
            &store.local(allowed).await.expect("allowed manifest stored"),
        )
        .unwrap();
        let allowed_chunk = iroh_blobs::Hash::from(allowed_manifest.chunks[0].hash);
        let denied = store
            .write_content(b"private bytes")
            .await
            .unwrap()
            .unwrap();
        let denied_manifest = crate::content::Manifest::decode(
            &store.local(denied).await.expect("private manifest stored"),
        )
        .unwrap();
        let denied_chunk = iroh_blobs::Hash::from(denied_manifest.chunks[0].hash);
        let thumbnail = iroh_blobs::Hash::from([2; 32]);
        let ino = ns
            .create_file(shared, "allowed.png", Some(&allowed), 1, 1)
            .unwrap();
        assert!(ns.set_thumb(ino, &thumbnail));
        ns.create_file(private, "denied.txt", Some(&denied), 1, 1)
            .unwrap();
        let ns = Arc::new(Mutex::new(ns));

        assert!(peer_may_read_blob(&policy, &ns, &store, &peer, allowed).await);
        assert!(peer_may_read_blob(&policy, &ns, &store, &peer, allowed_chunk).await);
        assert!(peer_may_read_blob(&policy, &ns, &store, &peer, thumbnail).await);
        assert!(!peer_may_read_blob(&policy, &ns, &store, &peer, denied).await);
        assert!(!peer_may_read_blob(&policy, &ns, &store, &peer, denied_chunk).await);
        assert!(
            !peer_may_read_whole_workspace(&policy, &peer),
            "a subtree grant must not unlock the monolithic namespace"
        );

        handle.replace(WorkspaceAcl::new());
        assert!(
            !peer_may_read_blob(&policy, &ns, &store, &peer, allowed_chunk).await,
            "revocation must affect the next request on a live connection"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::too_many_lines)] // Every guarded blob class and get-many path.
    async fn record_session_guards_manifest_chunks_thumbnails_and_get_many_live() {
        use collab_types::{
            Ed25519PublicKey, FileInviteSpec, FileInviteSubject, FilePermission, mint_file_invite,
        };
        use iroh_blobs::protocol::{ChunkRangesSeq, GetManyRequest};
        use qos_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};

        let temp = tempfile::tempdir().unwrap();
        let (server_ep, _server_lookup, server_identity, server_root, _server_device) =
            bind_certified_test_endpoint_with_root().await;
        let (client_ep, _client_lookup, client_identity, client_root, _client_device) =
            bind_certified_test_endpoint_with_root().await;
        let workspace_id = uuid::Uuid::new_v4();
        let authority =
            Arc::new(ShareAuthority::open(temp.path().join("share-authority.json")).unwrap());
        let repository =
            Arc::new(RecordRepository::open(temp.path().join("record-repository")).unwrap());
        let access_registry = Arc::new(RecordAccessRegistry::new(32, 60_000).unwrap());
        let services = RecordServices {
            authority: authority.clone(),
            repository,
            access_registry: access_registry.clone(),
            mutation_sink: None,
            materializer: None,
            availability: None,
            workspace_authority: None,
        };
        let store = Arc::new(ContentStore::memory());
        let records = build_record_plane(
            &server_ep,
            &server_identity,
            &None,
            Arc::clone(&store),
            workspace_id,
            Some(services),
        )
        .unwrap()
        .unwrap();

        let allowed_manifest = store
            .write_content(b"record-scoped content")
            .await
            .unwrap()
            .unwrap();
        let allowed_chunk = iroh_blobs::Hash::from(
            crate::content::Manifest::decode(&store.local(allowed_manifest).await.unwrap())
                .unwrap()
                .chunks[0]
                .hash,
        );
        let thumbnail = store
            .ingest(Bytes::from_static(b"record thumbnail"))
            .await
            .unwrap();
        let sibling_manifest = store
            .write_content(b"sibling secret")
            .await
            .unwrap()
            .unwrap();
        let mut namespace = Namespace::new("host").unwrap();
        let allowed_ino = namespace
            .create_file(ROOT_INO, "allowed.png", Some(&allowed_manifest), 21, 1)
            .unwrap();
        assert!(namespace.set_thumb(allowed_ino, &thumbnail));
        let allowed_record = namespace.record_id_for(allowed_ino).unwrap();
        namespace
            .create_file(ROOT_INO, "sibling.txt", Some(&sibling_manifest), 14, 1)
            .unwrap();
        let namespace = Arc::new(Mutex::new(namespace));

        let client_credentials = client_identity.credentials();
        let client_cert = client_credentials.cert().clone();
        let peer = AuthenticatedFsPeer {
            user_public_key: client_cert.user_public_key,
            device_public_key: client_cert.device_public_key,
            device_label: client_cert.device_label.clone(),
            capabilities: client_cert.capabilities.clone(),
            cert: client_cert,
        };
        let issuer = Ed25519PublicKey::from_bytes(server_root.verifying_key().to_bytes());
        let mint_session = || {
            let token = mint_file_invite(
                FileInviteSpec {
                    workspace_id,
                    record_id: allowed_record,
                    subject: FileInviteSubject::Person(Ed25519PublicKey::from_bytes(
                        peer.user_public_key,
                    )),
                    permission: FilePermission::View,
                    expires_at_ms: None,
                    max_redemptions: None,
                },
                &server_root,
            )
            .unwrap();
            authority
                .register(&token, &[issuer], unix_now_ns() / 1_000_000)
                .unwrap();
            let session = Arc::new(
                crate::share_session::ShareSession::open_named(
                    workspace_id,
                    peer.clone(),
                    token.clone(),
                    authority.clone(),
                    Arc::new(server_identity.clone()),
                )
                .unwrap(),
            );
            access_registry.register(session);
            token
        };
        let token = mint_session();

        // An enforced but empty workspace ACL proves all authority below comes
        // from the live exact-record session, never ambient workspace access.
        let policy: PeerPolicy = Some(AclHandle::new(WorkspaceAcl::new()));
        let peer_key = client_identity.device_public_key();
        assert!(
            admit_blob_caller_with_records(&policy, Some(client_ep.id()), Some(&records)).is_ok()
        );
        for hash in [allowed_manifest, allowed_chunk, thumbnail] {
            assert!(
                peer_may_read_blob_with_records(
                    &policy,
                    &namespace,
                    &store,
                    &peer_key,
                    None,
                    hash,
                    Some(&records),
                )
                .await,
                "record session must reach its manifest, chunks, and thumbnail"
            );
        }
        assert!(
            !peer_may_read_blob_with_records(
                &policy,
                &namespace,
                &store,
                &peer_key,
                None,
                sibling_manifest,
                Some(&records),
            )
            .await,
            "a sibling hash is never authority"
        );

        // Exercise the real provider interceptor. Each single authorized blob
        // transfers, the sibling is denied, and a mixed get-many is rejected
        // before either member is imported into the client store.
        let _router = Router::builder(server_ep.clone())
            .accept(
                iroh_blobs::ALPN,
                store.blobs_protocol(blob_guard(
                    policy.clone(),
                    namespace.clone(),
                    store.clone(),
                    Some(records.clone()),
                    None,
                )),
            )
            .spawn();
        for hash in [allowed_manifest, allowed_chunk, thumbnail] {
            let connection = client_ep
                .connect(server_ep.addr(), iroh_blobs::ALPN)
                .await
                .unwrap();
            assert!(
                iroh_blobs::get::request::get_blob(connection, hash)
                    .await
                    .is_ok(),
                "authorized record blob must transfer through the real guard"
            );
        }
        let connection = client_ep
            .connect(server_ep.addr(), iroh_blobs::ALPN)
            .await
            .unwrap();
        assert!(
            iroh_blobs::get::request::get_blob(connection, sibling_manifest)
                .await
                .is_err(),
            "sibling fetch must fail at the provider"
        );
        let mixed_client_store = ContentStore::memory();
        let connection = client_ep
            .connect(server_ep.addr(), iroh_blobs::ALPN)
            .await
            .unwrap();
        assert!(
            mixed_client_store
                .store()
                .remote()
                .execute_get_many(
                    connection,
                    GetManyRequest::new(
                        vec![allowed_manifest, sibling_manifest],
                        ChunkRangesSeq::all(),
                    ),
                )
                .await
                .is_err(),
            "one unauthorized member must reject the complete get-many request"
        );
        assert!(!mixed_client_store.has(allowed_manifest).await);
        assert!(!mixed_client_store.has(sibling_manifest).await);

        authority.revoke(token.id).unwrap();
        assert!(
            !peer_may_read_blob_with_records(
                &policy,
                &namespace,
                &store,
                &peer_key,
                None,
                allowed_manifest,
                Some(&records),
            )
            .await,
            "grant revocation must affect the next request"
        );

        // A fresh grant restores only record authority. Revoking the certified
        // device then removes it again on the immediately following lookup.
        let _fresh_token = mint_session();
        assert!(
            peer_may_read_blob_with_records(
                &policy,
                &namespace,
                &store,
                &peer_key,
                None,
                allowed_manifest,
                Some(&records),
            )
            .await
        );
        let mut revocation = RevocationEntry {
            user_public_key: peer.user_public_key,
            revoked_device_public_key: peer.device_public_key,
            revoked_at_ns: unix_now_ns().saturating_sub(1),
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, &client_root).unwrap();
        server_identity
            .revocations()
            .write()
            .unwrap()
            .insert(revocation)
            .unwrap();
        assert!(
            !peer_may_read_blob_with_records(
                &policy,
                &namespace,
                &store,
                &peer_key,
                None,
                allowed_manifest,
                Some(&records),
            )
            .await,
            "device revocation must affect the next request"
        );
    }

    /// A workspace grant names the human root, while the connection arrives
    /// from one certified device. This is the identity-layer behavior the
    /// endpoint-only prototype could not express.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn certified_human_grant_bootstraps_from_an_owned_device() {
        let (ep_a, look_a, auth_a) = bind_certified_endpoint().await;
        let (ep_b, _look_b, auth_b) = bind_certified_endpoint().await;
        let store = Arc::new(ContentStore::memory());
        let manifest = store
            .write_content(b"human-shared content")
            .await
            .unwrap()
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(ep_a.clone(), store.clone()));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        {
            let mut namespace = namespace.lock().unwrap();
            namespace.create_dir(ROOT_INO, "human-shared", 1).unwrap();
            namespace
                .create_file(ROOT_INO, "human.txt", Some(&manifest), 20, 1)
                .unwrap();
        }
        let policy = policy_granting(&[auth_b.user_public_key()]);
        let server = ep_a.clone();
        let (_workspace, ticket) = Workspace::host(
            ep_a,
            look_a,
            store,
            hydrator,
            namespace,
            auth_a,
            Some(policy),
        )
        .await
        .unwrap();

        let document = fetch_remote_doc(&ep_b, ticket.node, &auth_b)
            .await
            .expect("a device certified by the granted human must bootstrap");
        let joined = Namespace::from_bytes("joiner", &document).unwrap();
        assert!(joined.entry_at("human-shared").is_some());
        let connection = ep_b.connect(server.addr(), iroh_blobs::ALPN).await.unwrap();
        iroh_blobs::get::request::get_blob(connection, manifest)
            .await
            .expect("the same certified human grant must authorize blob hydration");
    }

    #[tokio::test]
    async fn blob_acl_resolves_only_a_live_certified_device_to_its_person() {
        use qos_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};

        let (_server_ep, _server_lookup, server_identity, _server_root, _server_device) =
            bind_certified_test_endpoint_with_root().await;
        let (client_ep, _client_lookup, client_identity, client_root, _client_device) =
            bind_certified_test_endpoint_with_root().await;
        let (_stranger_ep, _stranger_lookup, stranger_identity) = bind_certified_endpoint().await;

        let client_cert = client_identity.credentials().cert().clone();
        let peer = AuthenticatedFsPeer {
            user_public_key: client_cert.user_public_key,
            device_public_key: client_cert.device_public_key,
            device_label: client_cert.device_label.clone(),
            capabilities: client_cert.capabilities.clone(),
            cert: client_cert,
        };
        let policy = policy_granting(&[peer.user_public_key]);
        let store = ContentStore::memory();
        let manifest = store
            .write_content(b"person-owned blob")
            .await
            .unwrap()
            .unwrap();
        let mut namespace = Namespace::new("host").unwrap();
        namespace
            .create_file(ROOT_INO, "person.txt", Some(&manifest), 17, 1)
            .unwrap();
        let namespace = Arc::new(Mutex::new(namespace));

        assert_eq!(
            server_identity.certified_person_for_device(peer.device_public_key),
            None,
            "a presented but unauthenticated device is never mapped to a person"
        );
        assert_eq!(
            server_identity.certified_person_for_device(stranger_identity.device_public_key()),
            None,
            "an unrelated certified device has not authenticated to this mount"
        );
        assert!(
            !peer_may_read_blob_with_records(
                &Some(policy.clone()),
                &namespace,
                &store,
                &peer.device_public_key,
                None,
                manifest,
                None,
            )
            .await,
            "the ACL intentionally contains no device-key grant"
        );

        // `authenticate_connection` invokes this only after the certificate,
        // challenge signature, endpoint binding, and revocation checks pass.
        server_identity.remember_peer(&peer, client_ep.addr());
        let person = server_identity
            .certified_person_for_device(peer.device_public_key)
            .expect("the authenticated device must resolve to its certified human root");
        assert_eq!(person, peer.user_public_key);
        assert!(
            peer_may_read_blob_with_records(
                &Some(policy.clone()),
                &namespace,
                &store,
                &peer.device_public_key,
                Some(person),
                manifest,
                None,
            )
            .await,
            "a person-only ACL authorizes that person's distinct certified device"
        );

        let mut revocation = RevocationEntry {
            user_public_key: peer.user_public_key,
            revoked_device_public_key: peer.device_public_key,
            revoked_at_ns: unix_now_ns().saturating_sub(1),
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, &client_root).unwrap();
        server_identity
            .revocations()
            .write()
            .unwrap()
            .insert(revocation)
            .unwrap();
        assert_eq!(
            server_identity.certified_person_for_device(peer.device_public_key),
            None,
            "revocation is re-evaluated for the next blob request"
        );
        assert!(
            !peer_may_read_blob_with_records(
                &Some(policy.clone()),
                &namespace,
                &store,
                &peer.device_public_key,
                server_identity.certified_person_for_device(peer.device_public_key),
                manifest,
                None,
            )
            .await
        );
    }

    /// Certificate possession and an ACL grant are both insufficient once
    /// the admitting mount has learned an effective root-signed revocation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_sync_rejects_a_revoked_certified_device_before_mode() {
        use qos_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};

        let (ep_a, look_a, auth_a) = bind_certified_endpoint().await;
        let (ep_b, _look_b, auth_b, root_b, _device_b) =
            bind_certified_test_endpoint_with_root().await;
        let mut revocation = RevocationEntry {
            user_public_key: auth_b.user_public_key(),
            revoked_device_public_key: auth_b.device_public_key(),
            revoked_at_ns: unix_now_ns().saturating_sub(1),
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, &root_b).unwrap();
        auth_a
            .revocations
            .write()
            .unwrap()
            .insert(revocation)
            .unwrap();

        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(ep_a.clone(), store.clone()));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let policy = policy_granting(&[auth_b.user_public_key()]);
        let (_workspace, ticket) = Workspace::host(
            ep_a,
            look_a,
            store,
            hydrator,
            namespace,
            auth_a,
            Some(policy),
        )
        .await
        .unwrap();

        let result = fetch_remote_doc(&ep_b, ticket.node, &auth_b).await;
        assert!(
            result.is_err(),
            "revoked peer must receive no mode response"
        );
    }

    /// The negative on the document plane: a peer holding no grant asks the
    /// host for the workspace document and gets nothing it can read a
    /// workspace out of — asserted on the bytes at the far endpoint, not on
    /// the gate having been consulted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stranger_receives_no_namespace_document() {
        let (ep_a, look_a, auth_a) = bind_certified_endpoint().await;
        let (ep_b, _look_b, auth_b) = bind_certified_endpoint().await;

        let store_a = Arc::new(ContentStore::memory());
        let hyd_a = Arc::new(Hydrator::new(ep_a.clone(), store_a.clone()));
        let ns_a = Arc::new(Mutex::new(Namespace::new("A").unwrap()));
        ns_a.lock()
            .unwrap()
            .create_dir(ROOT_INO, "secret-plans", 1)
            .unwrap();

        // The workspace grants somebody — just not B.
        let someone_else = identity_of(iroh::SecretKey::generate().public());
        let (_ws_a, ticket) = Workspace::host(
            ep_a.clone(),
            look_a,
            store_a.clone(),
            hyd_a,
            ns_a,
            auth_a,
            Some(policy_granting(&[someone_else])),
        )
        .await
        .unwrap();

        let leaked = fetch_remote_doc(&ep_b, ticket.node.clone(), &auth_b)
            .await
            .unwrap_or_default();
        assert!(
            leaked.is_empty(),
            "a peer with no grant must not be able to load the host's workspace document"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_subtree_reader_receives_no_monolithic_namespace_document() {
        let (ep_a, look_a, auth_a) = bind_certified_endpoint().await;
        let (ep_b, _look_b, auth_b) = bind_certified_endpoint().await;

        let mut acl = WorkspaceAcl::new();
        acl.grant_read(identity(&ep_b), "/shared");
        let policy = Some(AclHandle::new(acl));
        let store_a = Arc::new(ContentStore::memory());
        let hyd_a = Arc::new(Hydrator::new(ep_a.clone(), store_a.clone()));
        let ns_a = Arc::new(Mutex::new(Namespace::new("A").unwrap()));
        ns_a.lock()
            .unwrap()
            .create_dir(ROOT_INO, "private", 1)
            .unwrap();
        let (_ws_a, ticket) =
            Workspace::host(ep_a.clone(), look_a, store_a, hyd_a, ns_a, auth_a, policy)
                .await
                .unwrap();

        let leaked = fetch_remote_doc(&ep_b, ticket.node, &auth_b)
            .await
            .unwrap_or_default();
        assert!(
            leaked.is_empty(),
            "a subtree reader must not receive a whole document naming private paths"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_read_only_peer_pulls_updates_but_cannot_author() {
        let (ep_a, look_a, auth_a) = bind_certified_endpoint().await;
        let (ep_b, look_b, auth_b) = bind_certified_endpoint().await;
        let policy = Some(policy_read_write(&[identity(&ep_a)], &[identity(&ep_b)]));

        let store_a = Arc::new(ContentStore::memory());
        let hyd_a = Arc::new(Hydrator::new(ep_a.clone(), store_a.clone()));
        let ns_a = Arc::new(Mutex::new(Namespace::new("A").unwrap()));
        let (_ws_a, ticket) = Workspace::host(
            ep_a.clone(),
            look_a,
            store_a,
            hyd_a,
            ns_a.clone(),
            auth_a,
            policy.clone(),
        )
        .await
        .unwrap();

        let doc = fetch_remote_doc(&ep_b, ticket.node.clone(), &auth_b)
            .await
            .unwrap();
        let ns_b = Arc::new(Mutex::new(Namespace::from_bytes("B", &doc).unwrap()));
        let store_b = Arc::new(ContentStore::memory());
        let hyd_b = Arc::new(Hydrator::new(ep_b.clone(), store_b.clone()));
        let _ws_b = Workspace::join(
            ep_b.clone(),
            look_b,
            store_b,
            hyd_b,
            ns_b.clone(),
            ticket.clone(),
            auth_b.clone(),
            policy,
        )
        .await
        .unwrap();

        ns_b.lock()
            .unwrap()
            .create_file(ROOT_INO, "readers-change", None, 0, 2)
            .unwrap();
        assert!(
            sync_with_peer(&ep_b, ticket.node, &ns_b, &auth_b)
                .await
                .is_err(),
            "the host must close a read-only peer's bidirectional sync"
        );
        assert!(
            ns_a.lock().unwrap().entry_at("readers-change").is_none(),
            "a read-only peer's local Automerge changes must never reach the writer"
        );

        ns_a.lock()
            .unwrap()
            .create_file(ROOT_INO, "owners-change", None, 0, 3)
            .unwrap();
        assert!(
            wait_until(|| ns_b.lock().unwrap().entry_at("owners-change").is_some()).await,
            "a read-only peer must still pull the owner's updates"
        );
        assert!(
            ns_a.lock().unwrap().entry_at("readers-change").is_none(),
            "the owner's reconcile must not symmetrically sync with a read-only peer"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fresh_record_join_finishes_initial_projection_before_returning() {
        let server_temp = tempfile::tempdir().unwrap();
        let client_temp = tempfile::tempdir().unwrap();
        let (server_ep, server_lookup, server_identity, server_root, server_device) =
            bind_certified_test_endpoint_with_root().await;
        let (client_ep, client_lookup, client_identity, _client_root, _client_device) =
            bind_certified_test_endpoint_with_root().await;
        let workspace_id = Ticket::workspace_id_for_topic(*server_ep.id().as_bytes());
        let record_id = FileRecordId::new();
        let server_actor = FileActor {
            person: Ed25519PublicKey(server_root.verifying_key().to_bytes()),
            device: Ed25519PublicKey(server_identity.device_public_key()),
        };
        let server_services = record_services(server_temp.path());
        publish_record_metadata(
            Arc::clone(&server_services.repository),
            workspace_id,
            record_id,
            server_actor,
            server_device,
            37,
        );
        let server_store = Arc::new(ContentStore::memory());
        let server_hydrator = Arc::new(Hydrator::new(server_ep.clone(), Arc::clone(&server_store)));
        let server_policy = Some(policy_granting(&[client_identity.user_public_key()]));
        let (_server_workspace, ticket) = Workspace::host_with_records(
            server_ep.clone(),
            server_lookup,
            server_store,
            server_hydrator,
            Arc::new(Mutex::new(Namespace::new("initial-record-server").unwrap())),
            server_identity.clone(),
            server_policy,
            server_services,
        )
        .await
        .unwrap();

        let document = fetch_remote_doc(&client_ep, ticket.node.clone(), &client_identity)
            .await
            .unwrap();
        let client_services = record_services(client_temp.path());
        let client_repository = Arc::clone(&client_services.repository);
        let client_store = Arc::new(ContentStore::memory());
        let client_hydrator = Arc::new(Hydrator::new(client_ep.clone(), Arc::clone(&client_store)));
        let client_policy = Some(policy_read_write(&[server_identity.user_public_key()], &[]));
        let _client_workspace = Workspace::join_with_records(
            client_ep,
            client_lookup,
            client_store,
            client_hydrator,
            Arc::new(Mutex::new(
                Namespace::from_bytes("initial-record-client", &document).unwrap(),
            )),
            ticket,
            client_identity,
            client_policy,
            client_services,
        )
        .await
        .unwrap();

        assert_eq!(
            client_repository.highest_sequence(
                RecordRepositoryKey {
                    workspace_id,
                    record_id,
                },
                &server_actor,
            ),
            Some(1),
            "join must not return before the bootstrap peer's signed projection is local"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::too_many_lines)] // Live ACL and origin-validation scenario.
    async fn record_mesh_enforces_live_root_acl_and_accepts_only_direct_origin_streams() {
        let server_temp = tempfile::tempdir().unwrap();
        let client_temp = tempfile::tempdir().unwrap();
        let (server_ep, server_lookup, server_identity, server_root, server_device) =
            bind_certified_test_endpoint_with_root().await;
        let (client_ep, _client_lookup, client_identity, _client_root, _client_device) =
            bind_certified_test_endpoint_with_root().await;
        let workspace_id = Ticket::workspace_id_for_topic(*server_ep.id().as_bytes());
        let server_services = record_services(server_temp.path());
        let direct_record = FileRecordId::new();
        let server_actor = FileActor {
            person: Ed25519PublicKey(server_root.verifying_key().to_bytes()),
            device: Ed25519PublicKey(server_identity.device_public_key()),
        };
        publish_record_metadata(
            Arc::clone(&server_services.repository),
            workspace_id,
            direct_record,
            server_actor,
            server_device,
            41,
        );

        // A peer may store another device's authenticated stream, but the
        // direct-origin mesh deliberately refuses to relay it until actor
        // certificate bundles are part of this protocol.
        let relayed_record = FileRecordId::new();
        let relayed_person = ed25519_dalek::SigningKey::from_bytes(&[0x61; 32]);
        let relayed_device = ed25519_dalek::SigningKey::from_bytes(&[0x62; 32]);
        let relayed_actor = FileActor {
            person: Ed25519PublicKey(relayed_person.verifying_key().to_bytes()),
            device: Ed25519PublicKey(relayed_device.verifying_key().to_bytes()),
        };
        publish_record_metadata(
            Arc::clone(&server_services.repository),
            workspace_id,
            relayed_record,
            relayed_actor,
            relayed_device,
            99,
        );

        let mut subtree_acl = WorkspaceAcl::new();
        subtree_acl.grant_read(identity(&client_ep), "/shared");
        let server_acl = AclHandle::new(subtree_acl);
        let server_policy = Some(server_acl.clone());
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(server_ep.clone(), Arc::clone(&store)));
        let (_server_workspace, ticket) = Workspace::host_with_records(
            server_ep.clone(),
            server_lookup,
            store,
            hydrator,
            Arc::new(Mutex::new(Namespace::new("server").unwrap())),
            server_identity.clone(),
            server_policy,
            server_services.clone(),
        )
        .await
        .unwrap();
        assert_eq!(ticket.workspace_id(), workspace_id);

        let denied_request = RecordMeshRequestV1 {
            protocol_version: RECORD_MESH_PROTOCOL_VERSION,
            workspace_id,
            operation: RecordMeshOperationV1::Inventory {
                after: None,
                limit: 16,
            },
        };
        let (_, denied) = record_mesh_request(
            &client_ep,
            server_ep.addr(),
            &client_identity,
            &denied_request,
        )
        .await
        .unwrap();
        assert!(matches!(denied.result, RecordMeshResultV1::Denied));

        let mut root_acl = WorkspaceAcl::new();
        root_acl.grant_read(identity(&client_ep), "/");
        server_acl.replace(root_acl);
        let client_policy = Some(policy_read_write(&[identity(&server_ep)], &[]));
        let client_services = record_services(client_temp.path());
        let client_store = Arc::new(ContentStore::memory());
        let client_records = build_record_plane(
            &client_ep,
            &client_identity,
            &client_policy,
            client_store,
            workspace_id,
            Some(client_services.clone()),
        )
        .unwrap()
        .unwrap();
        pull_record_mesh_from_peer(
            &client_ep,
            server_ep.addr(),
            &client_identity,
            &client_policy,
            &client_records,
        )
        .await
        .unwrap();

        let direct_key = RecordRepositoryKey {
            workspace_id,
            record_id: direct_record,
        };
        assert_eq!(
            client_services
                .repository
                .highest_sequence(direct_key, &server_actor),
            Some(1)
        );
        assert_eq!(
            client_services
                .repository
                .reconciliation_batch(direct_key)
                .unwrap()
                .snapshot
                .metadata
                .unwrap()
                .byte_len,
            41
        );
        assert!(
            !client_services.repository.contains(RecordRepositoryKey {
                workspace_id,
                record_id: relayed_record,
            }),
            "the authenticated server must not relay a third actor's stream"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn root_writers_still_sync_bidirectionally() {
        let (ep_a, look_a, auth_a) = bind_certified_endpoint().await;
        let (ep_b, look_b, auth_b) = bind_certified_endpoint().await;
        let policy = Some(policy_read_write(&[identity(&ep_a), identity(&ep_b)], &[]));

        let store_a = Arc::new(ContentStore::memory());
        let hyd_a = Arc::new(Hydrator::new(ep_a.clone(), store_a.clone()));
        let ns_a = Arc::new(Mutex::new(Namespace::new("A").unwrap()));
        let (_ws_a, ticket) = Workspace::host(
            ep_a.clone(),
            look_a,
            store_a,
            hyd_a,
            ns_a.clone(),
            auth_a.clone(),
            policy.clone(),
        )
        .await
        .unwrap();

        let doc = fetch_remote_doc(&ep_b, ticket.node.clone(), &auth_b)
            .await
            .unwrap();
        let ns_b = Arc::new(Mutex::new(Namespace::from_bytes("B", &doc).unwrap()));
        let store_b = Arc::new(ContentStore::memory());
        let hyd_b = Arc::new(Hydrator::new(ep_b.clone(), store_b.clone()));
        let _ws_b = Workspace::join(
            ep_b.clone(),
            look_b,
            store_b,
            hyd_b,
            ns_b.clone(),
            ticket.clone(),
            auth_b.clone(),
            policy,
        )
        .await
        .unwrap();

        ns_b.lock()
            .unwrap()
            .create_file(ROOT_INO, "from-b", None, 0, 2)
            .unwrap();
        sync_with_peer(&ep_b, ticket.node.clone(), &ns_b, &auth_b)
            .await
            .expect("B is authorised to sync into A");
        assert!(ns_a.lock().unwrap().entry_at("from-b").is_some());

        ns_a.lock()
            .unwrap()
            .create_file(ROOT_INO, "from-a", None, 0, 3)
            .unwrap();
        sync_with_peer(&ep_a, ep_b.addr(), &ns_a, &auth_a)
            .await
            .expect("A is authorised to sync into B");
        assert!(ns_b.lock().unwrap().entry_at("from-a").is_some());
    }

    /// The negative on the content plane: a stranger who already knows a
    /// blob's hash (easy to learn, impossible to keep secret) still can't
    /// fetch the bytes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stranger_cannot_hydrate_content() {
        let (ep_a, look_a, auth_a) = bind_certified_endpoint().await;
        let (ep_b, _look_b, _auth_b) = bind_certified_endpoint().await;

        let store_a = Arc::new(ContentStore::memory());
        let hyd_a = Arc::new(Hydrator::new(ep_a.clone(), store_a.clone()));
        let ns_a = Arc::new(Mutex::new(Namespace::new("A").unwrap()));
        let secret = b"the contents of a workspace nobody invited them to";
        let hash = store_a.write_content(secret).await.unwrap().unwrap();

        let someone_else = identity_of(iroh::SecretKey::generate().public());
        let (_ws_a, _ticket) = Workspace::host(
            ep_a.clone(),
            look_a,
            store_a.clone(),
            hyd_a,
            ns_a,
            auth_a,
            Some(policy_granting(&[someone_else])),
        )
        .await
        .unwrap();

        let store_b = Arc::new(ContentStore::memory());
        let hyd_b = Hydrator::new(ep_b.clone(), store_b.clone());
        hyd_b.add_peer(ep_a.addr());

        let got = hyd_b.read(hash, 0, secret.len() as u64).await;
        assert!(
            got.is_err(),
            "a peer with no grant must not be served a blob it knows the hash of, got {:?}",
            got.map(|b| String::from_utf8_lossy(&b).into_owned()),
        );
        assert!(
            !store_b.has(hash).await,
            "and nothing may have landed in its store"
        );
    }

    /// Same host, same policy, an invited peer: bootstrap, gossip and
    /// hydration all still work — without this the negatives above would be
    /// satisfied by a mount that serves nobody.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_entitled_peer_still_bootstraps_and_hydrates() {
        let (ep_a, look_a, auth_a) = bind_certified_endpoint().await;
        let (ep_b, look_b, auth_b) = bind_certified_endpoint().await;
        let policy = policy_granting(&[identity(&ep_a), identity(&ep_b)]);

        let store_a = Arc::new(ContentStore::memory());
        let hyd_a = Arc::new(Hydrator::new(ep_a.clone(), store_a.clone()));
        let ns_a = Arc::new(Mutex::new(Namespace::new("A").unwrap()));
        ns_a.lock()
            .unwrap()
            .create_dir(ROOT_INO, "shared", 1)
            .unwrap();
        let content = b"a shared collaborative document body";
        let hash = store_a.write_content(content).await.unwrap().unwrap();

        let (ws_a, ticket) = Workspace::host(
            ep_a.clone(),
            look_a,
            store_a.clone(),
            hyd_a.clone(),
            ns_a.clone(),
            auth_a,
            Some(policy.clone()),
        )
        .await
        .unwrap();

        let doc = fetch_remote_doc(&ep_b, ticket.node.clone(), &auth_b)
            .await
            .expect("an entitled peer bootstraps the document");
        let ns_b = Arc::new(Mutex::new(Namespace::from_bytes("B", &doc).unwrap()));
        assert!(
            ns_b.lock().unwrap().entry_at("shared").is_some(),
            "the invited peer sees the host's tree"
        );

        let store_b = Arc::new(ContentStore::memory());
        let hyd_b = Arc::new(Hydrator::new(ep_b.clone(), store_b.clone()));
        let _ws_b = Workspace::join(
            ep_b.clone(),
            look_b,
            store_b.clone(),
            hyd_b.clone(),
            ns_b.clone(),
            ticket,
            auth_b,
            Some(policy),
        )
        .await
        .unwrap();

        {
            let delta = {
                let mut n = ns_a.lock().unwrap();
                n.create_file(ROOT_INO, "doc.txt", Some(&hash), content.len() as u64, 7)
                    .unwrap();
                n.save_incremental()
            };
            ws_a.sync.broadcast_delta(delta);
        }
        assert!(
            wait_until(|| ns_b.lock().unwrap().entry_at("doc.txt").is_some()).await,
            "an entitled peer must still receive the host's changes"
        );

        let got = hyd_b
            .read(hash, 0, content.len() as u64)
            .await
            .expect("an entitled peer hydrates content");
        assert_eq!(got.as_slice(), content, "and reads the host's real bytes");
    }
}
