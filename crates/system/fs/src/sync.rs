//! The content plane: a mount's blobs served to the peers that hydrate from
//! it, and the signed workspace ACL a joiner reads.
//!
//! A workspace's tree itself travels as the workspace's Files record
//! ([`crate::tree_record`]); only file bytes and the ACL come over this
//! plane, and every connection is authenticated as a certified device of a
//! person before it is served.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::Result;
use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::{Connection, Endpoint, RecvStream, SendStream, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh_base::EndpointAddr;
use serde::{Deserialize, Serialize};
use tilo_user_identity::RevocationList;

use crate::acl::{AclHandle, IdentityKey};
use crate::authenticated_transport::{AuthenticatedTransportConfig, authenticate_fs_peer_parts_at};
use crate::blob_access::{BlobAccessIndex, Requester};
use crate::clock::unix_now_ns;
use crate::hydration::Hydrator;
use crate::identity::PeerCredentials;
use crate::namespace::Namespace;
use crate::peer_auth::AuthenticatedFsPeer;
use crate::store::ContentStore;
use crate::workspace_authority::{
    AuthorityRequest, AuthorityResponse, TILO_FS_AUTHORITY_ALPN, WorkspaceAuthority,
    read_frame as read_authority_frame, write_frame as write_authority_frame,
};
use tilo_lock::MutexRecover as _;

/// Certified local identity and live revocation state required by every
/// workspace-sync connection.
#[derive(Clone)]
pub struct FsTransportIdentity {
    credentials: PeerCredentials,
    revocations: Arc<RwLock<RevocationList>>,
    gate: crate::transport_gate::TransportGate,
    certified_peers: Arc<RwLock<std::collections::HashMap<IdentityKey, AuthenticatedFsPeer>>>,
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
        }
    }

    #[must_use]
    pub fn with_transport_gate(mut self, gate: crate::transport_gate::TransportGate) -> Self {
        self.gate = gate;
        self
    }

    fn remember_peer(&self, peer: &AuthenticatedFsPeer) {
        if self.certification_is_current(peer, unix_now_ns())
            && let Ok(mut peers) = self.certified_peers.write()
        {
            peers.insert(peer.device_public_key, peer.clone());
        }
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

    fn revocation_snapshot(&self) -> Result<RevocationList> {
        self.revocations
            .read()
            .map(|list| list.clone())
            .map_err(|_| anyhow::anyhow!("peer revocation state is poisoned"))
    }
}

impl FsTransportIdentity {
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
    identity.remember_peer(&peer);
    Ok(peer)
}

/// A shareable invitation to join a workspace: how to reach a peer + the
/// workspace's random 256-bit name.
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
/// address lookup (so sessions can dial peers we learn about by id).
/// Returns both so callers can register peer addresses.
///
/// The `NodeId` this produces is the device's Ed25519 public key —
/// iroh's secret key and the device key are the same 32 bytes, which is
/// what makes the node id a *name* that still means the same thing after
/// a restart.
pub async fn bind_endpoint_with_key(secret: iroh::SecretKey) -> Result<(Endpoint, MemoryLookup)> {
    let lookup = MemoryLookup::new();
    let relays = tilo_endpoint_reachability::relays();
    let endpoint = endpoint_builder(secret, &lookup, &relays).bind().await?;
    Ok((endpoint, lookup))
}

/// [`bind_endpoint_with_key`], with the person's relays held back until
/// something may need to dial this device: no home relay, net report,
/// address discovery or port mapping until the returned
/// [`crate::reachability::Reachability`] is woken. For a daemon
/// that waits to be dialled (`fs serve`); one that dials out at once has
/// no reason to wait.
pub async fn bind_dormant_endpoint_with_key(
    secret: iroh::SecretKey,
) -> Result<(Endpoint, MemoryLookup, crate::reachability::Reachability)> {
    let lookup = MemoryLookup::new();
    let relays = tilo_endpoint_reachability::relays();
    let endpoint = endpoint_builder(secret, &lookup, &relays)
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::empty()))
        .bind()
        .await?;
    let reachability = crate::reachability::Reachability::defer(
        &endpoint,
        &relays.relays,
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

/// The endpoint a mount or a `fs serve` reaches its peers on: the person's
/// relays ([`tilo_endpoint_reachability::relays`]), and number 0's directory
/// only as a reader — a filesystem peer is dialled by the devices that
/// mounted it, which hold its address already, so publishing its key would
/// only sign in for a lookup nothing makes. With a relay of the person's own
/// (or none) number 0's directory is not read either.
fn endpoint_builder(
    secret: iroh::SecretKey,
    lookup: &MemoryLookup,
    relays: &tilo_endpoint_reachability::EndpointRelays,
) -> iroh::endpoint::Builder {
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
    let mut builder = Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Custom(relays.relays.clone()))
        .transport_config(transport)
        .secret_key(secret)
        .address_lookup(lookup.clone());
    if relays.n0_dns {
        builder = builder.address_lookup(tilo_endpoint_reachability::Watchonly);
    }
    builder
}

/// Bind an endpoint under a freshly-generated key. **The node id differs
/// on every call**, so nothing durable may be keyed on it — for tests
/// and throwaway probes only; a real mount uses
/// [`bind_endpoint_with_key`] so its identity is the device's.
pub async fn bind_ephemeral_endpoint() -> Result<(Endpoint, MemoryLookup)> {
    bind_endpoint_with_key(iroh::SecretKey::generate()).await
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

fn authenticated_peer_may_read_whole_workspace(
    policy: &PeerPolicy,
    peer: &AuthenticatedFsPeer,
) -> bool {
    peer_may_read_whole_workspace(policy, &peer.user_public_key)
        || peer_may_read_whole_workspace(policy, &peer.device_public_key)
}

/// An iroh endpoint id read as a Tilo identity — the same 32 bytes (see
/// [`bind_endpoint_with_key`]), letting a capability written against a
/// device key decide who may open a connection.
fn identity_of(id: iroh::EndpointId) -> IdentityKey {
    *id.as_bytes()
}

/// The connect-time verdict for a blob-plane caller, and the identity to
/// remember for requests riding the connection. `None` endpoint id means
/// the transport couldn't name the caller: refuse, since an unnamed
/// caller can't be checked.
fn admit_blob_caller(
    policy: &PeerPolicy,
    endpoint_id: Option<iroh::EndpointId>,
) -> Result<IdentityKey, iroh_blobs::provider::events::AbortReason> {
    use iroh_blobs::provider::events::AbortReason;

    let Some(id) = endpoint_id else {
        return Err(AbortReason::Permission);
    };
    let peer = identity_of(id);
    if peer_may_read_anything(policy, &peer) {
        Ok(peer)
    } else {
        Err(AbortReason::Permission)
    }
}
/// Does `peer` currently read at least one live path naming `hash` as file
/// content or thumbnail content? Hash knowledge alone is never authority.
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
async fn authorize_ask(
    policy: &PeerPolicy,
    index: &BlobAccessIndex,
    store: &ContentStore,
    caller: (IdentityKey, Option<IdentityKey>),
    ask: &BlobAsk,
) -> bool {
    if policy.is_none() {
        return true;
    }
    let (device, person) = caller;
    let requester = Requester {
        policy,
        device,
        person,
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
        let mut verdict = admit_blob_caller(&self.policy, endpoint_id);
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
                    authorize_ask(&guard.policy, &guard.index, &guard.store, caller, &ask).await
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
    identity: Option<FsTransportIdentity>,
) -> Option<iroh_blobs::provider::events::EventSender> {
    use iroh_blobs::provider::events::{
        AbortReason, ConnectMode, EventMask, EventSender, ObserveMode, ProviderMessage,
        RequestMode, ThrottleMode,
    };

    policy.as_ref()?;
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

/// A live, replicated workspace: a CRDT namespace kept in sync with peers.
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

/// A live workspace's content plane: the blobs this device holds, served to
/// the peers its mount hydrates from.
pub struct Workspace {
    pub namespace: Arc<Mutex<Namespace>>,
    _router: Router,
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
    /// peers. `policy` is what the content plane serves *through* (see
    /// [`PeerPolicy`]); `None` hosts unenforced. `authority` is the signed
    /// ACL state a joiner reads over the authority ALPN.
    pub fn host(
        node: SyncNode,
        authority: Option<&Arc<WorkspaceAuthority>>,
    ) -> Result<(Workspace, Ticket)> {
        let topic = *node.endpoint.id().as_bytes();
        let ticket = Ticket {
            node: node.endpoint.addr(),
            workspace: topic,
        };
        let workspace = Self::start(node, ticket.workspace_id(), &[], authority);
        Ok((workspace, ticket))
    }

    /// Join the workspace described by `ticket`. A joiner serves too (what
    /// it hydrates, peers may hydrate from it), so it carries the same
    /// `policy` a host does.
    pub fn join(
        node: SyncNode,
        ticket: &Ticket,
        authority: Option<&Arc<WorkspaceAuthority>>,
    ) -> Result<Workspace> {
        Ok(Self::start(
            node,
            ticket.workspace_id(),
            std::slice::from_ref(&ticket.node),
            authority,
        ))
    }

    fn start(
        node: SyncNode,
        workspace_id: uuid::Uuid,
        bootstrap: &[EndpointAddr],
        authority: Option<&Arc<WorkspaceAuthority>>,
    ) -> Workspace {
        let SyncNode {
            endpoint,
            lookup,
            store,
            hydrator,
            namespace,
            identity,
            policy,
        } = node;
        // Register bootstrap peers so the mount can hydrate from them.
        for address in bootstrap {
            lookup.add_endpoint_info(address.clone());
            hydrator.add_peer(address.clone());
        }
        let builder = Router::builder(endpoint.clone()).accept(
            iroh_blobs::ALPN,
            store.blobs_protocol(blob_guard(
                policy.clone(),
                namespace.clone(),
                store.clone(),
                Some(identity.clone()),
            )),
        );
        let builder = match authority {
            Some(authority) => builder.accept(
                TILO_FS_AUTHORITY_ALPN,
                AuthorityProtocol {
                    endpoint,
                    identity,
                    policy,
                    authority: Arc::clone(authority),
                    workspace_id,
                },
            ),
            None => builder,
        };
        Workspace {
            namespace,
            _router: builder.spawn(),
        }
    }
}
