//! Iroh-gossip actor for workspace collaboration.
//!
//! Direct paths, NAT traversal and stateless relay fallback are selected by
//! iroh. Tilo only names authenticated endpoint ids and workspace topics.

pub(crate) mod governor_lane;
pub(crate) mod native_ingress;

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
use tilo_relay::{Inbound as HostInbound, RelayClient, RelayCoeditSession};
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
pub use tilo_identity_host::custody::HostIdentity;

/// Enrollment-bound identity expected behind a distinct relay service key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomePeerExpectation {
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
}

/// A compositor-loop wake handed to the actor ([`NetCmd::SetWake`]).
#[derive(Clone)]
pub struct LoopWake(pub crate::session_runtime::Wake);
impl std::fmt::Debug for LoopWake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoopWake")
    }
}

#[derive(Debug)]
pub enum NetCmd {
    /// Ask `ws`'s workspace hosts to replay the room's retained history, as
    /// for a first subscription: a local app joined a record whose earlier
    /// traffic this device dropped because nothing held it then.
    Replay {
        ws: WorkspaceId,
    },
    /// Publish one canonical live frame (presence, previews) on `ws`'s
    /// workspace hosts' live planes, where browsers and agents meet this
    /// device. Lossy.
    PublishLive {
        ws: WorkspaceId,
        frame: Vec<u8>,
    },
    NativeConnect(native_ingress::Connect),
    /// Ask a governed room's governor, on its room's native relay session.
    GovernorAction {
        ws: u64,
        room: uuid::Uuid,
        request: Box<governor_lane::Request>,
    },
    NativePublish {
        ws: u64,
    },
    /// Wake the compositor loop when legacy-window traffic arrives: an idle
    /// headless loop otherwise sleeps until something else wakes it.
    SetWake(LoopWake),
    /// This device's own topology entries (`SyncResp`) for a room another
    /// device governs, to its workspace hosts as a retained delta: the
    /// governor admits them within this device's registered namespace.
    PublishHostedTopology {
        ws: u64,
        msg: NeighbourMsg,
    },
    NativeMedia(super::native_media::Command),
    NativeEnrollment(super::invite::enrollment_driver::Command),
    /// Ask the room authority of the relay at `relay`, as `certificate`, from
    /// this device's own endpoint: the relay accepts only the certificate of
    /// the endpoint it authenticated.
    RoomAdmin {
        relay: EndpointAddr,
        certificate: DeviceCert,
        command: tilo_relay::rooms::RoomCommand,
        reply: std::sync::mpsc::SyncSender<Result<tilo_relay::rooms::RoomReply, String>>,
    },
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
        transfer: NearbyTransferCheckpoint,
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
    AttachActionBroker {
        queue: crate::shell::peer_actions::PeerActionQueue,
        host: uuid::Uuid,
        rooms: Vec<uuid::Uuid>,
    },
}

/// One immutable Nearby spool object and the receiver-confirmed checkpoint
/// its stream resumes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NearbyTransferCheckpoint {
    pub transfer_id: [u8; 16],
    pub request_id: [u8; 16],
    pub total_bytes: u64,
    pub resume_offset: u64,
    /// BLAKE3 of the first `resume_offset` bytes, as the receiver holds them.
    pub prefix_hash: [u8; 32],
    pub content_hash: [u8; 32],
}

/// Concrete data-plane route for one workspace-control session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceControlRoute {
    Direct { target: EndpointId },
    Relayed { via: EndpointId },
}

#[derive(Debug, Clone)]
pub enum NetEvent {
    /// A live frame another live member of `ws` published through a
    /// workspace host, attributed to the device the host authenticated.
    Live {
        ws: WorkspaceId,
        from: [u8; 32],
        frame: Vec<u8>,
    },
    NativeInput(native_ingress::Input),
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
    HostReady {
        ws: WorkspaceId,
        peer: PeerId,
    },
    Msg {
        ws: WorkspaceId,
        peer: PeerId,
        membership_epoch: Option<MembershipEpoch>,
        /// A workspace host replayed it from its retained, host-ordered
        /// history before answering the subscription: old by nature.
        replayed: bool,
        msg: NeighbourMsg,
    },
    /// A document this device published earlier, which a workspace host
    /// replayed from its retained history: how a device that lost its local
    /// state gets its own changes back. Live echoes never arrive here.
    OwnReplay {
        ws: WorkspaceId,
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
    NativeHostConnected {
        ws: u64,
        serial: u64,
        hint: Box<tilo_peer_state::join::TransportHint>,
        certificate: DeviceCert,
        continuity: u64,
        result: Box<Result<RelayCoeditSession, String>>,
    },
    ActionClosed {
        ws: WorkspaceId,
        peer: EndpointId,
        session: std::sync::Weak<RelayCoeditSession>,
    },
    Topic {
        ws: WorkspaceId,
        event: GossipEvent,
    },
    Host {
        ws: WorkspaceId,
        session: std::sync::Weak<RelayCoeditSession>,
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
    NativeInput(native_ingress::Input),
    Ready,
    Voice(tilo_relay::coedit::VoiceEvent),
    Live {
        from: [u8; 32],
        frame: Vec<u8>,
    },
    Message {
        author: EndpointId,
        fingerprint: [u8; 32],
        /// Sent before the host's `Ready`: part of its replay.
        replayed: bool,
        msg: NeighbourMsg,
    },
    Closed,
}

struct TopicRuntime {
    sender: GossipSender,
    pump: tokio::task::JoinHandle<()>,
}

struct HostRuntime {
    native_scope: Option<Arc<native_ingress::Scope>>,
    /// The private lane to a governed room's governor, opened on first use.
    governor: Option<governor_lane::Lane>,
    actions: Option<tokio::task::JoinHandle<()>>,
    session: Arc<RelayCoeditSession>,
    pump: tokio::task::JoinHandle<()>,
    /// Set while the host writes a replay, until its `Ready`.
    replaying: Arc<std::sync::atomic::AtomicBool>,
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

/// The channel governance a device published to one room: each message's
/// retention channel and its encoded frame.
type GovernanceLog = VecDeque<([u8; 32], Vec<u8>)>;

/// Mutable state shared across every `select!` branch of [`run`]'s event
/// loop, bundled into a struct so each branch's handling can be split into
/// its own method instead of one long function body.
struct ActorState {
    endpoint: Endpoint,
    lookup: MemoryLookup,
    host_identity: Option<HostIdentity>,
    native_media: super::native_media::Registry,
    native_enrollment: super::invite::enrollment_driver::Registry,
    native_routes: native_ingress::Routes,
    listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
    gossip: Gossip,
    internal_tx: mpsc::Sender<InternalEvent>,
    topics: HashMap<WorkspaceId, TopicRuntime>,
    subscribers: HashMap<WorkspaceId, mpsc::Sender<NetEvent>>,
    peers: HashMap<EndpointId, EndpointAddr>,
    hosts: HashMap<EndpointId, (EndpointAddr, Option<HomePeerExpectation>)>,
    host_sessions: HashMap<(WorkspaceId, EndpointId), HostRuntime>,
    action_broker: Option<(
        crate::shell::peer_actions::PeerActionQueue,
        uuid::Uuid,
        HashMap<WorkspaceId, uuid::Uuid>,
    )>,
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
    /// When each recent remote-input message arrived (see [`mark_new_input`]).
    recent_input: HashMap<(WorkspaceId, [u8; 32]), Instant>,
    /// Channel governance this device published to each room, by retention
    /// channel (see [`remember_governance`]).
    governance: HashMap<WorkspaceId, GovernanceLog>,
    /// Retained state published to a room while none of its hosts had a
    /// session (see [`ActorState::hold_for_host`]).
    host_backlog: HashMap<WorkspaceId, HostBacklog>,
    wake: Option<crate::session_runtime::Wake>,
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

/// The exact certified device a personal-fabric stream dials, and the
/// personal workspace and user key its frames are authenticated under.
struct PersonalStreamRoute {
    endpoint: Endpoint,
    addr: EndpointAddr,
    target: EndpointId,
    workspace: WorkspaceId,
    user_public_key: [u8; 32],
}

/// One local file offered to a personal device under `transfer_id`.
struct PersonalMediaUpload {
    transfer_id: [u8; 16],
    file_name: String,
    total_bytes: u64,
    file: std::fs::File,
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

impl Drop for HostRuntime {
    fn drop(&mut self) {
        if let Some(scope) = &self.native_scope {
            scope.close();
        }
        self.pump.abort();
        if let Some(task) = &self.actions {
            task.abort();
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
        if self
            .peers_seen
            .get(&(ws, peer))
            .is_none_or(|membership| membership.epoch != epoch)
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
            Some(NetCmd::NativeMedia(command)) => {
                let sessions = &self.host_sessions;
                self.native_media
                    .handle(command, self.host_identity.as_ref(), &|ws| {
                        native_ingress::native_session(sessions, ws)
                    });
            }
            Some(NetCmd::NativeConnect(connect)) => self.native_connect(connect),
            Some(NetCmd::GovernorAction { ws, room, request }) => {
                self.governor_action(ws, room, request);
            }
            Some(NetCmd::NativePublish { ws }) => self.drive_native_publication(ws),
            Some(NetCmd::NativeEnrollment(command)) => self.native_enrollment.handle(
                command,
                self.endpoint.clone(),
                self.host_identity.as_ref(),
            ),
            Some(NetCmd::RoomAdmin {
                relay,
                certificate,
                command,
                reply,
            }) => {
                let client =
                    tilo_relay::rooms::RoomAdminClient::new(self.endpoint.clone(), certificate);
                tokio::spawn(async move {
                    let answer = client.send(relay, command).await;
                    let _ = reply.send(answer.map_err(|error| error.to_string()));
                });
            }
            Some(NetCmd::AttachActionBroker { queue, host, rooms }) => {
                let rooms: HashMap<_, _> = rooms
                    .into_iter()
                    .filter_map(|room| {
                        tilo_relay_protocol::room_workspace_id(&room.to_string())
                            .ok()
                            .map(|ws| (ws, room))
                    })
                    .collect();
                self.action_broker = Some((queue, host, rooms));
                // Reconnect existing state-only subscriptions once. There is
                // never a competing same-device private subscriber.
                self.host_sessions.clear();
            }
            Some(NetCmd::JoinWorkspace { ws, on_event }) => {
                self.handle_join_workspace(ws, on_event).await;
            }
            Some(NetCmd::LeaveWorkspace { ws }) => {
                self.native_routes.remove(ws);
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
            Some(NetCmd::Replay { ws }) => {
                for ((session_ws, _), runtime) in &self.host_sessions {
                    if *session_ws == ws && runtime.native_scope.is_none() {
                        runtime
                            .replaying
                            .store(true, std::sync::atomic::Ordering::Release);
                        let _ = runtime.session.resubscribe();
                    }
                }
            }
            Some(NetCmd::PublishLive { ws, frame }) => {
                for ((session_ws, _), runtime) in &self.host_sessions {
                    if *session_ws == ws {
                        let _ = runtime.session.publish_live(frame.clone());
                    }
                }
            }
            Some(NetCmd::Publish { ws, msg }) => {
                remember_governance(&mut self.governance, ws, &msg);
                if self.native_routes.contains(ws)
                    && matches!(
                        &msg,
                        NeighbourMsg::AppDocument { .. }
                            | NeighbourMsg::SyncReq { .. }
                            | NeighbourMsg::SyncResp { .. }
                    )
                {
                    if matches!(msg, NeighbourMsg::AppDocument { .. }) {
                        self.drive_native_publication(ws);
                    }
                    return true;
                }
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
                    self.hold_for_host(ws, &msg, false);
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
            Some(NetCmd::SetWake(LoopWake(wake))) => self.wake = Some(wake),
            Some(NetCmd::PublishHostedTopology { ws, msg }) => {
                if matches!(msg, NeighbourMsg::SyncResp { .. }) {
                    publish(
                        &self.topics,
                        &self.host_sessions,
                        ws,
                        msg,
                        false,
                        false,
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
                if self.native_routes.contains(ws) {
                    return true;
                }
                self.hold_for_host(ws, &msg, true);
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
                transfer,
                file,
            }) => {
                self.publish_nearby_transfer(ws, user_public_key, target, transfer, file);
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
                let mut routes = HostRouteScheduler {
                    endpoint: &self.endpoint,
                    identity,
                    internal_tx: &self.internal_tx,
                    sessions: &self.host_sessions,
                    connecting: &mut self.connecting_hosts,
                    content_discovery_tasks: &mut self.content_discovery_tasks,
                };
                for ws in self.topics.keys().copied() {
                    if self.native_routes.contains(ws) {
                        continue;
                    }
                    routes.schedule(
                        ws,
                        addr.clone(),
                        expectation.clone(),
                        self.action_broker
                            .as_ref()
                            .is_some_and(|(_, _, rooms)| rooms.contains_key(&ws)),
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
        let route = PersonalStreamRoute {
            endpoint: self.endpoint.clone(),
            addr,
            target,
            workspace: ws,
            user_public_key,
        };
        let upload = PersonalMediaUpload {
            transfer_id,
            file_name,
            total_bytes,
            file,
        };
        let internal = self.internal_tx.clone();
        report(PersonalMediaTransferStatus::Started, 0);
        tokio::spawn(async move {
            let outcome = stream_personal_media(
                route,
                upload,
                internal.clone(),
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

    fn publish_nearby_transfer(
        &self,
        ws: WorkspaceId,
        user_public_key: [u8; 32],
        target: EndpointId,
        transfer: NearbyTransferCheckpoint,
        file: std::fs::File,
    ) {
        let NearbyTransferCheckpoint {
            transfer_id,
            request_id,
            total_bytes,
            resume_offset,
            ..
        } = transfer;
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
        let route = PersonalStreamRoute {
            endpoint: self.endpoint.clone(),
            addr,
            target,
            workspace: ws,
            user_public_key,
        };
        report(PersonalMediaTransferStatus::Started, resume_offset);
        tokio::spawn(async move {
            let sent = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(resume_offset));
            let result = stream_nearby_transfer(
                route,
                transfer,
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
        if !self.native_routes.contains(ws)
            && let Some(identity) = self.host_identity.as_ref()
        {
            let mut routes = HostRouteScheduler {
                endpoint: &self.endpoint,
                identity,
                internal_tx: &self.internal_tx,
                sessions: &self.host_sessions,
                connecting: &mut self.connecting_hosts,
                content_discovery_tasks: &mut self.content_discovery_tasks,
            };
            for (addr, expectation) in self.hosts.values().cloned() {
                routes.schedule(
                    ws,
                    addr.clone(),
                    expectation,
                    self.action_broker
                        .as_ref()
                        .is_some_and(|(_, _, rooms)| rooms.contains_key(&ws)),
                );
            }
        }
    }

    /// Handle one [`InternalEvent`]. Returns `false` once the internal
    /// channel has closed, so [`run`]'s loop should stop.
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive dispatcher over every internal actor event"
    )]
    async fn handle_internal(&mut self, internal: Option<InternalEvent>) -> bool {
        match internal {
            Some(InternalEvent::ActionClosed { ws, peer, session }) => {
                if self.host_sessions.get(&(ws, peer)).is_some_and(|runtime| {
                    std::sync::Weak::ptr_eq(&session, &Arc::downgrade(&runtime.session))
                }) {
                    self.handle_host_event(ws, peer, HostEvent::Closed).await;
                }
            }
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
            Some(InternalEvent::NativeHostConnected {
                ws,
                serial,
                hint,
                certificate,
                continuity,
                result,
            }) => self.native_host_connected(ws, serial, *hint, certificate, continuity, result),
            Some(InternalEvent::Host {
                ws,
                peer,
                session,
                event,
            }) => {
                if self.host_sessions.get(&(ws, peer)).is_some_and(|runtime| {
                    std::sync::Weak::ptr_eq(&session, &Arc::downgrade(&runtime.session))
                }) {
                    self.handle_host_event(ws, peer, event).await;
                }
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
                    replayed: false,
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
                        replayed: false,
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
        if self.native_routes.contains(ws) {
            return;
        }
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
                tracing::info!(ws, host = %peer, "workspace host session started");
                let session = Arc::new(session);
                self.announce_identity_to_host(ws, &session);
                for (channel, bytes) in self.governance.get(&ws).into_iter().flatten() {
                    if let Err(error) = session.publish_checkpoint(*channel, bytes.clone()) {
                        tracing::warn!(ws, %error, "could not send channel governance to the workspace host");
                    }
                }
                for (channel, retention, bytes) in self.host_backlog.remove(&ws).unwrap_or_default()
                {
                    let sent = match retention {
                        Retention::Checkpoint => session.publish_checkpoint(channel, bytes),
                        Retention::Delta => session.publish_retained_delta(channel, bytes),
                    };
                    if let Err(error) = sent {
                        tracing::warn!(ws, %error, "could not send held collaboration state to the workspace host");
                    }
                }
                if let Err(error) = session.live_join() {
                    tracing::warn!(ws, %error, "could not join the workspace host's live plane");
                }
                let replaying = Arc::new(std::sync::atomic::AtomicBool::new(true));
                let pump = spawn_host_pump(
                    ws,
                    peer,
                    Arc::clone(&session),
                    Arc::clone(&replaying),
                    self.internal_tx.clone(),
                    None,
                );
                self.host_sessions.insert(
                    (ws, peer),
                    HostRuntime {
                        replaying,
                        native_scope: None,
                        governor: None,
                        actions: self
                            .action_broker
                            .as_ref()
                            .and_then(|(queue, host, rooms)| {
                                // State-only/pinned compatibility and relay-role
                                // certificates never advertise an effect endpoint.
                                if self
                                    .hosts
                                    .get(&peer)
                                    .and_then(|(_, expected)| expected.as_ref())
                                    .is_none()
                                    || self.host_identity.as_ref().is_none_or(|identity| {
                                        identity.material_for(ws).0.capabilities.can_act_as_host
                                    })
                                {
                                    return None;
                                }
                                let room = *rooms.get(&ws)?;
                                let (queue, host, session, tx) = (
                                    queue.clone(),
                                    *host,
                                    session.clone(),
                                    self.internal_tx.clone(),
                                );
                                Some(tokio::spawn(async move {
                                    let generation = Arc::downgrade(&session);
                                    let _ = queue.serve(session, room, host).await;
                                    let _ = tx
                                        .send(InternalEvent::ActionClosed {
                                            ws,
                                            peer,
                                            session: generation,
                                        })
                                        .await;
                                }))
                            }),
                        session,
                        pump,
                    },
                );
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
            HostEvent::NativeInput(input) => Some(NetEvent::NativeInput(input)),
            HostEvent::Ready => {
                self.request_native_refresh(ws);
                Some(NetEvent::HostReady { ws, peer })
            }
            HostEvent::Live { from, frame } => Some(NetEvent::Live { ws, from, frame }),
            HostEvent::Voice(event) => {
                let session = self
                    .host_sessions
                    .get(&(ws, peer))
                    .map(|runtime| runtime.session.clone());
                self.native_media.voice(ws, &event, session.as_deref());
                None
            }
            HostEvent::Message {
                author,
                replayed: true,
                msg: msg @ NeighbourMsg::AppDocument { .. },
                ..
            } if self.is_own_device(&author) => Some(NetEvent::OwnReplay { ws, msg }),
            HostEvent::Message { author, .. } if self.is_own_device(&author) => {
                // The host replays retained state to every subscriber,
                // including what this device published earlier; gossip never
                // delivers a device its own messages, and nothing downstream
                // holds this device's own certificate to authenticate them.
                // Only its replayed documents come back (above).
                None
            }
            HostEvent::Message {
                author,
                fingerprint,
                replayed,
                msg,
            } => {
                if matches!(
                    msg,
                    NeighbourMsg::RemoteInput { .. } | NeighbourMsg::IdentityHello { .. }
                ) {
                    tracing::debug!(ws, author = %author, kind = ?std::mem::discriminant(&msg), "host session message");
                }
                // A remote-action retry is byte-identical on purpose; the
                // receiving host deduplicates it by invocation id instead. A
                // replay the host was asked for repeats what this device saw
                // before on purpose too: the broker drops what it holds.
                let new = match msg {
                    NeighbourMsg::RemoteAction { .. } => true,
                    NeighbourMsg::RemoteInput { .. } => {
                        mark_new_input(ws, fingerprint, Instant::now(), &mut self.recent_input)
                    }
                    _ => {
                        mark_new_message(ws, fingerprint, &mut self.recent, &mut self.recent_order)
                    }
                };
                if !new && !replayed {
                    return;
                }
                Some(NetEvent::Msg {
                    ws,
                    peer: author,
                    membership_epoch: None,
                    replayed,
                    msg,
                })
            }
            HostEvent::Closed => {
                tracing::info!(ws, host = %peer, "workspace host session ended; the next heartbeat dials it again");
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
            let legacy = matches!(
                &event,
                NetEvent::Msg {
                    msg: NeighbourMsg::RemoteInput { .. }
                        | NeighbourMsg::SurfaceStreamStart { .. }
                        | NeighbourMsg::SurfaceFrame { .. }
                        | NeighbourMsg::SurfaceStreamEnd { .. },
                    ..
                }
            );
            let _ = tx.send(event).await;
            if legacy && let Some(wake) = &self.wake {
                wake();
            }
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
        self.retry_native_routes();
        let rooms: Vec<_> = self.host_sessions.keys().map(|(ws, _)| *ws).collect();
        for ws in rooms {
            self.request_native_refresh(ws);
            self.drive_native_publication(ws);
        }
        if let Ok(mut out) = self.listen_addrs.lock() {
            out.clear();
            out.push(self.endpoint.addr());
        }
        if let Some(identity) = self.host_identity.as_ref() {
            let mut routes = HostRouteScheduler {
                endpoint: &self.endpoint,
                identity,
                internal_tx: &self.internal_tx,
                sessions: &self.host_sessions,
                connecting: &mut self.connecting_hosts,
                content_discovery_tasks: &mut self.content_discovery_tasks,
            };
            for ws in self.topics.keys().copied() {
                if self.native_routes.contains(ws) {
                    continue;
                }
                for (addr, expectation) in self.hosts.values().cloned() {
                    routes.schedule(
                        ws,
                        addr,
                        expectation,
                        self.action_broker
                            .as_ref()
                            .is_some_and(|(_, _, rooms)| rooms.contains_key(&ws)),
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
            ts: tilo_clock::unix_now_ms(),
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

    /// Whether a host-relayed message was authored by this device: the host
    /// names authors by the device key it authenticated them with.
    fn is_own_device(&self, author: &EndpointId) -> bool {
        self.host_identity.as_ref().is_some_and(|identity| {
            *author.as_bytes() == identity.device_signing_key.verifying_key().to_bytes()
        }) || *author == self.endpoint.id()
    }

    /// Send this device's certificate through a workspace host. Members that
    /// reach this device only through the host (browsers, shells on another
    /// network) bind it to the author the host authenticated, as gossip
    /// neighbours bind it to their direct connection; the host retains it for
    /// members that join later.
    /// Keep `msg` for the room's host when the host should retain it but no
    /// session to a configured host is up: a session is replaced (a pinned
    /// home peer takes over an unpinned dial) or redialled, and what a device
    /// writes meanwhile (a record it creates at launch, say) would otherwise
    /// reach no host, and every later change of it would depend on changes
    /// the host's other devices never get. The next session sends it first.
    fn hold_for_host(&mut self, ws: WorkspaceId, msg: &NeighbourMsg, shell_checkpoint: bool) {
        if self.hosts.is_empty()
            || self
                .host_sessions
                .keys()
                .any(|(session_ws, _)| *session_ws == ws)
        {
            return;
        }
        let (Some((channel, kind)), Ok(bytes)) =
            (retention(msg, shell_checkpoint), postcard::to_allocvec(msg))
        else {
            return;
        };
        let backlog = self.host_backlog.entry(ws).or_default();
        backlog.bytes = backlog.bytes.saturating_add(bytes.len());
        backlog.entries.push_back((channel, kind, bytes));
        while backlog.entries.len() > MAX_HOST_BACKLOG || backlog.bytes > MAX_HOST_BACKLOG_BYTES {
            let Some((_, _, dropped)) = backlog.entries.pop_front() else {
                break;
            };
            backlog.bytes = backlog.bytes.saturating_sub(dropped.len());
            tracing::debug!(
                ws,
                "collaboration state held for the workspace host overflowed; the oldest is dropped"
            );
        }
    }

    fn announce_identity_to_host(&self, ws: WorkspaceId, session: &RelayCoeditSession) {
        let Some(identity) = self.host_identity.as_ref() else {
            return;
        };
        let (cert, revocations) = identity.material_for(ws);
        let hello = NeighbourMsg::IdentityHello {
            cert: Box::new(cert),
            revocations,
        };
        let (Some((channel, _)), Ok(bytes)) =
            (retention(&hello, false), postcard::to_allocvec(&hello))
        else {
            return;
        };
        if let Err(error) = session.publish_checkpoint(channel, bytes) {
            tracing::warn!(ws, %error, "could not send the identity announcement to the workspace host");
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
    clippy::too_many_lines,
    reason = "one audited streaming state machine keeps prefix verification, bounded chunks, progress, and the authenticated finish together"
)]
async fn stream_nearby_transfer(
    route: PersonalStreamRoute,
    transfer: NearbyTransferCheckpoint,
    file: std::fs::File,
    progress: Option<mpsc::Sender<NetEvent>>,
    sent_bytes: std::sync::Arc<std::sync::atomic::AtomicU64>,
) -> Result<(), ()> {
    let PersonalStreamRoute {
        endpoint,
        addr,
        target,
        workspace,
        user_public_key,
    } = route;
    let NearbyTransferCheckpoint {
        transfer_id,
        request_id,
        total_bytes,
        resume_offset,
        prefix_hash,
        content_hash,
    } = transfer;
    // File reads go through the I/O pool, never a network worker: the
    // resumed prefix can be most of a large file.
    let file = Arc::new(file);
    let prefix = Arc::clone(&file);
    let mut hasher = tilo_rt::io::run(move || {
        use std::io::Read as _;
        let mut reader = &*prefix;
        let mut hasher = blake3::Hasher::new();
        let mut verify = vec![0_u8; PERSONAL_MEDIA_CHUNK_BYTES];
        let mut verified = 0_u64;
        while verified < resume_offset {
            let want = usize::try_from(
                resume_offset
                    .saturating_sub(verified)
                    .min(PERSONAL_MEDIA_CHUNK_BYTES as u64),
            )
            .map_err(std::io::Error::other)?;
            let window = verify
                .get_mut(..want)
                .ok_or(std::io::ErrorKind::InvalidInput)?;
            let read = reader.read(window)?;
            let chunk = window
                .get(..read)
                .filter(|chunk| !chunk.is_empty())
                .ok_or(std::io::ErrorKind::UnexpectedEof)?;
            hasher.update(chunk);
            verified = verified.saturating_add(u64::try_from(read).map_err(std::io::Error::other)?);
        }
        Ok(hasher)
    })
    .await
    .map_err(|_| ())?;
    if hasher.finalize().as_bytes() != &prefix_hash {
        return Err(());
    }
    let connection = tokio::time::timeout(
        Duration::from_secs(10),
        endpoint.connect(addr, PERSONAL_FABRIC_ALPN),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    let mut offset = resume_offset;
    while offset < total_bytes {
        let want = usize::try_from(
            total_bytes
                .saturating_sub(offset)
                .min(PERSONAL_MEDIA_CHUNK_BYTES as u64),
        )
        .map_err(|_| ())?;
        let bytes = tilo_rt::io::read_at(Arc::clone(&file), offset, want)
            .await
            .map_err(|_| ())?;
        if bytes.is_empty() {
            return Err(());
        }
        let read = bytes.len();
        hasher.update(&bytes);
        send_nearby_transfer_frame(
            &connection,
            workspace,
            user_public_key,
            PersonalNearbyTransferFrame::Chunk {
                transfer_id,
                request_id,
                offset,
                bytes,
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
    let tail = tilo_rt::io::read_at(Arc::clone(&file), offset, 1)
        .await
        .map_err(|_| ())?;
    if !tail.is_empty() {
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
    clippy::too_many_lines,
    reason = "one bounded transfer: connect, manifest, chunk loop and cancellation share local state"
)]
async fn stream_personal_media(
    route: PersonalStreamRoute,
    upload: PersonalMediaUpload,
    internal: mpsc::Sender<InternalEvent>,
    sent_bytes: std::sync::Arc<std::sync::atomic::AtomicU64>,
    generation: u64,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> PersonalMediaStreamOutcome {
    let PersonalStreamRoute {
        endpoint,
        addr,
        target,
        workspace,
        user_public_key,
    } = route;
    let PersonalMediaUpload {
        transfer_id,
        file_name,
        total_bytes,
        file,
    } = upload;
    let connect = tokio::time::timeout(
        Duration::from_secs(10),
        endpoint.connect(addr, PERSONAL_FABRIC_ALPN),
    );
    let connection = tokio::select! {
        result = connect => match result {
            Ok(Ok(connection)) => connection,
            _ => return PersonalMediaStreamOutcome::Failed,
        },
        () = personal_media_cancelled(&mut cancel) => return PersonalMediaStreamOutcome::Cancelled,
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
        result = start => result.map_or(PersonalMediaStreamOutcome::Failed, |()| PersonalMediaStreamOutcome::Completed),
        () = personal_media_cancelled(&mut cancel) => PersonalMediaStreamOutcome::Cancelled,
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

    // Reads go through the I/O pool, never a network worker.
    let file = Arc::new(file);
    let mut offset = 0_u64;
    let mut last_reported = 0_u64;
    let mut hasher = blake3::Hasher::new();
    let transfer: Result<(), PersonalMediaStreamOutcome> = async {
        while offset < total_bytes {
            let remaining = total_bytes.saturating_sub(offset);
            let want = usize::try_from(remaining.min(PERSONAL_MEDIA_CHUNK_BYTES as u64))
                .map_err(|_| PersonalMediaStreamOutcome::Failed)?;
            let bytes = tokio::select! {
                result = tilo_rt::io::read_at(Arc::clone(&file), offset, want) => result.map_err(|_| PersonalMediaStreamOutcome::Failed)?,
                () = personal_media_cancelled(&mut cancel) => return Err(PersonalMediaStreamOutcome::Cancelled),
            };
            if bytes.is_empty() {
                return Err(PersonalMediaStreamOutcome::Failed);
            }
            let read = bytes.len();
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
                result = send => result.map_err(|()| PersonalMediaStreamOutcome::Failed)?,
                () = personal_media_cancelled(&mut cancel) => return Err(PersonalMediaStreamOutcome::Cancelled),
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
        let trailing = tokio::select! {
            result = tilo_rt::io::read_at(Arc::clone(&file), offset, 1) => result.map_err(|_| PersonalMediaStreamOutcome::Failed)?,
            () = personal_media_cancelled(&mut cancel) => return Err(PersonalMediaStreamOutcome::Cancelled),
        };
        if !trailing.is_empty() {
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
            result = finish => result.map_err(|()| PersonalMediaStreamOutcome::Failed),
            () = personal_media_cancelled(&mut cancel) => Err(PersonalMediaStreamOutcome::Cancelled),
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
    // A changed value and a dropped sender both end the wait.
    let _ = cancel.changed().await;
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
        native_media: super::native_media::Registry::default(),
        native_enrollment: super::invite::enrollment_driver::Registry::default(),
        native_routes: native_ingress::Routes::default(),
        listen_addrs,
        gossip,
        internal_tx,
        topics: HashMap::new(),
        subscribers: HashMap::new(),
        peers: HashMap::new(),
        hosts: HashMap::new(),
        host_sessions: HashMap::new(),
        action_broker: None,
        connecting_hosts: HashSet::new(),
        content_discovery_tasks: HashMap::new(),
        peers_seen: HashMap::new(),
        next_membership_epoch: 0,
        personal_fabric_ingress: HashMap::new(),
        recent: HashSet::new(),
        recent_order: VecDeque::new(),
        recent_input: HashMap::new(),
        governance: HashMap::new(),
        host_backlog: HashMap::new(),
        wake: None,
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

/// Channel governance kept per room for the host sessions that start later.
const MAX_REMEMBERED_GOVERNANCE: usize = 512;

/// Bounds of what one room holds for its host while no session is up.
const MAX_HOST_BACKLOG: usize = 4096;
const MAX_HOST_BACKLOG_BYTES: usize = 32 * 1024 * 1024;

#[derive(Default)]
struct HostBacklog {
    entries: VecDeque<([u8; 32], Retention, Vec<u8>)>,
    bytes: usize,
}

impl IntoIterator for HostBacklog {
    type Item = ([u8; 32], Retention, Vec<u8>);
    type IntoIter = std::collections::vec_deque::IntoIter<Self::Item>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

/// Keep `msg` when it is channel governance, the latest copy per retention
/// channel. A host session that was down when it was published (they
/// restart, and a pinned home peer replaces an unpinned dial) gets it when
/// it starts: a channel's genesis is announced once, and a device that is
/// not a member yet learns only from the host that the channel exists.
fn remember_governance(
    remembered: &mut HashMap<WorkspaceId, GovernanceLog>,
    ws: WorkspaceId,
    msg: &NeighbourMsg,
) {
    if !matches!(
        msg,
        NeighbourMsg::MlsGovernance { .. }
            | NeighbourMsg::MlsAccessRequest { .. }
            | NeighbourMsg::MlsGovernanceAck { .. }
    ) {
        return;
    }
    let (Some((channel, Retention::Checkpoint)), Ok(bytes)) =
        (retention(msg, false), postcard::to_allocvec(msg))
    else {
        return;
    };
    let room = remembered.entry(ws).or_default();
    room.retain(|(kept, _)| *kept != channel);
    if room.len() >= MAX_REMEMBERED_GOVERNANCE {
        room.pop_front();
    }
    room.push_back((channel, bytes));
}

fn message_fingerprint(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// Remote input is not deduplicated for good: a second click or key press is
/// byte-identical to the first. Only a copy arriving within [`INPUT_COPY`]
/// (the same event over another route to this room) is dropped.
fn mark_new_input(
    ws: WorkspaceId,
    fingerprint: [u8; 32],
    now: Instant,
    recent: &mut HashMap<(WorkspaceId, [u8; 32]), Instant>,
) -> bool {
    const INPUT_COPY: Duration = Duration::from_millis(30);
    if recent.len() > 256 {
        recent.retain(|_, seen| now.saturating_duration_since(*seen) < INPUT_COPY);
    }
    match recent.insert((ws, fingerprint), now) {
        Some(seen) => now.saturating_duration_since(seen) >= INPUT_COPY,
        None => true,
    }
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

/// The actor state a host-route scheduling step reads: its endpoint, host
/// identity and live sessions; and records into: in-flight connections and
/// the single discovery-task registry.
struct HostRouteScheduler<'a> {
    endpoint: &'a Endpoint,
    identity: &'a HostIdentity,
    internal_tx: &'a mpsc::Sender<InternalEvent>,
    sessions: &'a HashMap<(WorkspaceId, EndpointId), HostRuntime>,
    connecting: &'a mut HashSet<(WorkspaceId, EndpointId)>,
    content_discovery_tasks:
        &'a mut HashMap<(WorkspaceId, EndpointId), tokio::task::JoinHandle<()>>,
}

impl HostRouteScheduler<'_> {
    #[allow(
        clippy::too_many_lines,
        reason = "one scheduler transition atomically coordinates the endpoint, host identity, route state, and single discovery-task registry"
    )]
    fn schedule(
        &mut self,
        ws: WorkspaceId,
        addr: EndpointAddr,
        expectation: Option<HomePeerExpectation>,
        action_v2: bool,
    ) {
        let (endpoint, identity, internal_tx, sessions) = (
            self.endpoint,
            self.identity,
            self.internal_tx,
            self.sessions,
        );
        let connecting = &mut *self.connecting;
        let content_discovery_tasks = &mut *self.content_discovery_tasks;
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
        let action_v2 = action_v2 && !device_cert.capabilities.can_act_as_host;
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
            let result = tokio::time::timeout(Duration::from_secs(10), async {
                if action_v2 {
                    RelayClient::connect_v2_to_enrolled_home_peer(
                        endpoint,
                        addr.clone(),
                        expectation.user_public_key,
                        expectation.device_public_key,
                        device_cert,
                        identity.device_signing_key.clone(),
                        revocations,
                    )
                    .await
                } else {
                    RelayClient::connect_to_enrolled_home_peer(
                        endpoint,
                        addr.clone(),
                        expectation.user_public_key,
                        expectation.device_public_key,
                        device_cert,
                        identity.device_signing_key.clone(),
                        revocations,
                    )
                    .await
                }
            })
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
}

/// Submit an authenticated relay-discovered Files peer to the running FUSE
/// daemon. This is deliberately a detached worker: endpoint registration may
/// wait on the group-restricted daemon socket but must never delay networking
/// or the compositor. The filesystem owns persistence and hydration policy.
#[allow(
    clippy::items_after_statements,
    reason = "the async write extension is local to the detached filesystem-registration worker and kept beside its only use"
)]
async fn register_content_peer(registration: tilo_relay::ContentPeerRegistration) -> bool {
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

/// How often a workspace host session asks the host for a keep-alive.
const HOST_PING_INTERVAL: Duration = Duration::from_secs(5);
/// How long a host may stay silent, keep-alives included, before its
/// session is dropped and dialled again. A connection whose host session
/// ended without closing it (or whose host stopped reading) otherwise
/// looks alive forever.
const HOST_SILENCE_LIMIT: Duration = Duration::from_secs(20);

fn spawn_host_pump(
    ws: WorkspaceId,
    host: EndpointId,
    session: Arc<RelayCoeditSession>,
    replaying: Arc<std::sync::atomic::AtomicBool>,
    tx: mpsc::Sender<InternalEvent>,
    native_scope: Option<Arc<native_ingress::Scope>>,
) -> tokio::task::JoinHandle<()> {
    use std::sync::atomic::Ordering;
    tokio::spawn(async move {
        let mut ping = tokio::time::interval(HOST_PING_INTERVAL);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let inbound = tokio::select! {
                inbound = session.recv() => inbound,
                _ = ping.tick() => {
                    if session.silent_for() > HOST_SILENCE_LIMIT {
                        tracing::warn!(ws, host = %host, "workspace host stopped answering; reconnecting");
                        break;
                    }
                    let _ = session.ping();
                    continue;
                }
            };
            let Some(inbound) = inbound else {
                break;
            };
            let replay = replaying.load(Ordering::Acquire);
            let event = match inbound {
                HostInbound::Ready { .. } => {
                    replaying.store(false, Ordering::Release);
                    HostEvent::Ready
                }
                HostInbound::Live { from, frame } => HostEvent::Live { from, frame },
                HostInbound::Voice(event) => HostEvent::Voice(event),
                HostInbound::RemoteAction { from, frame } => {
                    let Ok(author) = EndpointId::from_bytes(&from) else {
                        continue;
                    };
                    HostEvent::Message {
                        author,
                        // Never deduplicated; see `handle_host_event`.
                        fingerprint: [0; 32],
                        replayed: false,
                        msg: NeighbourMsg::RemoteAction { frame },
                    }
                }
                HostInbound::Delta { author, data } if native_scope.is_some() => {
                    let Ok(msg) = postcard::from_bytes(&data) else {
                        continue;
                    };
                    let Some(scope) = native_scope.as_ref() else {
                        continue;
                    };
                    HostEvent::NativeInput(scope.input(author, replay, msg))
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
                        replayed: replay,
                        msg,
                    }
                }
            };
            if tx
                .send(InternalEvent::Host {
                    ws,
                    peer: host,
                    session: Arc::downgrade(&session),
                    event,
                })
                .await
                .is_err()
            {
                return;
            }
        }
        // Fence already queued evidence before notifying the actor of closure.
        if let Some(scope) = native_scope {
            scope.close();
        }
        let _ = tx
            .send(InternalEvent::Host {
                ws,
                peer: host,
                session: Arc::downgrade(&session),
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
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    use super::*;
    use getrandom::{SysRng, rand_core::UnwrapErr};

    #[test]
    fn a_repeated_click_is_new_but_its_copy_over_another_route_is_not() {
        let mut recent = HashMap::new();
        let start = Instant::now();
        assert!(mark_new_input(1, [7; 32], start, &mut recent));
        assert!(
            !mark_new_input(1, [7; 32], start + Duration::from_millis(1), &mut recent),
            "the same event arriving over a second relay session"
        );
        assert!(
            mark_new_input(1, [7; 32], start + Duration::from_millis(200), &mut recent),
            "a second, byte-identical click"
        );
        assert!(mark_new_input(
            2,
            [7; 32],
            start + Duration::from_millis(200),
            &mut recent
        ));
    }

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
    fn personal_bindings_are_exact_generation_and_membership_scoped() {
        // Phase `personal_media_generation_is_exact_and_stale_completion_cannot_remove_replacement`.
        {
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
        // Phase `departed_personal_connection_cannot_rebind_after_membership_reconnect`.
        {
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
        // Phase `nearby_transfer_frame_round_trips_exact_binding_and_bounded_chunk`.
        {
            let wire = PersonalFabricWireFrame {
                workspace: 42,
                user_public_key: [3; 32],
                payload: PersonalFabricPayload::NearbyTransfer(
                    PersonalNearbyTransferFrame::Chunk {
                        transfer_id: [4; 16],
                        request_id: [5; 16],
                        offset: 99,
                        bytes: vec![7; PERSONAL_MEDIA_CHUNK_BYTES],
                    },
                ),
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
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    async fn two_actors_exchange_and_announce_identity_over_iroh() {
        // Phase `two_actors_exchange_over_iroh`.
        {
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
                let event = tilo_test_env::within("a message on rx_a", rx_a.recv())
                    .await
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
                let event = tilo_test_env::within("a message on rx_a", rx_a.recv())
                    .await
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
                let event = tilo_test_env::within("a message on rx_b", rx_b.recv())
                    .await
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
        // Phase `actors_announce_identity_when_a_neighbor_connects`.
        {
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
                tilo_test_env::within(
                    "identity announcement should not wait for the heartbeat",
                    async {
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
                    },
                )
                .await;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    async fn direct_collaboration_traffic_reaches_only_its_exact_target() {
        // Phase `dead_surface_viewer_does_not_stall_later_collaboration_messages`.
        {
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
                if let NetEvent::PeerJoined { peer, .. } =
                    tilo_test_env::within("a message on rx_a", rx_a.recv())
                        .await
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
                let event = tilo_test_env::within(
                    "dead media viewer stalled the collaboration actor",
                    rx_c.recv(),
                )
                .await
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
        // Phase `personal_fabric_action_reaches_only_the_exact_target`.
        {
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
                match tilo_test_env::within("a message on rx_a", rx_a.recv())
                    .await
                    .unwrap()
                {
                    NetEvent::PeerJoined { peer, .. } if peer == b_id => a_saw_b = true,
                    NetEvent::PeerJoined { peer, .. } if peer == c_id => a_saw_c = true,
                    _ => {}
                }
            }
            for receiver in [&mut rx_b, &mut rx_c] {
                loop {
                    let event = tilo_test_env::within("a message on receiver", receiver.recv())
                        .await
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
                let event = tilo_test_env::within("a message on rx_b", rx_b.recv())
                    .await
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
        // Phase `direct_workspace_control_input_reaches_only_the_exact_target`.
        {
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
                match tilo_test_env::within("a message on rx_a", rx_a.recv())
                    .await
                    .unwrap()
                {
                    NetEvent::PeerJoined { peer, .. } if peer == b_id => a_saw_b = true,
                    NetEvent::PeerJoined { peer, .. } if peer == c_id => a_saw_c = true,
                    _ => {}
                }
            }
            for receiver in [&mut rx_b, &mut rx_c] {
                loop {
                    let event = tilo_test_env::within("a message on receiver", receiver.recv())
                        .await
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
                let event = tilo_test_env::within("a message on rx_b", rx_b.recv())
                    .await
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
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(
        clippy::too_many_lines,
        reason = "the integration test verifies both receiver framing and the sender's ordered progress lifecycle"
    )]
    async fn personal_fabric_negotiates_alpn_and_streams_verified_media() {
        // Phase `personal_fabric_uses_its_negotiated_alpn`.
        {
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
                tilo_test_env::within("a message on rx_a", rx_a.recv())
                    .await
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
                let event = tilo_test_env::within("a message on rx_b", rx_b.recv())
                    .await
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
        // Phase `personal_media_streams_in_order_with_a_verified_finish_hash`.
        {
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
                tilo_test_env::within("a message on rx_a", rx_a.recv()).await
                    .unwrap(),
                NetEvent::PeerJoined { peer, .. } if peer == b_id
            ) {}
            while !matches!(
                tilo_test_env::within("a message on rx_b", rx_b.recv()).await
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
                let event = tilo_test_env::within("a message on rx_b", rx_b.recv())
                    .await
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
                let event = tilo_test_env::within("a message on rx_a", rx_a.recv())
                    .await
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
                shell_authority: None,
                checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::default(),
            }],
            ..RelayConfig::default()
        };
        let relay = Box::pin(Relay::start(
            config,
            RelayIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: host_key,
                device_cert: host_cert.clone(),
                revocations: RevocationList::new(),
            },
        ))
        .await
        .unwrap();
        relay
            .set_workspace_membership(42, true, vec![a_pub, b_pub], state.path().to_path_buf())
            .await
            .unwrap();
        (relay, state, host_cert)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(
        clippy::too_many_lines,
        reason = "the enrolled-host and continuity replay transcripts need setup, relay, rejection and restart actors together"
    )]
    async fn workspace_host_relays_authenticated_traffic_and_retains_routes() {
        // Phase `scheduled_enrolled_host_rejects_wrong_root_device_and_revocation`.
        // Exercise the exact scheduled enrolled-home-peer route, rather than a
        // direct `RelayClient` unit call. In particular, this protects against an
        // innocuous-looking refactor that drops `HomePeerExpectation` before the
        // relay dial.
        {
            let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            let other_root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            let (client_key, client_cert) = enrol(&root, DeviceCapabilities::primary(), "client");
            let (host_key, host_cert) = enrol(&root, DeviceCapabilities::host(), "home");
            let (_, wrong_device_cert) = enrol(&root, DeviceCapabilities::host(), "other-home");
            let state = tempfile::tempdir().unwrap();
            let relay = Box::pin(Relay::start(
                RelayConfig {
                    state_root: state.path().to_path_buf(),
                    workspaces: vec![WorkspaceBinding {
                        id: 42,
                        pin: true,
                        replication: ReplicationIntent::FullWorkspace,
                        admitted_users: Vec::new(),
                        shell_authority: None,
                        checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::default(),
                    }],
                    ..RelayConfig::default()
                },
                RelayIdentity {
                    user_public_key: root.verifying_key().to_bytes(),
                    device_signing_key: host_key,
                    device_cert: host_cert.clone(),
                    revocations: RevocationList::new(),
                },
            ))
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
                        HostRouteScheduler {
                            endpoint: &endpoint,
                            identity: &identity,
                            internal_tx: &tx,
                            sessions: &HashMap::new(),
                            connecting: &mut connecting,
                            content_discovery_tasks: &mut discoveries,
                        }
                        .schedule(42, relay_addr, Some(expectation), false);
                        let rejected = matches!(
                        tilo_test_env::within("a message on rx", rx.recv()).await,
                        Some(InternalEvent::HostConnected { result, .. }) if result.is_err()
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
        // Phase `two_actors_exchange_through_authenticated_workspace_host`.
        {
            let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
            let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
            let (relay, _state, _host_cert) = Box::pin(spawn_pinned_relay_for_two_members(
                &root,
                a_key.verifying_key().to_bytes(),
                b_key.verifying_key().to_bytes(),
            ))
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
                let event = tilo_test_env::within("a message on rx_b", rx_b.recv())
                    .await
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
        // Phase `remote_action_frames_fall_back_to_the_workspace_host`.
        // Two devices that share only a workspace host, with no direct route,
        // still exchange remote-action frames: the same bytes, delivered to the
        // addressed device attributed to the sender the host authenticated, and
        // a byte-identical retry is delivered again rather than deduplicated.
        {
            let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
            let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
            let (relay, _state, _host_cert) = Box::pin(spawn_pinned_relay_for_two_members(
                &root,
                a_key.verifying_key().to_bytes(),
                b_key.verifying_key().to_bytes(),
            ))
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
                let (peer, received) = timeout(tilo_test_env::hang_bound(), async {
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
        // Phase `failed_host_address_refresh_retains_files_route_until_explicit_removal`.
        {
            const TEST_HEARTBEAT: Duration = Duration::from_millis(100);
            let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
            let a_public = a_key.verifying_key().to_bytes();
            let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
            let (relay, _state, host_cert) = Box::pin(spawn_pinned_relay_for_two_members(
                &root,
                a_public,
                b_key.verifying_key().to_bytes(),
            ))
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
                let event = tilo_test_env::within("a message on rx_a", rx_a.recv())
                    .await
                    .unwrap();
                if matches!(event, NetEvent::HostReady { .. }) {
                    break;
                }
            }

            // A replacement address that cannot respond must not tear down the
            // already-discovered Files endpoint. The old actor implementation
            // called the permanent-removal path before attempting this dial.
            let refreshed = EndpointAddr::new(relay.node_addr().id)
                .with_ip_addr("192.0.2.1:9".parse().unwrap());
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
            tilo_test_env::within(
                "explicit removal must revoke enrolled Files routes",
                async {
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
                },
            )
            .await;
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
        // Phase `host_replays_the_latest_personal_continuity_checkpoint_after_close`.
        {
            let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
            let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
            let (relay, _state, _host_cert) = Box::pin(spawn_pinned_relay_for_two_members(
                &root,
                a_key.verifying_key().to_bytes(),
                b_key.verifying_key().to_bytes(),
            ))
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
                }) = tilo_test_env::within("a message on rx_b", rx_b.recv()).await
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
                }) = tilo_test_env::within("a message on rx_b", rx_b.recv()).await
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
                }) =
                    tilo_test_env::within("a message on replay_events", replay_events.recv()).await
                {
                    assert!(snapshot.is_empty(), "restarted B receives retained closure");
                    break;
                }
            }
        }
    }

    /// Spawn two actors, join workspace 42 on each, connect both to `relay`,
    /// and block until both report `HostReady`. Returns each actor's
    /// command sender and event receiver.
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
                let event = tilo_test_env::within("a message on rx", rx.recv())
                    .await
                    .unwrap();
                if matches!(event, NetEvent::HostReady { .. }) {
                    break;
                }
            }
        }
        (cmd_a, rx_a, cmd_b, rx_b)
    }

    /// Each device's certificate reaches the others through the host, which
    /// retains it; a device resubscribing receives the other member's again
    /// but never its own, which nothing on this side could authenticate. Its
    /// own documents do come back, marked as its own: a device that lost its
    /// local state gets its earlier changes from the host's history.
    #[allow(
        clippy::too_many_lines,
        reason = "one device's publish, restart and replay"
    )]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn host_replays_member_certificates_and_own_documents_but_not_own_certificate() {
        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
        let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
        let (relay, _state, _host_cert) = spawn_pinned_relay_for_two_members(
            &root,
            a_key.verifying_key().to_bytes(),
            b_key.verifying_key().to_bytes(),
        )
        .await;
        let a_device = a_key.verifying_key().to_bytes();
        let b_device = b_key.verifying_key().to_bytes();
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
        let signer = b_key.clone();
        let (_cmd_a, mut rx_a, cmd_b, _rx_b) = join_workspace_and_await_host_ready(
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
        let artifact = collab_types::document_artifact_id("so.tilo.notes", "room", "record");
        let mut op = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Incremental(b"b's earlier change".to_vec())
                .encode()
                .unwrap(),
            artifact,
            collab_types::Ed25519PublicKey::from_bytes(b_device),
            collab_types::AttestationId::default(),
            collab_types::OpClass::Splice,
            std::time::SystemTime::now(),
            1,
        );
        collab_types::sign_op(&mut op, &signer).unwrap();
        cmd_b
            .send(NetCmd::Publish {
                ws: 42,
                msg: NeighbourMsg::AppDocument {
                    peer: 7,
                    app_id: "so.tilo.notes".into(),
                    cluster_id: "room".into(),
                    record_id: "record".into(),
                    envelope: Box::new(op.clone()),
                },
            })
            .await
            .unwrap();
        tilo_test_env::within("the host fans B's document to A", async {
            loop {
                if let Some(NetEvent::Msg {
                    msg: NeighbourMsg::AppDocument { .. },
                    ..
                }) = rx_a.recv().await
                {
                    break;
                }
            }
        })
        .await;
        drop(cmd_b);

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
        let mut hellos = Vec::new();
        let mut own = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, replay_events.recv()).await {
            match event {
                NetEvent::Msg {
                    peer,
                    msg: NeighbourMsg::IdentityHello { cert, .. },
                    ..
                } => {
                    assert_eq!(*peer.as_bytes(), cert.device_public_key);
                    hellos.push(cert.device_public_key);
                }
                NetEvent::Msg {
                    peer,
                    msg: NeighbourMsg::AppDocument { .. },
                    ..
                } => assert_ne!(*peer.as_bytes(), b_device, "not as another member's"),
                NetEvent::OwnReplay {
                    msg: NeighbourMsg::AppDocument { envelope, .. },
                    ..
                } => own.push(*envelope),
                _ => {}
            }
        }
        assert!(hellos.contains(&a_device), "A's certificate is replayed");
        assert!(!hellos.contains(&b_device), "B's own certificate is not");
        assert_eq!(own, [op], "B's own document comes back from the host");
    }

    /// Channel governance published while no host session is up (before the
    /// first dial, or while a session is being replaced) reaches the host
    /// once one starts: a steward announces a channel's genesis once, and a
    /// browser learns only from the host that the channel exists.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn governance_published_before_a_host_session_reaches_the_host() {
        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
        let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
        let (relay, _state, _host_cert) = spawn_pinned_relay_for_two_members(
            &root,
            a_key.verifying_key().to_bytes(),
            b_key.verifying_key().to_bytes(),
        )
        .await;
        let identity = |key, cert| HostIdentity {
            device_cert: cert,
            device_signing_key: key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let genesis = collab_types::MlsGovernanceFrameV1::signed(
            collab_types::ChannelId::new(),
            [1; 32],
            1,
            [0; 32],
            0,
            collab_types::MlsGovernanceAction::Genesis,
            &a_key,
        )
        .unwrap();
        let (a, lookup_a) = identified_endpoint(&a_key).await;
        let (cmd_a, _task_a) = Actor::spawn_reporting_addrs(
            a,
            lookup_a,
            Some(identity(a_key, a_cert)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (events_a, mut rx_a) = mpsc::channel(16);
        cmd_a
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: events_a,
            })
            .await
            .unwrap();
        cmd_a
            .send(NetCmd::Publish {
                ws: 42,
                msg: NeighbourMsg::MlsGovernance {
                    room: "room".into(),
                    frame: Box::new(genesis.clone()),
                    delivery: 1,
                },
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
        tilo_test_env::within("A's host session is ready", async {
            while !matches!(rx_a.recv().await, Some(NetEvent::HostReady { .. })) {}
        })
        .await;

        let (b, lookup_b) = identified_endpoint(&b_key).await;
        let (cmd_b, _task_b) = Actor::spawn_reporting_addrs(
            b,
            lookup_b,
            Some(identity(b_key, b_cert)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (events_b, mut rx_b) = mpsc::channel(16);
        cmd_b
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: events_b,
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
        tilo_test_env::within("B receives A's genesis from the host", async {
            loop {
                if let Some(NetEvent::Msg {
                    msg: NeighbourMsg::MlsGovernance { frame, .. },
                    ..
                }) = rx_b.recv().await
                    && *frame == genesis
                {
                    break;
                }
            }
        })
        .await;
    }

    /// A document written while the device's host session is down (a
    /// record created at launch, during the first session's replacement)
    /// reaches the host once a session starts: every later change of the
    /// record depends on it.
    #[allow(
        clippy::too_many_lines,
        reason = "a host, two members and a session replaced mid-publication in one scenario"
    )]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_document_published_before_a_host_session_reaches_the_host() {
        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
        let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
        let (relay, _state, _host_cert) = spawn_pinned_relay_for_two_members(
            &root,
            a_key.verifying_key().to_bytes(),
            b_key.verifying_key().to_bytes(),
        )
        .await;
        let identity = |key, cert| HostIdentity {
            device_cert: cert,
            device_signing_key: key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let a_device = a_key.verifying_key().to_bytes();
        let artifact = collab_types::document_artifact_id("so.tilo.notes", "room", "record");
        let mut op = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Incremental(b"written at launch".to_vec())
                .encode()
                .unwrap(),
            artifact,
            collab_types::Ed25519PublicKey::from_bytes(a_device),
            collab_types::AttestationId::default(),
            collab_types::OpClass::Splice,
            std::time::SystemTime::now(),
            1,
        );
        collab_types::sign_op(&mut op, &a_key).unwrap();
        let (a, lookup_a) = identified_endpoint(&a_key).await;
        let (cmd_a, _task_a) = Actor::spawn_reporting_addrs(
            a,
            lookup_a,
            Some(identity(a_key, a_cert)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (events_a, mut rx_a) = mpsc::channel(16);
        cmd_a
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: events_a,
            })
            .await
            .unwrap();
        // The host is known but its session is not up yet.
        cmd_a
            .send(NetCmd::ConnectHost {
                addr: relay.node_addr(),
                expectation: None,
            })
            .await
            .unwrap();
        cmd_a
            .send(NetCmd::Publish {
                ws: 42,
                msg: NeighbourMsg::AppDocument {
                    peer: 7,
                    app_id: "so.tilo.notes".into(),
                    cluster_id: "room".into(),
                    record_id: "record".into(),
                    envelope: Box::new(op.clone()),
                },
            })
            .await
            .unwrap();
        tilo_test_env::within("A's host session is ready", async {
            while !matches!(rx_a.recv().await, Some(NetEvent::HostReady { .. })) {}
        })
        .await;

        let (b, lookup_b) = identified_endpoint(&b_key).await;
        let (cmd_b, _task_b) = Actor::spawn_reporting_addrs(
            b,
            lookup_b,
            Some(identity(b_key, b_cert)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (events_b, mut rx_b) = mpsc::channel(16);
        cmd_b
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: events_b,
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
        tilo_test_env::within("B receives A's launch-time document from the host", async {
            loop {
                if let Some(NetEvent::Msg {
                    msg: NeighbourMsg::AppDocument { envelope, .. },
                    ..
                }) = rx_b.recv().await
                    && *envelope == op
                {
                    break;
                }
            }
        })
        .await;
    }

    /// Native shells are live members of their workspace host: a presence
    /// frame one publishes reaches the other attributed to its device, the
    /// same plane browsers and agents publish on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_frames_cross_between_shells_through_the_workspace_host() {
        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
        let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
        let (relay, _state, _host_cert) = spawn_pinned_relay_for_two_members(
            &root,
            a_key.verifying_key().to_bytes(),
            b_key.verifying_key().to_bytes(),
        )
        .await;
        let a_device = a_key.verifying_key().to_bytes();
        let (a, lookup_a) = identified_endpoint(&a_key).await;
        let (b, lookup_b) = identified_endpoint(&b_key).await;
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
        // A sealed frame: the host relays its bytes unchanged.
        let frame = tilo_relay_protocol::live::SealedLiveFrame {
            kind: tilo_relay_protocol::live::LiveKind::Presence,
            seq: 1,
            record: [3; 32],
            channel: [7; 16],
            epoch: 1,
            payload: vec![0xa5; 48],
        }
        .encode()
        .unwrap();
        // A frame in the clear never leaves.
        let cleartext = tilo_relay_protocol::live::LiveFrame::Presence(
            tilo_relay_protocol::live::PresenceFrame {
                seq: 1,
                sent_at_ms: 1,
                focus: None,
                pointer: Some([3.0, 4.0]),
                selection: None,
                drag: None,
                participant: None,
            },
        )
        .encode()
        .unwrap();
        // The live join follows the subscription; resend until it is in.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            for frame in [cleartext.clone(), frame.clone()] {
                cmd_a
                    .send(NetCmd::PublishLive { ws: 42, frame })
                    .await
                    .unwrap();
            }
            match tokio::time::timeout(Duration::from_millis(200), rx_b.recv()).await {
                Ok(Some(NetEvent::Live {
                    ws,
                    from,
                    frame: received,
                })) => {
                    assert_eq!(ws, 42);
                    assert_eq!(from, a_device, "attributed to the authenticated device");
                    assert_eq!(received, frame);
                    break;
                }
                _ => assert!(
                    tokio::time::Instant::now() < deadline,
                    "no live frame crossed"
                ),
            }
        }
    }

    /// What a host writes before answering a subscription is its replay of
    /// retained history and is marked so (receivers exempt it from their
    /// freshness window); what it fans out afterwards is live.
    #[allow(
        clippy::too_many_lines,
        reason = "one host, two members and the replay/live boundary in a single scenario"
    )]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn host_replay_is_marked_until_the_host_answers_ready() {
        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
        let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
        let (relay, _state, _host_cert) = spawn_pinned_relay_for_two_members(
            &root,
            a_key.verifying_key().to_bytes(),
            b_key.verifying_key().to_bytes(),
        )
        .await;
        let a_device = a_key.verifying_key().to_bytes();
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
        let (cmd_a, _rx_a, cmd_b, _rx_b) = join_workspace_and_await_host_ready(
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
        drop(cmd_b);

        let (b_restarted, lookup_restarted) = identified_endpoint(&b_restart_key).await;
        let (cmd_restarted, _task_restarted) = Actor::spawn_reporting_addrs(
            b_restarted,
            lookup_restarted,
            Some(identity(b_restart_key, b_restart_cert)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (events, mut rx) = mpsc::channel(64);
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
        let mut replayed_hello = None;
        loop {
            match timeout(Duration::from_secs(15), rx.recv())
                .await
                .unwrap()
                .unwrap()
            {
                NetEvent::Msg {
                    msg: NeighbourMsg::IdentityHello { cert, .. },
                    replayed,
                    membership_epoch: None,
                    ..
                } if cert.device_public_key == a_device => replayed_hello = Some(replayed),
                NetEvent::HostReady { .. } => break,
                _ => {}
            }
        }
        assert_eq!(
            replayed_hello,
            Some(true),
            "A's retained certificate is replay"
        );

        // Asked again (a local app joined a record late), the host replays
        // the room once more, and that is replay too.
        cmd_restarted.send(NetCmd::Replay { ws: 42 }).await.unwrap();
        let mut replayed_again = None;
        loop {
            match timeout(Duration::from_secs(15), rx.recv())
                .await
                .unwrap()
                .unwrap()
            {
                NetEvent::Msg {
                    msg: NeighbourMsg::IdentityHello { cert, .. },
                    replayed,
                    membership_epoch: None,
                    ..
                } if cert.device_public_key == a_device => replayed_again = Some(replayed),
                NetEvent::HostReady { .. } if replayed_again.is_some() => break,
                _ => {}
            }
        }
        assert_eq!(replayed_again, Some(true));

        cmd_a
            .send(NetCmd::Publish {
                ws: 42,
                msg: NeighbourMsg::SyncReq {
                    since: vec![],
                    room: "after-ready".into(),
                    invite_id: None,
                },
            })
            .await
            .unwrap();
        loop {
            if let NetEvent::Msg {
                msg: NeighbourMsg::SyncReq { room, .. },
                replayed,
                ..
            } = timeout(Duration::from_secs(15), rx.recv())
                .await
                .unwrap()
                .unwrap()
                && room == "after-ready"
            {
                assert!(!replayed, "a fan-out after Ready is live");
                break;
            }
        }
    }

    /// The room's governance transport of one device, over its real actor:
    /// channel governance goes to the room's workspace host (and gossip) like
    /// any collaboration message, and documents are collected on the side.
    struct ActorChannelTransport {
        cmd: mpsc::Sender<NetCmd>,
        events: Mutex<mpsc::Receiver<NetEvent>>,
        room: crate::shell::state::RoomId,
        documents: Mutex<Vec<collab_types::SignedOp>>,
        host_ready: std::sync::atomic::AtomicBool,
    }

    impl ActorChannelTransport {
        fn new(
            cmd: mpsc::Sender<NetCmd>,
            events: mpsc::Receiver<NetEvent>,
            room: crate::shell::state::RoomId,
        ) -> Self {
            Self {
                cmd,
                events: Mutex::new(events),
                room,
                documents: Mutex::new(Vec::new()),
                host_ready: std::sync::atomic::AtomicBool::new(false),
            }
        }

        fn host_ready(&self) -> bool {
            self.host_ready.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl crate::shell::channel_encryption::GovernanceTransport for ActorChannelTransport {
        fn publish(&self, msg: NeighbourMsg) -> bool {
            self.cmd.try_send(NetCmd::Publish { ws: 42, msg }).is_ok()
        }

        fn take_inbound(&self) -> Vec<crate::shell::network::InboundChannelGovernance> {
            use crate::shell::network::InboundChannelGovernance as Inbound;
            let room = self.room;
            let mut inbound = Vec::new();
            let mut events = self.events.lock().unwrap();
            while let Ok(event) = events.try_recv() {
                let msg = match event {
                    NetEvent::Msg { msg, .. } => msg,
                    NetEvent::HostReady { .. } => {
                        self.host_ready
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        continue;
                    }
                    _ => continue,
                };
                match msg {
                    NeighbourMsg::MlsGovernance { frame, .. } => {
                        inbound.push(Inbound::Frame { room, frame });
                    }
                    NeighbourMsg::MlsAccessRequest { request, .. } => {
                        inbound.push(Inbound::AccessRequest { room, request });
                    }
                    NeighbourMsg::MlsGovernanceAck { ack, .. } => {
                        inbound.push(Inbound::Ack { room, ack });
                    }
                    NeighbourMsg::AppDocument { envelope, .. } => {
                        self.documents.lock().unwrap().push(*envelope);
                    }
                    _ => {}
                }
            }
            inbound
        }
    }

    fn channel_service(
        key: &ed25519_dalek::SigningKey,
        dir: &std::path::Path,
        directory: crate::shell::channel_encryption::testing::Directory,
    ) -> crate::shell::channel_encryption::ChannelEncryption {
        crate::shell::channel_encryption::ChannelEncryption::open(
            crate::shell::channel_encryption::ChannelEncryptionConfig {
                root: dir.join("channels"),
                vault: channel_crypto::VaultKey::new(key.to_bytes()),
                device_key: key.clone(),
            },
            Box::new(directory),
            Arc::new(Mutex::new(
                crate::shell::collab_core::BrokerChannels::default(),
            )),
        )
        .unwrap()
    }

    fn every_file(dir: &std::path::Path, out: &mut Vec<Vec<u8>>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                every_file(&path, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.push(bytes);
            }
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    /// End to end through two real actors and a real workspace host:
    ///
    /// - a new channel is encrypted from creation, and what the relay stores
    ///   (governance and document history alike) holds no plaintext;
    /// - a member device whose network fails right after asking for access
    ///   still joins: the host retained the steward's frames and the steward
    ///   kept redelivering, so the restarted device gets its Welcome;
    /// - it reads what the steward wrote; once the policy drops it, the
    ///   removal commit rotates the keys and later content is closed to it.
    #[allow(
        clippy::too_many_lines,
        reason = "one transcript across two devices, a host outage and a restart"
    )]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn channel_encryption_runs_end_to_end_through_the_workspace_host() {
        use crate::shell::channel_encryption::ChannelAccess;
        use crate::shell::channel_encryption::testing::{Directory, Policy};
        use std::time::Instant;

        const SECRET: &[u8] = b"quarterly figures nobody outside may read";
        const LATER: &[u8] = b"written after the laptop lost access";
        let user_root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let (a_key, a_cert) = enrol(&user_root, DeviceCapabilities::primary(), "desktop");
        let (b_key, b_cert) = enrol(&user_root, DeviceCapabilities::member(), "laptop");
        let device = |key: &ed25519_dalek::SigningKey| {
            collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes())
        };
        let (relay, relay_state, _host_cert) = spawn_pinned_relay_for_two_members(
            &user_root,
            a_key.verifying_key().to_bytes(),
            b_key.verifying_key().to_bytes(),
        )
        .await;
        let (a, lookup_a) = identified_endpoint(&a_key).await;
        let (b, lookup_b) = identified_endpoint(&b_key).await;
        let identity = |key: &ed25519_dalek::SigningKey, cert: &DeviceCert| HostIdentity {
            device_cert: cert.clone(),
            device_signing_key: key.clone(),
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let (cmd_a, rx_a, cmd_b, rx_b) = join_workspace_and_await_host_ready(
            a,
            lookup_a,
            identity(&a_key, &a_cert),
            b,
            lookup_b,
            identity(&b_key, &b_cert),
            &relay,
            HEARTBEAT_INTERVAL,
        )
        .await;
        let room = crate::shell::state::RoomId::new();
        let workspace = room.to_string();
        let channel = collab_types::document_channel_id("so.tilo.writer", &workspace, "q3");
        let artifact = collab_types::document_artifact_id("so.tilo.writer", &workspace, "q3");
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(device(&b_key));
        let (a_dir, b_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut steward = channel_service(
            &a_key,
            a_dir.path(),
            Directory {
                governs: true,
                steward: device(&a_key),
                policy: policy.clone(),
            },
        );
        let member_directory = || Directory {
            governs: false,
            steward: device(&a_key),
            policy: Policy::default(),
        };
        let steward_transport = ActorChannelTransport::new(cmd_a.clone(), rx_a, room);
        let first_laptop = ActorChannelTransport::new(cmd_b.clone(), rx_b, room);

        // Encrypted from creation; the laptop waits and asks once.
        assert!(matches!(
            steward.access(room, channel, Instant::now()),
            ChannelAccess::Ready(_)
        ));
        let generation = steward.generation();
        let mut member = channel_service(&b_key, b_dir.path(), member_directory());
        assert!(matches!(
            member.access(room, channel, Instant::now()),
            ChannelAccess::Waiting
        ));
        member.pump(&first_laptop, Instant::now(), 1_000);
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tilo_test_env::within("the steward admits the laptop from its request", async {
            while steward.generation() == generation {
                tick.tick().await;
                steward.pump(&steward_transport, Instant::now(), 1_000);
            }
        })
        .await;

        // The laptop's network fails before it processes any answer: its
        // session to the host goes down and the process with everything it
        // had queued is gone.
        cmd_b
            .send(NetCmd::DisconnectHost {
                id: relay.node_addr().id,
            })
            .await
            .unwrap();
        drop(member);
        drop(first_laptop);
        drop(cmd_b);

        // The laptop restarts: a fresh actor on the same device key, its
        // channel state reopened from disk.
        let (b_again, lookup_again) = identified_endpoint(&b_key).await;
        let (cmd_again, _task) = Actor::spawn_reporting_addrs(
            b_again,
            lookup_again,
            Some(identity(&b_key, &b_cert)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (events, events_rx) = mpsc::channel(64);
        cmd_again
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: events,
            })
            .await
            .unwrap();
        cmd_again
            .send(NetCmd::ConnectHost {
                addr: relay.node_addr(),
                expectation: None,
            })
            .await
            .unwrap();
        let laptop = ActorChannelTransport::new(cmd_again, events_rx, room);
        let mut member = channel_service(&b_key, b_dir.path(), member_directory());
        let member_keys = tilo_test_env::within(
            "the restarted laptop joins from retained and redelivered frames",
            async {
                loop {
                    tick.tick().await;
                    steward.pump(&steward_transport, Instant::now(), 1_000);
                    member.pump(&laptop, Instant::now(), 1_000);
                    if laptop.host_ready()
                        && let ChannelAccess::Ready(keys) =
                            member.access(room, channel, Instant::now())
                    {
                        break keys;
                    }
                }
            },
        )
        .await;

        // The steward writes; the laptop reads it through the host.
        let ChannelAccess::Ready(keys) = steward.access(room, channel, Instant::now()) else {
            panic!("steward keys");
        };
        let (metadata, ciphertext) = keys
            .seal_content(artifact, collab_types::ContentRetention::Checkpoint, SECRET)
            .unwrap();
        let mut op = collab_types::SignedOp::new_unsigned_with_encryption(
            ciphertext.clone(),
            artifact,
            device(&a_key),
            collab_types::AttestationId::default(),
            collab_types::OpClass::Splice,
            std::time::SystemTime::now(),
            1,
            metadata,
        );
        collab_types::sign_op(&mut op, &a_key).unwrap();
        cmd_a
            .send(NetCmd::Publish {
                ws: 42,
                msg: NeighbourMsg::AppDocument {
                    peer: 1,
                    app_id: "so.tilo.writer".into(),
                    cluster_id: workspace.clone(),
                    record_id: "q3".into(),
                    envelope: Box::new(op),
                },
            })
            .await
            .unwrap();
        let received =
            tilo_test_env::within("the document reaches the laptop through the host", async {
                loop {
                    tick.tick().await;
                    member.pump(&laptop, Instant::now(), 1_000);
                    let next = laptop.documents.lock().unwrap().pop();
                    if let Some(op) = next {
                        break op;
                    }
                }
            })
            .await;
        assert_eq!(
            member_keys
                .open_content(artifact, &received.encryption.unwrap(), &received.payload)
                .unwrap()
                .as_slice(),
            SECRET
        );

        // What the host stored: ciphertext, and never the plaintext.
        let mut stored = Vec::new();
        every_file(relay_state.path(), &mut stored);
        assert!(
            stored.iter().any(|file| contains(file, &ciphertext)),
            "the host retains the encrypted checkpoint"
        );
        assert!(
            stored.iter().all(|file| !contains(file, SECRET)),
            "no stored byte of the host holds the plaintext"
        );

        // The laptop loses access: removal rotates the keys.
        policy.allowed.lock().unwrap().clear();
        tilo_test_env::within("the laptop learns of its removal", async {
            loop {
                tick.tick().await;
                steward.pump(&steward_transport, Instant::now(), 1_000);
                member.pump(&laptop, Instant::now(), 1_000);
                if matches!(
                    member.access(room, channel, Instant::now()),
                    ChannelAccess::Removed
                ) {
                    break;
                }
            }
        })
        .await;
        let ChannelAccess::Ready(keys) = steward.access(room, channel, Instant::now()) else {
            panic!("steward keys");
        };
        let (metadata, later) = keys
            .seal_content(artifact, collab_types::ContentRetention::Delta, LATER)
            .unwrap();
        assert!(
            member_keys
                .open_content(artifact, &metadata, &later)
                .is_err()
        );
    }
}
