//! The sync plane: namespace replication plus content hydration.
//!
//! Unenforced workspaces use iroh-gossip for live Automerge deltas and
//! `tilo-fs-sync/1` for bootstrap/anti-entropy. Enforced workspaces use only
//! the authenticated sync ALPN: gossip does not preserve the original author
//! needed for a write-authorisation decision. Iroh-blobs hydrates content on
//! read, guarded by the live namespace and ACL.
//!
//! A joiner must *load the host's document* so both share the same
//! Automerge object ids — independently-created docs would fork on
//! merge. The bootstrap exchange guarantees that shared history.
//!
//! Every request names the workspace it is for, and every namespace
//! payload either side sends is bound to that workspace by the sender's
//! device signature ([`crate::namespace_binding`]). A receiver applies
//! nothing that [`accept_namespace_payload`] refuses: another workspace's
//! tree, bytes that are not the ones signed, or a device whose holder may
//! not serve this workspace's namespace.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

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
use wire_frame::{FrameError, Framing};

use crate::acl::{AclHandle, IdentityKey};
use crate::authenticated_transport::{AuthenticatedTransportConfig, authenticate_fs_peer_parts_at};
use crate::blob_access::{BlobAccessIndex, Requester};
use crate::clock::{unix_now_ms, unix_now_ns};
use crate::hydration::Hydrator;
use crate::identity::PeerCredentials;
use crate::namespace::Namespace;
use crate::namespace_binding::{
    BoundNamespacePayloadV1, NamespaceBindingError, NamespacePayloadKind,
};
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
use session::{SessionPool, SessionProtocol};

/// Custom ALPN for the document exchange (bootstrap + incremental sync).
pub const TILO_FS_SYNC_ALPN: &[u8] = b"tilo-fs-sync/1";
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

    pub(crate) fn partitioned(&self) -> bool {
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

    /// Binds namespace `bytes` of `workspace_id` as this device, for a peer
    /// to verify with [`accept_namespace_payload`].
    pub(crate) fn bind_namespace(
        &self,
        workspace_id: uuid::Uuid,
        kind: NamespacePayloadKind,
        bytes: Vec<u8>,
    ) -> Result<BoundNamespacePayloadV1, NamespaceBindingError> {
        BoundNamespacePayloadV1::sign(workspace_id, kind, bytes, self.credentials.signing_key())
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

/// What a client asks of the sync ALPN, and for which workspace. A device
/// serving several workspaces under one key answers only for its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NamespaceRequestV1 {
    workspace_id: uuid::Uuid,
    mode: NamespaceModeV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum NamespaceModeV1 {
    /// The whole document (first-join bootstrap, read-only pull).
    Document,
    /// Incremental Automerge sync rounds between two writers.
    Sync,
}

/// One frame of a sync-ALPN exchange after the request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum NamespaceFrameV1 {
    /// The server does not serve this request.
    Refused(NamespaceRefusalV1),
    /// Namespace bytes bound to the request's workspace.
    Payload(BoundNamespacePayloadV1),
    /// A sync round with nothing to send.
    Idle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum NamespaceRefusalV1 {
    /// The request names a workspace this device does not serve here.
    OtherWorkspace,
    /// The requester may not read (or, to sync, write) the whole workspace.
    NotAuthorised,
}

impl std::fmt::Display for NamespaceRefusalV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::OtherWorkspace => "the peer does not serve the requested workspace",
            Self::NotAuthorised => "the peer does not serve this workspace's namespace to us",
        })
    }
}

/// The one check every namespace payload received from `peer` passes
/// before it is applied: `peer` may serve the workspace's namespace (read
/// the whole workspace for a document or changes, also write it for a sync
/// message, which authors changes), and the payload is bound to
/// `workspace_id`, as `kind`, by `peer`'s own device, over these bytes.
///
/// # Errors
///
/// Returns why the payload is refused.
pub(crate) fn accept_namespace_payload(
    policy: &PeerPolicy,
    workspace_id: uuid::Uuid,
    peer: &AuthenticatedFsPeer,
    kind: NamespacePayloadKind,
    payload: &BoundNamespacePayloadV1,
) -> Result<(), NamespaceBindingError> {
    let authorised = match kind {
        NamespacePayloadKind::SyncMessage => {
            authenticated_peer_may_write_whole_workspace(policy, peer)
        }
        NamespacePayloadKind::Document | NamespacePayloadKind::Changes => {
            authenticated_peer_may_read_whole_workspace(policy, peer)
        }
    };
    if !authorised {
        return Err(NamespaceBindingError::Unauthorised);
    }
    payload
        .binding
        .verify(workspace_id, kind, peer.device_public_key, &payload.bytes)
}

async fn write_namespace_frame(
    send: &mut SendStream,
    frame: &NamespaceFrameV1,
) -> std::io::Result<()> {
    Ok(SYNC_FRAMING.write_value_async(send, frame).await?)
}

async fn read_namespace_frame(recv: &mut RecvStream) -> std::io::Result<NamespaceFrameV1> {
    Ok(SYNC_FRAMING
        .read_value_async(recv)
        .await?
        .ok_or(FrameError::Closed)?)
}

/// Sends the request that opens a sync-ALPN exchange.
async fn open_namespace_exchange(
    endpoint: &Endpoint,
    peer: EndpointAddr,
    identity: &FsTransportIdentity,
    request: NamespaceRequestV1,
) -> Result<(AuthenticatedFsPeer, SendStream, RecvStream)> {
    let conn = endpoint.connect(peer, TILO_FS_SYNC_ALPN).await?;
    let (mut send, mut recv) = conn.open_bi().await?;
    let remote = authenticate_connection(endpoint, &conn, &mut recv, &mut send, identity).await?;
    let request = postcard::to_allocvec(&request)?;
    SYNC_FRAMING.write_async(&mut send, &request).await?;
    Ok((remote, send, recv))
}
/// Hard cap on a single sync frame (`DoS` guard).
const MAX_FRAME: usize = 64 * 1024 * 1024;
/// Bound aggregate sync-frame memory and Automerge CPU consumed by remote
/// peers. Router handlers are spawned per connection, so a frame limit alone
/// otherwise permits an authenticated but compromised peer to multiply the
/// allocation without bound.
const MAX_CONCURRENT_SYNC_CONNECTIONS: usize = 4;

/// Each round of the sync-mode exchange, in both directions: a
/// little-endian `u32` length, then the Automerge sync message, which is
/// empty when that side has nothing to send.
const SYNC_FRAMING: Framing = Framing::little_endian(MAX_FRAME);

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
    use iroh::endpoint::{QuicTransportConfig, VarInt};
    /// Unacknowledged bytes one stream, and one connection, may have in
    /// flight. QUIC's defaults (1.25 MB per stream, 10 MB per connection)
    /// cap a stream at about 30 MB/s and a connection at about 250 MB/s
    /// over a 40 ms round trip; content transfer needs a window of a few
    /// bandwidth-delay products.
    const STREAM_WINDOW: u32 = 16 * 1024 * 1024;
    const CONNECTION_WINDOW: u32 = 64 * 1024 * 1024;
    let transport = QuicTransportConfig::builder()
        .stream_receive_window(VarInt::from_u32(STREAM_WINDOW))
        .receive_window(VarInt::from_u32(CONNECTION_WINDOW))
        .send_window(u64::from(CONNECTION_WINDOW))
        .build();
    let lookup = MemoryLookup::new();
    let endpoint = Endpoint::builder(presets::N0)
        .transport_config(transport)
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

/// Fetch the full namespace document of `workspace_id` from a peer over
/// the bootstrap ALPN, bounded by a timeout so a stalled host can't hang
/// the caller. The document is returned only when `policy` lets the peer
/// read the whole workspace and the peer bound these exact bytes to
/// `workspace_id` ([`accept_namespace_payload`]).
///
/// # Errors
///
/// Returns an error when the partition gate is closed, when connection,
/// authentication, framing, or transport fails, or when the peer refuses
/// or serves a document that is not bound to `workspace_id` by an
/// authorised device.
pub async fn fetch_remote_doc(
    endpoint: &Endpoint,
    host: EndpointAddr,
    identity: &FsTransportIdentity,
    workspace_id: uuid::Uuid,
    policy: &PeerPolicy,
) -> Result<Vec<u8>> {
    if identity.partitioned() {
        anyhow::bail!("transport partition gate is closed");
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        let (remote, mut send, mut recv) = open_namespace_exchange(
            endpoint,
            host,
            identity,
            NamespaceRequestV1 {
                workspace_id,
                mode: NamespaceModeV1::Document,
            },
        )
        .await?;
        send.finish()?;
        // Documents are small (metadata only); a frame's 64 MiB bound is a
        // generous ceiling.
        let payload = match read_namespace_frame(&mut recv).await {
            Ok(NamespaceFrameV1::Payload(payload)) => payload,
            Ok(NamespaceFrameV1::Refused(refusal)) => {
                anyhow::bail!("peer did not serve a workspace document: {refusal}")
            }
            Ok(NamespaceFrameV1::Idle) | Err(_) => {
                anyhow::bail!("peer did not serve a workspace document")
            }
        };
        accept_namespace_payload(
            policy,
            workspace_id,
            &remote,
            NamespacePayloadKind::Document,
            &payload,
        )
        .map_err(|error| anyhow::anyhow!("refusing the peer's workspace document: {error}"))?;
        if payload.bytes.is_empty() {
            anyhow::bail!("peer did not serve a workspace document");
        }
        Ok::<_, anyhow::Error>(payload.bytes)
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
    workspace_id: uuid::Uuid,
    policy: &PeerPolicy,
) -> Result<bool> {
    sync_with_peer_at_boundaries(
        endpoint,
        peer,
        namespace,
        identity,
        (workspace_id, policy),
        None,
    )
    .await
}

/// Waits until no changeset is being published into this device's namespace
/// (see [`RecordRepository::publication_gate`]): namespace state sent while
/// the guard is held never holds half of one.
async fn changeset_boundary(
    repository: Option<&RecordRepository>,
) -> Option<tokio::sync::RwLockReadGuard<'_, ()>> {
    match repository {
        Some(repository) => Some(repository.publication_gate().read().await),
        None => None,
    }
}

/// [`sync_with_peer`], sending only namespace state between changesets.
async fn sync_with_peer_at_boundaries(
    endpoint: &Endpoint,
    peer: EndpointAddr,
    namespace: &Arc<Mutex<Namespace>>,
    identity: &FsTransportIdentity,
    (workspace_id, policy): (uuid::Uuid, &PeerPolicy),
    repository: Option<&RecordRepository>,
) -> Result<bool> {
    if identity.partitioned() {
        anyhow::bail!("transport partition gate is closed");
    }
    tokio::time::timeout(Duration::from_secs(15), async {
        let (remote, mut send, mut recv) = open_namespace_exchange(
            endpoint,
            peer,
            identity,
            NamespaceRequestV1 {
                workspace_id,
                mode: NamespaceModeV1::Sync,
            },
        )
        .await?;
        let mut state = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin_sync_with(&remote.device_public_key);
        // Initiator: write-then-read each round; the responder mirrors.
        let mut received_change = false;
        for _ in 0..200 {
            let msg = {
                let _gate = changeset_boundary(repository).await;
                namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .sync_generate(&mut state)
            };
            let sent_nothing = msg.is_none();
            let frame = match msg {
                Some(msg) => NamespaceFrameV1::Payload(identity.bind_namespace(
                    workspace_id,
                    NamespacePayloadKind::SyncMessage,
                    msg,
                )?),
                None => NamespaceFrameV1::Idle,
            };
            write_namespace_frame(&mut send, &frame).await?;
            let peer_msg = match read_namespace_frame(&mut recv).await? {
                NamespaceFrameV1::Payload(payload) => {
                    accept_namespace_payload(
                        policy,
                        workspace_id,
                        &remote,
                        NamespacePayloadKind::SyncMessage,
                        &payload,
                    )
                    .map_err(|error| {
                        anyhow::anyhow!("refusing the peer's namespace sync message: {error}")
                    })?;
                    payload.bytes
                }
                NamespaceFrameV1::Idle => Vec::new(),
                NamespaceFrameV1::Refused(refusal) => {
                    anyhow::bail!("peer refused namespace sync: {refusal}")
                }
            };
            if !peer_msg.is_empty() {
                received_change = true;
                let _ = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .sync_receive(&mut state, &peer_msg);
            }
            if sent_nothing && peer_msg.is_empty() {
                // Complete: the next exchange with this device resumes here.
                namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .finish_sync_with(remote.device_public_key, &state);
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
    /// The workspace this namespace is; requests for any other are refused.
    workspace_id: uuid::Uuid,
    inflight: Arc<tokio::sync::Semaphore>,
    /// Whose publication gate namespace reads wait on, so a changeset
    /// being published is never served half.
    records: Option<Arc<RecordRepository>>,
}

impl SyncProtocol {
    async fn gate(&self) -> Option<tokio::sync::RwLockReadGuard<'_, ()>> {
        match &self.records {
            Some(repository) => Some(repository.publication_gate().read().await),
            None => None,
        }
    }

    /// The responder's Automerge sync rounds with a workspace writer.
    async fn serve_sync_rounds(
        &self,
        peer: &AuthenticatedFsPeer,
        send: &mut SendStream,
        recv: &mut RecvStream,
    ) -> Result<(), AcceptError> {
        // Responder: read-then-write each round (mirrors the initiator).
        let mut state = self
            .namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin_sync_with(&peer.device_public_key);
        for _ in 0..200 {
            // The authority handle is live. A newer signed
            // tombstone/revocation cuts an already-connected
            // writer off at the next protocol round.
            if !authenticated_peer_may_write_whole_workspace(&self.policy, peer) {
                let _ = send.finish();
                break;
            }
            let peer_msg = match read_namespace_frame(recv)
                .await
                .map_err(AcceptError::from_err)?
            {
                NamespaceFrameV1::Payload(payload) => {
                    if let Err(error) = accept_namespace_payload(
                        &self.policy,
                        self.workspace_id,
                        peer,
                        NamespacePayloadKind::SyncMessage,
                        &payload,
                    ) {
                        eprintln!(
                            "refusing a namespace sync message from {}: {error}",
                            hex::encode(&peer.device_public_key[..5])
                        );
                        let _ = send.finish();
                        break;
                    }
                    payload.bytes
                }
                NamespaceFrameV1::Idle => Vec::new(),
                NamespaceFrameV1::Refused(_) => break,
            };
            if !peer_msg.is_empty() {
                let _ = self
                    .namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .sync_receive(&mut state, &peer_msg);
            }
            let msg = {
                let _gate = self.gate().await;
                self.namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .sync_generate(&mut state)
            };
            let sent_nothing = msg.is_none();
            let frame = match msg {
                Some(msg) => NamespaceFrameV1::Payload(
                    self.identity
                        .bind_namespace(self.workspace_id, NamespacePayloadKind::SyncMessage, msg)
                        .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))?,
                ),
                None => NamespaceFrameV1::Idle,
            };
            write_namespace_frame(send, &frame)
                .await
                .map_err(AcceptError::from_err)?;
            if peer_msg.is_empty() && sent_nothing {
                // Complete: the next exchange with this device resumes here.
                self.namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .finish_sync_with(peer.device_public_key, &state);
                break;
            }
        }
        Ok(())
    }

    /// Answers one authenticated peer's request: refused unless it names
    /// this workspace and the peer may read it (write it, to sync), then
    /// the document or the sync rounds, each payload bound to this
    /// workspace.
    async fn serve_request(
        &self,
        peer: &AuthenticatedFsPeer,
        send: &mut SendStream,
        recv: &mut RecvStream,
    ) -> Result<(), AcceptError> {
        let request = SYNC_FRAMING
            .read_async(recv)
            .await
            .and_then(|frame| frame.ok_or(FrameError::Closed))
            .map_err(AcceptError::from_err)?;
        let Ok(request) = postcard::from_bytes::<NamespaceRequestV1>(&request) else {
            let _ = send.finish();
            return Ok(());
        };
        let authorised = match request.mode {
            NamespaceModeV1::Document => {
                authenticated_peer_may_read_whole_workspace(&self.policy, peer)
            }
            NamespaceModeV1::Sync => {
                authenticated_peer_may_write_whole_workspace(&self.policy, peer)
            }
        };
        let refusal = if request.workspace_id != self.workspace_id {
            Some(NamespaceRefusalV1::OtherWorkspace)
        } else if authorised {
            None
        } else {
            Some(NamespaceRefusalV1::NotAuthorised)
        };
        if let Some(refusal) = refusal {
            write_namespace_frame(send, &NamespaceFrameV1::Refused(refusal))
                .await
                .map_err(AcceptError::from_err)?;
            let _ = send.finish();
            return Ok(());
        }
        let bind = |kind, bytes| {
            self.identity
                .bind_namespace(self.workspace_id, kind, bytes)
                .map(NamespaceFrameV1::Payload)
                .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))
        };
        match request.mode {
            NamespaceModeV1::Document => {
                let gate = self.gate().await;
                let doc = self
                    .namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .save();
                drop(gate);
                write_namespace_frame(send, &bind(NamespacePayloadKind::Document, doc)?)
                    .await
                    .map_err(AcceptError::from_err)?;
                let _ = send.finish();
            }
            NamespaceModeV1::Sync => {
                self.serve_sync_rounds(peer, send, recv).await?;
            }
        }
        Ok(())
    }
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
            // request or namespace data is accepted until the transport key
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
            self.serve_request(&peer, &mut send, &mut recv).await?;
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

/// One blob request checked the way the provider checks it, against a
/// fresh index of the namespace.
#[cfg(test)]
async fn peer_may_read_blob_with_records(
    policy: &PeerPolicy,
    namespace: &Arc<Mutex<Namespace>>,
    store: &ContentStore,
    peer: &IdentityKey,
    certified_person: Option<IdentityKey>,
    hash: iroh_blobs::Hash,
    records: Option<&RecordPlane>,
) -> bool {
    let index = BlobAccessIndex::new(Arc::clone(namespace));
    authorize_blob_request(
        policy,
        &index,
        store,
        records,
        (*peer, certified_person),
        &[hash],
        &[],
    )
    .await
}

/// What one blob request asks for.
enum BlobAsk {
    /// Whole blobs, with `context`: further hashes named in the same request
    /// with nothing asked of them (the manifests of requested chunks).
    Blobs {
        wanted: Vec<iroh_blobs::Hash>,
        context: Vec<iroh_blobs::Hash>,
    },
    /// Positions of a file version's manifest: the manifest itself (0) and
    /// its chunks.
    Manifest {
        root: iroh_blobs::Hash,
        children: Vec<u64>,
    },
}

impl BlobAsk {
    fn len(&self) -> usize {
        match self {
            Self::Blobs { wanted, .. } => wanted.len(),
            Self::Manifest { children, .. } => children.len(),
        }
    }
}

/// Whether the caller `(device, certified person)` may receive every blob
/// in `wanted`; `context` are further hashes named in the same request with
/// nothing asked of them (the manifests of requested chunks).
#[cfg(test)]
async fn authorize_blob_request(
    policy: &PeerPolicy,
    index: &BlobAccessIndex,
    store: &ContentStore,
    records: Option<&RecordPlane>,
    caller: (IdentityKey, Option<IdentityKey>),
    wanted: &[iroh_blobs::Hash],
    context: &[iroh_blobs::Hash],
) -> bool {
    authorize_ask(
        policy,
        index,
        store,
        records,
        caller,
        &BlobAsk::Blobs {
            wanted: wanted.to_vec(),
            context: context.to_vec(),
        },
    )
    .await
}

async fn authorize_ask(
    policy: &PeerPolicy,
    index: &BlobAccessIndex,
    store: &ContentStore,
    records: Option<&RecordPlane>,
    caller: (IdentityKey, Option<IdentityKey>),
    ask: &BlobAsk,
) -> bool {
    if policy.is_none() && records.is_none() {
        return true;
    }
    let (device, person) = caller;
    let readable_records = records.map(|records| {
        records
            .access_registry
            .readable_record_ids_for_device(
                records.workspace_id,
                collab_types::Ed25519PublicKey(device),
            )
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    });
    let requester = Requester {
        policy,
        device,
        person,
        readable_records,
    };
    match ask {
        BlobAsk::Blobs { wanted, context } => {
            index.authorize(store, &requester, wanted, context).await
        }
        BlobAsk::Manifest { root, children } => {
            index
                .authorize_manifest_children(store, &requester, *root, children)
                .await
        }
    }
}

/// Blob requests authorised at once; further requests wait for a slot,
/// which pushes back on the requesting connections.
const MAX_CONCURRENT_BLOB_AUTHORIZATIONS: usize = 64;

type BlobCallers = HashMap<u64, (IdentityKey, Option<IdentityKey>)>;

/// Everything one blob-request decision needs, shared by the guard's tasks.
#[derive(Clone)]
struct BlobGuard {
    policy: PeerPolicy,
    index: Arc<BlobAccessIndex>,
    store: Arc<ContentStore>,
    records: Option<RecordPlane>,
    callers: Arc<Mutex<BlobCallers>>,
    slots: Arc<tokio::sync::Semaphore>,
}

impl BlobGuard {
    /// The connect-time verdict, remembering who is on the connection.
    fn admit(
        &self,
        identity: Option<&FsTransportIdentity>,
        endpoint_id: Option<iroh::EndpointId>,
        connection: u64,
    ) -> iroh_blobs::provider::events::EventResult {
        use iroh_blobs::provider::events::AbortReason;

        let device = endpoint_id.map(identity_of);
        let person = device.and_then(|device| {
            identity.and_then(|identity| identity.certified_person_for_device(device))
        });
        let mut verdict =
            admit_blob_caller_with_records(&self.policy, endpoint_id, self.records.as_ref());
        if verdict.is_err()
            && person.is_some_and(|person| peer_may_read_anything(&self.policy, &person))
        {
            verdict = device.ok_or(AbortReason::Permission);
        }
        match verdict {
            Ok(peer) => {
                self.callers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(connection, (peer, person));
                Ok(())
            }
            Err(reason) => {
                eprintln!(
                    "iroh-blobs: refusing {} — it reads nothing in this workspace",
                    endpoint_id.map_or_else(|| "unnamed peer".into(), |id| id.to_string())
                );
                Err(reason)
            }
        }
    }

    /// Decides one request on its own task and answers on `reply`. Waits
    /// for a free slot first, so a flood of requests is pushed back.
    async fn decide<R, F>(&self, connection: u64, ask: BlobAsk, reply: R)
    where
        R: FnOnce(iroh_blobs::provider::events::EventResult) -> F + Send + 'static,
        F: std::future::Future<Output = ()> + Send,
    {
        use iroh_blobs::provider::events::AbortReason;

        let caller = self
            .callers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&connection)
            .copied();
        let Ok(slot) = Arc::clone(&self.slots).acquire_owned().await else {
            return;
        };
        let guard = self.clone();
        tokio::spawn(async move {
            let ok = match caller {
                Some(caller) => {
                    authorize_ask(
                        &guard.policy,
                        &guard.index,
                        &guard.store,
                        guard.records.as_ref(),
                        caller,
                        &ask,
                    )
                    .await
                }
                None => false,
            };
            drop(slot);
            if !ok {
                eprintln!(
                    "iroh-blobs: refusing {} unreadable blob(s) on connection {connection}",
                    ask.len()
                );
            }
            reply(if ok {
                Ok(())
            } else {
                Err(AbortReason::Permission)
            })
            .await;
        });
    }
}

/// Closed connections whose caller the blob guard still recognises.
const RETAINED_CLOSED_CALLERS: usize = 256;

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
///
/// Requests are authorised concurrently against a [`BlobAccessIndex`]: the
/// live namespace, ACL and record grants decide every request, as before,
/// but an unchanged namespace is not rescanned and no request waits for
/// another's decision.
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
    let (sender, mut rx) = EventSender::channel(256, mask);
    let guard = BlobGuard {
        policy,
        index: Arc::new(BlobAccessIndex::new(namespace)),
        store,
        records,
        callers: Arc::new(Mutex::new(HashMap::new())),
        slots: Arc::new(tokio::sync::Semaphore::new(
            MAX_CONCURRENT_BLOB_AUTHORIZATIONS,
        )),
    };
    // The answer to one request, sent once its decision is made.
    macro_rules! reply {
        ($tx:expr) => {{
            let tx = $tx;
            move |result| async move {
                tx.send(result).await.ok();
            }
        }};
    }
    tokio::spawn(async move {
        // A connection's close can be reported before the requests it
        // already carried, and its id can be reused by a later connection.
        // A caller therefore stays registered after its last connection of
        // that id closed, until a bounded number of later closes has passed.
        let mut open: HashMap<u64, usize> = HashMap::new();
        let mut closed: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
        // Revocation, rename, and deleting the last readable reference all
        // take effect for the next request without ending the connection.
        while let Some(msg) = rx.recv().await {
            match msg {
                ProviderMessage::ClientConnected(msg) => {
                    let id = msg.inner.connection_id;
                    let verdict = guard.admit(identity.as_ref(), msg.inner.endpoint_id, id);
                    if verdict.is_ok() {
                        let n = open.entry(id).or_default();
                        *n = n.saturating_add(1);
                    }
                    msg.tx.send(verdict).await.ok();
                }
                ProviderMessage::ConnectionClosed(msg) => {
                    let id = msg.inner.connection_id;
                    let last = open.get_mut(&id).is_none_or(|n| {
                        *n = n.saturating_sub(1);
                        *n == 0
                    });
                    if last {
                        closed.push_back(id);
                    }
                    while closed.len() > RETAINED_CLOSED_CALLERS {
                        let Some(old) = closed.pop_front() else { break };
                        if open.get(&old).is_none_or(|n| *n == 0) {
                            open.remove(&old);
                            guard
                                .callers
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .remove(&old);
                        }
                    }
                }
                ProviderMessage::GetRequestReceived(msg) => {
                    let request = &msg.inner.request;
                    let hash = request.hash;
                    let ask = if request.ranges.is_blob() {
                        BlobAsk::Blobs {
                            wanted: vec![hash],
                            context: Vec::new(),
                        }
                    } else if request.ranges.is_infinite() {
                        // An open-ended sequence would reach past the chunks
                        // of a manifest; this plane never asks for one.
                        msg.tx.send(Err(AbortReason::Permission)).await.ok();
                        continue;
                    } else {
                        // Ranges of a file version: its chunks are readable
                        // exactly when the version is.
                        BlobAsk::Manifest {
                            root: hash,
                            children: request
                                .ranges
                                .iter_non_empty_infinite()
                                .map(|(offset, _)| offset)
                                .collect(),
                        }
                    };
                    guard
                        .decide(msg.inner.connection_id, ask, reply!(msg.tx))
                        .await;
                }
                ProviderMessage::GetManyRequestReceived(msg) => {
                    let request = &msg.inner.request;
                    let mut wanted = Vec::with_capacity(request.hashes.len());
                    let mut context = Vec::new();
                    for (hash, ranges) in request.hashes.iter().zip(request.ranges.iter_infinite())
                    {
                        if ranges.is_empty() {
                            context.push(*hash);
                        } else {
                            wanted.push(*hash);
                        }
                    }
                    guard
                        .decide(
                            msg.inner.connection_id,
                            BlobAsk::Blobs { wanted, context },
                            reply!(msg.tx),
                        )
                        .await;
                }
                ProviderMessage::ObserveRequestReceived(msg) => {
                    let hash = msg.inner.request.hash;
                    guard
                        .decide(
                            msg.inner.connection_id,
                            BlobAsk::Blobs {
                                wanted: vec![hash],
                                context: Vec::new(),
                            },
                            reply!(msg.tx),
                        )
                        .await;
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
        let response = authority_response(
            &self.authority,
            &self.policy,
            self.workspace_id,
            &peer,
            request,
        );
        write_authority_frame(&mut send, &response)
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

/// The authority exchange: the governor may publish a newer signed
/// revision; the governor and workspace readers receive the current one.
fn authority_response(
    authority: &WorkspaceAuthority,
    policy: &PeerPolicy,
    workspace_id: uuid::Uuid,
    peer: &AuthenticatedFsPeer,
    request: AuthorityRequest,
) -> AuthorityResponse {
    if request.workspace != workspace_id {
        return AuthorityResponse { authority: None };
    }
    let governor = peer.user_public_key == authority.governor();
    if let Some(published) = request.publish
        && governor
    {
        let _ = authority.apply(published);
    }
    AuthorityResponse {
        authority: (governor || authenticated_peer_may_read_whole_workspace(policy, peer))
            .then(|| authority.current())
            .flatten(),
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
        replica_ack_response(
            &self.identity,
            &self.policy,
            self.workspace_id,
            &self.repository,
            &self.store,
            peer,
            request,
        )
        .await
    }
}

/// A signed receipt for `request` if this device durably holds the exact
/// revision and `peer` may read the record; shared by the per-request
/// receipt protocol and the session protocol.
async fn replica_ack_response(
    identity: &FsTransportIdentity,
    policy: &PeerPolicy,
    workspace_id: uuid::Uuid,
    repository: &RecordRepository,
    store: &ContentStore,
    peer: &AuthenticatedFsPeer,
    request: &ReplicaAckRequestV1,
) -> ReplicaAckResponseV1 {
    if request.validate().is_err() || request.workspace_id != workspace_id {
        return ReplicaAckResponseV1::Denied;
    }
    let requester = fs_protocol::FileActor {
        person: collab_types::Ed25519PublicKey(peer.user_public_key),
        device: collab_types::Ed25519PublicKey(peer.device_public_key),
    };
    let key = RecordRepositoryKey {
        workspace_id,
        record_id: request.revision.record_id,
    };
    let Ok(snapshot) = repository.snapshot(
        key,
        &requester,
        &DirectMeshAuthorizer {
            peer,
            identity,
            policy,
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
    if store
        .verify_durable_revision(&request.revision)
        .await
        .is_err()
    {
        return ReplicaAckResponseV1::Unavailable;
    }
    let actor = fs_protocol::FileActor {
        person: collab_types::Ed25519PublicKey(identity.user_public_key()),
        device: collab_types::Ed25519PublicKey(identity.device_public_key()),
    };
    match ReplicaAckV1::signed(
        request,
        actor,
        unix_now_ms(),
        identity.credentials().signing_key(),
    ) {
        Ok(receipt) => ReplicaAckResponseV1::Acknowledged(receipt),
        Err(_) => ReplicaAckResponseV1::Unavailable,
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

/// The local node a workspace's sync runs on: its endpoint and peer lookup,
/// content store and hydrator, the namespace it projects, the transport
/// identity it authenticates as and the [`PeerPolicy`] it serves through.
pub struct SyncNode {
    pub endpoint: Endpoint,
    pub lookup: MemoryLookup,
    pub store: Arc<ContentStore>,
    pub hydrator: Arc<Hydrator>,
    pub namespace: Arc<Mutex<Namespace>>,
    pub identity: FsTransportIdentity,
    pub policy: PeerPolicy,
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
            SyncNode {
                endpoint,
                lookup,
                store,
                hydrator,
                namespace,
                identity,
                policy,
            },
            None,
        )
        .await
    }

    /// Host a workspace with the exact-record sharing protocol enabled.
    ///
    /// # Errors
    ///
    /// Returns an error when the gossip topic or protocol router fails to start.
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
            SyncNode {
                endpoint,
                lookup,
                store,
                hydrator,
                namespace,
                identity,
                policy,
            },
            Some(records),
        )
        .await
    }

    async fn host_inner(
        node: SyncNode,
        records: Option<RecordServices>,
    ) -> Result<(Workspace, Ticket)> {
        let SyncNode {
            endpoint,
            lookup,
            store,
            hydrator,
            namespace,
            identity,
            policy,
        } = node;
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
        let sessions = Arc::new(SessionPool::new(
            endpoint.clone(),
            identity.clone(),
            ticket.workspace_id(),
        ));
        let ws = Self::start(
            SyncNode {
                endpoint,
                lookup,
                store,
                hydrator,
                namespace,
                identity,
                policy,
            },
            (topic, ticket.workspace_id()),
            vec![],
            record_plane,
            sessions,
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
            SyncNode {
                endpoint,
                lookup,
                store,
                hydrator,
                namespace,
                identity,
                policy,
            },
            ticket,
            None,
        )
        .await
    }

    /// Join a workspace and serve/consume its exact-record sharing protocol.
    ///
    /// # Errors
    ///
    /// Returns an error if the gossip topic or protocol router fails to start.
    pub async fn join_with_records(
        node: SyncNode,
        ticket: Ticket,
        records: RecordServices,
    ) -> Result<Workspace> {
        Self::join_inner(node, ticket, Some(records)).await
    }

    async fn join_inner(
        node: SyncNode,
        ticket: Ticket,
        records: Option<RecordServices>,
    ) -> Result<Workspace> {
        let SyncNode {
            endpoint,
            lookup,
            store,
            hydrator,
            namespace,
            identity,
            policy,
        } = node;
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
        let sessions = Arc::new(SessionPool::new(
            endpoint.clone(),
            identity.clone(),
            ticket.workspace_id(),
        ));
        if let Some(records) = &record_plane {
            let initial_mesh = tokio::time::timeout(
                Duration::from_secs(10),
                pull_records_from_peer(
                    &sessions,
                    &endpoint,
                    ticket.node.clone(),
                    &identity,
                    &policy,
                    records,
                    &hydrator,
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
            SyncNode {
                endpoint,
                lookup,
                store,
                hydrator,
                namespace,
                identity,
                policy,
            },
            (topic, ticket.workspace_id()),
            vec![ticket.node],
            record_plane,
            sessions,
        )
        .await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "startup owns the ordered lifecycle and cleanup of every sync protocol task"
    )]
    async fn start(
        node: SyncNode,
        (topic_bytes, workspace_id): ([u8; 32], uuid::Uuid),
        bootstrap: Vec<EndpointAddr>,
        record_plane: Option<RecordPlane>,
        sessions: Arc<SessionPool>,
    ) -> Result<Workspace> {
        let SyncNode {
            endpoint,
            lookup,
            store,
            hydrator,
            namespace,
            identity,
            policy,
        } = node;
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
            workspace_id,
            inflight: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_SYNC_CONNECTIONS)),
            records: record_plane
                .as_ref()
                .map(|records| Arc::clone(&records.repository)),
        };
        let replica_ack_protocol = record_plane.as_ref().map(|records| ReplicaAckProtocol {
            endpoint: endpoint.clone(),
            identity: identity.clone(),
            policy: policy.clone(),
            workspace_id: records.workspace_id,
            repository: Arc::clone(&records.repository),
            store: Arc::clone(&store),
        });
        let (announce_tx, announce_rx) = mpsc::unbounded_channel();
        let session_proto = SessionProtocol {
            endpoint: endpoint.clone(),
            identity: identity.clone(),
            policy: policy.clone(),
            namespace: namespace.clone(),
            store: Arc::clone(&store),
            records: record_plane.clone(),
            workspace_id,
            announcements: announce_tx,
            pool: Arc::downgrade(&sessions),
        };
        sessions.serve_with(session_proto.clone());
        let gossip = Gossip::builder().spawn(endpoint.clone());
        let builder = Router::builder(endpoint.clone())
            .accept(TILO_FS_SESSION_ALPN, session_proto)
            .accept(
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
                record_plane
                    .as_ref()
                    .map(|records| Arc::clone(&records.repository)),
            ))));
            (Some(sender), None)
        } else {
            // Enforced workspaces cannot relay unauthenticated gossip deltas,
            // but local mutations still wake authenticated anti-entropy. This
            // avoids making a live NAS wait for a polling tick and provides a
            // prompt retry after a transient exchange failure.
            (None, Some(local_rx))
        };
        // Anti-entropy backstop: periodically exchange namespace changes and
        // records with every peer and resolve conflicts, so a missed delta
        // or a mid-join window can't leave a peer permanently diverged.
        tasks.push(AbortOnDrop(tokio::spawn(reconcile_loop(ReconcileLoop {
            endpoint,
            namespace: namespace.clone(),
            hydrator: hydrator.clone(),
            sender: gossip_sender,
            identity,
            policy,
            workspace_id,
            records: record_plane,
            local_rx: reconcile_rx,
            announcements: announce_rx,
            sessions,
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

/// The bidirectional pump: broadcast local deltas, apply remote deltas,
/// and learn peer addresses for hydration as neighbors come up.
async fn pump_loop(
    mut receiver: GossipReceiver,
    sender: GossipSender,
    mut local_rx: mpsc::UnboundedReceiver<Bytes>,
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    sync: Arc<SyncHandle>,
    records: Option<Arc<RecordRepository>>,
) {
    loop {
        tokio::select! {
            maybe = local_rx.recv() => {
                match maybe {
                    Some(delta) => {
                        // A delta carries every change since the previous
                        // one: never send it in the middle of a changeset.
                        let _gate = changeset_boundary(records.as_deref()).await;
                        let _ = sender.broadcast(delta).await;
                    }
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
                            let _gate = changeset_boundary(records.as_deref()).await;
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
    fn actor_is_certified(&self, actor: &fs_protocol::FileActor) -> bool {
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
        actor: &fs_protocol::FileActor,
        _record_id: fs_protocol::FileRecordId,
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
        let direct_actor = fs_protocol::FileActor {
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
                    materializer.reconcile_record(&records.repository, key)?;
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
            unix_now_ms(),
        )?;
        connection.close(0u32.into(), b"");
        Ok::<_, anyhow::Error>(observed)
    })
    .await
    .map_err(|_| anyhow::anyhow!("replica acknowledgement exchange timed out"))?
}

/// A receipt with less than this left is renewed.
const RECEIPT_RENEW_BEFORE_MS: u64 = 60 * 1000;

/// Current revisions `device` has not acknowledged recently: receipts last
/// minutes, so a pass asks only for new revisions and ones about to lapse,
/// not for every record every few seconds.
fn revisions_awaiting_receipt(
    records: &RecordPlane,
    registry: &Mutex<crate::availability::AvailabilityRegistry>,
    device: collab_types::Ed25519PublicKey,
) -> Vec<fs_protocol::ContentRevisionV1> {
    let revisions = records
        .repository
        .current_content_revisions(records.workspace_id);
    let renew_after = unix_now_ms().saturating_add(RECEIPT_RENEW_BEFORE_MS);
    let registry = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    revisions
        .into_iter()
        .filter(|revision| {
            !registry.get(revision.record_id).is_some_and(|record| {
                record.current_version == Some(revision.version_id)
                    && record.replica_observations.iter().any(|receipt| {
                        receipt.device == device
                            && receipt.version_id == revision.version_id
                            && receipt.expires_at_ms > renew_after
                    })
            })
        })
        .collect()
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
    let device = collab_types::Ed25519PublicKey(*remote.id.as_bytes());
    for revision in revisions_awaiting_receipt(records, registry, device) {
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
    revision: fs_protocol::ContentRevisionV1,
    local_device: collab_types::Ed25519PublicKey,
    observation: crate::availability::ReplicaObservation,
    observed_at: u64,
) -> Result<(), crate::availability::AvailabilityError> {
    let mut registry = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    record_replica_observation(
        &mut registry,
        revision,
        local_device,
        observation,
        observed_at,
    )
}

/// Records a verified receipt for `revision`, tracking the revision first
/// when this registry has no row for it yet.
fn record_replica_observation(
    registry: &mut crate::availability::AvailabilityRegistry,
    revision: fs_protocol::ContentRevisionV1,
    local_device: collab_types::Ed25519PublicKey,
    observation: crate::availability::ReplicaObservation,
    observed_at: u64,
) -> Result<(), crate::availability::AvailabilityError> {
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

/// How often every peer is reconciled without a change or announcement.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(3);
/// Peers one pass exchanges with at once.
const PASS_CONCURRENCY: usize = 8;
/// How long a pass waits for one peer's exchange once it has begun. The
/// exchange is not cancelled (that could stop between a durable record and
/// its projection); it finishes on its own and holds its slot, and its peer
/// is not started again until it has. The pass moves on without it.
const PEER_DEADLINE: Duration = Duration::from_secs(20);

/// One peer's share of a reconciliation pass.
struct PeerPass<'a> {
    endpoint: &'a Endpoint,
    namespace: &'a Arc<Mutex<Namespace>>,
    hydrator: &'a Hydrator,
    sender: Option<&'a GossipSender>,
    identity: &'a FsTransportIdentity,
    policy: &'a PeerPolicy,
    /// The workspace every namespace payload must be bound to.
    workspace_id: uuid::Uuid,
    records: Option<&'a RecordPlane>,
    /// Both sides write the whole workspace: exchange namespace changes.
    exchange_namespace: bool,
    /// This device may not write: only take the peer's namespace changes.
    read_only: bool,
    /// Which records to project again even if nothing new arrives: all of
    /// them on the first pass, the ones still unplaced on timer passes.
    reconcile: crate::record_materializer::ReconcileScope,
}

impl PeerPass<'_> {
    async fn broadcast(&self, delta: Vec<u8>) {
        if !delta.is_empty()
            && let Some(sender) = self.sender
        {
            // A delta carries every change since the last one, including a
            // changeset being published meanwhile.
            let _gate = changeset_boundary(self.records.map(|r| &*r.repository)).await;
            let _ = sender.broadcast(Bytes::from(delta)).await;
        }
    }

    /// The pass over the peer's session: every exchange on its one
    /// connection. Returns whether namespace or records changed.
    async fn over_session(&self, sessions: &SessionPool, peer: &session::Session) -> bool {
        // Each exchange stands on its own, as with the per-request
        // protocols: one the peer refuses does not hold back the others.
        let report = |what: &str, error: &anyhow::Error| {
            if !self.identity.partitioned() {
                eprintln!("{what} with {} failed: {error:#}", peer.addr.id);
            }
        };
        let mut changed = false;
        if self.exchange_namespace {
            // Automerge sync is symmetric; only between two writers, which
            // the serving side checks independently.
            match session::sync_namespace(
                sessions,
                peer,
                self.namespace,
                self.records.map(|records| &*records.repository),
                self.policy,
            )
            .await
            {
                Ok(received) => {
                    changed |= received;
                    let resolution = self
                        .namespace
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .post_sync_resolve();
                    self.broadcast(resolution).await;
                }
                Err(error) => report("namespace sync", &error),
            }
        } else if self.read_only {
            match session::pull_namespace(sessions, peer, self.namespace, self.policy).await {
                Ok((received, resolution)) => {
                    changed |= received;
                    self.broadcast(resolution).await;
                }
                Err(error) => report("namespace pull", &error),
            }
        }
        let namespace_done = Instant::now();
        if let Some(records) = self.records {
            if let Some(authority) = &records.workspace_authority
                && let Err(error) = session::exchange_authority(
                    sessions,
                    peer,
                    self.identity,
                    authority,
                    records.workspace_id,
                )
                .await
            {
                report("workspace authority reconciliation", &error);
            }
            // A namespace change may place records that arrived earlier.
            let scope = match self.reconcile {
                crate::record_materializer::ReconcileScope::Touched if changed => {
                    crate::record_materializer::ReconcileScope::Unplaced
                }
                scope => scope,
            };
            let pull_started = Instant::now();
            match session::pull_records(
                sessions,
                peer,
                self.identity,
                self.policy,
                records,
                self.hydrator,
                scope,
            )
            .await
            {
                Ok(pulled) => changed |= pulled,
                Err(error) => report("record reconciliation", &error),
            }
            let pull_done = Instant::now();
            if let Err(error) =
                session::collect_replica_acks(sessions, peer, self.identity, records).await
            {
                report("replica receipts", &error);
            }
            if std::env::var_os("TILO_FS_NATIVE_TRACE").is_some() {
                eprintln!(
                    "  session with {}: authority {} ms, records {} ms, receipts {} ms",
                    peer.addr.id.fmt_short(),
                    pull_started.duration_since(namespace_done).as_millis(),
                    pull_done.duration_since(pull_started).as_millis(),
                    pull_done.elapsed().as_millis()
                );
            }
        }
        changed
    }

    /// The pass through the per-request protocols, for a peer without the
    /// session protocol. Returns whether namespace or records changed.
    async fn per_request(&self, peer: EndpointAddr) -> bool {
        let mut changed = false;
        if self.exchange_namespace {
            match sync_with_peer_at_boundaries(
                self.endpoint,
                peer.clone(),
                self.namespace,
                self.identity,
                (self.workspace_id, self.policy),
                self.records.map(|records| &*records.repository),
            )
            .await
            {
                Ok(received_change) => {
                    changed |= received_change;
                    let resolution = self
                        .namespace
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .post_sync_resolve();
                    self.broadcast(resolution).await;
                }
                Err(error) if !self.identity.partitioned() => {
                    eprintln!("namespace sync with {} failed: {error:#}", peer.id);
                }
                Err(_) => {}
            }
        }
        // A read-only device takes namespace changes only over a session,
        // as the changes beyond its own heads ([`session::pull_namespace`]).
        // A peer without the session protocol is never asked for its whole
        // document on every pass; it serves this device records only.
        self.per_request_records(peer, changed).await
    }

    /// The record part of [`Self::per_request`].
    async fn per_request_records(&self, peer: EndpointAddr, mut changed: bool) -> bool {
        if let Some(records) = self.records {
            if let Some(authority) = &records.workspace_authority
                && let Err(error) = pull_workspace_authority_from_peer(
                    self.endpoint,
                    peer.clone(),
                    self.identity,
                    authority,
                    records.workspace_id,
                )
                .await
                && !self.identity.partitioned()
            {
                eprintln!("workspace authority reconciliation failed: {error}");
            }
            match pull_record_mesh_from_peer(
                self.endpoint,
                peer.clone(),
                self.identity,
                self.policy,
                records,
            )
            .await
            {
                Ok(pulled) => changed |= pulled,
                Err(error) if !self.identity.partitioned() => {
                    eprintln!("record mesh reconciliation failed: {error}");
                }
                Err(_) => {}
            }
            collect_replica_acks_from_peer(
                self.endpoint,
                peer,
                self.identity,
                self.policy,
                records,
            )
            .await;
        }
        changed
    }
}

/// What every peer exchange of the reconciliation loop works on. Owned, so
/// an exchange can outlive the pass that stopped waiting for it.
struct PassShared {
    endpoint: Endpoint,
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    sender: Option<GossipSender>,
    identity: FsTransportIdentity,
    policy: PeerPolicy,
    workspace_id: uuid::Uuid,
    records: Option<RecordPlane>,
    sessions: Arc<SessionPool>,
    /// This device may write the whole workspace.
    may_write: bool,
}

/// What one pass does with each peer.
#[derive(Clone, Copy, Debug)]
struct PassPlan {
    /// Tell the peer first that this device published changes, so it pulls
    /// them without waiting for its own timer.
    announce: bool,
    reconcile: crate::record_materializer::ReconcileScope,
}

impl PassShared {
    /// One peer's share of a pass: the announcement if `plan` asks for it,
    /// then every exchange over the peer's session, or through the
    /// per-request protocols when it speaks no session. Returns whether
    /// namespace or records changed.
    async fn exchange(&self, peer: EndpointAddr, plan: PassPlan) -> bool {
        let session = match self.sessions.get(&peer).await {
            Ok(session) => session,
            Err(error) => {
                if !self.identity.partitioned() {
                    eprintln!("no session with {}: {error:#}", peer.id);
                }
                return false;
            }
        };
        let remote_may_write = self.policy.is_none()
            || self
                .identity
                .certified_peer_for_device(*peer.id.as_bytes())
                .is_some_and(|remote| {
                    authenticated_peer_may_write_whole_workspace(&self.policy, &remote)
                });
        let context = PeerPass {
            endpoint: &self.endpoint,
            namespace: &self.namespace,
            hydrator: &self.hydrator,
            sender: self.sender.as_ref(),
            identity: &self.identity,
            policy: &self.policy,
            workspace_id: self.workspace_id,
            records: self.records.as_ref(),
            exchange_namespace: self.may_write && remote_may_write,
            read_only: !self.may_write,
            reconcile: plan.reconcile,
        };
        let Some(session) = session else {
            // The peer speaks only the per-request protocols.
            return context.per_request(peer).await;
        };
        if plan.announce
            && let Err(error) = session::announce(&self.sessions, &session).await
        {
            eprintln!("announcing to {} failed: {error:#}", session.addr.id);
        }
        let changed = context.over_session(&self.sessions, &session).await;
        if session.is_closed() {
            // The next pass connects afresh.
            self.sessions.forget(&session).await;
        }
        changed
    }
}

/// Runs each pass's peer exchanges concurrently: at most
/// [`PASS_CONCURRENCY`] at once, at most one per peer at a time, and each
/// waited for at most `deadline`, so one unreachable or stalled peer holds
/// back neither the others nor the pass.
struct PeerExchanges {
    shared: Arc<PassShared>,
    slots: Arc<tokio::sync::Semaphore>,
    /// Peers with an exchange in progress, possibly from an earlier pass.
    busy: Arc<Mutex<HashSet<iroh_base::PublicKey>>>,
    /// Set by an exchange that changed namespace or records; taken by the
    /// pass that next looks, which covers exchanges that outlived their
    /// own pass.
    changed: Arc<std::sync::atomic::AtomicBool>,
    deadline: Duration,
}

/// A peer's place in [`PeerExchanges::busy`], given back when its exchange
/// ends however it ends.
struct BusyPeer {
    busy: Arc<Mutex<HashSet<iroh_base::PublicKey>>>,
    peer: iroh_base::PublicKey,
}

impl Drop for BusyPeer {
    fn drop(&mut self) {
        self.busy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.peer);
    }
}

impl PeerExchanges {
    fn new(shared: PassShared, deadline: Duration) -> Self {
        Self {
            shared: Arc::new(shared),
            slots: Arc::new(tokio::sync::Semaphore::new(PASS_CONCURRENCY)),
            busy: Arc::new(Mutex::new(HashSet::new())),
            changed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            deadline,
        }
    }

    /// One pass over `peers`, skipping any still exchanging since an
    /// earlier pass. Returns whether namespace or records changed since the
    /// previous pass looked.
    async fn run(&self, peers: Vec<EndpointAddr>, plan: PassPlan) -> bool {
        let started: Vec<_> = peers
            .into_iter()
            .filter_map(|peer| {
                let claimed = self
                    .busy
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(peer.id);
                claimed.then(|| {
                    let busy = BusyPeer {
                        busy: Arc::clone(&self.busy),
                        peer: peer.id,
                    };
                    self.start(peer, busy, plan)
                })
            })
            .collect();
        futures_util::future::join_all(started).await;
        self.changed
            .swap(false, std::sync::atomic::Ordering::AcqRel)
    }

    /// Starts the exchange with `peer` once a slot is free, and waits for it
    /// at most the deadline.
    async fn start(&self, peer: EndpointAddr, busy: BusyPeer, plan: PassPlan) {
        let Ok(slot) = Arc::clone(&self.slots).acquire_owned().await else {
            return;
        };
        let shared = Arc::clone(&self.shared);
        let changed = Arc::clone(&self.changed);
        let exchange = tokio::spawn(async move {
            if shared.exchange(peer, plan).await {
                changed.store(true, std::sync::atomic::Ordering::Release);
            }
            drop(busy);
            drop(slot);
        });
        // On timeout the task carries on detached; dropping its handle does
        // not abort it.
        let _ = tokio::time::timeout(self.deadline, exchange).await;
    }
}

/// The records a peer holds that this device lacks, over a session when the
/// peer speaks it and through the per-request mesh otherwise.
async fn pull_records_from_peer(
    sessions: &SessionPool,
    endpoint: &Endpoint,
    remote: EndpointAddr,
    identity: &FsTransportIdentity,
    policy: &PeerPolicy,
    records: &RecordPlane,
    hydrator: &Hydrator,
) -> Result<bool> {
    match sessions.get(&remote).await? {
        Some(session) => {
            let pulled = session::pull_records(
                sessions,
                &session,
                identity,
                policy,
                records,
                hydrator,
                crate::record_materializer::ReconcileScope::Everything,
            )
            .await;
            if pulled.is_err() {
                sessions.forget(&session).await;
            }
            pulled
        }
        None => pull_record_mesh_from_peer(endpoint, remote, identity, policy, records).await,
    }
}

struct ReconcileLoop {
    endpoint: Endpoint,
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    sender: Option<GossipSender>,
    identity: FsTransportIdentity,
    policy: PeerPolicy,
    workspace_id: uuid::Uuid,
    records: Option<RecordPlane>,
    local_rx: Option<mpsc::UnboundedReceiver<Bytes>>,
    /// Peers that announced new changes over a session.
    announcements: mpsc::UnboundedReceiver<iroh_base::PublicKey>,
    sessions: Arc<SessionPool>,
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
        workspace_id,
        records,
        mut local_rx,
        mut announcements,
        sessions,
        sync,
    } = reconcile;
    let may_write = peer_may_write_whole_workspace(&policy, &identity.user_public_key())
        || peer_may_write_whole_workspace(&policy, &identity.device_public_key());
    let exchanges = PeerExchanges::new(
        PassShared {
            endpoint,
            namespace: namespace.clone(),
            hydrator: hydrator.clone(),
            sender: sender.clone(),
            identity: identity.clone(),
            policy,
            workspace_id,
            records: records.clone(),
            sessions,
            may_write,
        },
        PEER_DEADLINE,
    );
    let mut pending_thumbnails = HashMap::new();
    // `mount` bootstraps a namespace before this task starts, so schedule its
    // previews once even if the first anti-entropy exchange is clean.
    let mut refresh_thumbnails = true;
    let mut first_pass = true;
    let trace = std::env::var_os("TILO_FS_NATIVE_TRACE").is_some();
    loop {
        // A local change or a peer's announcement starts a pass at once;
        // otherwise the timer does.
        let (local_change, timer) = tokio::select! {
            () = tokio::time::sleep(RECONCILE_INTERVAL) => (false, true),
            mutation = async {
                match &mut local_rx {
                    Some(receiver) => receiver.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if mutation.is_none() {
                    local_rx = None;
                }
                (true, false)
            }
            Some(_) = announcements.recv() => (false, false),
        };
        // A burst of changes is one pass: what arrived meanwhile is covered.
        let local_change = local_change
            | local_rx.as_mut().is_some_and(|receiver| {
                std::iter::from_fn(|| receiver.try_recv().ok()).count() > 0
            });
        while announcements.try_recv().is_ok() {}
        let pass_started = Instant::now();
        if trace {
            eprintln!("  sync pass begins (local change {local_change}, timer {timer})");
        }
        for peer in identity.authenticated_peer_addrs() {
            hydrator.add_peer(peer);
        }
        let known_peers = hydrator.known_peers();
        let peer_count = known_peers.len();
        // Peers pull what this device just published without waiting for
        // their own timer, or for this pass: each is announced to first, on
        // the session its exchange then uses.
        let plan = PassPlan {
            announce: local_change,
            // Namespace changes can also arrive through a peer's own
            // exchange with this device; timer passes heal whatever they
            // placed.
            reconcile: if first_pass {
                crate::record_materializer::ReconcileScope::Everything
            } else if timer {
                crate::record_materializer::ReconcileScope::Unplaced
            } else {
                crate::record_materializer::ReconcileScope::Touched
            },
        };
        let namespace_changed = exchanges.run(known_peers, plan).await;
        first_pass = false;
        if trace {
            eprintln!(
                "  sync pass over {peer_count} peer(s) took {} ms (changed {namespace_changed})",
                pass_started.elapsed().as_millis()
            );
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

        // Conflicts come from what peers sent: a pass that only announced
        // this device's own changes and received nothing has none to find,
        // and a burst of local changes must not rescan every record per
        // change. Timer passes still look.
        if !may_write || (local_change && !namespace_changed) {
            continue;
        }
        if let Some(records) = &records {
            let actor = fs_protocol::FileActor {
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
                    crate::conflict::materialize(&mut ns, &c, len, unix_now_ms())
                };
                if !delta.is_empty()
                    && let Some(sender) = &sender
                {
                    let _gate = changeset_boundary(records.as_ref().map(|r| &*r.repository)).await;
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

mod session;
pub use session::TILO_FS_SESSION_ALPN;

#[cfg(test)]
mod tests;
