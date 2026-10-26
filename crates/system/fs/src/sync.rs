//! The sync plane: signed-record replication and content hydration.
//!
//! Structural truth is the signed record log. The namespace is a local
//! projection of those records, so peers exchange records and never a second
//! Automerge tree. The retired whole-namespace ALPN answers with an explicit
//! unsupported-version refusal. Iroh-blobs hydrates content on read, guarded
//! by the live namespace and ACL.

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
use iroh_gossip::api::{Event, GossipReceiver};
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
use crate::peer_auth::AuthenticatedFsPeer;
use crate::record_access::RecordAccessRegistry;
use crate::record_protocol::{RecordProtocol, TILO_FS_RECORD_ALPN};
use crate::record_replication::{RecordAction, RecordAuthorizer};
use crate::record_repository::{RecordRepository, RecordRepositoryKey};
use crate::replica_ack::{ReplicaAckRequestV1, ReplicaAckResponseV1, ReplicaAckV1};
use crate::share_authority::ShareAuthority;
use crate::share_session::LiveActorCertification;
use crate::store::ContentStore;
use crate::workspace_authority::{
    AuthorityRequest, AuthorityResponse, TILO_FS_AUTHORITY_ALPN, WorkspaceAuthority,
    read_frame as read_authority_frame, write_frame as write_authority_frame,
};
use session::{SessionPool, SessionProtocol};
use tilo_lock::MutexRecover as _;

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

/// The retired sync ALPN's response frame. Old peers still reach this ALPN;
/// they are told the whole-namespace protocol is unsupported rather than
/// served by a second structural engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum NamespaceFrameV1 {
    /// The server does not serve this request.
    Refused(NamespaceRefusalV1),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum NamespaceRefusalV1 {
    /// The whole-namespace Automerge protocol is retired.
    UnsupportedVersion,
    /// The request names a workspace this device does not serve here.
    OtherWorkspace,
    /// The requester may not read (or, to sync, write) the whole workspace.
    NotAuthorised,
}

impl std::fmt::Display for NamespaceRefusalV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedVersion => {
                "the whole-namespace protocol is unsupported; use the signed record protocol"
            }
            Self::OtherWorkspace => "the peer does not serve the requested workspace",
            Self::NotAuthorised => "the peer does not serve this workspace's namespace to us",
        })
    }
}

async fn write_namespace_frame(
    send: &mut SendStream,
    frame: &NamespaceFrameV1,
) -> std::io::Result<()> {
    Ok(SYNC_FRAMING.write_value_async(send, frame).await?)
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
    pub fn encode(&self) -> Result<String> {
        Ok(hex::encode(postcard::to_allocvec(self)?))
    }

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
pub async fn bind_endpoint_with_key(secret: iroh::SecretKey) -> Result<(Endpoint, MemoryLookup)> {
    let lookup = MemoryLookup::new();
    let endpoint = endpoint_builder(secret, &lookup).bind().await?;
    Ok((endpoint, lookup))
}

/// [`bind_endpoint_with_key`], with the default relays held back until
/// something may need to dial this device: no home relay, net report,
/// address discovery or port mapping until the returned
/// [`crate::reachability::Reachability`] is woken. For a daemon
/// that waits to be dialled (`fs serve`); one that dials out at once has
/// no reason to wait.
pub async fn bind_dormant_endpoint_with_key(
    secret: iroh::SecretKey,
) -> Result<(Endpoint, MemoryLookup, crate::reachability::Reachability)> {
    let lookup = MemoryLookup::new();
    let endpoint = endpoint_builder(secret, &lookup)
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::empty()))
        .bind()
        .await?;
    let reachability = crate::reachability::Reachability::defer(
        &endpoint,
        &iroh::endpoint::default_relay_mode().relay_map(),
        tokio::runtime::Handle::current(),
        None,
    );
    Ok((endpoint, lookup, reachability))
}

/// How long a connection, and each of its paths, may go without traffic
/// before it sends a keep-alive. iroh's default is 5 s on every path, for
/// quick failover on busy connections; a connection that carries traffic
/// never sends one, so this only paces idle ones, such as a share
/// recipient's held stream and its pooled blob connection, each of which
/// cost both ends a wakeup per path every 5 s. 25 s keeps the NAT binding
/// of a UDP flow (commonly 30 s or more) and stays below a relay path's
/// 30 s idle timeout.
pub(crate) const IDLE_KEEP_ALIVE: Duration = Duration::from_secs(25);

/// How long a connection, or one of its paths, may hear nothing before it
/// is given up, in milliseconds: room for a keep-alive and its
/// retransmissions. A peer that vanished without closing is noticed within
/// this. iroh's defaults are 30 s per connection and 15 s per path, which
/// a 25 s keep-alive would not reliably beat.
pub(crate) const IDLE_TIMEOUT_MS: u32 = 60_000;

/// The endpoint a mount or a `fs serve` reaches its peers on: number 0's
/// relays, and number 0's directory only as a reader — a filesystem peer is
/// dialled by the devices that mounted it, which hold its address already,
/// so publishing its key would only sign in for a lookup nothing makes.
fn endpoint_builder(secret: iroh::SecretKey, lookup: &MemoryLookup) -> iroh::endpoint::Builder {
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
        .keep_alive_interval(IDLE_KEEP_ALIVE)
        .default_path_keep_alive_interval(IDLE_KEEP_ALIVE)
        .default_path_max_idle_timeout(Duration::from_millis(u64::from(IDLE_TIMEOUT_MS)))
        .max_idle_timeout(Some(VarInt::from_u32(IDLE_TIMEOUT_MS).into()))
        .build();
    Endpoint::builder(presets::Minimal)
        .address_lookup(tilo_endpoint_reachability::Watchonly)
        .relay_mode(iroh::endpoint::default_relay_mode())
        .transport_config(transport)
        .secret_key(secret)
        .address_lookup(lookup.clone())
}

/// Bind an endpoint under a freshly-generated key. **The node id differs
/// on every call**, so nothing durable may be keyed on it — for tests
/// and throwaway probes only; a real mount uses
/// [`bind_endpoint_with_key`] so its identity is the device's.
pub async fn bind_ephemeral_endpoint() -> Result<(Endpoint, MemoryLookup)> {
    bind_endpoint_with_key(iroh::SecretKey::generate()).await
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
            .lock_recover()
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
/// Refuses the retired whole-namespace Automerge ALPN by version. Signed
/// records are the only structural replication.
struct LegacyNamespaceProtocol {
    endpoint: Endpoint,
    identity: FsTransportIdentity,
    inflight: Arc<tokio::sync::Semaphore>,
}

impl std::fmt::Debug for LegacyNamespaceProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LegacyNamespaceProtocol")
    }
}

impl ProtocolHandler for LegacyNamespaceProtocol {
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
        let exchange = async {
            let _peer = authenticate_connection(
                &self.endpoint,
                &connection,
                &mut recv,
                &mut send,
                &self.identity,
            )
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
            // Read the old request so a client that already sent it sees a
            // framed refusal rather than a transport error. Its contents do
            // not matter: no namespace bytes are ever served.
            let _ = SYNC_FRAMING
                .read_async(&mut recv)
                .await
                .and_then(|frame| frame.ok_or(FrameError::Closed));
            write_namespace_frame(
                &mut send,
                &NamespaceFrameV1::Refused(NamespaceRefusalV1::UnsupportedVersion),
            )
            .await
            .map_err(AcceptError::from_err)?;
            let _ = send.finish();
            Ok::<(), AcceptError>(())
        };
        match tokio::time::timeout(Duration::from_secs(30), exchange).await {
            Ok(res) => res?,
            Err(_) => return Ok(()),
        }
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
/// One blob request checked the way the provider checks it, against a
/// fresh index of the namespace.
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
                    .lock_recover()
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

        let caller = self.callers.lock_recover().get(&connection).copied();
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
                            guard.callers.lock_recover().remove(&old);
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
    /// Woken when peers are heard from outside the reconciliation loop (a
    /// gossip delta, a neighbour), so its idle timer stops backing off.
    activity: Arc<tokio::sync::Notify>,
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

    /// Peers were heard from outside the reconciliation loop.
    fn notify_activity(&self) {
        self.activity.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn test_handle() -> Arc<Self> {
        let (local_tx, _local_rx) = mpsc::unbounded_channel();
        let (record_index_generation, _) = tokio::sync::watch::channel(0_u64);
        Arc::new(Self {
            local_tx,
            record_index_generation,
            activity: Arc::new(tokio::sync::Notify::new()),
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

/// A signed receipt for `request` if this device durably holds the exact
/// revision and `peer` may read the record, as a session answers it.
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

fn build_record_plane(
    endpoint: &Endpoint,
    identity: &FsTransportIdentity,
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
    Ok(Some(RecordPlane {
        protocol,
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

    /// Join an existing workspace described by `ticket`. A signed-record
    /// joiner starts with an empty local projection and rebuilds it from its
    /// first record pull; a joiner serves too (anti-entropy is symmetric), so
    /// it carries the same `policy` a host does.
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
                // peer after startup.
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

        let legacy_namespace_proto = LegacyNamespaceProtocol {
            endpoint: endpoint.clone(),
            identity: identity.clone(),
            inflight: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_SYNC_CONNECTIONS)),
        };
        let (announce_tx, announce_rx) = mpsc::unbounded_channel();
        let session_proto = SessionProtocol {
            endpoint: endpoint.clone(),
            identity: identity.clone(),
            policy: policy.clone(),
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
        let builder = match &record_plane {
            Some(records) => builder.accept(TILO_FS_RECORD_ALPN, records.protocol.clone()),
            None => builder,
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
                .accept(TILO_FS_SYNC_ALPN, legacy_namespace_proto)
                .spawn()
        } else {
            builder
                .accept(TILO_FS_SYNC_ALPN, legacy_namespace_proto)
                .spawn()
        };

        let (local_tx, local_rx) = mpsc::unbounded_channel();
        let (record_index_generation, _) = tokio::sync::watch::channel(0_u64);
        let sync = Arc::new(SyncHandle {
            local_tx,
            record_index_generation,
            activity: Arc::new(tokio::sync::Notify::new()),
        });
        let mut tasks = Vec::new();
        let gossip_discovery = policy.is_none();
        // Anti-entropy backstop: pull signed records from every peer, so a
        // missed announcement or a mid-join window cannot leave a peer
        // permanently diverged.
        let reconcile_rx = if gossip_discovery {
            // Gossip only discovers neighbours. Its messages carry no
            // structural state: signed records are the one replication.
            let topic = TopicId::from_bytes(topic_bytes);
            let gtopic = if boot_ids.is_empty() {
                gossip.subscribe(topic, vec![]).await?
            } else {
                gossip.subscribe_and_join(topic, boot_ids).await?
            };
            let (_sender, receiver) = gtopic.split();
            tasks.push(AbortOnDrop(tokio::spawn(pump_loop(
                receiver,
                local_rx,
                hydrator.clone(),
                Arc::clone(&sync),
            ))));
            None
        } else {
            Some(local_rx)
        };
        tasks.push(AbortOnDrop(tokio::spawn(reconcile_loop(ReconcileLoop {
            namespace: namespace.clone(),
            hydrator: hydrator.clone(),
            identity,
            policy,
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
    namespace.lock_recover().prime_baseline();
}

/// The bidirectional pump: broadcast local deltas, apply remote deltas,
/// and learn peer addresses for hydration as neighbors come up.
async fn pump_loop(
    mut receiver: GossipReceiver,
    mut local_rx: mpsc::UnboundedReceiver<Bytes>,
    hydrator: Arc<Hydrator>,
    sync: Arc<SyncHandle>,
) {
    loop {
        tokio::select! {
            maybe = local_rx.recv() => {
                match maybe {
                    Some(_delta) => {
                        // A local structural change wakes record anti-entropy.
                        // The namespace delta itself is never replicated: the
                        // signed records are.
                        sync.notify_activity();
                    }
                    None => break, // all SyncHandles dropped
                }
            }
            ev = receiver.next() => {
                match ev {
                    Some(Ok(Event::Received(_))) => {
                        // Gossip is discovery only. Structural truth arrives
                        // through the authenticated record session.
                        sync.notify_activity();
                    }
                    Some(Ok(Event::NeighborUp(id))) => {
                        hydrator.add_peer(EndpointAddr::new(id));
                        sync.notify_activity();
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => {
                        // Transient gossip error: don't kill discovery; back
                        // off briefly and keep going.
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

/// Exchange the current signed workspace authority with one authenticated peer.
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
    let registry = registry.lock_recover();
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

/// How soon every peer is reconciled again after anything happened (a
/// local change, an announcement, a gossip delta, a pass that changed
/// something). While nothing happens the wait doubles per timer pass up to
/// [`IDLE_RECONCILE_CEILING`] ([`IdleBackoff`]).
const RECONCILE_INTERVAL: Duration = Duration::from_secs(3);
/// The longest an idle device waits between two timer passes: how long a
/// change whose announcement was lost can take to arrive.
const IDLE_RECONCILE_CEILING: Duration = Duration::from_secs(30);
/// How long after an exchange first fails passes keep the short interval,
/// so a peer that drops briefly is caught up with at once.
const FAILURE_RETRY_WINDOW: Duration = Duration::from_secs(60);
/// Peers one pass exchanges with at once.
const PASS_CONCURRENCY: usize = 8;
/// How long a pass waits for one peer's exchange once it has begun. The
/// exchange is not cancelled (that could stop between a durable record and
/// its projection); it finishes on its own and holds its slot, and its peer
/// is not started again until it has. The pass moves on without it.
const PEER_DEADLINE: Duration = Duration::from_secs(20);

/// One peer's share of a reconciliation pass.
struct PeerPass<'a> {
    hydrator: &'a Hydrator,
    identity: &'a FsTransportIdentity,
    policy: &'a PeerPolicy,
    records: Option<&'a RecordPlane>,
    /// Which records to project again even if nothing new arrives: all of
    /// them on the first pass, the ones still unplaced on timer passes.
    reconcile: crate::record_materializer::ReconcileScope,
}

impl PeerPass<'_> {
    /// The pass over the peer's session: every exchange on its one
    /// connection.
    async fn over_session(&self, sessions: &SessionPool, peer: &session::Session) -> PassOutcome {
        // Each exchange stands on its own: one the peer refuses does not
        // hold back the others.
        let failed = std::sync::atomic::AtomicBool::new(false);
        let report = |what: &str, error: &anyhow::Error| {
            failed.store(true, std::sync::atomic::Ordering::Relaxed);
            if !self.identity.partitioned() {
                eprintln!("{what} with {} failed: {error:#}", peer.addr.id);
            }
        };
        let mut changed = false;
        let authority_started = Instant::now();
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
            let pull_started = Instant::now();
            match session::pull_records(
                sessions,
                peer,
                self.identity,
                self.policy,
                records,
                self.hydrator,
                self.reconcile,
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
                    pull_started.duration_since(authority_started).as_millis(),
                    pull_done.duration_since(pull_started).as_millis(),
                    pull_done.elapsed().as_millis()
                );
            }
        }
        PassOutcome {
            changed,
            failed: failed.into_inner(),
        }
    }
}

/// What every peer exchange of the reconciliation loop works on. Owned, so
/// an exchange can outlive the pass that stopped waiting for it.
struct PassShared {
    hydrator: Arc<Hydrator>,
    identity: FsTransportIdentity,
    policy: PeerPolicy,
    records: Option<RecordPlane>,
    sessions: Arc<SessionPool>,
}

/// What one pass does with each peer.
#[derive(Clone, Copy, Debug)]
struct PassPlan {
    /// Tell the peer first that this device published changes, so it pulls
    /// them without waiting for its own timer.
    announce: bool,
    reconcile: crate::record_materializer::ReconcileScope,
}

/// What a pass, or one peer's share of it, came to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PassOutcome {
    /// Namespace or records changed.
    changed: bool,
    /// An exchange failed or a peer could not be reached.
    failed: bool,
}

/// When the reconciliation loop's next timer pass is due. After anything
/// happens it is [`RECONCILE_INTERVAL`]; each timer pass that changes
/// nothing doubles the wait, up to [`IDLE_RECONCILE_CEILING`], so an idle
/// device wakes twice a minute rather than twenty times. A pass where an
/// exchange failed keeps the short wait for [`FAILURE_RETRY_WINDOW`] after
/// the first failure in a row, then backs off as well, so an absent peer is
/// not dialled every few seconds forever. Collaborators' changes do not
/// wait on this: announcements, gossip and peers' own passes act at once.
#[derive(Debug)]
struct IdleBackoff {
    interval: Duration,
    failing_since: Option<Instant>,
}

impl Default for IdleBackoff {
    fn default() -> Self {
        Self {
            interval: RECONCILE_INTERVAL,
            failing_since: None,
        }
    }
}

impl IdleBackoff {
    /// How long after the last pass the next timer pass is due.
    const fn interval(&self) -> Duration {
        self.interval
    }

    /// Something happened outside a pass (a gossip delta, a neighbour):
    /// the next timer pass is due soon again.
    const fn activity(&mut self) {
        self.interval = RECONCILE_INTERVAL;
    }

    /// A pass ended at `now`; `timer` says whether the timer started it
    /// (rather than a local change or an announcement).
    fn after_pass(&mut self, timer: bool, outcome: PassOutcome, now: Instant) {
        if outcome.failed {
            let since = *self.failing_since.get_or_insert(now);
            if now.saturating_duration_since(since) < FAILURE_RETRY_WINDOW {
                self.interval = RECONCILE_INTERVAL;
                return;
            }
        } else {
            self.failing_since = None;
        }
        self.interval = if timer && !outcome.changed {
            self.interval.saturating_mul(2).min(IDLE_RECONCILE_CEILING)
        } else {
            RECONCILE_INTERVAL
        };
    }
}

/// When a timer of `interval` started now runs out.
fn timer_due(interval: Duration) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    now.checked_add(interval).unwrap_or(now)
}

impl PassShared {
    /// One peer's share of a pass: the announcement if `plan` asks for it,
    /// then every exchange over the peer's session.
    async fn exchange(&self, peer: EndpointAddr, plan: PassPlan) -> PassOutcome {
        let session = match self.sessions.get(&peer).await {
            Ok(session) => session,
            Err(error) => {
                if !self.identity.partitioned() {
                    eprintln!("no session with {}: {error:#}", peer.id);
                }
                return PassOutcome {
                    changed: false,
                    failed: true,
                };
            }
        };
        let context = PeerPass {
            hydrator: &self.hydrator,
            identity: &self.identity,
            policy: &self.policy,
            records: self.records.as_ref(),
            reconcile: plan.reconcile,
        };
        if plan.announce
            && let Err(error) = session::announce(&self.sessions, &session).await
        {
            eprintln!("announcing to {} failed: {error:#}", session.addr.id);
        }
        let outcome = context.over_session(&self.sessions, &session).await;
        if session.is_closed() {
            // The next pass connects afresh.
            self.sessions.forget(&session).await;
        }
        outcome
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
    /// Set, and taken, the same way by an exchange that failed.
    failed: Arc<std::sync::atomic::AtomicBool>,
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
        self.busy.lock_recover().remove(&self.peer);
    }
}

impl PeerExchanges {
    fn new(shared: PassShared, deadline: Duration) -> Self {
        Self {
            shared: Arc::new(shared),
            slots: Arc::new(tokio::sync::Semaphore::new(PASS_CONCURRENCY)),
            busy: Arc::new(Mutex::new(HashSet::new())),
            changed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            failed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            deadline,
        }
    }

    /// One pass over `peers`, skipping any still exchanging since an
    /// earlier pass. Says whether namespace or records changed, and whether
    /// an exchange failed, since the previous pass looked.
    async fn run(&self, peers: Vec<EndpointAddr>, plan: PassPlan) -> PassOutcome {
        let started: Vec<_> = peers
            .into_iter()
            .filter_map(|peer| {
                let claimed = self.busy.lock_recover().insert(peer.id);
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
        PassOutcome {
            changed: self
                .changed
                .swap(false, std::sync::atomic::Ordering::AcqRel),
            failed: self.failed.swap(false, std::sync::atomic::Ordering::AcqRel),
        }
    }

    /// Starts the exchange with `peer` once a slot is free, and waits for it
    /// at most the deadline.
    async fn start(&self, peer: EndpointAddr, busy: BusyPeer, plan: PassPlan) {
        let Ok(slot) = Arc::clone(&self.slots).acquire_owned().await else {
            return;
        };
        let shared = Arc::clone(&self.shared);
        let changed = Arc::clone(&self.changed);
        let failed = Arc::clone(&self.failed);
        let exchange = tokio::spawn(async move {
            let outcome = shared.exchange(peer, plan).await;
            if outcome.changed {
                changed.store(true, std::sync::atomic::Ordering::Release);
            }
            if outcome.failed {
                failed.store(true, std::sync::atomic::Ordering::Release);
            }
            drop(busy);
            drop(slot);
        });
        // On timeout the task carries on detached; dropping its handle does
        // not abort it.
        let _ = tokio::time::timeout(self.deadline, exchange).await;
    }
}

/// The records a peer holds that this device lacks, over its session.
async fn pull_records_from_peer(
    sessions: &SessionPool,
    remote: EndpointAddr,
    identity: &FsTransportIdentity,
    policy: &PeerPolicy,
    records: &RecordPlane,
    hydrator: &Hydrator,
) -> Result<bool> {
    let session = sessions.get(&remote).await?;
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

struct ReconcileLoop {
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    identity: FsTransportIdentity,
    policy: PeerPolicy,
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
        namespace,
        hydrator,
        identity,
        policy,
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
            hydrator: hydrator.clone(),
            identity: identity.clone(),
            policy,
            records: records.clone(),
            sessions,
        },
        PEER_DEADLINE,
    );
    let mut backoff = IdleBackoff::default();
    let mut due = timer_due(backoff.interval());
    let mut pending_thumbnails = HashMap::new();
    // `mount` bootstraps a namespace before this task starts, so schedule its
    // previews once even if the first anti-entropy exchange is clean.
    let mut refresh_thumbnails = true;
    let mut first_pass = true;
    let mut conflict_watch = conflict_watch::ConflictWatch::default();
    let trace = std::env::var_os("TILO_FS_NATIVE_TRACE").is_some();
    loop {
        // A local change or a peer's announcement starts a pass at once;
        // otherwise the timer does. Peers heard from elsewhere (gossip)
        // bring the timer back to its short interval without a pass.
        let (local_change, timer) = tokio::select! {
            () = tokio::time::sleep_until(due) => (false, true),
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
            () = sync.activity.notified() => {
                backoff.activity();
                due = due.min(timer_due(backoff.interval()));
                continue;
            }
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
        let outcome = exchanges.run(known_peers, plan).await;
        let namespace_changed = outcome.changed;
        first_pass = false;
        backoff.after_pass(timer, outcome, Instant::now());
        due = timer_due(backoff.interval());
        if trace {
            eprintln!(
                "  sync pass over {peer_count} peer(s) took {} ms (changed {namespace_changed}, \
                 failed {}); next timer pass in {} s",
                pass_started.elapsed().as_millis(),
                outcome.failed,
                backoff.interval().as_secs()
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
        // and a burst of local changes must not recheck records per change.
        // Other passes look only at what was written or merged since the
        // last look ([`conflict_watch`]), so an idle pass does nothing.
        if records.is_some() || !may_write || (local_change && !namespace_changed) {
            continue;
        }
        conflict_watch.begin_pass(Instant::now());
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
                    let conflict_published = conflict_watch.publish_record_conflicts(
                        &publisher,
                        &records.repository,
                        records.workspace_id,
                    );
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
        // Takes the namespace lock a step at a time: FUSE requests between
        // the steps never wait for the whole walk.
        let conflicts = conflict_watch.namespace_conflicts(&namespace);
        if trace {
            let pass = conflict_watch.namespace_pass();
            eprintln!(
                "  conflict scan: {} node(s) in {} lock hold(s), longest {} us",
                pass.nodes,
                pass.lock_holds,
                pass.longest_hold.as_micros()
            );
        }
        for c in conflicts {
            // Fetch just the loser's manifest for its length (chunks hydrate
            // lazily on read). Skipped if no peer can serve it yet; retried next pass.
            let Ok(len) = hydrator.content_len(c.loser).await else {
                conflict_watch.retry_namespace();
                continue;
            };
            let _ = {
                let mut ns = namespace.lock_recover();
                crate::conflict::materialize(&mut ns, &c, len, unix_now_ms())
            };
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

mod conflict_watch;
mod session;
pub use session::TILO_FS_SESSION_ALPN;

#[cfg(test)]
mod keep_alive_tests;
#[cfg(test)]
mod tests;
