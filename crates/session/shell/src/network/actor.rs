//! Iroh-gossip actor for workspace collaboration.
//!
//! Direct paths, NAT traversal and stateless relay fallback are selected by
//! iroh. Tilo only names authenticated endpoint ids and workspace topics.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use iroh::address_lookup::memory::MemoryLookup;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_gossip::api::{Event as GossipEvent, GossipSender};
use iroh_gossip::net::{GOSSIP_ALPN, Gossip};
use iroh_gossip::proto::TopicId;
use tilo_relay_client::{Inbound as HostInbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::mpsc;

use crate::network::protocol::{MAX_PERSONAL_CONTINUITY_CHECKPOINT_BYTES, NeighbourMsg};
use neighbour_wire::{Retention, retention};

#[cfg(test)]
static DEDICATED_SURFACE_FRAMES_RECEIVED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const PEER_TIMEOUT: Duration = Duration::from_secs(15);
const EVICTION_INTERVAL: Duration = Duration::from_secs(1);
const HOST_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(600);
const MAX_GOSSIP_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SURFACE_FRAME_BYTES: usize = 64 * 1024 * 1024;
const SURFACE_STREAM_ALPN: &[u8] = b"tilo-surface-stream/1";
const PERSONAL_FABRIC_ALPN: &[u8] = b"tilo-personal-fabric/1";
pub const PERSONAL_MEDIA_CHUNK_BYTES: usize = 256 * 1024;
const MAX_ACTIVE_PERSONAL_MEDIA_TRANSFERS: usize = 8;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SurfaceWireFrame {
    workspace: WorkspaceId,
    message: NeighbourMsg,
}

/// Versioned private-device traffic. A separate ALPN makes support
/// negotiation implicit and leaves the legacy Postcard enum untouched.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PersonalFabricPayload {
    Request,
    Snapshot(Vec<u8>),
    /// Ephemeral, exact-target command encoded independently of snapshot
    /// schema so it is never persisted or relayed through room history.
    Action(Vec<u8>),
    /// Bounded pieces of an explicitly requested, device-local media handoff.
    /// The surrounding personal-fabric route authenticates the source device;
    /// the receiver additionally verifies the final content hash before use.
    MediaTransfer(PersonalMediaTransferFrame),
    /// Shell-owned display session traffic. Frames are exact-target and
    /// ephemeral; authorization is rechecked against saved placement state.
    Display(crate::shell::device_fabric::PersonalDisplayMessage),
    /// Resumable generic Nearby object data. Offers and resume checkpoints
    /// travel as authenticated actions; this lane carries bounded bytes only.
    NearbyTransfer(PersonalNearbyTransferFrame),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PersonalMediaTransferFrame {
    Start {
        transfer_id: [u8; 16],
        file_name: String,
        total_bytes: u64,
    },
    Chunk {
        transfer_id: [u8; 16],
        offset: u64,
        bytes: Vec<u8>,
    },
    Finish {
        transfer_id: [u8; 16],
        content_hash: [u8; 32],
    },
    Abort {
        transfer_id: [u8; 16],
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PersonalNearbyTransferFrame {
    Chunk {
        transfer_id: [u8; 16],
        request_id: [u8; 16],
        offset: u64,
        bytes: Vec<u8>,
    },
    Finish {
        transfer_id: [u8; 16],
        request_id: [u8; 16],
        content_hash: [u8; 32],
    },
    Abort {
        transfer_id: [u8; 16],
        request_id: [u8; 16],
    },
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PersonalFabricWireFrame {
    workspace: WorkspaceId,
    user_public_key: [u8; 32],
    payload: PersonalFabricPayload,
}

pub type WorkspaceId = u64;
pub type PeerId = EndpointId;

/// Enrollment-bound identity expected behind a distinct relay service key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomePeerExpectation {
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
}

/// Device certificate chain used to authenticate to a Tilo workspace host.
#[derive(Clone)]
pub struct HostIdentity {
    pub device_cert: DeviceCert,
    pub device_signing_key: ed25519_dalek::SigningKey,
    /// Workspace-specific user certificates for this same device key.
    /// Missing entries deliberately fall back to `device_cert` for legacy and
    /// unbound workspaces.
    pub workspace_device_certs: HashMap<WorkspaceId, DeviceCert>,
    /// Root-signed revocations advertised beside each workspace certificate.
    pub workspace_revocations: HashMap<WorkspaceId, Vec<tilo_user_identity::RevocationEntry>>,
    /// System device-key/certificate paths. Production actors use these to
    /// refresh workspace identity material on every heartbeat; tests may use
    /// the in-memory maps alone.
    pub identity_paths: Option<tilo_user_identity::IdentityPaths>,
}

impl HostIdentity {
    pub(crate) fn material_for(
        &self,
        ws: WorkspaceId,
    ) -> (DeviceCert, Vec<tilo_user_identity::RevocationEntry>) {
        let fallback_cert = self
            .workspace_device_certs
            .get(&ws)
            .cloned()
            .unwrap_or_else(|| self.device_cert.clone());
        let fallback_revocations = self
            .workspace_revocations
            .get(&ws)
            .cloned()
            .unwrap_or_default();
        let Some(paths) = self.identity_paths.as_ref() else {
            return (fallback_cert, fallback_revocations);
        };
        let Ok(registry_store) = crate::identity_registry::IdentityRegistryStore::from_env() else {
            return (fallback_cert, fallback_revocations);
        };
        let Ok(registry) = registry_store.migrate_legacy_primary() else {
            return (fallback_cert, fallback_revocations);
        };
        let selected = registry.workspace_identities.iter().find_map(|(room, id)| {
            let room = crate::shell::state::RoomId::parse(room)?;
            (crate::shell::network::room_topic_id(room) == ws).then_some(id)
        });
        let identity_id = selected
            .cloned()
            .or_else(|| registry.default_identity.clone());
        let Some(identity_id) = identity_id else {
            return (fallback_cert, fallback_revocations);
        };
        let Some(profile) = registry.identities.get(&identity_id) else {
            return (fallback_cert, fallback_revocations);
        };
        let Ok(cert) = tilo_user_identity::persist::read_device_cert(
            &paths.device_cert_for_user(&profile.user_public_key),
        ) else {
            return (fallback_cert, fallback_revocations);
        };
        if tilo_user_identity::verify_device_cert(&cert).is_err()
            || cert.user_public_key != profile.user_public_key
            || cert.device_public_key != self.device_signing_key.verifying_key().to_bytes()
        {
            return (fallback_cert, fallback_revocations);
        }
        let revocations =
            crate::device_fleet::DeviceFleetStore::at(registry_store.identity_dir(), paths.clone())
                .load(&identity_id)
                .map_or_else(|_| fallback_revocations, |fleet| fleet.revocations);
        (cert, revocations)
    }
}

#[derive(Debug)]
pub enum NetCmd {
    JoinWorkspace {
        ws: WorkspaceId,
        on_event: mpsc::Sender<NetEvent>,
    },
    LeaveWorkspace {
        ws: WorkspaceId,
    },
    Publish {
        ws: WorkspaceId,
        msg: NeighbourMsg,
    },
    /// Publish live workspace-control traffic over the route selected by the
    /// canonical device mesh. Direct input uses the authenticated exact-target
    /// QUIC surface/control plane. Workspace-wide relay publication is refused
    /// because input must never be fanned out to other room participants.
    PublishWorkspaceControl {
        ws: WorkspaceId,
        msg: NeighbourMsg,
        route: WorkspaceControlRoute,
    },
    /// Send a remote-action frame to one exact authenticated device of the
    /// workspace over the direct surface/control plane. Never fanned out:
    /// action arguments and results are private to the two devices.
    PublishRemoteAction {
        ws: WorkspaceId,
        target: EndpointId,
        msg: NeighbourMsg,
    },
    /// Publish a shell-tree message that is known to contain the complete
    /// room history. Only the deliberate `HostReady` baseline uses this;
    /// ordinary `SyncResp` messages may be partial anti-entropy responses.
    PublishCheckpoint {
        ws: WorkspaceId,
        msg: NeighbourMsg,
    },
    /// Exchange same-person fabric state over its separately negotiated ALPN.
    PublishPersonalFabric {
        ws: WorkspaceId,
        user_public_key: [u8; 32],
        payload: PersonalFabricPayload,
    },
    /// Send an ephemeral personal-fabric action to one authenticated device.
    PublishPersonalFabricTarget {
        ws: WorkspaceId,
        user_public_key: [u8; 32],
        target: EndpointId,
        payload: PersonalFabricPayload,
    },
    /// Stream one local file to an exact certified personal device without
    /// copying host paths into a durable snapshot or buffering the file.
    PublishPersonalMedia {
        ws: WorkspaceId,
        user_public_key: [u8; 32],
        target: EndpointId,
        transfer_id: [u8; 16],
        file_name: String,
        total_bytes: u64,
        file: std::fs::File,
    },
    /// Stop only the active stream with this exact transport peer/id binding.
    CancelPersonalMedia {
        target: EndpointId,
        transfer_id: [u8; 16],
    },
    PublishNearbyTransfer {
        ws: WorkspaceId,
        user_public_key: [u8; 32],
        target: EndpointId,
        transfer_id: [u8; 16],
        request_id: [u8; 16],
        total_bytes: u64,
        resume_offset: u64,
        prefix_hash: [u8; 32],
        content_hash: [u8; 32],
        file: std::fs::File,
    },
    Connect {
        addr: EndpointAddr,
    },
    /// Connect to an always-on Tilo host. Messages continue to use the
    /// direct gossip path too; the host adds persistence and offline delivery.
    ConnectHost {
        addr: EndpointAddr,
        expectation: Option<HomePeerExpectation>,
    },
    /// Stop using a host removed from the authenticated local registry.
    DisconnectHost {
        id: EndpointId,
    },
}

/// Concrete data-plane route for one workspace-control session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceControlRoute {
    Direct { target: EndpointId },
    Relayed { via: EndpointId },
}

#[derive(Debug, Clone)]
pub enum NetEvent {
    PeerJoined {
        ws: WorkspaceId,
        peer: PeerId,
        membership_epoch: MembershipEpoch,
    },
    PeerLeft {
        ws: WorkspaceId,
        peer: PeerId,
        cause: PeerDepartureCause,
        membership_epoch: Option<MembershipEpoch>,
    },
    /// An authenticated durable host finished subscribing to this workspace.
    /// Distinct from a direct peer arrival: the shell uses this edge to seed
    /// transport-level checkpoints for future first-time/offline devices.
    HostReady { ws: WorkspaceId, peer: PeerId },
    Msg {
        ws: WorkspaceId,
        peer: PeerId,
        membership_epoch: Option<MembershipEpoch>,
        msg: NeighbourMsg,
    },
    PersonalFabric {
        ws: WorkspaceId,
        peer: PeerId,
        membership_epoch: MembershipEpoch,
        user_public_key: [u8; 32],
        payload: PersonalFabricPayload,
    },
    PersonalMediaTransfer {
        target: EndpointId,
        transfer_id: [u8; 16],
        sent_bytes: u64,
        total_bytes: u64,
        status: PersonalMediaTransferStatus,
    },
    NearbyTransferProgress {
        target: EndpointId,
        transfer_id: [u8; 16],
        sent_bytes: u64,
        total_bytes: u64,
        status: PersonalMediaTransferStatus,
    },
}

/// Actor-minted identity of one endpoint's membership incarnation on one
/// workspace topic. This value never crosses the wire: it is authority only
/// for local consumers of [`NetEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MembershipEpoch(u64);

impl MembershipEpoch {
    #[cfg(test)]
    pub(crate) const fn for_test(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonalMediaTransferStatus {
    Started,
    Progress,
    Completed,
    Failed,
    Cancelled,
}

/// Why the transport stopped treating a peer as live on a workspace topic.
///
/// This is transport evidence, not a statement about the person's intent:
/// a timeout can be an unreliable network just as easily as a closed app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerDepartureCause {
    /// Iroh gossip reported that the neighbour went away.
    GossipNeighborDown,
    /// A durable host's relay session closed.
    HostSessionClosed,
    /// The peer stopped refreshing its liveness heartbeat.
    LivenessTimedOut,
    /// The local topic subscription ended or was explicitly left.
    WorkspaceRouteClosed,
}

enum InternalEvent {
    Topic {
        ws: WorkspaceId,
        event: GossipEvent,
    },
    Host {
        ws: WorkspaceId,
        peer: EndpointId,
        event: HostEvent,
    },
    HostConnected {
        ws: WorkspaceId,
        peer: EndpointId,
        addr: EndpointAddr,
        // Boxed: `RelayCoeditSession` (an authenticated relay client plus
        // its subscription state) is far larger than every sibling variant,
        // so an unboxed `Result` here would size the whole enum to fit it.
        result: Box<Result<RelayCoeditSession, String>>,
    },
    SurfaceFrame {
        ws: WorkspaceId,
        peer: EndpointId,
        fingerprint: [u8; 32],
        msg: NeighbourMsg,
    },
    SurfaceFanoutComplete {
        ws: WorkspaceId,
        msg: NeighbourMsg,
        outcomes: Vec<SurfaceFanoutOutcome>,
        all_delivered: bool,
    },
    PersonalFabric {
        ws: WorkspaceId,
        peer: EndpointId,
        connection_incarnation: u64,
        user_public_key: [u8; 32],
        payload: PersonalFabricPayload,
    },
    PersonalFabricClosed {
        peer: EndpointId,
        connection_incarnation: u64,
    },
    PersonalMediaProgress {
        target: EndpointId,
        transfer_id: [u8; 16],
        generation: u64,
        sent_bytes: u64,
    },
    PersonalMediaFinished {
        target: EndpointId,
        transfer_id: [u8; 16],
        generation: u64,
        outcome: PersonalMediaStreamOutcome,
        sent_bytes: u64,
    },
    Closed {
        ws: WorkspaceId,
    },
}

struct SurfaceFanoutOutcome {
    peer: EndpointId,
    connection: Option<iroh::endpoint::Connection>,
    delivered: bool,
}

enum HostEvent {
    Ready,
    Message {
        author: EndpointId,
        fingerprint: [u8; 32],
        msg: NeighbourMsg,
    },
    Closed,
}

struct TopicRuntime {
    sender: GossipSender,
    pump: tokio::task::JoinHandle<()>,
}

struct HostRuntime {
    session: Arc<RelayCoeditSession>,
    pump: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
struct SurfaceStreamProtocol {
    internal_tx: mpsc::Sender<InternalEvent>,
    input_tx: mpsc::Sender<InternalEvent>,
}

impl std::fmt::Debug for SurfaceStreamProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SurfaceStreamProtocol")
    }
}

impl ProtocolHandler for SurfaceStreamProtocol {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let peer = connection.remote_id();
        let tx = self.internal_tx.clone();
        let input_tx = self.input_tx.clone();
        tokio::spawn(async move {
            loop {
                let Ok(mut recv) = connection.accept_uni().await else {
                    return;
                };
                let bytes = match recv.read_to_end(MAX_SURFACE_FRAME_BYTES).await {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        tracing::debug!(peer = %peer, %error, "surface frame stream read failed");
                        continue;
                    }
                };
                let Ok(frame) = postcard::from_bytes::<SurfaceWireFrame>(&bytes) else {
                    tracing::warn!(peer = %peer, "invalid dedicated surface frame");
                    continue;
                };
                if !matches!(
                    frame.message,
                    NeighbourMsg::SurfaceFrame { .. }
                        | NeighbourMsg::RemoteInput { .. }
                        | NeighbourMsg::RemoteAction { .. }
                ) {
                    tracing::warn!(peer = %peer, "invalid message on direct surface/control ALPN");
                    continue;
                }
                #[cfg(test)]
                if matches!(&frame.message, NeighbourMsg::SurfaceFrame { .. }) {
                    DEDICATED_SURFACE_FRAMES_RECEIVED
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                let Ok(message_bytes) = postcard::to_allocvec(&frame.message) else {
                    continue;
                };
                let fingerprint = message_fingerprint(&message_bytes);
                let event = InternalEvent::SurfaceFrame {
                    ws: frame.workspace,
                    peer,
                    fingerprint,
                    msg: frame.message,
                };
                if !enqueue_surface_event(&tx, &input_tx, event).await {
                    return;
                }
            }
        });
        Ok(())
    }
}

/// Keep latency-sensitive input independent from the lossy media queue. A
/// receiver that is briefly behind may discard an encoded frame, but must
/// still accept the following key or pointer event promptly.
async fn enqueue_surface_event(
    media_tx: &mpsc::Sender<InternalEvent>,
    input_tx: &mpsc::Sender<InternalEvent>,
    event: InternalEvent,
) -> bool {
    // Remote-action frames are requests and answers, not media: they take
    // the reliable queue with input.
    let is_input = matches!(
        &event,
        InternalEvent::SurfaceFrame {
            msg: NeighbourMsg::RemoteInput { .. } | NeighbourMsg::RemoteAction { .. },
            ..
        }
    );
    if is_input {
        input_tx.send(event).await.is_ok()
    } else {
        match media_tx.try_send(event) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => true,
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

#[derive(Clone)]
struct PersonalFabricProtocol {
    internal_tx: mpsc::Sender<InternalEvent>,
    next_connection_incarnation: Arc<AtomicU64>,
}

impl std::fmt::Debug for PersonalFabricProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PersonalFabricProtocol")
    }
}

impl ProtocolHandler for PersonalFabricProtocol {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let peer = connection.remote_id();
        let tx = self.internal_tx.clone();
        let connection_incarnation = self
            .next_connection_incarnation
            .fetch_add(1, Ordering::Relaxed)
            .max(1);
        tokio::spawn(async move {
            loop {
                let Ok(mut recv) = connection.accept_uni().await else {
                    let _ = tx
                        .send(InternalEvent::PersonalFabricClosed {
                            peer,
                            connection_incarnation,
                        })
                        .await;
                    return;
                };
                let bytes = match recv
                    .read_to_end(crate::shell::device_fabric::MAX_FABRIC_CHECKPOINT_BYTES + 128)
                    .await
                {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        tracing::debug!(peer = %peer, %error, "personal fabric stream read failed");
                        continue;
                    }
                };
                let Ok((frame, remaining)) =
                    postcard::take_from_bytes::<PersonalFabricWireFrame>(&bytes)
                else {
                    tracing::warn!(peer = %peer, "invalid personal fabric frame");
                    continue;
                };
                if !remaining.is_empty() {
                    tracing::warn!(peer = %peer, "personal fabric frame has trailing data");
                    continue;
                }
                if tx
                    .send(InternalEvent::PersonalFabric {
                        ws: frame.workspace,
                        peer,
                        connection_incarnation,
                        user_public_key: frame.user_public_key,
                        payload: frame.payload,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        Ok(())
    }
}

pub struct Actor;

impl Actor {
    pub fn spawn(
        endpoint: Endpoint,
        lookup: MemoryLookup,
    ) -> (mpsc::Sender<NetCmd>, tokio::task::JoinHandle<()>) {
        Self::spawn_reporting_addrs(endpoint, lookup, None, Arc::new(Mutex::new(Vec::new())))
    }

    pub fn spawn_reporting_addrs(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        host_identity: Option<HostIdentity>,
        listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
    ) -> (mpsc::Sender<NetCmd>, tokio::task::JoinHandle<()>) {
        Self::spawn_with_heartbeat(
            endpoint,
            lookup,
            host_identity,
            listen_addrs,
            HEARTBEAT_INTERVAL,
        )
    }

    /// [`Self::spawn_reporting_addrs`] with an explicit liveness and
    /// host-retry cadence, so a test can observe several heartbeats without
    /// waiting out the production interval.
    fn spawn_with_heartbeat(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        host_identity: Option<HostIdentity>,
        listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
        heartbeat: Duration,
    ) -> (mpsc::Sender<NetCmd>, tokio::task::JoinHandle<()>) {
        if let Ok(mut out) = listen_addrs.lock() {
            out.push(endpoint.addr());
        }
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let task = tokio::spawn(run(
            endpoint,
            lookup,
            host_identity,
            listen_addrs,
            cmd_rx,
            heartbeat,
        ));
        (cmd_tx, task)
    }
}

/// Mutable state shared across every `select!` branch of [`run`]'s event
/// loop, bundled into a struct so each branch's handling can be split into
/// its own method instead of one long function body.
struct ActorState {
    endpoint: Endpoint,
    lookup: MemoryLookup,
    host_identity: Option<HostIdentity>,
    listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
    gossip: Gossip,
    internal_tx: mpsc::Sender<InternalEvent>,
    topics: HashMap<WorkspaceId, TopicRuntime>,
    subscribers: HashMap<WorkspaceId, mpsc::Sender<NetEvent>>,
    peers: HashMap<EndpointId, EndpointAddr>,
    hosts: HashMap<EndpointId, (EndpointAddr, Option<HomePeerExpectation>)>,
    host_sessions: HashMap<(WorkspaceId, EndpointId), HostRuntime>,
    connecting_hosts: HashSet<(WorkspaceId, EndpointId)>,
    content_discovery_tasks: HashMap<(WorkspaceId, EndpointId), tokio::task::JoinHandle<()>>,
    peers_seen: HashMap<(WorkspaceId, EndpointId), PeerMembership>,
    next_membership_epoch: u64,
    /// Inbound ALPN connections are bound once to the membership active on
    /// their first frame. `None` is a tombstone retained until that concrete
    /// connection closes, so an old connection cannot rebind after reconnect.
    personal_fabric_ingress: HashMap<(EndpointId, u64), Option<(WorkspaceId, MembershipEpoch)>>,
    recent: HashSet<(WorkspaceId, [u8; 32])>,
    recent_order: VecDeque<(WorkspaceId, [u8; 32])>,
    /// Long-lived direct media connections. One unidirectional QUIC stream
    /// per frame avoids head-of-line blocking between viewers and frames.
    surface_connections: HashMap<EndpointId, iroh::endpoint::Connection>,
    /// At most one direct surface fanout per workspace may be dialing or
    /// writing. A producer can submit frames faster than an unreachable peer's
    /// dial timeout; retain only the newest pending frame instead of stalling
    /// the actor or building an unbounded queue of obsolete video.
    surface_fanout_tasks: HashMap<WorkspaceId, tokio::task::JoinHandle<()>>,
    pending_surface_frames: HashMap<WorkspaceId, NeighbourMsg>,
    personal_fabric_connections: HashMap<EndpointId, iroh::endpoint::Connection>,
    active_personal_media: HashMap<(EndpointId, [u8; 16]), ActivePersonalMediaTransfer>,
    next_personal_media_generation: u64,
}

struct ActivePersonalMediaTransfer {
    generation: u64,
    workspace: WorkspaceId,
    total_bytes: u64,
    sent_bytes: std::sync::Arc<std::sync::atomic::AtomicU64>,
    cancel: tokio::sync::watch::Sender<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersonalMediaStreamOutcome {
    Completed,
    Failed,
    Cancelled,
}

fn take_personal_media_generation(
    active: &mut HashMap<(EndpointId, [u8; 16]), ActivePersonalMediaTransfer>,
    key: (EndpointId, [u8; 16]),
    generation: u64,
) -> Option<ActivePersonalMediaTransfer> {
    active
        .get(&key)
        .is_some_and(|entry| entry.generation == generation)
        .then(|| active.remove(&key))
        .flatten()
}

#[derive(Clone, Copy)]
struct PeerMembership {
    last_seen: Instant,
    epoch: MembershipEpoch,
}

fn bind_personal_fabric_ingress(
    ingress: &mut HashMap<(EndpointId, u64), Option<(WorkspaceId, MembershipEpoch)>>,
    peer: EndpointId,
    connection_incarnation: u64,
    ws: WorkspaceId,
    epoch: MembershipEpoch,
) -> bool {
    let key = (peer, connection_incarnation);
    match ingress.get(&key).copied() {
        Some(Some(bound)) => bound == (ws, epoch),
        Some(None) => false,
        None => {
            ingress.insert(key, Some((ws, epoch)));
            true
        }
    }
}

fn revoke_personal_fabric_ingress(
    ingress: &mut HashMap<(EndpointId, u64), Option<(WorkspaceId, MembershipEpoch)>>,
    ws: WorkspaceId,
    epoch: MembershipEpoch,
) {
    for binding in ingress.values_mut() {
        if binding.is_some_and(|bound| bound == (ws, epoch)) {
            *binding = None;
        }
    }
}

impl ActorState {
    fn touch_or_mint_membership(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
    ) -> (MembershipEpoch, bool) {
        if let Some(membership) = self.peers_seen.get_mut(&(ws, peer)) {
            membership.last_seen = Instant::now();
            return (membership.epoch, false);
        }
        self.next_membership_epoch = self.next_membership_epoch.saturating_add(1).max(1);
        let epoch = MembershipEpoch(self.next_membership_epoch);
        self.peers_seen.insert(
            (ws, peer),
            PeerMembership {
                last_seen: Instant::now(),
                epoch,
            },
        );
        (epoch, true)
    }

    fn touch_existing_membership(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
    ) -> Option<MembershipEpoch> {
        let membership = self.peers_seen.get_mut(&(ws, peer))?;
        membership.last_seen = Instant::now();
        Some(membership.epoch)
    }

    fn remove_membership_if_epoch(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
        epoch: MembershipEpoch,
    ) -> bool {
        if !self
            .peers_seen
            .get(&(ws, peer))
            .is_some_and(|membership| membership.epoch == epoch)
        {
            return false;
        }
        self.peers_seen.remove(&(ws, peer));
        self.cancel_personal_media_route(ws, peer);
        revoke_personal_fabric_ingress(&mut self.personal_fabric_ingress, ws, epoch);
        if let Some(connection) = self.personal_fabric_connections.remove(&peer) {
            connection.close(0_u32.into(), b"membership departed");
        }
        true
    }

    /// Handle one [`NetCmd`]. Returns `false` once the command channel has
    /// closed, so [`run`]'s loop should stop.
    #[allow(
        clippy::too_many_lines,
        reason = "the actor command match is the single auditable transport mutation boundary"
    )]
    async fn handle_command(&mut self, command: Option<NetCmd>) -> bool {
        match command {
            Some(NetCmd::JoinWorkspace { ws, on_event }) => {
                self.handle_join_workspace(ws, on_event).await;
            }
            Some(NetCmd::LeaveWorkspace { ws }) => {
                let subscriber = self.subscribers.get(&ws).cloned();
                let leaving = self
                    .peers_seen
                    .iter()
                    .filter_map(|(&(peer_ws, peer), membership)| {
                        (peer_ws == ws).then_some((peer, membership.epoch))
                    })
                    .collect::<Vec<_>>();
                for (peer, epoch) in leaving {
                    if self.remove_membership_if_epoch(ws, peer, epoch)
                        && let Some(tx) = subscriber.as_ref()
                    {
                        let _ = tx
                            .send(NetEvent::PeerLeft {
                                ws,
                                peer,
                                cause: PeerDepartureCause::WorkspaceRouteClosed,
                                membership_epoch: Some(epoch),
                            })
                            .await;
                    }
                }
                self.subscribers.remove(&ws);
                self.peers_seen.retain(|(peer_ws, _), _| *peer_ws != ws);
                self.pending_surface_frames.remove(&ws);
                if let Some(task) = self.surface_fanout_tasks.remove(&ws) {
                    task.abort();
                }
                if let Some(topic) = self.topics.remove(&ws) {
                    topic.pump.abort();
                }
                let leaving: Vec<_> = self
                    .host_sessions
                    .keys()
                    .filter(|(session_ws, _)| *session_ws == ws)
                    .copied()
                    .collect();
                for key in leaving {
                    if let Some(runtime) = self.host_sessions.remove(&key) {
                        runtime.pump.abort();
                    }
                }
                let discoveries: Vec<_> = self
                    .content_discovery_tasks
                    .keys()
                    .filter(|(task_ws, _)| *task_ws == ws)
                    .copied()
                    .collect();
                for key in discoveries {
                    if let Some(task) = self.content_discovery_tasks.remove(&key) {
                        task.abort();
                    }
                }
            }
            Some(NetCmd::Publish { ws, msg }) => {
                if matches!(msg, NeighbourMsg::SurfaceFrame { .. }) {
                    // The host plane is already connected and independently
                    // useful. Enqueue it before any direct-peer dial timeout;
                    // only the compatibility gossip fallback depends on the
                    // outcome of dedicated media fanout.
                    publish(
                        &self.topics,
                        &self.host_sessions,
                        ws,
                        msg.clone(),
                        false,
                        false,
                        true,
                    )
                    .await;
                    if !self.start_surface_fanout(ws, msg.clone()) {
                        publish(
                            &self.topics,
                            &self.host_sessions,
                            ws,
                            msg,
                            false,
                            true,
                            false,
                        )
                        .await;
                    }
                } else {
                    publish(
                        &self.topics,
                        &self.host_sessions,
                        ws,
                        msg,
                        false,
                        true,
                        true,
                    )
                    .await;
                }
            }
            Some(NetCmd::PublishWorkspaceControl { ws, msg, route }) => {
                self.publish_workspace_control(ws, msg, route).await;
            }
            Some(NetCmd::PublishRemoteAction { ws, target, msg }) => {
                self.publish_remote_action(ws, target, msg).await;
            }
            Some(NetCmd::PublishCheckpoint { ws, msg }) => {
                publish(&self.topics, &self.host_sessions, ws, msg, true, true, true).await;
            }
            Some(NetCmd::PublishPersonalFabric {
                ws,
                user_public_key,
                payload,
            }) => {
                self.publish_personal_fabric(ws, user_public_key, payload)
                    .await;
            }
            Some(NetCmd::PublishPersonalFabricTarget {
                ws,
                user_public_key,
                target,
                payload,
            }) => {
                self.publish_personal_fabric_to(ws, user_public_key, payload, Some(target))
                    .await;
            }
            Some(NetCmd::PublishPersonalMedia {
                ws,
                user_public_key,
                target,
                transfer_id,
                file_name,
                total_bytes,
                file,
            }) => {
                self.publish_personal_media(
                    ws,
                    user_public_key,
                    target,
                    transfer_id,
                    file_name,
                    total_bytes,
                    file,
                );
            }
            Some(NetCmd::CancelPersonalMedia {
                target,
                transfer_id,
            }) => {
                self.cancel_personal_media((target, transfer_id));
            }
            Some(NetCmd::PublishNearbyTransfer {
                ws,
                user_public_key,
                target,
                transfer_id,
                request_id,
                total_bytes,
                resume_offset,
                prefix_hash,
                content_hash,
                file,
            }) => {
                self.publish_nearby_transfer(
                    ws,
                    user_public_key,
                    target,
                    transfer_id,
                    request_id,
                    total_bytes,
                    resume_offset,
                    prefix_hash,
                    content_hash,
                    file,
                );
            }
            Some(NetCmd::Connect { addr }) => {
                let id = addr.id;
                self.lookup.add_endpoint_info(addr.clone());
                self.peers.insert(id, addr);
                for topic in self.topics.values() {
                    if let Err(error) = topic.sender.join_peers(vec![id]).await {
                        tracing::warn!(peer = %id, %error, "iroh gossip peer join failed");
                    }
                }
            }
            Some(NetCmd::ConnectHost { addr, expectation }) => {
                let id = addr.id;
                let changed = self
                    .hosts
                    .get(&id)
                    .is_some_and(|known| known != &(addr.clone(), expectation.clone()));
                if changed {
                    // A new address for the same certified service replaces
                    // only its relay session. Its already authenticated Files
                    // endpoint remains usable until refreshed discovery proves
                    // a replacement; treating a reconnect as removal creates
                    // a needless offline window (and can strand hydration if
                    // the new relay hint is temporarily unreachable).
                    self.teardown_host(id, false);
                }
                self.lookup.add_endpoint_info(addr.clone());
                self.hosts.insert(id, (addr.clone(), expectation.clone()));
                let Some(identity) = self.host_identity.as_ref() else {
                    tracing::warn!(peer = %id, "cannot connect workspace host without a device certificate");
                    return true;
                };
                for ws in self.topics.keys().copied() {
                    schedule_host_topic(
                        &self.endpoint,
                        identity,
                        ws,
                        addr.clone(),
                        expectation.clone(),
                        &self.internal_tx,
                        &self.host_sessions,
                        &mut self.connecting_hosts,
                        &mut self.content_discovery_tasks,
                    );
                }
            }
            Some(NetCmd::DisconnectHost { id }) => self.teardown_host(id, true),
            None => return false,
        }
        true
    }

    /// Stop relay sessions and discovery tasks for one host.  `remove_content`
    /// is reserved for an explicit enrolled-home-peer removal: an address
    /// refresh must never revoke a still-valid Files hydration source.
    fn teardown_host(&mut self, id: EndpointId, remove_content: bool) {
        let enrolled_home_peer = self
            .hosts
            .remove(&id)
            .is_some_and(|(_, expectation)| expectation.is_some());
        self.connecting_hosts.retain(|(_, host)| *host != id);
        let sessions: Vec<_> = self
            .host_sessions
            .keys()
            .filter(|(_, host)| *host == id)
            .copied()
            .collect();
        for key in sessions {
            if let Some(runtime) = self.host_sessions.remove(&key) {
                runtime.pump.abort();
            }
        }
        let discoveries: Vec<_> = self
            .content_discovery_tasks
            .keys()
            .filter(|(_, host)| *host == id)
            .copied()
            .collect();
        for key in discoveries {
            if let Some(task) = self.content_discovery_tasks.remove(&key) {
                task.abort();
            }
        }
        if remove_content && enrolled_home_peer {
            // Discovery registrations are tied to the relay service identity,
            // not merely the direct Files endpoint.  Tear down that durable
            // route too: otherwise a removed home peer can return after an
            // fs-daemon restart from its persisted hydration catalog.
            tokio::spawn(async move {
                let _ = unregister_content_peers_for_relay(id).await;
            });
        }
    }

    /// Send an encoded surface frame over the media ALPN to every directly
    /// connected participant in this workspace. Returns true only when every
    /// currently-known direct participant was reached; otherwise the caller
    /// retains gossip as a compatibility fallback. The workspace host is
    /// still published to separately.
    fn start_surface_fanout(&mut self, ws: WorkspaceId, msg: NeighbourMsg) -> bool {
        if self.surface_fanout_tasks.contains_key(&ws) {
            self.pending_surface_frames.insert(ws, msg);
            return true;
        }
        let wire = SurfaceWireFrame {
            workspace: ws,
            message: msg.clone(),
        };
        let Ok(bytes) = postcard::to_allocvec(&wire) else {
            return false;
        };
        if bytes.len() > MAX_SURFACE_FRAME_BYTES {
            tracing::warn!(ws, bytes = bytes.len(), "surface frame exceeds wire limit");
            return false;
        }
        let workspace_peer_count = self
            .peers_seen
            .keys()
            .filter(|(peer_ws, _)| *peer_ws == ws)
            .count();
        let targets: Vec<_> = self
            .peers
            .values()
            .filter(|addr| self.peers_seen.contains_key(&(ws, addr.id)))
            .cloned()
            .collect();
        // Each viewer is an independent QUIC route. Dial/write them
        // concurrently so one unreachable peer cannot add 300ms in front of
        // every healthy viewer (or block later collaboration commands once
        // per peer). The wall-clock bound stays one dial timeout plus one
        // write timeout regardless of room size.
        let cached = self.surface_connections.clone();
        let endpoint = self.endpoint.clone();
        let internal_tx = self.internal_tx.clone();
        let task = tokio::spawn(async move {
            let mut pending = FuturesUnordered::new();
            for addr in targets {
                let peer = addr.id;
                let cached = cached.get(&peer).cloned();
                let endpoint = endpoint.clone();
                let bytes = bytes.clone();
                pending.push(async move {
                    let connection = if let Some(connection) = cached {
                        Some(connection)
                    } else {
                        tokio::time::timeout(
                            Duration::from_millis(300),
                            endpoint.connect(addr, SURFACE_STREAM_ALPN),
                        )
                        .await
                        .ok()
                        .and_then(Result::ok)
                    };
                    let Some(connection) = connection else {
                        return SurfaceFanoutOutcome {
                            peer,
                            connection: None,
                            delivered: false,
                        };
                    };
                    let send_frame = async {
                        let mut stream = connection.open_uni().await.map_err(|_| ())?;
                        stream.write_all(&bytes).await.map_err(|_| ())?;
                        stream.finish().map_err(|_| ())?;
                        Ok::<(), ()>(())
                    };
                    let delivered = matches!(
                        tokio::time::timeout(Duration::from_millis(40), send_frame).await,
                        Ok(Ok(()))
                    );
                    SurfaceFanoutOutcome {
                        peer,
                        connection: Some(connection),
                        delivered,
                    }
                });
            }
            let mut outcomes = Vec::new();
            let mut sent = 0usize;
            while let Some(outcome) = pending.next().await {
                if outcome.delivered {
                    sent = sent.saturating_add(1);
                }
                outcomes.push(outcome);
            }
            let _ = internal_tx
                .send(InternalEvent::SurfaceFanoutComplete {
                    ws,
                    msg,
                    outcomes,
                    all_delivered: workspace_peer_count > 0 && sent == workspace_peer_count,
                })
                .await;
        });
        self.surface_fanout_tasks.insert(ws, task);
        true
    }

    async fn publish_personal_fabric(
        &mut self,
        ws: WorkspaceId,
        user_public_key: [u8; 32],
        payload: PersonalFabricPayload,
    ) {
        self.publish_personal_fabric_to(ws, user_public_key, payload, None)
            .await;
    }

    async fn publish_personal_fabric_to(
        &mut self,
        ws: WorkspaceId,
        user_public_key: [u8; 32],
        payload: PersonalFabricPayload,
        exact_target: Option<EndpointId>,
    ) {
        if matches!(payload, PersonalFabricPayload::Action(ref bytes) if bytes.len() > crate::shell::device_fabric::MAX_FABRIC_ACTION_BYTES)
        {
            tracing::warn!(ws, "personal fabric action exceeds wire limit");
            return;
        }
        let wire = PersonalFabricWireFrame {
            workspace: ws,
            user_public_key,
            payload,
        };
        let Ok(bytes) = postcard::to_allocvec(&wire) else {
            return;
        };
        if bytes.len() > crate::shell::device_fabric::MAX_FABRIC_CHECKPOINT_BYTES + 128 {
            tracing::warn!(
                ws,
                bytes = bytes.len(),
                "personal fabric frame exceeds wire limit"
            );
            return;
        }
        let targets: Vec<_> = self
            .peers
            .values()
            .filter(|addr| self.peers_seen.contains_key(&(ws, addr.id)))
            .filter(|addr| exact_target.is_none_or(|target| addr.id == target))
            .cloned()
            .collect();
        for addr in targets {
            let peer = addr.id;
            let connection = if let Some(connection) = self.personal_fabric_connections.get(&peer) {
                connection.clone()
            } else {
                let Ok(Ok(connection)) = tokio::time::timeout(
                    Duration::from_millis(300),
                    self.endpoint.connect(addr, PERSONAL_FABRIC_ALPN),
                )
                .await
                else {
                    // An older Tilo peer simply does not negotiate this ALPN.
                    continue;
                };
                self.personal_fabric_connections
                    .insert(peer, connection.clone());
                connection
            };
            let send = async {
                let mut stream = connection.open_uni().await.map_err(|_| ())?;
                stream.write_all(&bytes).await.map_err(|_| ())?;
                stream.finish().map_err(|_| ())?;
                Ok::<(), ()>(())
            };
            if !matches!(
                tokio::time::timeout(Duration::from_millis(100), send).await,
                Ok(Ok(()))
            ) {
                self.personal_fabric_connections.remove(&peer);
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the arguments are the complete authenticated transfer manifest plus its local-only file handle"
    )]
    fn publish_personal_media(
        &mut self,
        ws: WorkspaceId,
        user_public_key: [u8; 32],
        target: EndpointId,
        transfer_id: [u8; 16],
        file_name: String,
        total_bytes: u64,
        file: std::fs::File,
    ) {
        let progress = self.subscribers.get(&ws).cloned();
        let report = |status, sent_bytes| {
            if let Some(progress) = progress.as_ref() {
                let _ = progress.try_send(NetEvent::PersonalMediaTransfer {
                    target,
                    transfer_id,
                    sent_bytes,
                    total_bytes,
                    status,
                });
            }
        };
        if transfer_id == [0; 16] {
            tracing::warn!(peer = %target, "refusing media transfer with an empty id");
            report(PersonalMediaTransferStatus::Failed, 0);
            return;
        }
        if !self.peers_seen.contains_key(&(ws, target)) {
            tracing::warn!(ws, peer = %target, "personal media target is no longer live");
            report(PersonalMediaTransferStatus::Failed, 0);
            return;
        }
        let Some(addr) = self.peers.get(&target).cloned() else {
            tracing::warn!(peer = %target, "personal media target has no dial address");
            report(PersonalMediaTransferStatus::Failed, 0);
            return;
        };
        let key = (target, transfer_id);
        if self.active_personal_media.contains_key(&key) {
            tracing::warn!(peer = %target, "refusing duplicate active personal media transfer");
            return;
        }
        if self.active_personal_media.len() >= MAX_ACTIVE_PERSONAL_MEDIA_TRANSFERS {
            tracing::warn!(peer = %target, "refusing media transfer while source streams are full");
            report(PersonalMediaTransferStatus::Failed, 0);
            return;
        }
        self.next_personal_media_generation = self.next_personal_media_generation.wrapping_add(1);
        let generation = self.next_personal_media_generation;
        let sent_bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (cancel, cancel_rx) = tokio::sync::watch::channel(false);
        self.active_personal_media.insert(
            key,
            ActivePersonalMediaTransfer {
                generation,
                workspace: ws,
                total_bytes,
                sent_bytes: std::sync::Arc::clone(&sent_bytes),
                cancel,
            },
        );
        let endpoint = self.endpoint.clone();
        let internal = self.internal_tx.clone();
        report(PersonalMediaTransferStatus::Started, 0);
        tokio::spawn(async move {
            let outcome = stream_personal_media(
                endpoint,
                addr,
                ws,
                user_public_key,
                transfer_id,
                file_name,
                total_bytes,
                file,
                internal.clone(),
                target,
                std::sync::Arc::clone(&sent_bytes),
                generation,
                cancel_rx,
            )
            .await;
            let _ = internal
                .send(InternalEvent::PersonalMediaFinished {
                    target,
                    transfer_id,
                    generation,
                    outcome,
                    sent_bytes: sent_bytes.load(std::sync::atomic::Ordering::Relaxed),
                })
                .await;
        });
    }

    fn report_personal_media(
        &self,
        workspace: WorkspaceId,
        target: EndpointId,
        transfer_id: [u8; 16],
        sent_bytes: u64,
        total_bytes: u64,
        status: PersonalMediaTransferStatus,
    ) {
        if let Some(progress) = self.subscribers.get(&workspace) {
            let _ = progress.try_send(NetEvent::PersonalMediaTransfer {
                target,
                transfer_id,
                sent_bytes: sent_bytes.min(total_bytes),
                total_bytes,
                status,
            });
        }
    }

    fn cancel_personal_media(&mut self, key: (EndpointId, [u8; 16])) -> bool {
        let Some(active) = self.active_personal_media.remove(&key) else {
            return false;
        };
        let _ = active.cancel.send(true);
        self.report_personal_media(
            active.workspace,
            key.0,
            key.1,
            active.sent_bytes.load(std::sync::atomic::Ordering::Relaxed),
            active.total_bytes,
            PersonalMediaTransferStatus::Cancelled,
        );
        true
    }

    fn cancel_personal_media_route(&mut self, workspace: WorkspaceId, target: EndpointId) {
        let keys = self
            .active_personal_media
            .iter()
            .filter_map(|(key, active)| {
                (key.0 == target && active.workspace == workspace).then_some(*key)
            })
            .collect::<Vec<_>>();
        for key in keys {
            self.cancel_personal_media(key);
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one resumable transfer is described by its route, identity, resume point and integrity digests together"
    )]
    fn publish_nearby_transfer(
        &self,
        ws: WorkspaceId,
        user_public_key: [u8; 32],
        target: EndpointId,
        transfer_id: [u8; 16],
        request_id: [u8; 16],
        total_bytes: u64,
        resume_offset: u64,
        prefix_hash: [u8; 32],
        content_hash: [u8; 32],
        file: std::fs::File,
    ) {
        let progress = self.subscribers.get(&ws).cloned();
        let report = |status, sent_bytes| {
            if let Some(progress) = progress.as_ref() {
                let _ = progress.try_send(NetEvent::NearbyTransferProgress {
                    target,
                    transfer_id,
                    sent_bytes,
                    total_bytes,
                    status,
                });
            }
        };
        if transfer_id == [0; 16]
            || request_id == [0; 16]
            || resume_offset > total_bytes
            || !self.peers_seen.contains_key(&(ws, target))
        {
            report(PersonalMediaTransferStatus::Failed, resume_offset);
            return;
        }
        let Some(addr) = self.peers.get(&target).cloned() else {
            report(PersonalMediaTransferStatus::Failed, resume_offset);
            return;
        };
        let endpoint = self.endpoint.clone();
        report(PersonalMediaTransferStatus::Started, resume_offset);
        tokio::spawn(async move {
            let sent = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(resume_offset));
            let result = stream_nearby_transfer(
                endpoint,
                addr,
                ws,
                user_public_key,
                target,
                transfer_id,
                request_id,
                total_bytes,
                resume_offset,
                prefix_hash,
                content_hash,
                file,
                progress.clone(),
                std::sync::Arc::clone(&sent),
            )
            .await;
            if let Some(progress) = progress {
                let _ = progress
                    .send(NetEvent::NearbyTransferProgress {
                        target,
                        transfer_id,
                        sent_bytes: sent.load(std::sync::atomic::Ordering::Relaxed),
                        total_bytes,
                        status: if result.is_ok() {
                            PersonalMediaTransferStatus::Completed
                        } else {
                            PersonalMediaTransferStatus::Failed
                        },
                    })
                    .await;
            }
        });
    }

    /// Send consented input to one exact authenticated device. Gossip has no
    /// addressed-send primitive, so using its broadcast here would disclose
    /// keyboard and pointer events to every participant in the workspace.
    async fn publish_workspace_control(
        &mut self,
        ws: WorkspaceId,
        msg: NeighbourMsg,
        route: WorkspaceControlRoute,
    ) {
        if !matches!(msg, NeighbourMsg::RemoteInput { .. }) {
            tracing::warn!(ws, "refused non-input workspace-control publication");
            return;
        }
        match route {
            WorkspaceControlRoute::Direct { target } => {
                self.publish_direct(ws, target, msg).await;
            }
            WorkspaceControlRoute::Relayed { via } => {
                tracing::warn!(
                    ws,
                    relay = %via,
                    "refused workspace-control input on a workspace-wide relay"
                );
            }
        }
    }

    /// Send a remote-action frame to exactly `target`: directly when the
    /// device is live on this workspace's mesh, otherwise through an
    /// authenticated workspace host, which routes it to that device alone.
    async fn publish_remote_action(
        &mut self,
        ws: WorkspaceId,
        target: EndpointId,
        msg: NeighbourMsg,
    ) {
        let NeighbourMsg::RemoteAction { frame } = msg else {
            tracing::warn!(
                ws,
                "refused a non-action message on the remote-action route"
            );
            return;
        };
        if self.peers_seen.contains_key(&(ws, target))
            && self
                .publish_direct(
                    ws,
                    target,
                    NeighbourMsg::RemoteAction {
                        frame: frame.clone(),
                    },
                )
                .await
        {
            return;
        }
        let relayed = self
            .host_sessions
            .iter()
            .filter(|((session_ws, _), _)| *session_ws == ws)
            .any(|(_, runtime)| {
                runtime
                    .session
                    .publish_remote_action(*target.as_bytes(), frame.clone())
                    .is_ok()
            });
        if !relayed {
            tracing::warn!(ws, peer = %target, "no direct or relayed route for a remote action");
        }
    }

    /// Deliver one message to exactly `target` on the authenticated direct
    /// surface/control plane. The peer must be live on this workspace.
    /// Returns whether the message was written.
    async fn publish_direct(
        &mut self,
        ws: WorkspaceId,
        target: EndpointId,
        msg: NeighbourMsg,
    ) -> bool {
        if !self.peers_seen.contains_key(&(ws, target)) {
            tracing::warn!(ws, peer = %target, "direct route is no longer live");
            return false;
        }
        // Input must land within an interactive frame budget or not at all;
        // an action frame (up to a catalogue page) is worth waiting for.
        let budget = if matches!(msg, NeighbourMsg::RemoteAction { .. }) {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(40)
        };
        let wire = SurfaceWireFrame {
            workspace: ws,
            message: msg,
        };
        let Ok(bytes) = postcard::to_allocvec(&wire) else {
            return false;
        };
        let addr = self
            .peers
            .get(&target)
            .cloned()
            .unwrap_or_else(|| EndpointAddr::new(target));
        let connection = if let Some(connection) = self.surface_connections.get(&target) {
            connection.clone()
        } else {
            let Ok(Ok(connection)) = tokio::time::timeout(
                Duration::from_millis(300),
                self.endpoint.connect(addr, SURFACE_STREAM_ALPN),
            )
            .await
            else {
                tracing::warn!(ws, peer = %target, "direct connection failed");
                return false;
            };
            self.surface_connections.insert(target, connection.clone());
            connection
        };
        let send = async {
            let mut stream = connection.open_uni().await.map_err(|_| ())?;
            stream.write_all(&bytes).await.map_err(|_| ())?;
            stream.finish().map_err(|_| ())?;
            Ok::<(), ()>(())
        };
        if matches!(tokio::time::timeout(budget, send).await, Ok(Ok(()))) {
            return true;
        }
        self.surface_connections.remove(&target);
        tracing::warn!(ws, peer = %target, "direct publish failed");
        false
    }

    /// [`NetCmd::JoinWorkspace`] arm of [`Self::handle_command`], split out
    /// to keep that dispatcher short.
    async fn handle_join_workspace(&mut self, ws: WorkspaceId, on_event: mpsc::Sender<NetEvent>) {
        self.subscribers.insert(ws, on_event);
        if let std::collections::hash_map::Entry::Vacant(e) = self.topics.entry(ws) {
            let bootstrap = self.peers.keys().copied().collect();
            match self.gossip.subscribe(topic_id(ws), bootstrap).await {
                Ok(topic) => {
                    let (sender, mut receiver) = topic.split();
                    let tx = self.internal_tx.clone();
                    let pump = tokio::spawn(async move {
                        while let Some(result) = receiver.next().await {
                            match result {
                                Ok(event) => {
                                    if tx.send(InternalEvent::Topic { ws, event }).await.is_err() {
                                        return;
                                    }
                                }
                                Err(error) => {
                                    tracing::warn!(ws, %error, "iroh gossip subscription failed");
                                    break;
                                }
                            }
                        }
                        let _ = tx.send(InternalEvent::Closed { ws }).await;
                    });
                    e.insert(TopicRuntime { sender, pump });
                }
                Err(error) => tracing::warn!(ws, %error, "could not join iroh workspace topic"),
            }
        }
        if let Some(identity) = self.host_identity.as_ref() {
            for (addr, expectation) in self.hosts.values().cloned() {
                schedule_host_topic(
                    &self.endpoint,
                    identity,
                    ws,
                    addr.clone(),
                    expectation,
                    &self.internal_tx,
                    &self.host_sessions,
                    &mut self.connecting_hosts,
                    &mut self.content_discovery_tasks,
                );
            }
        }
    }

    /// Handle one [`InternalEvent`]. Returns `false` once the internal
    /// channel has closed, so [`run`]'s loop should stop.
    async fn handle_internal(&mut self, internal: Option<InternalEvent>) -> bool {
        match internal {
            Some(InternalEvent::Topic { ws, event }) => {
                self.handle_topic_event(ws, event).await;
            }
            Some(InternalEvent::Closed { ws }) => {
                self.handle_workspace_closed(ws).await;
            }
            Some(InternalEvent::HostConnected {
                ws,
                peer,
                addr,
                result,
            }) => {
                self.handle_host_connected(ws, peer, &addr, result);
            }
            Some(InternalEvent::Host { ws, peer, event }) => {
                self.handle_host_event(ws, peer, event).await;
            }
            Some(InternalEvent::SurfaceFrame {
                ws,
                peer,
                fingerprint,
                msg,
            }) => {
                self.handle_surface_frame(ws, peer, fingerprint, msg).await;
            }
            Some(InternalEvent::SurfaceFanoutComplete {
                ws,
                msg,
                outcomes,
                all_delivered,
            }) => {
                self.handle_surface_fanout_complete(ws, msg, outcomes, all_delivered)
                    .await;
            }
            Some(InternalEvent::PersonalFabric {
                ws,
                peer,
                connection_incarnation,
                user_public_key,
                payload,
            }) => {
                self.handle_personal_fabric(
                    ws,
                    peer,
                    connection_incarnation,
                    user_public_key,
                    payload,
                )
                .await;
            }
            Some(InternalEvent::PersonalFabricClosed {
                peer,
                connection_incarnation,
            }) => {
                self.personal_fabric_ingress
                    .remove(&(peer, connection_incarnation));
            }
            Some(InternalEvent::PersonalMediaProgress {
                target,
                transfer_id,
                generation,
                sent_bytes,
            }) => {
                let key = (target, transfer_id);
                if let Some(active) = self
                    .active_personal_media
                    .get(&key)
                    .filter(|active| active.generation == generation)
                {
                    self.report_personal_media(
                        active.workspace,
                        target,
                        transfer_id,
                        sent_bytes,
                        active.total_bytes,
                        PersonalMediaTransferStatus::Progress,
                    );
                }
            }
            Some(InternalEvent::PersonalMediaFinished {
                target,
                transfer_id,
                generation,
                outcome,
                sent_bytes,
            }) => {
                self.handle_personal_media_finished(
                    target,
                    transfer_id,
                    generation,
                    outcome,
                    sent_bytes,
                );
            }
            None => return false,
        }
        true
    }

    /// [`InternalEvent::Closed`] arm of [`Self::handle_internal`], split
    /// out to keep that dispatcher short.
    async fn handle_workspace_closed(&mut self, ws: WorkspaceId) {
        self.topics.remove(&ws);
        let subscriber = self.subscribers.get(&ws).cloned();
        let closed = self
            .peers_seen
            .iter()
            .filter_map(|(&(peer_ws, peer), membership)| {
                (peer_ws == ws).then_some((peer, membership.epoch))
            })
            .collect::<Vec<_>>();
        for (peer, epoch) in closed {
            if self.remove_membership_if_epoch(ws, peer, epoch)
                && let Some(tx) = subscriber.as_ref()
            {
                let _ = tx
                    .send(NetEvent::PeerLeft {
                        ws,
                        peer,
                        cause: PeerDepartureCause::WorkspaceRouteClosed,
                        membership_epoch: Some(epoch),
                    })
                    .await;
            }
        }
    }

    /// [`InternalEvent::SurfaceFrame`] arm of [`Self::handle_internal`], split
    /// out to keep that dispatcher short.
    async fn handle_surface_frame(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
        fingerprint: [u8; 32],
        msg: NeighbourMsg,
    ) {
        // A bare ALPN connection is not workspace admission. Require
        // the peer to have appeared on this workspace's authenticated
        // gossip topic first; ShellNetwork performs the capability
        // and claimed-publisher checks before dispatch.
        let Some(membership_epoch) = self.touch_existing_membership(ws, peer) else {
            tracing::warn!(ws, peer = %peer, "surface frame from unknown workspace peer");
            return;
        };
        // Input has no event id or timestamp, so two deliberate
        // identical key/button events must not be content-deduped.
        // A remote-action retry is byte-identical on purpose; the
        // receiving host deduplicates it by invocation id instead.
        if (matches!(
            &msg,
            NeighbourMsg::RemoteInput { .. } | NeighbourMsg::RemoteAction { .. }
        ) || mark_new_message(ws, fingerprint, &mut self.recent, &mut self.recent_order))
            && let Some(tx) = self.subscribers.get(&ws)
        {
            let _ = tx
                .send(NetEvent::Msg {
                    ws,
                    peer,
                    membership_epoch: Some(membership_epoch),
                    msg,
                })
                .await;
        }
    }

    /// [`InternalEvent::SurfaceFanoutComplete`] arm of [`Self::handle_internal`], split
    /// out to keep that dispatcher short.
    async fn handle_surface_fanout_complete(
        &mut self,
        ws: WorkspaceId,
        msg: NeighbourMsg,
        outcomes: Vec<SurfaceFanoutOutcome>,
        all_delivered: bool,
    ) {
        self.surface_fanout_tasks.remove(&ws);
        if !self.subscribers.contains_key(&ws) {
            self.pending_surface_frames.remove(&ws);
            return;
        }
        for outcome in outcomes {
            if outcome.delivered && self.peers_seen.contains_key(&(ws, outcome.peer)) {
                if let Some(connection) = outcome.connection {
                    self.surface_connections.insert(outcome.peer, connection);
                }
            } else {
                self.surface_connections.remove(&outcome.peer);
            }
        }
        if !all_delivered {
            publish(
                &self.topics,
                &self.host_sessions,
                ws,
                msg,
                false,
                true,
                false,
            )
            .await;
        }
        if let Some(next) = self.pending_surface_frames.remove(&ws)
            && !self.start_surface_fanout(ws, next.clone())
        {
            publish(
                &self.topics,
                &self.host_sessions,
                ws,
                next,
                false,
                true,
                false,
            )
            .await;
        }
    }

    /// [`InternalEvent::PersonalFabric`] arm of [`Self::handle_internal`], split
    /// out to keep that dispatcher short.
    async fn handle_personal_fabric(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
        connection_incarnation: u64,
        user_public_key: [u8; 32],
        payload: PersonalFabricPayload,
    ) {
        // Topic membership establishes reachability only. ShellNetwork
        // still binds the frame to the peer's certified person/device.
        let Some(membership_epoch) = self.touch_existing_membership(ws, peer) else {
            tracing::warn!(ws, peer = %peer, "personal fabric frame from unknown peer");
            return;
        };
        if !bind_personal_fabric_ingress(
            &mut self.personal_fabric_ingress,
            peer,
            connection_incarnation,
            ws,
            membership_epoch,
        ) {
            tracing::warn!(ws, peer = %peer, "personal fabric connection crossed membership authority");
            return;
        }
        if let Some(tx) = self.subscribers.get(&ws) {
            let _ = tx
                .send(NetEvent::PersonalFabric {
                    ws,
                    peer,
                    membership_epoch,
                    user_public_key,
                    payload,
                })
                .await;
        }
    }

    /// [`InternalEvent::PersonalMediaFinished`] arm of [`Self::handle_internal`], split
    /// out to keep that dispatcher short.
    fn handle_personal_media_finished(
        &mut self,
        target: EndpointId,
        transfer_id: [u8; 16],
        generation: u64,
        outcome: PersonalMediaStreamOutcome,
        sent_bytes: u64,
    ) {
        let key = (target, transfer_id);
        if let Some(active) =
            take_personal_media_generation(&mut self.active_personal_media, key, generation)
        {
            let status = match outcome {
                PersonalMediaStreamOutcome::Completed => PersonalMediaTransferStatus::Completed,
                PersonalMediaStreamOutcome::Failed => PersonalMediaTransferStatus::Failed,
                PersonalMediaStreamOutcome::Cancelled => PersonalMediaTransferStatus::Cancelled,
            };
            self.report_personal_media(
                active.workspace,
                target,
                transfer_id,
                sent_bytes,
                active.total_bytes,
                status,
            );
        }
    }

    /// [`InternalEvent::Topic`] arm of [`Self::handle_internal`], split out
    /// to keep that dispatcher short.
    async fn handle_topic_event(&mut self, ws: WorkspaceId, event: GossipEvent) {
        if matches!(event, GossipEvent::NeighborUp(_)) {
            // Authenticate this device as soon as the link is usable. State
            // sync can otherwise arrive before the periodic identity
            // announcement and be rejected by the peer.
            self.announce_identity(ws).await;
        }
        let Some(tx) = self.subscribers.get(&ws).cloned() else {
            return;
        };
        let event = match event {
            GossipEvent::NeighborUp(peer) => {
                let (membership_epoch, _) = self.touch_or_mint_membership(ws, peer);
                Some(NetEvent::PeerJoined {
                    ws,
                    peer,
                    membership_epoch,
                })
            }
            GossipEvent::NeighborDown(peer) => {
                // Gossip does not attach a connection incarnation to a down
                // edge. Revoking the current epoch here could let a delayed
                // Down(N) kill a newer Up(N+1). Keep the exact membership
                // until its actor-owned heartbeat deadline confirms loss.
                tracing::debug!(ws, peer = %peer, "gossip neighbour down awaits liveness confirmation");
                None
            }
            GossipEvent::Received(message) => {
                let peer = message.delivered_from;
                let (membership_epoch, rediscovered) = self.touch_or_mint_membership(ws, peer);
                let fingerprint = message_fingerprint(&message.content);
                if !mark_new_message(ws, fingerprint, &mut self.recent, &mut self.recent_order) {
                    return;
                }
                match postcard::from_bytes::<NeighbourMsg>(&message.content) {
                    Ok(NeighbourMsg::Heartbeat { .. }) if rediscovered => {
                        Some(NetEvent::PeerJoined {
                            ws,
                            peer,
                            membership_epoch,
                        })
                    }
                    Ok(NeighbourMsg::Heartbeat { .. }) => None,
                    Ok(msg) => Some(NetEvent::Msg {
                        ws,
                        peer,
                        membership_epoch: Some(membership_epoch),
                        msg,
                    }),
                    Err(error) => {
                        tracing::warn!(ws, %error, "invalid collaboration message");
                        None
                    }
                }
            }
            GossipEvent::Lagged => {
                tracing::warn!(
                    ws,
                    "iroh gossip receiver lagged; anti-entropy will recover durable state"
                );
                None
            }
        };
        if let Some(event) = event {
            let _ = tx.send(event).await;
        }
    }

    /// [`InternalEvent::HostConnected`] arm of [`Self::handle_internal`],
    /// split out to keep that dispatcher short.
    fn handle_host_connected(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
        attempted_addr: &EndpointAddr,
        result: Box<Result<RelayCoeditSession, String>>,
    ) {
        self.connecting_hosts.remove(&(ws, peer));
        if !self.topics.contains_key(&ws)
            || self
                .hosts
                .get(&peer)
                .is_none_or(|(current, _)| current != attempted_addr)
        {
            return;
        }
        match *result {
            Ok(session) => {
                let session = Arc::new(session);
                let pump =
                    spawn_host_pump(ws, peer, Arc::clone(&session), self.internal_tx.clone());
                self.host_sessions
                    .insert((ws, peer), HostRuntime { session, pump });
            }
            Err(error) => {
                tracing::warn!(ws, peer = %peer, %error, "workspace host connection failed");
            }
        }
    }

    /// [`InternalEvent::Host`] arm of [`Self::handle_internal`], split out
    /// to keep that dispatcher short.
    async fn handle_host_event(&mut self, ws: WorkspaceId, peer: EndpointId, event: HostEvent) {
        let mapped = match event {
            HostEvent::Ready => Some(NetEvent::HostReady { ws, peer }),
            HostEvent::Message {
                author,
                fingerprint,
                msg,
            } => {
                // A remote-action retry is byte-identical on purpose; the
                // receiving host deduplicates it by invocation id instead.
                if !matches!(msg, NeighbourMsg::RemoteAction { .. })
                    && !mark_new_message(ws, fingerprint, &mut self.recent, &mut self.recent_order)
                {
                    return;
                }
                Some(NetEvent::Msg {
                    ws,
                    peer: author,
                    membership_epoch: None,
                    msg,
                })
            }
            HostEvent::Closed => {
                if let Some(runtime) = self.host_sessions.remove(&(ws, peer)) {
                    runtime.pump.abort();
                }
                Some(NetEvent::PeerLeft {
                    ws,
                    peer,
                    cause: PeerDepartureCause::HostSessionClosed,
                    membership_epoch: None,
                })
            }
        };
        if let (Some(event), Some(tx)) = (mapped, self.subscribers.get(&ws)) {
            let _ = tx.send(event).await;
        }
    }

    /// Drop peers that have gone quiet for longer than [`PEER_TIMEOUT`].
    async fn run_eviction_tick(&mut self) {
        let now = Instant::now();
        let stale: Vec<_> = self
            .peers_seen
            .iter()
            .filter_map(|(&(ws, peer), membership)| {
                (now.duration_since(membership.last_seen) > PEER_TIMEOUT).then_some((
                    ws,
                    peer,
                    membership.epoch,
                ))
            })
            .collect();
        for (ws, peer, membership_epoch) in stale {
            if !self.remove_membership_if_epoch(ws, peer, membership_epoch) {
                continue;
            }
            if let Some(tx) = self.subscribers.get(&ws) {
                let _ = tx
                    .send(NetEvent::PeerLeft {
                        ws,
                        peer,
                        cause: PeerDepartureCause::LivenessTimedOut,
                        membership_epoch: Some(membership_epoch),
                    })
                    .await;
            }
        }
    }

    /// Refresh the advertised listen address, retry any workspace hosts not
    /// yet connected, and broadcast this device's identity + a liveness
    /// heartbeat on every joined topic.
    async fn run_heartbeat_tick(&mut self) {
        if let Ok(mut out) = self.listen_addrs.lock() {
            out.clear();
            out.push(self.endpoint.addr());
        }
        if let Some(identity) = self.host_identity.as_ref() {
            for ws in self.topics.keys().copied() {
                for (addr, expectation) in self.hosts.values().cloned() {
                    schedule_host_topic(
                        &self.endpoint,
                        identity,
                        ws,
                        addr,
                        expectation,
                        &self.internal_tx,
                        &self.host_sessions,
                        &mut self.connecting_hosts,
                        &mut self.content_discovery_tasks,
                    );
                }
            }
        }
        let workspaces: Vec<_> = self.topics.keys().copied().collect();
        for ws in workspaces {
            self.announce_identity(ws).await;
        }
        let msg = NeighbourMsg::Heartbeat {
            peer: crate::network::behaviour::peer_liveness_handle(&self.endpoint.id()),
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
        };
        if let Ok(bytes) = postcard::to_allocvec(&msg) {
            for topic in self.topics.values() {
                let _ = topic.sender.broadcast(Bytes::from(bytes.clone())).await;
            }
        }
        // What this machine can encode and decode rides along with every
        // heartbeat, so a sharer can pick a codec each viewer takes and
        // notices when a viewer disappears.
        let advert = crate::network::media_caps::local_advert(
            crate::network::behaviour::peer_liveness_handle(&self.endpoint.id()),
        );
        if let Ok(bytes) = postcard::to_allocvec(&advert) {
            for topic in self.topics.values() {
                let _ = topic.sender.broadcast(Bytes::from(bytes.clone())).await;
            }
        }
    }

    /// Send this device's certificate to its direct neighbors on `ws`.
    ///
    /// Identity announcements are deliberately neighbor-only: receivers bind
    /// the certificate to the endpoint that delivered it, so forwarding one
    /// through another gossip peer would make that check fail.
    async fn announce_identity(&self, ws: WorkspaceId) {
        let (Some(identity), Some(topic)) = (self.host_identity.as_ref(), self.topics.get(&ws))
        else {
            return;
        };
        let (cert, revocations) = identity.material_for(ws);
        let hello = NeighbourMsg::IdentityHello {
            cert: Box::new(cert),
            revocations,
        };
        let bytes = match postcard::to_allocvec(&hello) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(ws, %error, "could not encode collaboration identity announcement");
                return;
            }
        };
        if let Err(error) = topic.sender.broadcast_neighbors(Bytes::from(bytes)).await {
            tracing::warn!(ws, %error, "could not send collaboration identity announcement");
        }
    }

    /// Re-announce every live workspace-host session so a late-joining
    /// subscriber still sees `HostReady`.
    async fn run_host_checkpoint_tick(&self) {
        for &(ws, peer) in self.host_sessions.keys() {
            if let Some(tx) = self.subscribers.get(&ws) {
                let _ = tx.send(NetEvent::HostReady { ws, peer }).await;
            }
        }
    }
}

async fn send_personal_media_frame(
    connection: &iroh::endpoint::Connection,
    workspace: WorkspaceId,
    user_public_key: [u8; 32],
    frame: PersonalMediaTransferFrame,
) -> Result<(), ()> {
    let wire = PersonalFabricWireFrame {
        workspace,
        user_public_key,
        payload: PersonalFabricPayload::MediaTransfer(frame),
    };
    let bytes = postcard::to_allocvec(&wire).map_err(|_| ())?;
    if bytes.len() > crate::shell::device_fabric::MAX_FABRIC_CHECKPOINT_BYTES + 128 {
        return Err(());
    }
    let send = async {
        let mut stream = connection.open_uni().await.map_err(|_| ())?;
        stream.write_all(&bytes).await.map_err(|_| ())?;
        stream.finish().map_err(|_| ())?;
        match stream.stopped().await.map_err(|_| ())? {
            None => Ok::<(), ()>(()),
            Some(_) => Err(()),
        }
    };
    tokio::time::timeout(Duration::from_secs(30), send)
        .await
        .map_err(|_| ())?
}

async fn send_nearby_transfer_frame(
    connection: &iroh::endpoint::Connection,
    workspace: WorkspaceId,
    user_public_key: [u8; 32],
    frame: PersonalNearbyTransferFrame,
) -> Result<(), ()> {
    let wire = PersonalFabricWireFrame {
        workspace,
        user_public_key,
        payload: PersonalFabricPayload::NearbyTransfer(frame),
    };
    let bytes = postcard::to_allocvec(&wire).map_err(|_| ())?;
    if bytes.len() > crate::shell::device_fabric::MAX_FABRIC_CHECKPOINT_BYTES + 128 {
        return Err(());
    }
    let send = async {
        let mut stream = connection.open_uni().await.map_err(|_| ())?;
        stream.write_all(&bytes).await.map_err(|_| ())?;
        stream.finish().map_err(|_| ())?;
        match stream.stopped().await.map_err(|_| ())? {
            None => Ok(()),
            Some(_) => Err(()),
        }
    };
    tokio::time::timeout(Duration::from_secs(30), send)
        .await
        .map_err(|_| ())?
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one audited streaming state machine keeps prefix verification, bounded chunks, progress, and the authenticated finish together"
)]
async fn stream_nearby_transfer(
    endpoint: Endpoint,
    addr: EndpointAddr,
    workspace: WorkspaceId,
    user_public_key: [u8; 32],
    target: EndpointId,
    transfer_id: [u8; 16],
    request_id: [u8; 16],
    total_bytes: u64,
    resume_offset: u64,
    prefix_hash: [u8; 32],
    content_hash: [u8; 32],
    mut file: std::fs::File,
    progress: Option<mpsc::Sender<NetEvent>>,
    sent_bytes: std::sync::Arc<std::sync::atomic::AtomicU64>,
) -> Result<(), ()> {
    use std::io::{Read as _, Seek as _};
    use tokio::io::AsyncReadExt as _;

    let mut hasher = blake3::Hasher::new();
    let mut verify = vec![0_u8; PERSONAL_MEDIA_CHUNK_BYTES];
    let mut verified = 0_u64;
    while verified < resume_offset {
        let want = usize::try_from(
            resume_offset
                .saturating_sub(verified)
                .min(PERSONAL_MEDIA_CHUNK_BYTES as u64),
        )
        .map_err(|_| ())?;
        let read = file
            .read(verify.get_mut(..want).ok_or(())?)
            .map_err(|_| ())?;
        if read == 0 {
            return Err(());
        }
        hasher.update(verify.get(..read).ok_or(())?);
        verified = verified.saturating_add(u64::try_from(read).map_err(|_| ())?);
    }
    if hasher.finalize().as_bytes() != &prefix_hash {
        return Err(());
    }
    file.seek(std::io::SeekFrom::Start(resume_offset))
        .map_err(|_| ())?;
    let connection = tokio::time::timeout(
        Duration::from_secs(10),
        endpoint.connect(addr, PERSONAL_FABRIC_ALPN),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    let mut file = tokio::fs::File::from_std(file);
    let mut offset = resume_offset;
    let mut buffer = vec![0_u8; PERSONAL_MEDIA_CHUNK_BYTES];
    while offset < total_bytes {
        let want = usize::try_from(
            total_bytes
                .saturating_sub(offset)
                .min(PERSONAL_MEDIA_CHUNK_BYTES as u64),
        )
        .map_err(|_| ())?;
        let read = file
            .read(buffer.get_mut(..want).ok_or(())?)
            .await
            .map_err(|_| ())?;
        if read == 0 {
            return Err(());
        }
        hasher.update(buffer.get(..read).ok_or(())?);
        send_nearby_transfer_frame(
            &connection,
            workspace,
            user_public_key,
            PersonalNearbyTransferFrame::Chunk {
                transfer_id,
                request_id,
                offset,
                bytes: buffer.get(..read).ok_or(())?.to_vec(),
            },
        )
        .await?;
        offset = offset.saturating_add(u64::try_from(read).map_err(|_| ())?);
        sent_bytes.store(offset, std::sync::atomic::Ordering::Relaxed);
        if let Some(progress) = progress.as_ref() {
            let _ = progress.try_send(NetEvent::NearbyTransferProgress {
                target,
                transfer_id,
                sent_bytes: offset,
                total_bytes,
                status: PersonalMediaTransferStatus::Progress,
            });
        }
    }
    let mut tail = [0_u8; 1];
    if file.read(&mut tail).await.map_err(|_| ())? != 0 {
        return Err(());
    }
    if hasher.finalize().as_bytes() != &content_hash {
        return Err(());
    }
    let result = send_nearby_transfer_frame(
        &connection,
        workspace,
        user_public_key,
        PersonalNearbyTransferFrame::Finish {
            transfer_id,
            request_id,
            content_hash,
        },
    )
    .await;
    if result.is_err() {
        let _ = send_nearby_transfer_frame(
            &connection,
            workspace,
            user_public_key,
            PersonalNearbyTransferFrame::Abort {
                transfer_id,
                request_id,
            },
        )
        .await;
    }
    result
}

#[allow(
    clippy::too_many_arguments,
    reason = "the arguments mirror one immutable transfer manifest and its exact transport route"
)]
async fn stream_personal_media(
    endpoint: Endpoint,
    addr: EndpointAddr,
    workspace: WorkspaceId,
    user_public_key: [u8; 32],
    transfer_id: [u8; 16],
    file_name: String,
    total_bytes: u64,
    file: std::fs::File,
    internal: mpsc::Sender<InternalEvent>,
    target: EndpointId,
    sent_bytes: std::sync::Arc<std::sync::atomic::AtomicU64>,
    generation: u64,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> PersonalMediaStreamOutcome {
    use tokio::io::AsyncReadExt as _;

    let connect = tokio::time::timeout(
        Duration::from_secs(10),
        endpoint.connect(addr, PERSONAL_FABRIC_ALPN),
    );
    let connection = tokio::select! {
        result = connect => match result {
            Ok(Ok(connection)) => connection,
            _ => return PersonalMediaStreamOutcome::Failed,
        },
        _ = personal_media_cancelled(&mut cancel) => return PersonalMediaStreamOutcome::Cancelled,
    };
    let start = send_personal_media_frame(
        &connection,
        workspace,
        user_public_key,
        PersonalMediaTransferFrame::Start {
            transfer_id,
            file_name,
            total_bytes,
        },
    );
    let start_outcome = tokio::select! {
        result = start => result.map_or(PersonalMediaStreamOutcome::Failed, |_| PersonalMediaStreamOutcome::Completed),
        _ = personal_media_cancelled(&mut cancel) => PersonalMediaStreamOutcome::Cancelled,
    };
    if start_outcome != PersonalMediaStreamOutcome::Completed {
        if start_outcome == PersonalMediaStreamOutcome::Cancelled {
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                send_personal_media_frame(
                    &connection,
                    workspace,
                    user_public_key,
                    PersonalMediaTransferFrame::Abort { transfer_id },
                ),
            )
            .await;
            return PersonalMediaStreamOutcome::Cancelled;
        }
        return PersonalMediaStreamOutcome::Failed;
    }

    let mut file = tokio::fs::File::from_std(file);
    let mut offset = 0_u64;
    let mut last_reported = 0_u64;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; PERSONAL_MEDIA_CHUNK_BYTES];
    let transfer: Result<(), PersonalMediaStreamOutcome> = async {
        while offset < total_bytes {
            let remaining = total_bytes.saturating_sub(offset);
            let want = usize::try_from(remaining.min(PERSONAL_MEDIA_CHUNK_BYTES as u64))
                .map_err(|_| PersonalMediaStreamOutcome::Failed)?;
            let window = buffer
                .get_mut(..want)
                .ok_or(PersonalMediaStreamOutcome::Failed)?;
            let read = tokio::select! {
                result = file.read(window) => result.map_err(|_| PersonalMediaStreamOutcome::Failed)?,
                _ = personal_media_cancelled(&mut cancel) => return Err(PersonalMediaStreamOutcome::Cancelled),
            };
            if read == 0 {
                return Err(PersonalMediaStreamOutcome::Failed);
            }
            let bytes = buffer
                .get(..read)
                .ok_or(PersonalMediaStreamOutcome::Failed)?
                .to_vec();
            hasher.update(&bytes);
            let send = send_personal_media_frame(
                &connection,
                workspace,
                user_public_key,
                PersonalMediaTransferFrame::Chunk {
                    transfer_id,
                    offset,
                    bytes,
                },
            );
            tokio::select! {
                result = send => result.map_err(|_| PersonalMediaStreamOutcome::Failed)?,
                _ = personal_media_cancelled(&mut cancel) => return Err(PersonalMediaStreamOutcome::Cancelled),
            }
            offset = offset.saturating_add(
                u64::try_from(read).map_err(|_| PersonalMediaStreamOutcome::Failed)?,
            );
            sent_bytes.store(offset, std::sync::atomic::Ordering::Relaxed);
            if offset == total_bytes || offset.saturating_sub(last_reported) >= 8 * 1024 * 1024 {
                let _ = internal.try_send(InternalEvent::PersonalMediaProgress {
                    target,
                    transfer_id,
                    generation,
                    sent_bytes: offset,
                });
                last_reported = offset;
            }
        }
        let mut trailing = [0_u8; 1];
        let trailing_read = tokio::select! {
            result = file.read(&mut trailing) => result.map_err(|_| PersonalMediaStreamOutcome::Failed)?,
            _ = personal_media_cancelled(&mut cancel) => return Err(PersonalMediaStreamOutcome::Cancelled),
        };
        if trailing_read != 0 {
            return Err(PersonalMediaStreamOutcome::Failed);
        }
        let finish = send_personal_media_frame(
            &connection,
            workspace,
            user_public_key,
            PersonalMediaTransferFrame::Finish {
                transfer_id,
                content_hash: *hasher.finalize().as_bytes(),
            },
        );
        tokio::select! {
            result = finish => result.map_err(|_| PersonalMediaStreamOutcome::Failed),
            _ = personal_media_cancelled(&mut cancel) => Err(PersonalMediaStreamOutcome::Cancelled),
        }
    }
    .await;
    match transfer {
        Ok(()) => PersonalMediaStreamOutcome::Completed,
        Err(outcome) => {
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                send_personal_media_frame(
                    &connection,
                    workspace,
                    user_public_key,
                    PersonalMediaTransferFrame::Abort { transfer_id },
                ),
            )
            .await;
            outcome
        }
    }
}

async fn personal_media_cancelled(cancel: &mut tokio::sync::watch::Receiver<bool>) {
    if *cancel.borrow() {
        return;
    }
    if cancel.changed().await.is_err() {
        return;
    }
}

async fn run(
    endpoint: Endpoint,
    lookup: MemoryLookup,
    host_identity: Option<HostIdentity>,
    listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
    mut cmd_rx: mpsc::Receiver<NetCmd>,
    heartbeat_interval: Duration,
) {
    let (internal_tx, mut internal_rx) = mpsc::channel(256);
    let (input_tx, mut input_rx) = mpsc::channel(64);
    let gossip = Gossip::builder()
        .max_message_size(MAX_GOSSIP_MESSAGE_BYTES)
        .spawn(endpoint.clone());
    let router = Router::builder(endpoint.clone())
        .accept(GOSSIP_ALPN, gossip.clone())
        .accept(
            SURFACE_STREAM_ALPN,
            SurfaceStreamProtocol {
                internal_tx: internal_tx.clone(),
                input_tx,
            },
        )
        .accept(
            PERSONAL_FABRIC_ALPN,
            PersonalFabricProtocol {
                internal_tx: internal_tx.clone(),
                next_connection_incarnation: Arc::new(AtomicU64::new(1)),
            },
        )
        .spawn();
    let mut state = ActorState {
        endpoint,
        lookup,
        host_identity,
        listen_addrs,
        gossip,
        internal_tx,
        topics: HashMap::new(),
        subscribers: HashMap::new(),
        peers: HashMap::new(),
        hosts: HashMap::new(),
        host_sessions: HashMap::new(),
        connecting_hosts: HashSet::new(),
        content_discovery_tasks: HashMap::new(),
        peers_seen: HashMap::new(),
        next_membership_epoch: 0,
        personal_fabric_ingress: HashMap::new(),
        recent: HashSet::new(),
        recent_order: VecDeque::new(),
        surface_connections: HashMap::new(),
        surface_fanout_tasks: HashMap::new(),
        pending_surface_frames: HashMap::new(),
        personal_fabric_connections: HashMap::new(),
        active_personal_media: HashMap::new(),
        next_personal_media_generation: 0,
    };
    let mut heartbeat = tokio::time::interval(heartbeat_interval);
    let mut eviction = tokio::time::interval(EVICTION_INTERVAL);
    let mut host_checkpoint = tokio::time::interval(HOST_CHECKPOINT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    eviction.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    host_checkpoint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await;
    eviction.tick().await;
    host_checkpoint.tick().await;

    loop {
        tokio::select! {
            biased;
            input = input_rx.recv() => {
                if !state.handle_internal(input).await {
                    break;
                }
            }
            command = cmd_rx.recv() => {
                if !state.handle_command(command).await {
                    break;
                }
            }
            internal = internal_rx.recv() => {
                if !state.handle_internal(internal).await {
                    break;
                }
            }
            _ = eviction.tick() => state.run_eviction_tick().await,
            _ = heartbeat.tick() => state.run_heartbeat_tick().await,
            _ = host_checkpoint.tick() => state.run_host_checkpoint_tick().await,
        }
    }

    for (_, topic) in state.topics {
        topic.pump.abort();
    }
    for (_, host) in state.host_sessions {
        host.pump.abort();
    }
    for (_, task) in state.surface_fanout_tasks {
        task.abort();
    }
    let _ = router.shutdown().await;
}

async fn publish(
    topics: &HashMap<WorkspaceId, TopicRuntime>,
    host_sessions: &HashMap<(WorkspaceId, EndpointId), HostRuntime>,
    ws: WorkspaceId,
    msg: NeighbourMsg,
    shell_checkpoint: bool,
    broadcast_gossip: bool,
    publish_hosts: bool,
) {
    if let NeighbourMsg::PersonalContinuityCheckpoint { snapshot, .. } = &msg
        && snapshot.len() > MAX_PERSONAL_CONTINUITY_CHECKPOINT_BYTES
    {
        tracing::warn!(
            ws,
            bytes = snapshot.len(),
            "personal continuity checkpoint exceeds transport limit"
        );
        return;
    }
    let bytes = match postcard::to_allocvec(&msg) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(ws, %error, "collaboration message encode failed");
            return;
        }
    };
    let retention = retention(&msg, shell_checkpoint);

    // Host clients enqueue synchronously into their already-authenticated
    // QUIC pumps. Do that before awaiting gossip: a slow or reconnecting
    // direct mesh must not add its delay to the independent hosted path.
    // This also keeps collaboration live through a transient failure to join
    // the gossip topic, provided an authenticated workspace host is ready.
    if publish_hosts {
        for ((session_ws, _), runtime) in host_sessions {
            if *session_ws != ws {
                continue;
            }
            let result = match retention {
                Some((channel, Retention::Checkpoint)) => {
                    runtime.session.publish_checkpoint(channel, bytes.clone())
                }
                Some((channel, Retention::Delta)) => runtime
                    .session
                    .publish_retained_delta(channel, bytes.clone()),
                None => runtime.session.publish_presence(bytes.clone()),
            };
            if let Err(error) = result {
                tracing::warn!(ws, %error, "workspace host publish failed");
            }
        }
    }
    if broadcast_gossip {
        if let Some(topic) = topics.get(&ws) {
            if let Err(error) = topic.sender.broadcast(Bytes::from(bytes)).await {
                tracing::warn!(ws, %error, "iroh gossip publish failed");
            }
        } else if host_sessions
            .keys()
            .all(|(session_ws, _)| *session_ws != ws)
        {
            tracing::warn!(
                ws,
                "collaboration publish has no direct topic or workspace host"
            );
        }
    }
}

fn message_fingerprint(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn mark_new_message(
    ws: WorkspaceId,
    fingerprint: [u8; 32],
    recent: &mut HashSet<(WorkspaceId, [u8; 32])>,
    order: &mut VecDeque<(WorkspaceId, [u8; 32])>,
) -> bool {
    const MAX_RECENT: usize = 1024;
    let key = (ws, fingerprint);
    if !recent.insert(key) {
        return false;
    }
    order.push_back(key);
    if order.len() > MAX_RECENT
        && let Some(oldest) = order.pop_front()
    {
        recent.remove(&oldest);
    }
    true
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one scheduler transition atomically coordinates the endpoint, host identity, route state, and single discovery-task registry"
)]
fn schedule_host_topic(
    endpoint: &Endpoint,
    identity: &HostIdentity,
    ws: WorkspaceId,
    addr: EndpointAddr,
    expectation: Option<HomePeerExpectation>,
    internal_tx: &mpsc::Sender<InternalEvent>,
    sessions: &HashMap<(WorkspaceId, EndpointId), HostRuntime>,
    connecting: &mut HashSet<(WorkspaceId, EndpointId)>,
    content_discovery_tasks: &mut HashMap<(WorkspaceId, EndpointId), tokio::task::JoinHandle<()>>,
) {
    let host = addr.id;
    if let Some(expectation) = expectation.clone()
        && !content_discovery_tasks.contains_key(&(ws, host))
    {
        let discovery_endpoint = endpoint.clone();
        let discovery_addr = addr.clone();
        let discovery_identity = identity.clone();
        let task = tokio::spawn(async move {
            let mut retry = Duration::from_secs(1);
            loop {
                let (discovery_cert, entries) = discovery_identity.material_for(ws);
                let mut discovery_revocations = RevocationList::new();
                if entries
                    .into_iter()
                    .any(|entry| discovery_revocations.insert(entry).is_err())
                {
                    tracing::warn!(ws, "workspace identity has an invalid revocation entry");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
                let discovered = tokio::time::timeout(
                    Duration::from_secs(10),
                    RelayClient::discover_content_peer(
                        &discovery_endpoint,
                        discovery_addr.clone(),
                        ws,
                        expectation.user_public_key,
                        expectation.device_public_key,
                        discovery_cert.clone(),
                        discovery_identity.device_signing_key.clone(),
                        discovery_revocations.clone(),
                    ),
                )
                .await;
                match discovered {
                    Ok(Ok(content_peer)) => {
                        let _ = register_content_peer(content_peer).await;
                        // Re-advertise periodically. Registration persists the
                        // peer, while this also repairs a daemon replacement
                        // without coupling its lifecycle to the relay session.
                        retry = Duration::from_secs(30);
                    }
                    Ok(Err(error)) => {
                        tracing::debug!(%error, ws, "relay content discovery is not ready");
                        retry = retry.saturating_mul(2).min(Duration::from_secs(10));
                    }
                    Err(_) => {
                        tracing::debug!(ws, "relay content discovery timed out");
                        retry = retry.saturating_mul(2).min(Duration::from_secs(10));
                    }
                }
                tokio::time::sleep(retry).await;
            }
        });
        content_discovery_tasks.insert((ws, host), task);
    }
    if sessions.contains_key(&(ws, host)) || !connecting.insert((ws, host)) {
        return;
    }
    let endpoint = endpoint.clone();
    let identity = identity.clone();
    let (device_cert, revocation_entries) = identity.material_for(ws);
    let mut revocations = RevocationList::new();
    for entry in revocation_entries {
        if revocations.insert(entry).is_err() {
            tracing::warn!(ws, "workspace identity has an invalid revocation entry");
            connecting.remove(&(ws, host));
            return;
        }
    }
    let Some(expectation) = expectation else {
        // Invited/provider hosts can carry collaboration state, but only an
        // enrolled home peer may nominate a Files hydration endpoint.
        let tx = internal_tx.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                RelayClient::connect_to_pinned_host(
                    endpoint,
                    addr.clone(),
                    device_cert,
                    identity.device_signing_key.clone(),
                    revocations,
                ),
            )
            .await
            .map_err(|_| "workspace host connection timed out".to_string())
            .and_then(|result| result.map_err(|error| error.to_string()))
            .and_then(|client| {
                RelayCoeditSession::join(
                    client,
                    ws,
                    identity.device_signing_key.verifying_key().to_bytes(),
                )
                .map_err(|error| error.to_string())
            });
            let _ = tx
                .send(InternalEvent::HostConnected {
                    ws,
                    peer: host,
                    addr,
                    result: Box::new(result),
                })
                .await;
        });
        return;
    };
    let tx = internal_tx.clone();
    tokio::spawn(async move {
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            RelayClient::connect_to_enrolled_home_peer(
                endpoint,
                addr.clone(),
                expectation.user_public_key,
                expectation.device_public_key,
                device_cert,
                identity.device_signing_key.clone(),
                revocations,
            ),
        )
        .await
        .map_err(|_| "workspace host connection timed out".to_string())
        .and_then(|result| result.map_err(|error| error.to_string()))
        .and_then(|client| {
            RelayCoeditSession::join(
                client,
                ws,
                identity.device_signing_key.verifying_key().to_bytes(),
            )
            .map_err(|error| error.to_string())
        });
        let _ = tx
            .send(InternalEvent::HostConnected {
                ws,
                peer: host,
                addr,
                result: Box::new(result),
            })
            .await;
    });
}

/// Submit an authenticated relay-discovered Files peer to the running FUSE
/// daemon. This is deliberately a detached worker: endpoint registration may
/// wait on the group-restricted daemon socket but must never delay networking
/// or the compositor. The filesystem owns persistence and hydration policy.
#[allow(
    clippy::items_after_statements,
    reason = "the async write extension is local to the detached filesystem-registration worker and kept beside its only use"
)]
async fn register_content_peer(registration: tilo_relay_client::ContentPeerRegistration) -> bool {
    if cfg!(test) && std::env::var_os("TILO_FS_CONTENT_PEER_PUBLISHER").is_none() {
        return true;
    }
    let publisher = std::env::var_os("TILO_FS_CONTENT_PEER_PUBLISHER")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || std::path::PathBuf::from("/usr/bin/fs"),
            std::path::PathBuf::from,
        );
    let cache_dir = std::env::var_os("TILO_FS_CONTENT_PEER_CACHE_DIR")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || std::path::PathBuf::from("/var/cache/quick-fs"),
            std::path::PathBuf::from,
        );
    let Ok(payload) = serde_json::to_vec(&registration) else {
        tracing::warn!("could not serialize relay-discovered filesystem registration");
        return false;
    };
    let mut child = match tokio::process::Command::new(&publisher)
        .arg("register-content-peer")
        .arg("--cache-dir")
        .arg(&cache_dir)
        .arg("--endpoint-stdin")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(%error, publisher = %publisher.display(), "could not start filesystem content-peer registration");
            return false;
        }
    };
    use tokio::io::AsyncWriteExt as _;
    let Some(mut stdin) = child.stdin.take() else {
        tracing::warn!("filesystem content-peer registration has no stdin");
        return false;
    };
    if let Err(error) = stdin.write_all(&payload).await {
        tracing::warn!(%error, "could not send discovered endpoint to filesystem daemon");
        return false;
    }
    drop(stdin);
    match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
        Ok(Ok(status)) if status.success() => true,
        Ok(Ok(status)) => {
            tracing::warn!(?status, "filesystem content-peer registration failed");
            false
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "could not wait for filesystem content-peer registration");
            false
        }
        Err(_) => {
            tracing::warn!("filesystem content-peer registration timed out");
            false
        }
    }
}

/// Ask the running filesystem daemon to forget all endpoint/ticket pairs
/// authenticated by a removed enrolled relay. The daemon owns both its live
/// Hydrator and the durable catalog, so the shell cannot leave one stale.
async fn unregister_content_peers_for_relay(relay: EndpointId) -> bool {
    #[cfg(test)]
    noted_content_peer_removals()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(relay);
    if cfg!(test) && std::env::var_os("TILO_FS_CONTENT_PEER_PUBLISHER").is_none() {
        return true;
    }
    let publisher = std::env::var_os("TILO_FS_CONTENT_PEER_PUBLISHER")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || std::path::PathBuf::from("/usr/bin/fs"),
            std::path::PathBuf::from,
        );
    let cache_dir = std::env::var_os("TILO_FS_CONTENT_PEER_CACHE_DIR")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || std::path::PathBuf::from("/var/cache/quick-fs"),
            std::path::PathBuf::from,
        );
    match tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(&publisher)
            .arg("unregister-content-peer")
            .arg("--cache-dir")
            .arg(cache_dir)
            .arg("--relay-service-id")
            .arg(hex::encode(relay.as_bytes()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    {
        Ok(Ok(status)) if status.success() => true,
        Ok(Ok(status)) => {
            tracing::warn!(?status, relay = %relay, "filesystem content-peer removal failed");
            false
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, relay = %relay, "could not start filesystem content-peer removal");
            false
        }
        Err(_) => {
            tracing::warn!(relay = %relay, "filesystem content-peer removal timed out");
            false
        }
    }
}

#[cfg(test)]
fn noted_content_peer_removals() -> &'static Mutex<Vec<EndpointId>> {
    static REMOVALS: std::sync::OnceLock<Mutex<Vec<EndpointId>>> = std::sync::OnceLock::new();
    REMOVALS.get_or_init(|| Mutex::new(Vec::new()))
}

fn spawn_host_pump(
    ws: WorkspaceId,
    host: EndpointId,
    session: Arc<RelayCoeditSession>,
    tx: mpsc::Sender<InternalEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(inbound) = session.recv().await {
            let event = match inbound {
                HostInbound::Ready { .. } => HostEvent::Ready,
                HostInbound::RemoteAction { from, frame } => {
                    let Ok(author) = EndpointId::from_bytes(&from) else {
                        continue;
                    };
                    HostEvent::Message {
                        author,
                        // Never deduplicated; see `handle_host_event`.
                        fingerprint: [0; 32],
                        msg: NeighbourMsg::RemoteAction { frame },
                    }
                }
                HostInbound::Delta { author, data }
                | HostInbound::Presence {
                    author,
                    payload: data,
                } => {
                    let Ok(author) = EndpointId::from_bytes(&author) else {
                        continue;
                    };
                    let fingerprint = message_fingerprint(&data);
                    let Ok(msg) = postcard::from_bytes(&data) else {
                        continue;
                    };
                    HostEvent::Message {
                        author,
                        fingerprint,
                        msg,
                    }
                }
            };
            if tx
                .send(InternalEvent::Host {
                    ws,
                    peer: host,
                    event,
                })
                .await
                .is_err()
            {
                return;
            }
        }
        let _ = tx
            .send(InternalEvent::Host {
                ws,
                peer: host,
                event: HostEvent::Closed,
            })
            .await;
    })
}

fn topic_id(ws: WorkspaceId) -> TopicId {
    let mut hasher = blake3::Hasher::new_derive_key("tilo-workspace-gossip-v1");
    hasher.update(&ws.to_le_bytes());
    TopicId::from_bytes(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use getrandom::{SysRng, rand_core::UnwrapErr};

    fn active_personal_media(generation: u64) -> ActivePersonalMediaTransfer {
        let (cancel, _receiver) = tokio::sync::watch::channel(false);
        ActivePersonalMediaTransfer {
            generation,
            workspace: 7,
            total_bytes: 8,
            sent_bytes: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            cancel,
        }
    }

    #[test]
    fn personal_media_generation_is_exact_and_stale_completion_cannot_remove_replacement() {
        let first = iroh::SecretKey::from_bytes(&[0xB1; 32]).public();
        let sibling = iroh::SecretKey::from_bytes(&[0xB2; 32]).public();
        let transfer_id = [0xB3; 16];
        let key = (first, transfer_id);
        let sibling_key = (sibling, transfer_id);
        let mut active = HashMap::from([
            (key, active_personal_media(12)),
            (sibling_key, active_personal_media(4)),
        ]);

        assert!(take_personal_media_generation(&mut active, key, 11).is_none());
        assert!(
            active.contains_key(&key),
            "old worker cannot remove replacement"
        );
        assert!(
            take_personal_media_generation(&mut active, sibling_key, 12).is_none(),
            "same id on another target is isolated"
        );
        assert!(take_personal_media_generation(&mut active, key, 12).is_some());
        assert!(active.contains_key(&sibling_key));
    }

    #[test]
    fn departed_personal_connection_cannot_rebind_after_membership_reconnect() {
        let peer = iroh::SecretKey::from_bytes(&[0xA1; 32]).public();
        let old_epoch = MembershipEpoch::for_test(7);
        let new_epoch = MembershipEpoch::for_test(8);
        let mut ingress = HashMap::new();
        assert!(bind_personal_fabric_ingress(
            &mut ingress,
            peer,
            11,
            21,
            old_epoch,
        ));
        revoke_personal_fabric_ingress(&mut ingress, 21, old_epoch);
        assert!(
            !bind_personal_fabric_ingress(&mut ingress, peer, 11, 21, new_epoch),
            "the old QUIC connection stays tombstoned after reconnect"
        );
        assert!(
            bind_personal_fabric_ingress(&mut ingress, peer, 12, 21, new_epoch),
            "a fresh connection may bind the fresh membership epoch"
        );
    }
    use iroh::endpoint::presets;
    use tilo_relay::{
        Relay, RelayConfig, ReplicationIntent, WorkspaceBinding, daemon::RelayIdentity,
    };
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason, sign_device_cert,
        sign_revocation_entry,
    };
    use tokio::time::timeout;

    async fn endpoint() -> (Endpoint, MemoryLookup) {
        let lookup = MemoryLookup::new();
        let endpoint = Endpoint::builder(presets::N0)
            .address_lookup(lookup.clone())
            .bind()
            .await
            .unwrap();
        (endpoint, lookup)
    }

    #[tokio::test]
    async fn remote_input_bypasses_a_saturated_surface_frame_queue() {
        let (endpoint, _lookup) = endpoint().await;
        let peer = endpoint.id();
        let (media_tx, mut media_rx) = mpsc::channel(1);
        let (input_tx, mut input_rx) = mpsc::channel(1);
        media_tx
            .send(InternalEvent::Closed { ws: 7 })
            .await
            .unwrap();

        let queued = enqueue_surface_event(
            &media_tx,
            &input_tx,
            InternalEvent::SurfaceFrame {
                ws: 7,
                peer,
                fingerprint: [0; 32],
                msg: NeighbourMsg::RemoteInput {
                    peer: 9,
                    surface: 11,
                    event: vec![1],
                },
            },
        )
        .await;

        assert!(queued);
        assert!(matches!(
            input_rx.try_recv(),
            Ok(InternalEvent::SurfaceFrame {
                msg: NeighbourMsg::RemoteInput { surface: 11, .. },
                ..
            })
        ));
        assert!(matches!(
            media_rx.try_recv(),
            Ok(InternalEvent::Closed { ws: 7 })
        ));
    }

    #[test]
    fn nearby_transfer_frame_round_trips_exact_binding_and_bounded_chunk() {
        let wire = PersonalFabricWireFrame {
            workspace: 42,
            user_public_key: [3; 32],
            payload: PersonalFabricPayload::NearbyTransfer(PersonalNearbyTransferFrame::Chunk {
                transfer_id: [4; 16],
                request_id: [5; 16],
                offset: 99,
                bytes: vec![7; PERSONAL_MEDIA_CHUNK_BYTES],
            }),
        };
        let encoded = postcard::to_allocvec(&wire).unwrap();
        assert!(encoded.len() < crate::shell::device_fabric::MAX_FABRIC_CHECKPOINT_BYTES);
        let decoded: PersonalFabricWireFrame = postcard::from_bytes(&encoded).unwrap();
        let PersonalFabricPayload::NearbyTransfer(PersonalNearbyTransferFrame::Chunk {
            transfer_id,
            request_id,
            offset,
            bytes,
        }) = decoded.payload
        else {
            panic!("wrong nearby transfer frame");
        };
        assert_eq!(transfer_id, [4; 16]);
        assert_eq!(request_id, [5; 16]);
        assert_eq!(offset, 99);
        assert_eq!(bytes.len(), PERSONAL_MEDIA_CHUNK_BYTES);
    }

    async fn identified_endpoint(key: &ed25519_dalek::SigningKey) -> (Endpoint, MemoryLookup) {
        let lookup = MemoryLookup::new();
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
            .address_lookup(lookup.clone())
            .bind()
            .await
            .unwrap();
        (endpoint, lookup)
    }

    fn enrol(
        root: &ed25519_dalek::SigningKey,
        capabilities: DeviceCapabilities,
        label: &str,
    ) -> (ed25519_dalek::SigningKey, DeviceCert) {
        let key = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        (key, cert)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_actors_exchange_over_iroh() {
        let (a, lookup_a) = endpoint().await;
        let (b, lookup_b) = endpoint().await;
        let source_peer = crate::network::behaviour::peer_liveness_handle(&a.id());
        let b_addr = b.addr();
        let (cmd_a, _a_task) = Actor::spawn(a, lookup_a);
        let (cmd_b, _b_task) = Actor::spawn(b, lookup_b);
        let (event_a, mut rx_a) = mpsc::channel(16);
        let (event_b, mut rx_b) = mpsc::channel(16);
        cmd_a
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: event_a,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: event_b,
            })
            .await
            .unwrap();
        cmd_a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
        loop {
            let event = timeout(Duration::from_secs(10), rx_a.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(event, NetEvent::PeerJoined { .. }) {
                break;
            }
        }
        cmd_b
            .send(NetCmd::Publish {
                ws: 42,
                msg: NeighbourMsg::SyncReq {
                    since: vec![],
                    room: String::new(),
                    invite_id: None,
                },
            })
            .await
            .unwrap();
        loop {
            let event = timeout(Duration::from_secs(10), rx_a.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(
                event,
                NetEvent::Msg {
                    msg: NeighbourMsg::SyncReq { .. },
                    ..
                }
            ) {
                break;
            }
        }

        let dedicated_before =
            DEDICATED_SURFACE_FRAMES_RECEIVED.load(std::sync::atomic::Ordering::Relaxed);
        cmd_a
            .send(NetCmd::Publish {
                ws: 42,
                msg: NeighbourMsg::SurfaceFrame {
                    surface: 7,
                    source_peer,
                    encoded_h264: vec![0, 0, 0, 1, 0x65],
                    ts: 99,
                },
            })
            .await
            .unwrap();
        loop {
            let event = timeout(Duration::from_secs(10), rx_b.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(
                event,
                NetEvent::Msg {
                    msg: NeighbourMsg::SurfaceFrame {
                        surface: 7,
                        ts: 99,
                        ..
                    },
                    ..
                }
            ) {
                break;
            }
        }
        assert!(
            DEDICATED_SURFACE_FRAMES_RECEIVED.load(std::sync::atomic::Ordering::Relaxed)
                > dedicated_before,
            "surface frame should arrive through the dedicated QUIC ALPN"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn dead_surface_viewer_does_not_stall_later_collaboration_messages() {
        let (a, lookup_a) = endpoint().await;
        let (b, lookup_b) = endpoint().await;
        let (c, lookup_c) = endpoint().await;
        let source_peer = crate::network::behaviour::peer_liveness_handle(&a.id());
        let b_addr = b.addr();
        let c_addr = c.addr();
        let (cmd_a, task_a) = Actor::spawn(a, lookup_a);
        let (cmd_b, task_b) = Actor::spawn(b, lookup_b);
        let (cmd_c, task_c) = Actor::spawn(c, lookup_c);
        let (event_a, mut rx_a) = mpsc::channel(32);
        let (event_b, _rx_b) = mpsc::channel(32);
        let (event_c, mut rx_c) = mpsc::channel(32);
        for (command, events) in [(&cmd_a, event_a), (&cmd_b, event_b), (&cmd_c, event_c)] {
            command
                .send(NetCmd::JoinWorkspace {
                    ws: 44,
                    on_event: events,
                })
                .await
                .unwrap();
        }
        cmd_a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
        cmd_a.send(NetCmd::Connect { addr: c_addr }).await.unwrap();
        let mut joined = HashSet::new();
        while joined.len() < 2 {
            if let NetEvent::PeerJoined { peer, .. } = timeout(Duration::from_secs(10), rx_a.recv())
                .await
                .unwrap()
                .unwrap()
            {
                joined.insert(peer);
            }
        }

        // Leave A's authenticated membership evidence intact while making B's
        // dedicated media address unreachable. A direct dial may consume the
        // full media timeout, but it must happen outside the actor loop.
        task_b.abort();
        let _ = task_b.await;
        tokio::time::sleep(Duration::from_millis(25)).await;
        let started = Instant::now();
        cmd_a
            .send(NetCmd::Publish {
                ws: 44,
                msg: NeighbourMsg::SurfaceFrame {
                    surface: 9,
                    source_peer,
                    encoded_h264: vec![0, 0, 0, 1, 0x65],
                    ts: 1,
                },
            })
            .await
            .unwrap();
        cmd_a
            .send(NetCmd::Publish {
                ws: 44,
                msg: NeighbourMsg::SyncReq {
                    since: vec![],
                    room: "responsive".into(),
                    invite_id: None,
                },
            })
            .await
            .unwrap();
        loop {
            let event = timeout(Duration::from_millis(250), rx_c.recv())
                .await
                .expect("dead media viewer stalled the collaboration actor")
                .unwrap();
            if matches!(
                event,
                NetEvent::Msg {
                    msg: NeighbourMsg::SyncReq { ref room, .. },
                    ..
                } if room == "responsive"
            ) {
                break;
            }
        }
        assert!(started.elapsed() < Duration::from_millis(250));
        task_a.abort();
        task_c.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn personal_fabric_uses_its_negotiated_alpn() {
        let (a, lookup_a) = endpoint().await;
        let (b, lookup_b) = endpoint().await;
        let b_addr = b.addr();
        let (cmd_a, _a_task) = Actor::spawn(a, lookup_a);
        let (cmd_b, _b_task) = Actor::spawn(b, lookup_b);
        let (event_a, mut rx_a) = mpsc::channel(16);
        let (event_b, mut rx_b) = mpsc::channel(16);
        cmd_a
            .send(NetCmd::JoinWorkspace {
                ws: 43,
                on_event: event_a,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::JoinWorkspace {
                ws: 43,
                on_event: event_b,
            })
            .await
            .unwrap();
        cmd_a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
        while !matches!(
            timeout(Duration::from_secs(10), rx_a.recv())
                .await
                .unwrap()
                .unwrap(),
            NetEvent::PeerJoined { .. }
        ) {}

        cmd_a
            .send(NetCmd::PublishPersonalFabric {
                ws: 43,
                user_public_key: [0xA5; 32],
                payload: PersonalFabricPayload::Snapshot(vec![1, 2, 3]),
            })
            .await
            .unwrap();
        loop {
            let event = timeout(Duration::from_secs(10), rx_b.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(
                event,
                NetEvent::PersonalFabric {
                    ws: 43,
                    user_public_key,
                    payload: PersonalFabricPayload::Snapshot(bytes),
                    ..
                } if user_public_key == [0xA5; 32] && bytes == vec![1, 2, 3]
            ) {
                break;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(
        clippy::too_many_lines,
        reason = "the integration test verifies both receiver framing and the sender's ordered progress lifecycle"
    )]
    async fn personal_media_streams_in_order_with_a_verified_finish_hash() {
        let (a, lookup_a) = endpoint().await;
        let (b, lookup_b) = endpoint().await;
        let a_id = a.id();
        let b_id = b.id();
        let b_addr = b.addr();
        let (cmd_a, _a_task) = Actor::spawn(a, lookup_a);
        let (cmd_b, _b_task) = Actor::spawn(b, lookup_b);
        let (event_a, mut rx_a) = mpsc::channel(16);
        let (event_b, mut rx_b) = mpsc::channel(16);
        for (command, events) in [(&cmd_a, event_a), (&cmd_b, event_b)] {
            command
                .send(NetCmd::JoinWorkspace {
                    ws: 45,
                    on_event: events,
                })
                .await
                .unwrap();
        }
        cmd_a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
        while !matches!(
            timeout(Duration::from_secs(10), rx_a.recv())
                .await
                .unwrap()
                .unwrap(),
            NetEvent::PeerJoined { peer, .. } if peer == b_id
        ) {}
        while !matches!(
            timeout(Duration::from_secs(10), rx_b.recv())
                .await
                .unwrap()
                .unwrap(),
            NetEvent::PeerJoined { peer, .. } if peer == a_id
        ) {}

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private-host-name.flac");
        let bytes = vec![0x9a; PERSONAL_MEDIA_CHUNK_BYTES.saturating_add(31)];
        std::fs::write(&path, &bytes).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let transfer_id = [0x45; 16];
        cmd_a
            .send(NetCmd::PublishPersonalMedia {
                ws: 45,
                user_public_key: [0xA6; 32],
                target: b_id,
                transfer_id,
                file_name: "song.flac".into(),
                total_bytes: u64::try_from(bytes.len()).unwrap(),
                file,
            })
            .await
            .unwrap();

        let mut received = Vec::new();
        loop {
            let event = timeout(Duration::from_secs(10), rx_b.recv())
                .await
                .unwrap()
                .unwrap();
            let NetEvent::PersonalFabric {
                payload: PersonalFabricPayload::MediaTransfer(frame),
                ..
            } = event
            else {
                continue;
            };
            match frame {
                PersonalMediaTransferFrame::Start {
                    transfer_id: id,
                    file_name,
                    total_bytes,
                } => {
                    assert_eq!(id, transfer_id);
                    assert_eq!(file_name, "song.flac");
                    assert_eq!(total_bytes, u64::try_from(bytes.len()).unwrap());
                }
                PersonalMediaTransferFrame::Chunk {
                    transfer_id: id,
                    offset,
                    bytes: chunk,
                } => {
                    assert_eq!(id, transfer_id);
                    assert_eq!(offset, u64::try_from(received.len()).unwrap());
                    received.extend_from_slice(&chunk);
                }
                PersonalMediaTransferFrame::Finish {
                    transfer_id: id,
                    content_hash,
                } => {
                    assert_eq!(id, transfer_id);
                    assert_eq!(content_hash, *blake3::hash(&received).as_bytes());
                    break;
                }
                PersonalMediaTransferFrame::Abort { .. } => panic!("transfer aborted"),
            }
        }
        assert_eq!(received, bytes);

        let mut statuses = Vec::new();
        loop {
            let event = timeout(Duration::from_secs(10), rx_a.recv())
                .await
                .unwrap()
                .unwrap();
            let NetEvent::PersonalMediaTransfer {
                target,
                transfer_id: id,
                sent_bytes,
                total_bytes,
                status,
            } = event
            else {
                continue;
            };
            assert_eq!(target, b_id);
            assert_eq!(id, transfer_id);
            assert!(sent_bytes <= total_bytes);
            statuses.push((status, sent_bytes));
            if status == PersonalMediaTransferStatus::Completed {
                break;
            }
        }
        assert_eq!(
            statuses.first(),
            Some(&(PersonalMediaTransferStatus::Started, 0))
        );
        assert!(statuses.iter().any(|(status, sent)| {
            *status == PersonalMediaTransferStatus::Progress
                && *sent == u64::try_from(bytes.len()).unwrap()
        }));
        assert_eq!(
            statuses.last(),
            Some(&(
                PersonalMediaTransferStatus::Completed,
                u64::try_from(bytes.len()).unwrap()
            ))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn personal_fabric_action_reaches_only_the_exact_target() {
        let (a, lookup_a) = endpoint().await;
        let (b, lookup_b) = endpoint().await;
        let (c, lookup_c) = endpoint().await;
        let a_id = a.id();
        let b_id = b.id();
        let c_id = c.id();
        let b_addr = b.addr();
        let c_addr = c.addr();
        let (cmd_a, _a_task) = Actor::spawn(a, lookup_a);
        let (cmd_b, _b_task) = Actor::spawn(b, lookup_b);
        let (cmd_c, _c_task) = Actor::spawn(c, lookup_c);
        let (event_a, mut rx_a) = mpsc::channel(16);
        let (event_b, mut rx_b) = mpsc::channel(16);
        let (event_c, mut rx_c) = mpsc::channel(16);
        for (cmd, events) in [(&cmd_a, event_a), (&cmd_b, event_b), (&cmd_c, event_c)] {
            cmd.send(NetCmd::JoinWorkspace {
                ws: 44,
                on_event: events,
            })
            .await
            .unwrap();
        }
        cmd_a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
        cmd_a.send(NetCmd::Connect { addr: c_addr }).await.unwrap();

        let mut a_saw_b = false;
        let mut a_saw_c = false;
        while !(a_saw_b && a_saw_c) {
            match timeout(Duration::from_secs(10), rx_a.recv())
                .await
                .unwrap()
                .unwrap()
            {
                NetEvent::PeerJoined { peer, .. } if peer == b_id => a_saw_b = true,
                NetEvent::PeerJoined { peer, .. } if peer == c_id => a_saw_c = true,
                _ => {}
            }
        }
        for receiver in [&mut rx_b, &mut rx_c] {
            loop {
                let event = timeout(Duration::from_secs(10), receiver.recv())
                    .await
                    .unwrap()
                    .unwrap();
                if matches!(event, NetEvent::PeerJoined { peer, .. } if peer == a_id) {
                    break;
                }
            }
        }

        cmd_a
            .send(NetCmd::PublishPersonalFabricTarget {
                ws: 44,
                user_public_key: [0xB4; 32],
                target: b_id,
                payload: PersonalFabricPayload::Action(vec![4, 4]),
            })
            .await
            .unwrap();
        loop {
            let event = timeout(Duration::from_secs(10), rx_b.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(
                event,
                NetEvent::PersonalFabric {
                    payload: PersonalFabricPayload::Action(bytes),
                    ..
                } if bytes == vec![4, 4]
            ) {
                break;
            }
        }
        while let Ok(Some(event)) = timeout(Duration::from_millis(500), rx_c.recv()).await {
            assert!(
                !matches!(
                    event,
                    NetEvent::PersonalFabric {
                        payload: PersonalFabricPayload::Action(_),
                        ..
                    }
                ),
                "personal action leaked to a non-target device"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn direct_workspace_control_input_reaches_only_the_exact_target() {
        let (a, lookup_a) = endpoint().await;
        let (b, lookup_b) = endpoint().await;
        let (c, lookup_c) = endpoint().await;
        let a_id = a.id();
        let b_id = b.id();
        let c_id = c.id();
        let b_addr = b.addr();
        let c_addr = c.addr();
        let (cmd_a, _a_task) = Actor::spawn(a, lookup_a);
        let (cmd_b, _b_task) = Actor::spawn(b, lookup_b);
        let (cmd_c, _c_task) = Actor::spawn(c, lookup_c);
        let (event_a, mut rx_a) = mpsc::channel(16);
        let (event_b, mut rx_b) = mpsc::channel(16);
        let (event_c, mut rx_c) = mpsc::channel(16);
        for (cmd, events) in [(&cmd_a, event_a), (&cmd_b, event_b), (&cmd_c, event_c)] {
            cmd.send(NetCmd::JoinWorkspace {
                ws: 43,
                on_event: events,
            })
            .await
            .unwrap();
        }
        cmd_a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
        cmd_a.send(NetCmd::Connect { addr: c_addr }).await.unwrap();

        let mut a_saw_b = false;
        let mut a_saw_c = false;
        while !(a_saw_b && a_saw_c) {
            match timeout(Duration::from_secs(10), rx_a.recv())
                .await
                .unwrap()
                .unwrap()
            {
                NetEvent::PeerJoined { peer, .. } if peer == b_id => a_saw_b = true,
                NetEvent::PeerJoined { peer, .. } if peer == c_id => a_saw_c = true,
                _ => {}
            }
        }
        for receiver in [&mut rx_b, &mut rx_c] {
            loop {
                let event = timeout(Duration::from_secs(10), receiver.recv())
                    .await
                    .unwrap()
                    .unwrap();
                if matches!(event, NetEvent::PeerJoined { peer, .. } if peer == a_id) {
                    break;
                }
            }
        }

        for _ in 0..2 {
            cmd_a
                .send(NetCmd::PublishWorkspaceControl {
                    ws: 43,
                    msg: NeighbourMsg::RemoteInput {
                        peer: crate::network::behaviour::peer_liveness_handle(&a_id),
                        surface: 7,
                        event: vec![0x51, 0x4f, 0x53],
                    },
                    route: WorkspaceControlRoute::Direct { target: b_id },
                })
                .await
                .unwrap();
        }

        let mut delivered = 0;
        while delivered < 2 {
            let event = timeout(Duration::from_secs(10), rx_b.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(
                event,
                NetEvent::Msg {
                    msg: NeighbourMsg::RemoteInput { surface: 7, .. },
                    ..
                }
            ) {
                delivered += 1;
            }
        }
        while let Ok(Some(event)) = timeout(Duration::from_millis(500), rx_c.recv()).await {
            assert!(
                !matches!(
                    event,
                    NetEvent::Msg {
                        msg: NeighbourMsg::RemoteInput { .. },
                        ..
                    }
                ),
                "direct input leaked to a non-target workspace peer"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn actors_announce_identity_when_a_neighbor_connects() {
        let root_a = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let root_b = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let (key_a, cert_a) = enrol(&root_a, DeviceCapabilities::primary(), "a");
        let (key_b, cert_b) = enrol(&root_b, DeviceCapabilities::primary(), "b");
        let expected_a = cert_a.device_public_key;
        let expected_b = cert_b.device_public_key;
        let (endpoint_a, lookup_a) = identified_endpoint(&key_a).await;
        let (endpoint_b, lookup_b) = identified_endpoint(&key_b).await;
        let addr_b = endpoint_b.addr();
        let identity_a = HostIdentity {
            device_cert: cert_a,
            device_signing_key: key_a,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let identity_b = HostIdentity {
            device_cert: cert_b,
            device_signing_key: key_b,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let (cmd_a, _task_a) = Actor::spawn_reporting_addrs(
            endpoint_a,
            lookup_a,
            Some(identity_a),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (cmd_b, _task_b) = Actor::spawn_reporting_addrs(
            endpoint_b,
            lookup_b,
            Some(identity_b),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (events_a, mut rx_a) = mpsc::channel(16);
        let (events_b, mut rx_b) = mpsc::channel(16);
        cmd_a
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: events_a,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: events_b,
            })
            .await
            .unwrap();
        cmd_a.send(NetCmd::Connect { addr: addr_b }).await.unwrap();

        for (rx, expected) in [(&mut rx_a, expected_b), (&mut rx_b, expected_a)] {
            timeout(Duration::from_secs(4), async {
                loop {
                    if let Some(NetEvent::Msg {
                        msg: NeighbourMsg::IdentityHello { cert, .. },
                        ..
                    }) = rx.recv().await
                        && cert.device_public_key == expected
                    {
                        break;
                    }
                }
            })
            .await
            .expect("identity announcement should not wait for the heartbeat");
        }
    }

    /// Spin up a relay pinned to workspace 42 and admit `a_pub`/`b_pub` to
    /// it. The returned `TempDir` must be kept alive for as long as the
    /// relay is used — dropping it deletes the on-disk state root.
    async fn spawn_pinned_relay_for_two_members(
        root: &ed25519_dalek::SigningKey,
        a_pub: [u8; 32],
        b_pub: [u8; 32],
    ) -> (Relay, tempfile::TempDir, DeviceCert) {
        let (host_key, host_cert) = enrol(root, DeviceCapabilities::host(), "host");
        let state = tempfile::tempdir().unwrap();
        let config = RelayConfig {
            state_root: state.path().to_path_buf(),
            workspaces: vec![WorkspaceBinding {
                id: 42,
                pin: true,
                replication: ReplicationIntent::FullWorkspace,
                admitted_users: Vec::new(),
            }],
            ..RelayConfig::default()
        };
        let relay = Relay::start(
            config,
            RelayIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: host_key,
                device_cert: host_cert.clone(),
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        relay
            .set_workspace_membership(42, true, vec![a_pub, b_pub], state.path().to_path_buf())
            .await
            .unwrap();
        (relay, state, host_cert)
    }

    /// Exercise the exact scheduled enrolled-home-peer route, rather than a
    /// direct `RelayClient` unit call. In particular, this protects against an
    /// innocuous-looking refactor that drops `HomePeerExpectation` before the
    /// relay dial.
    #[allow(
        clippy::too_many_lines,
        reason = "the enrolled-host integration transcript needs setup, relay, and rejection assertions together"
    )]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn scheduled_enrolled_host_rejects_wrong_root_device_and_revocation() {
        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let other_root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let (client_key, client_cert) = enrol(&root, DeviceCapabilities::primary(), "client");
        let (host_key, host_cert) = enrol(&root, DeviceCapabilities::host(), "home");
        let (_, wrong_device_cert) = enrol(&root, DeviceCapabilities::host(), "other-home");
        let state = tempfile::tempdir().unwrap();
        let relay = Relay::start(
            RelayConfig {
                state_root: state.path().to_path_buf(),
                workspaces: vec![WorkspaceBinding {
                    id: 42,
                    pin: true,
                    replication: ReplicationIntent::FullWorkspace,
                    admitted_users: Vec::new(),
                }],
                ..RelayConfig::default()
            },
            RelayIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: host_key,
                device_cert: host_cert.clone(),
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        relay
            .set_workspace_membership(
                42,
                true,
                vec![client_key.verifying_key().to_bytes()],
                state.path().to_path_buf(),
            )
            .await
            .unwrap();

        let attempt =
            |expectation: HomePeerExpectation,
             revocations: Vec<tilo_user_identity::RevocationEntry>| {
                let relay_addr = relay.node_addr();
                let client_key = client_key.clone();
                let client_cert = client_cert.clone();
                async move {
                    let (endpoint, _) = identified_endpoint(&client_key).await;
                    let identity = HostIdentity {
                        device_cert: client_cert,
                        device_signing_key: client_key,
                        workspace_device_certs: HashMap::new(),
                        workspace_revocations: HashMap::from([(42, revocations)]),
                        identity_paths: None,
                    };
                    let (tx, mut rx) = mpsc::channel(2);
                    let mut connecting = HashSet::new();
                    let mut discoveries = HashMap::new();
                    schedule_host_topic(
                        &endpoint,
                        &identity,
                        42,
                        relay_addr,
                        Some(expectation),
                        &tx,
                        &HashMap::new(),
                        &mut connecting,
                        &mut discoveries,
                    );
                    let rejected = matches!(
                        timeout(Duration::from_secs(5), rx.recv()).await,
                        Ok(Some(InternalEvent::HostConnected { result, .. })) if result.is_err()
                    );
                    for (_, task) in discoveries {
                        task.abort();
                    }
                    endpoint.close().await;
                    rejected
                }
            };
        let host_device = host_cert.device_public_key;
        assert!(
            attempt(
                HomePeerExpectation {
                    user_public_key: other_root.verifying_key().to_bytes(),
                    device_public_key: host_device,
                },
                Vec::new(),
            )
            .await
        );
        assert!(
            attempt(
                HomePeerExpectation {
                    user_public_key: root.verifying_key().to_bytes(),
                    device_public_key: wrong_device_cert.device_public_key,
                },
                Vec::new(),
            )
            .await
        );
        let mut revocation = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: host_device,
            revoked_at_ns: 1,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, &root).unwrap();
        assert!(
            attempt(
                HomePeerExpectation {
                    user_public_key: root.verifying_key().to_bytes(),
                    device_public_key: host_device,
                },
                vec![revocation],
            )
            .await
        );
    }

    /// Spawn two actors, join workspace 42 on each, connect both to `relay`,
    /// and block until both report `HostReady`. Returns each actor's
    /// command sender and event receiver.
    #[allow(
        clippy::too_many_arguments,
        reason = "each peer's endpoint, lookup and identity, plus the shared relay and cadence"
    )]
    async fn join_workspace_and_await_host_ready(
        a: Endpoint,
        lookup_a: MemoryLookup,
        identity_a: HostIdentity,
        b: Endpoint,
        lookup_b: MemoryLookup,
        identity_b: HostIdentity,
        relay: &Relay,
        heartbeat: Duration,
    ) -> (
        mpsc::Sender<NetCmd>,
        mpsc::Receiver<NetEvent>,
        mpsc::Sender<NetCmd>,
        mpsc::Receiver<NetEvent>,
    ) {
        let (cmd_a, _task_a) = Actor::spawn_with_heartbeat(
            a,
            lookup_a,
            Some(identity_a),
            Arc::new(Mutex::new(Vec::new())),
            heartbeat,
        );
        let (cmd_b, _task_b) = Actor::spawn_with_heartbeat(
            b,
            lookup_b,
            Some(identity_b),
            Arc::new(Mutex::new(Vec::new())),
            heartbeat,
        );
        let (event_a, mut rx_a) = mpsc::channel(16);
        let (event_b, mut rx_b) = mpsc::channel(16);
        cmd_a
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: event_a,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: event_b,
            })
            .await
            .unwrap();
        cmd_a
            .send(NetCmd::ConnectHost {
                addr: relay.node_addr(),
                expectation: None,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::ConnectHost {
                addr: relay.node_addr(),
                expectation: None,
            })
            .await
            .unwrap();
        for rx in [&mut rx_a, &mut rx_b] {
            loop {
                let event = timeout(Duration::from_secs(15), rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
                if matches!(event, NetEvent::HostReady { .. }) {
                    break;
                }
            }
        }
        (cmd_a, rx_a, cmd_b, rx_b)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_actors_exchange_through_authenticated_workspace_host() {
        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
        let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
        let (relay, _state, _host_cert) = spawn_pinned_relay_for_two_members(
            &root,
            a_key.verifying_key().to_bytes(),
            b_key.verifying_key().to_bytes(),
        )
        .await;

        let (a, lookup_a) = identified_endpoint(&a_key).await;
        let (b, lookup_b) = identified_endpoint(&b_key).await;
        let identity_a = HostIdentity {
            device_cert: a_cert,
            device_signing_key: a_key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let identity_b = HostIdentity {
            device_cert: b_cert,
            device_signing_key: b_key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let (cmd_a, _rx_a, _cmd_b, mut rx_b) = join_workspace_and_await_host_ready(
            a,
            lookup_a,
            identity_a,
            b,
            lookup_b,
            identity_b,
            &relay,
            HEARTBEAT_INTERVAL,
        )
        .await;

        cmd_a
            .send(NetCmd::Publish {
                ws: 42,
                msg: NeighbourMsg::SyncReq {
                    since: vec![],
                    room: "hosted".into(),
                    invite_id: None,
                },
            })
            .await
            .unwrap();
        loop {
            let event = timeout(Duration::from_secs(15), rx_b.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(
                event,
                NetEvent::Msg {
                    msg: NeighbourMsg::SyncReq { .. },
                    ..
                }
            ) {
                break;
            }
        }
    }

    /// Two devices that share only a workspace host, with no direct route,
    /// still exchange remote-action frames: the same bytes, delivered to the
    /// addressed device attributed to the sender the host authenticated, and
    /// a byte-identical retry is delivered again rather than deduplicated.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn remote_action_frames_fall_back_to_the_workspace_host() {
        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
        let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
        let (relay, _state, _host_cert) = spawn_pinned_relay_for_two_members(
            &root,
            a_key.verifying_key().to_bytes(),
            b_key.verifying_key().to_bytes(),
        )
        .await;
        let (a, lookup_a) = identified_endpoint(&a_key).await;
        let (b, lookup_b) = identified_endpoint(&b_key).await;
        let a_id = a.id();
        let b_id = b.id();
        let identity = |key: ed25519_dalek::SigningKey, cert| HostIdentity {
            device_cert: cert,
            device_signing_key: key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let (cmd_a, _rx_a, _cmd_b, mut rx_b) = join_workspace_and_await_host_ready(
            a,
            lookup_a,
            identity(a_key, a_cert),
            b,
            lookup_b,
            identity(b_key, b_cert),
            &relay,
            HEARTBEAT_INTERVAL,
        )
        .await;

        let frame = br#"{"version":1}"#.to_vec();
        for attempt in 0..2 {
            cmd_a
                .send(NetCmd::PublishRemoteAction {
                    ws: 42,
                    target: b_id,
                    msg: NeighbourMsg::RemoteAction {
                        frame: frame.clone(),
                    },
                })
                .await
                .unwrap();
            let (peer, received) = timeout(Duration::from_secs(15), async {
                loop {
                    if let Some(NetEvent::Msg {
                        peer,
                        msg: NeighbourMsg::RemoteAction { frame },
                        ..
                    }) = rx_b.recv().await
                    {
                        return (peer, frame);
                    }
                }
            })
            .await
            .unwrap_or_else(|_| panic!("relayed attempt {attempt} was not delivered"));
            assert_eq!(peer, a_id, "attributed to the authenticated sender");
            assert_eq!(received, frame, "the host carries the frame untouched");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn failed_host_address_refresh_retains_files_route_until_explicit_removal() {
        const TEST_HEARTBEAT: Duration = Duration::from_millis(100);
        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
        let a_public = a_key.verifying_key().to_bytes();
        let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
        let (relay, _state, host_cert) =
            spawn_pinned_relay_for_two_members(&root, a_public, b_key.verifying_key().to_bytes())
                .await;
        let (a, lookup_a) = identified_endpoint(&a_key).await;
        let (b, lookup_b) = identified_endpoint(&b_key).await;
        let identity = |key, cert| HostIdentity {
            device_cert: cert,
            device_signing_key: key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let (cmd_a, mut rx_a, _cmd_b, _rx_b) = join_workspace_and_await_host_ready(
            a,
            lookup_a,
            identity(a_key, a_cert),
            b,
            lookup_b,
            identity(b_key, b_cert),
            &relay,
            TEST_HEARTBEAT,
        )
        .await;

        // Upgrade the existing host route to an enrolled home-peer route.
        // The subsequent address refresh must retain that route's Files
        // endpoint; it is a reconnect, not a home-peer removal.
        let expectation = HomePeerExpectation {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: host_cert.device_public_key,
        };
        cmd_a
            .send(NetCmd::ConnectHost {
                addr: relay.node_addr(),
                expectation: Some(expectation.clone()),
            })
            .await
            .unwrap();
        loop {
            let event = timeout(Duration::from_secs(15), rx_a.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(event, NetEvent::HostReady { .. }) {
                break;
            }
        }

        // A replacement address that cannot respond must not tear down the
        // already-discovered Files endpoint. The old actor implementation
        // called the permanent-removal path before attempting this dial.
        let refreshed =
            EndpointAddr::new(relay.node_addr().id).with_ip_addr("192.0.2.1:9".parse().unwrap());
        cmd_a
            .send(NetCmd::ConnectHost {
                addr: refreshed,
                expectation: Some(expectation),
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !noted_content_peer_removals()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&relay.node_addr().id),
            "an address refresh must retain the enrolled peer's Files route"
        );

        cmd_a
            .send(NetCmd::DisconnectHost {
                id: relay.node_addr().id,
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if noted_content_peer_removals()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .contains(&relay.node_addr().id)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("explicit removal must revoke enrolled Files routes");
        for _ in 0..100 {
            if !relay.is_subscribed(42, &a_public).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!relay.is_subscribed(42, &a_public).await);
        // Several retry rounds pass; any of them would redial a host it still held.
        tokio::time::sleep(TEST_HEARTBEAT * 4).await;
        assert!(
            !relay.is_subscribed(42, &a_public).await,
            "a removed host must not be redialled by the heartbeat retry loop"
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the continuity replay integration transcript intentionally retains both initial and restart actors"
    )]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn host_replays_the_latest_personal_continuity_checkpoint_after_close() {
        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
        let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
        let (relay, _state, _host_cert) = spawn_pinned_relay_for_two_members(
            &root,
            a_key.verifying_key().to_bytes(),
            b_key.verifying_key().to_bytes(),
        )
        .await;
        let (a, lookup_a) = identified_endpoint(&a_key).await;
        let (b, lookup_b) = identified_endpoint(&b_key).await;
        let b_restart_key = b_key.clone();
        let b_restart_cert = b_cert.clone();
        let identity = |key, cert| HostIdentity {
            device_cert: cert,
            device_signing_key: key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let (cmd_a, _rx_a, _cmd_b, mut rx_b) = join_workspace_and_await_host_ready(
            a,
            lookup_a,
            identity(a_key, a_cert),
            b,
            lookup_b,
            identity(b_key, b_cert),
            &relay,
            HEARTBEAT_INTERVAL,
        )
        .await;
        let checkpoint = |bytes| NeighbourMsg::PersonalContinuityCheckpoint {
            user_public_key: root.verifying_key().to_bytes(),
            publisher: 7,
            snapshot: bytes,
        };
        cmd_a
            .send(NetCmd::PublishCheckpoint {
                ws: 42,
                msg: checkpoint(vec![1]),
            })
            .await
            .unwrap();
        loop {
            if let Some(NetEvent::Msg {
                msg: NeighbourMsg::PersonalContinuityCheckpoint { snapshot, .. },
                ..
            }) = timeout(Duration::from_secs(10), rx_b.recv()).await.unwrap()
            {
                assert_eq!(snapshot, vec![1]);
                break;
            }
        }
        cmd_a
            .send(NetCmd::PublishCheckpoint {
                ws: 42,
                msg: checkpoint(Vec::new()),
            })
            .await
            .unwrap();
        loop {
            if let Some(NetEvent::Msg {
                msg: NeighbourMsg::PersonalContinuityCheckpoint { snapshot, .. },
                ..
            }) = timeout(Duration::from_secs(10), rx_b.recv()).await.unwrap()
            {
                assert!(snapshot.is_empty(), "closure replaces live checkpoint");
                break;
            }
        }

        // A fresh actor with B's durable device identity models a shell
        // restart while A is offline. The host must replay the retained
        // closure baseline rather than resurrecting A's earlier checkpoint.
        let (b_restarted, lookup_restarted) = identified_endpoint(&b_restart_key).await;
        let (cmd_restarted, _task_restarted) = Actor::spawn_reporting_addrs(
            b_restarted,
            lookup_restarted,
            Some(identity(b_restart_key, b_restart_cert)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (events, mut replay_events) = mpsc::channel(16);
        cmd_restarted
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: events,
            })
            .await
            .unwrap();
        cmd_restarted
            .send(NetCmd::ConnectHost {
                addr: relay.node_addr(),
                expectation: None,
            })
            .await
            .unwrap();
        loop {
            if let Some(NetEvent::Msg {
                msg: NeighbourMsg::PersonalContinuityCheckpoint { snapshot, .. },
                ..
            }) = timeout(Duration::from_secs(10), replay_events.recv())
                .await
                .unwrap()
            {
                assert!(snapshot.is_empty(), "restarted B receives retained closure");
                break;
            }
        }
    }
}
