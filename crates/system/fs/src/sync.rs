//! The sync plane: namespace replication plus content hydration.
//!
//! Unenforced workspaces use iroh-gossip for live Automerge deltas and
//! `tilo-fs-sync/0` for bootstrap/anti-entropy. Enforced workspaces use only
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
use futures_util::StreamExt;
use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::{Connection, Endpoint, RecvStream, SendStream, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh_base::EndpointAddr;
use iroh_blobs::Hash;
use iroh_gossip::api::{Event, GossipReceiver, GossipSender};
use iroh_gossip::net::{GOSSIP_ALPN, Gossip};
use iroh_gossip::proto::TopicId;
use serde::{Deserialize, Serialize};
use tilo_user_identity::RevocationList;
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
    RECORD_MESH_PROTOCOL_VERSION, RecordMeshEndV1, RecordMeshOperationV1, RecordMeshRequestV1,
    RecordMeshResultV1, RecordMeshStreamV1, TILO_FS_RECORD_MESH_ALPN, read_record_mesh_request,
    read_record_mesh_response, serve_authorized_request, write_record_mesh_request,
    write_record_mesh_response,
};
use crate::record_protocol::{RecordProtocol, TILO_FS_RECORD_ALPN};
use crate::record_replication::{RecordAction, RecordAuthorizer};
use crate::record_repository::{RecordRepository, RecordRepositoryKey};
use crate::replica_ack::{
    REPLICA_ACK_TIMEOUT, ReplicaAckRequestV1, ReplicaAckResponseV1, ReplicaAckV1,
    TILO_FS_REPLICA_ACK_ALPN, read_frame as read_replica_ack_frame,
    write_frame as write_replica_ack_frame,
};
use crate::share_authority::ShareAuthority;
use crate::share_session::LiveActorCertification;
use crate::store::ContentStore;
use crate::workspace_authority::{
    AuthorityRequest, AuthorityResponse, TILO_FS_AUTHORITY_ALPN, WorkspaceAuthority,
    read_frame as read_authority_frame, write_frame as write_authority_frame,
};

/// Custom ALPN for the document exchange (bootstrap + incremental sync).
pub const TILO_FS_SYNC_ALPN: &[u8] = b"tilo-fs-sync/0";
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
    #[allow(
        clippy::suspicious_operation_groupings,
        reason = "the direct certificate predicate keeps every authenticated identity equality visible"
    )]
    fn certification_is_current(&self, peer: &AuthenticatedFsPeer, now_ns: u64) -> bool {
        if peer.user_public_key != peer.cert.user_public_key
            || peer.device_public_key != peer.cert.device_public_key
            || peer.cert.issued_at_ns > now_ns
            || tilo_user_identity::verify_device_cert_at(&peer.cert, now_ns).is_err()
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
/// Bound aggregate sync-frame memory and Automerge CPU consumed by remote
/// peers. Router handlers are spawned per connection, so a frame limit alone
/// otherwise permits an authenticated but compromised peer to multiply the
/// allocation without bound.
const MAX_CONCURRENT_SYNC_CONNECTIONS: usize = 4;

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
        let conn = endpoint.connect(host, TILO_FS_SYNC_ALPN).await?;
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
        let conn = endpoint.connect(peer, TILO_FS_SYNC_ALPN).await?;
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
            *retry_at = now
                .checked_add(Duration::from_secs(delay.min(30)))
                .unwrap_or(now);
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

/// An iroh endpoint id read as a Tilo identity — the same 32 bytes (see
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
    inflight: Arc<tokio::sync::Semaphore>,
}

impl std::fmt::Debug for SyncProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyncProtocol")
    }
}

impl ProtocolHandler for SyncProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let Ok(permit) = Arc::clone(&self.inflight).try_acquire_owned() else {
            connection.close(2u32.into(), b"sync busy");
            return Ok(());
        };
        if self.identity.partitioned() {
            connection.close(1u32.into(), b"partitioned");
            return Ok(());
        }
        let (mut send, mut recv) = connection.accept_bi().await?;
        // Bounded so a client that opens a stream but never sends/closes
        // can't leak an accept task or connection forever.
        let exchange = async {
            // This is the first application-layer exchange. No
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
        // The router requires this handler to remain alive until the peer sees
        // the graceful stream FIN and closes the connection. Release scarce
        // sync admission first, so a peer cannot retain capacity during that
        // transport-only wait.
        drop(permit);
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

#[allow(
    clippy::significant_drop_tightening,
    reason = "namespace and record guards are deliberately released only after one coherent authorization decision"
)]
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
/// outright: this is a *read* plane; content arrives via the FUSE write
/// path where the write-side capability model is enforced.
#[allow(
    clippy::too_many_lines,
    reason = "one protocol guard keeps all blob request variants under the same read-only authorization boundary"
)]
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
    record_index_generation: tokio::sync::watch::Sender<u64>,
}

impl SyncHandle {
    /// Queue a freshly-produced local delta for broadcast (no-op if empty).
    pub fn broadcast_delta(&self, delta: Vec<u8>) {
        if !delta.is_empty() {
            let _ = self.local_tx.send(Bytes::from(delta));
        }
    }

    /// Subscribe to coalesced completed namespace mutations.  Consumers only
    /// need the latest generation: rebuilding an ACL view for every member of
    /// a remote burst would be wasted work and could publish an intermediate
    /// projection.
    pub fn subscribe_record_index_generation(&self) -> tokio::sync::watch::Receiver<u64> {
        self.record_index_generation.subscribe()
    }

    pub(crate) fn notify_record_index_changed(&self) {
        self.record_index_generation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    #[cfg(test)]
    pub(crate) fn test_handle() -> Arc<Self> {
        let (local_tx, _local_rx) = mpsc::unbounded_channel();
        let (record_index_generation, _) = tokio::sync::watch::channel(0_u64);
        Arc::new(Self {
            local_tx,
            record_index_generation,
        })
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
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit transport, authority, storage, and record dependencies make the host trust boundary auditable"
    )]
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

    #[allow(
        clippy::too_many_arguments,
        reason = "the internal host constructor preserves the explicit trust-boundary inputs of the public entry points"
    )]
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
    #[allow(
        clippy::too_many_arguments,
        reason = "the join boundary mirrors host authority inputs and adds the authenticated invitation ticket"
    )]
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
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit transport, authority, storage, and record dependencies make the join trust boundary auditable"
    )]
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

    #[allow(
        clippy::too_many_arguments,
        reason = "the internal join constructor preserves the explicit trust-boundary inputs of the public entry points"
    )]
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
        if let Some(records) = &record_plane {
            let initial_mesh = tokio::time::timeout(
                Duration::from_secs(10),
                pull_record_mesh_from_peer(
                    &endpoint,
                    ticket.node.clone(),
                    &identity,
                    &policy,
                    records,
                ),
            )
            .await
            .map_err(|_| anyhow::anyhow!("initial record mesh timed out"))
            .and_then(std::convert::identity);
            if let Err(error) = initial_mesh
                && !identity.partitioned()
            {
                // Persisted/offline mounts remain available. The periodic
                // mesh worker below keeps retrying a temporarily unavailable
                // or older peer after startup.
                eprintln!("initial record mesh reconciliation failed: {error}");
            }
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

    #[allow(
        clippy::too_many_arguments,
        reason = "startup wires explicit authenticated services whose authority must not be hidden in an unvalidated bag"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "startup owns the ordered lifecycle and cleanup of every sync protocol task"
    )]
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
        prime_namespace_baseline(&namespace);

        // Register bootstrap peers so gossip can dial them and we can hydrate.
        let boot_ids = register_bootstrap_peers(&lookup, &hydrator, &bootstrap);

        let sync_proto = SyncProtocol {
            endpoint: endpoint.clone(),
            identity: identity.clone(),
            namespace: namespace.clone(),
            policy: policy.clone(),
            inflight: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_SYNC_CONNECTIONS)),
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
                .accept(TILO_FS_RECORD_ALPN, records.protocol.clone())
                .accept(TILO_FS_RECORD_MESH_ALPN, records.mesh_protocol.clone())
                .accept(TILO_FS_REPLICA_ACK_ALPN, replica_ack),
            _ => builder,
        };
        let builder = if let Some(records) = &record_plane {
            if let Some(authority) = &records.workspace_authority {
                builder.accept(
                    TILO_FS_AUTHORITY_ALPN,
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
                .accept(TILO_FS_SYNC_ALPN, sync_proto)
                .spawn()
        } else {
            builder.accept(TILO_FS_SYNC_ALPN, sync_proto).spawn()
        };

        let (local_tx, local_rx) = mpsc::unbounded_channel();
        let (record_index_generation, _) = tokio::sync::watch::channel(0_u64);
        let sync = Arc::new(SyncHandle {
            local_tx,
            record_index_generation,
        });
        let mut tasks = Vec::new();
        let (gossip_sender, reconcile_rx) = if policy.is_none() {
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
                Arc::clone(&sync),
            ))));
            (Some(sender), None)
        } else {
            // Enforced workspaces cannot relay unauthenticated gossip deltas,
            // but local mutations still wake authenticated anti-entropy. This
            // avoids making a live NAS wait for a polling tick and provides a
            // prompt retry after a transient exchange failure.
            (None, Some(local_rx))
        };
        // Anti-entropy backstop: periodically pull+merge peers' full docs and
        // resolve conflicts, so a missed gossip delta or a mid-join window
        // can't leave a peer permanently diverged.
        tasks.push(AbortOnDrop(tokio::spawn(reconcile_loop(ReconcileLoop {
            endpoint,
            namespace: namespace.clone(),
            hydrator: hydrator.clone(),
            sender: gossip_sender,
            identity,
            policy,
            records: record_plane,
            local_rx: reconcile_rx,
            sync: Arc::clone(&sync),
        }))));

        Ok(Workspace {
            namespace,
            sync,
            _router: router,
            _tasks: tasks,
        })
    }
}

fn register_bootstrap_peers(
    lookup: &MemoryLookup,
    hydrator: &Hydrator,
    bootstrap: &[EndpointAddr],
) -> Vec<iroh_base::PublicKey> {
    bootstrap
        .iter()
        .map(|address| {
            lookup.add_endpoint_info(address.clone());
            hydrator.add_peer(address.clone());
            address.id
        })
        .collect()
}

fn prime_namespace_baseline(namespace: &Arc<Mutex<Namespace>>) {
    namespace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .prime_baseline();
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
    sync: Arc<SyncHandle>,
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
                        sync.notify_record_index_changed();
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
                    None => break, // stream ended
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
        // A home peer is commonly a second device certified to the same
        // human as the desktop which authored the record. The envelope still
        // verifies the original device signature; the live transport cert
        // permits that human's directly-connected sibling device to relay it.
        // Never extend this across people: collaborator A cannot vouch for
        // collaborator B's device or attribution.
        actor.person.as_bytes() == &self.peer.user_public_key
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
    let connection = endpoint.connect(remote, TILO_FS_RECORD_MESH_ALPN).await?;
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
    let connection = endpoint.connect(remote, TILO_FS_AUTHORITY_ALPN).await?;
    let (mut send, mut recv) = connection.open_bi().await?;
    let _peer =
        authenticate_connection(endpoint, &connection, &mut recv, &mut send, identity).await?;
    write_authority_frame(
        &mut send,
        &AuthorityRequest {
            workspace: workspace_id,
            // The governor is the only identity the remote protocol permits
            // to publish. Sending its current signed envelope makes ACL
            // reconciliation bidirectional: a desktop can push a revocation
            // to an always-on NAS instead of waiting for the NAS to happen to
            // initiate the next connection. Collaborators remain pull-only.
            publish: (identity.user_public_key() == authority.governor())
                .then(|| authority.current())
                .flatten(),
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

/// Exchange the current signed workspace authority with one authenticated peer.
///
/// # Errors
///
/// Returns an error when transport authentication, authority framing, or the
/// remote exchange fails.
pub async fn submit_workspace_authority(
    endpoint: &Endpoint,
    remote: EndpointAddr,
    identity: &FsTransportIdentity,
    workspace: uuid::Uuid,
    publish: Option<crate::workspace_authority::WorkspaceAuthorityEnvelope>,
) -> Result<Option<crate::workspace_authority::WorkspaceAuthorityEnvelope>> {
    let connection = endpoint.connect(remote, TILO_FS_AUTHORITY_ALPN).await?;
    let (mut send, mut recv) = connection.open_bi().await?;
    let _ = authenticate_connection(endpoint, &connection, &mut recv, &mut send, identity).await?;
    write_authority_frame(&mut send, &AuthorityRequest { workspace, publish }).await?;
    send.finish()?;
    let response: AuthorityResponse = read_authority_frame(&mut recv).await?;
    connection.close(0u32.into(), b"");
    Ok(response.authority)
}

#[allow(
    clippy::too_many_lines,
    reason = "the record pull is one ordered validation and durable-commit transaction across a single peer response"
)]
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
            .filter(|cursor| cursor.actor.person == direct_actor.person)
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
        let connection = endpoint.connect(remote, TILO_FS_REPLICA_ACK_ALPN).await?;
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

#[allow(
    clippy::significant_drop_tightening,
    reason = "the availability lock spans the atomic observation update and its persistence decision"
)]
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

struct ReconcileLoop {
    endpoint: Endpoint,
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    sender: Option<GossipSender>,
    identity: FsTransportIdentity,
    policy: PeerPolicy,
    records: Option<RecordPlane>,
    local_rx: Option<mpsc::UnboundedReceiver<Bytes>>,
    sync: Arc<SyncHandle>,
}

#[allow(
    clippy::too_many_lines,
    reason = "the reconciliation task keeps fetch, validation, publication, and retry ordering in one lifecycle loop"
)]
async fn reconcile_loop(reconcile: ReconcileLoop) {
    let ReconcileLoop {
        endpoint,
        namespace,
        hydrator,
        sender,
        identity,
        policy,
        records,
        mut local_rx,
        sync,
    } = reconcile;
    let may_write = peer_may_write_whole_workspace(&policy, &identity.user_public_key())
        || peer_may_write_whole_workspace(&policy, &identity.device_public_key());
    let mut pending_thumbnails = HashMap::new();
    // `mount` bootstraps a namespace before this task starts, so schedule its
    // previews once even if the first anti-entropy exchange is clean.
    let mut refresh_thumbnails = true;
    loop {
        if let Some(receiver) = &mut local_rx {
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(3)) => {}
                mutation = receiver.recv() => {
                    if mutation.is_none() {
                        local_rx = None;
                    }
                }
            }
        } else {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }

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

        if namespace_changed {
            sync.notify_record_index_changed();
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
    use std::sync::atomic::Ordering;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, IdentityPaths, adopt_device_key,
        persist::write_device_cert, sign_device_cert,
    };

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
mod tests;
