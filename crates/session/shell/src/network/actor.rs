//! Iroh-gossip actor for workspace collaboration.
//!
//! Direct paths, NAT traversal and stateless relay fallback are selected by
//! iroh. Tilo only names authenticated endpoint ids and workspace topics.

pub(crate) mod governor_lane;
pub(crate) mod native_ingress;

/// A workspace host connection's error, as the actor reports it. A host at
/// another peer protocol version is also the person's to know about: one of
/// the two devices has to update (`crate::shell::collaborator_versions`).
fn relay_error(host: &iroh::EndpointId, error: &tilo_relay_session::ClientError) -> String {
    if let Some(theirs) = error.peer_version() {
        crate::shell::collaborator_versions::note(host.to_string(), String::new(), theirs);
    }
    error.to_string()
}

#[cfg(test)]
mod nearby_transfer_tests;
pub(crate) mod room_governance;

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
use tilo_relay_session::pointer_pacing::{Pacing, PointerPacer, slot_pacing};
use tilo_relay_session::{Inbound as HostInbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::mpsc;

use crate::network::protocol::{MAX_PERSONAL_CONTINUITY_CHECKPOINT_BYTES, NeighbourMsg, SurfaceId};
use crate::network::reachability::{Need, Reachability};
use neighbour_wire::{Retention, retention};
use tilo_lock::MutexRecover as _;

#[cfg(test)]
static DEDICATED_SURFACE_FRAMES_RECEIVED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// How often the actor retries what is missing (a topic subscription, a
/// direct peer, a workspace host) and re-reads this device's identity
/// material and codecs, to announce a change the tick it lands. Local work: a
/// converged, idle desktop sends nothing on it. Armed only while there is something to retry
/// or someone to tell ([`ActorState::needs_upkeep`]): a desktop alone with
/// its rooms never wakes for it.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// Peer liveness, kept by the transport rather than by messages. A gossip
/// neighbour is a member while its link is up: iroh sends a QUIC keep-alive
/// after 5 s without traffic, and a connection that hears nothing for this
/// long ends ([`transport_config`]), taking the link down. A peer known only
/// by its traffic, without a link, stays a member this long after it was
/// last heard.
const PEER_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a peer whose gossip link went down stays a member, so a
/// reconnect racing the down edge neither drops nor re-admits it.
///
/// The departure bound: a peer that leaves cleanly (quits, leaves the room,
/// closes its endpoint) is dropped within this grace; one that vanishes
/// (power, network) within [`PEER_TIMEOUT`] plus this grace, 20 s. Both
/// hold with no application message sent while idle; before the transport
/// carried liveness, both were 15 to 16 s after the last 5 s heartbeat.
const DEPARTURE_GRACE: Duration = Duration::from_secs(5);
/// A neighbour's identity hello is answered with this device's own unless
/// it announced itself on the topic this recently. A neighbour announces
/// itself on a link it sees as new, such as after it restarted, which this
/// side may not have seen as new and so not answered: without its answer
/// the neighbour never learns who this device is. The bound keeps two
/// devices from answering each other's answers.
const IDENTITY_ANSWER_INTERVAL: Duration = Duration::from_secs(10);
/// Personal fabric frames kept for a peer while it is being dialled; older
/// ones give way, as each snapshot supersedes the one before.
const PERSONAL_FABRIC_FRAMES_WAITING: usize = 16;
/// How long a personal fabric connection is kept with no frame either way.
/// The fabric speaks in bursts (a request, the snapshots answering it, an
/// action); between them the devices' gossip link keeps them in touch, and
/// a second connection held open would cost both ends its keep-alives for
/// nothing. The next frame dials again.
const PERSONAL_FABRIC_IDLE: Duration = PEER_TIMEOUT;
/// How often each workspace host session is asked for a fresh checkpoint
/// ([`ActorState::run_host_checkpoint_tick`]); armed only while there is a
/// host session to ask.
const HOST_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(600);
const MAX_GOSSIP_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SURFACE_FRAME_BYTES: usize = 64 * 1024 * 1024;
type SurfaceFanoutTasks = HashMap<(WorkspaceId, SurfaceId), (u64, tokio::task::JoinHandle<()>)>;

fn finish_surface_fanout(
    tasks: &mut SurfaceFanoutTasks,
    key: (WorkspaceId, SurfaceId),
    generation: u64,
) -> bool {
    if tasks
        .get(&key)
        .is_none_or(|(active, _)| *active != generation)
    {
        return false;
    }
    tasks.remove(&key);
    true
}

/// Encoded delta frames reference earlier units: only raw captures may be
/// coalesced. Keep their order within the existing one-frame byte budget.
struct SurfaceFrameBacklog {
    frames: VecDeque<NeighbourMsg>,
    bytes: usize,
    codec: Option<media::StreamCodec>,
    awaiting_keyframe: bool,
}

impl Default for SurfaceFrameBacklog {
    fn default() -> Self {
        Self {
            frames: VecDeque::new(),
            bytes: 0,
            codec: Some(media::StreamCodec::H264),
            awaiting_keyframe: false,
        }
    }
}

impl SurfaceFrameBacklog {
    fn frame_bytes(msg: &NeighbourMsg) -> usize {
        let payload = match msg {
            NeighbourMsg::SurfaceFrame { encoded_h264, .. } => encoded_h264.capacity(),
            _ => 0,
        };
        // Pop shrinks whenever retained deque capacity exceeds four slots
        // per remaining unit. Charge that storage as well as Vec capacity.
        payload.saturating_add(std::mem::size_of::<NeighbourMsg>().saturating_mul(4))
    }

    fn lose_references(&mut self) {
        self.frames = VecDeque::new();
        self.bytes = 0;
        self.awaiting_keyframe = self.codec.is_some();
    }

    fn admit(&mut self, msg: &NeighbourMsg) -> bool {
        let keyframe = match msg {
            NeighbourMsg::SurfaceFrame { encoded_h264, .. } => self
                .codec
                .is_none_or(|codec| media::realtime::is_keyframe(codec, encoded_h264)),
            _ => false,
        };
        if self.awaiting_keyframe && !keyframe {
            return false;
        }
        if keyframe {
            self.awaiting_keyframe = false;
        }
        true
    }

    fn push(&mut self, msg: NeighbourMsg) {
        let independent = match &msg {
            NeighbourMsg::SurfaceFrame { encoded_h264, .. } => self
                .codec
                .is_none_or(|codec| media::realtime::is_keyframe(codec, encoded_h264)),
            _ => false,
        };
        // A fresh keyframe supersedes the entire older pending chain. This
        // keeps a slow route from accumulating multiple GOPs of stale video.
        if independent {
            self.lose_references();
        }
        let bytes = Self::frame_bytes(&msg);
        if self.bytes.saturating_add(bytes) > MAX_SURFACE_FRAME_BYTES {
            self.lose_references();
        }
        if bytes <= MAX_SURFACE_FRAME_BYTES && self.admit(&msg) {
            self.bytes = self.bytes.saturating_add(bytes);
            self.frames.push_back(msg);
        }
    }

    fn pop(&mut self) -> Option<NeighbourMsg> {
        let msg = self.frames.pop_front()?;
        self.bytes = self.bytes.saturating_sub(Self::frame_bytes(&msg));
        if self.frames.len().saturating_mul(4) < self.frames.capacity() {
            self.frames.shrink_to_fit();
        }
        Some(msg)
    }
}
const SURFACE_STREAM_ALPN: &[u8] = b"tilo-surface-stream/1";
const PERSONAL_FABRIC_ALPN: &[u8] = b"tilo-personal-fabric/1";
/// Largest byte chunk one personal-fabric transfer frame carries.
pub const PERSONAL_TRANSFER_CHUNK_BYTES: usize = 256 * 1024;
const MAX_ACTIVE_PERSONAL_TRANSFERS: usize = 8;

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
    /// The one lane for file bytes between a person's devices: Nearby sends
    /// and media handoffs alike. Offers and resume checkpoints travel as
    /// authenticated actions; this lane carries bounded bytes only.
    NearbyTransfer(PersonalNearbyTransferFrame),
    /// One signed mutation of a record in the person's own room. Its room is
    /// implied by the route: the fabric only ever joins one person's devices.
    /// `record_id` is the sealed wire name, as on a room topic.
    Document {
        app_id: String,
        record_id: String,
        envelope: Box<collab_types::SignedOp>,
    },
    /// A device of the person asking for content one of `app_id`'s person
    /// records references. Its room is the person's, implied by the route.
    RecordContentWant {
        app_id: String,
        hash: String,
    },
    /// One bounded piece of that content, for the device that asked alone.
    RecordContentPiece {
        app_id: String,
        hash: String,
        total: u64,
        offset: u64,
        bytes: Vec<u8>,
    },
    /// Optional owner-authored screen observation, apart from the schema-6
    /// checkpoint.
    DisplayConnectors {
        stamp: crate::shell::device_fabric::FabricStamp,
        outputs: Vec<crate::shell::device_fabric::DisplayConnectorPlacement>,
    },
    /// Rich output observations follow the same certified, opt-in route.
    DisplayOutputs {
        stamp: crate::shell::device_fabric::FabricStamp,
        outputs: Vec<crate::shell::device_fabric::DisplayOutputObservation>,
    },
    /// Word to the device that asked ([`Self::RecordContentWant`]) that this
    /// one keeps none of that content.
    RecordContentLacks {
        app_id: String,
        hash: String,
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

/// The HTTPS routes of a host that publishes no iroh address
/// ([`NetCmd::NativeConnect`]).
#[derive(Clone)]
pub struct HttpsRoute(pub Arc<dyn tilo_relay_client::HttpClient>);
impl std::fmt::Debug for HttpsRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HttpsRoute")
    }
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
    /// A neighbour on `ws` sent something this device cannot place without
    /// the neighbour's identity: announce this device's own, which the
    /// neighbour answers with its (`IDENTITY_ANSWER_INTERVAL`).
    AnnounceIdentity {
        ws: WorkspaceId,
    },
    /// Ask the host of the joined room on `ws` to replay what it retains:
    /// something an app message let go earlier waited for has arrived, and
    /// the message is decided again when it comes back.
    NativeReplay {
        ws: WorkspaceId,
    },
    /// Publish one canonical live frame (presence, previews) on `ws`'s
    /// workspace hosts' live planes, where browsers and agents meet this
    /// device. Lossy.
    PublishLive {
        ws: WorkspaceId,
        frame: Vec<u8>,
    },
    /// Hold an enrolled room's relay session, over its host's HTTPS routes
    /// when given, else over the iroh endpoint its hint names.
    NativeConnect(native_ingress::Connect, Option<HttpsRoute>),
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
    PrivateMessages(tokio::sync::oneshot::Sender<super::private_messages::Transport>),
    /// What this device has received from its peers, by kind.
    WireTally(tokio::sync::oneshot::Sender<Arc<WireTally>>),
    /// This device's own topology entries (`SyncResp`) for a room another
    /// device governs, to its workspace hosts as a retained delta: the
    /// governor admits them within this device's registered namespace.
    PublishHostedTopology {
        ws: u64,
        msg: NeighbourMsg,
    },
    /// A message for `ws`'s workspace hosts only, held until a host session
    /// is up, never gossiped: what only the hosts of the person's own
    /// continuity topic may keep for their other devices.
    PublishToHosts {
        ws: u64,
        msg: NeighbourMsg,
    },
    NativeMedia(super::native_media::Command),
    NativeEnrollment(super::invite::enrollment_driver::Command),
    /// Invoke what a relay answers itself (a room's governance, a member's
    /// own business in it, or the host's own operations) on a relay session,
    /// and answer the relay's receipt.
    RoomGovernance(Box<room_governance::Request>),
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
    /// Send one shared window's stream (its start, frames, end and control
    /// grants) to exactly the viewers named by their peer handles, over the
    /// authenticated direct media route. No other participant of the
    /// workspace, its gossip topic or its workspace host receives any of it.
    PublishSurfaceTo {
        ws: WorkspaceId,
        msg: NeighbourMsg,
        viewers: Vec<crate::network::protocol::PeerId>,
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
    /// Send one piece of a record's content to the device that asked for it.
    PublishRecordContent(RecordContentDelivery),
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
    /// Stop only the active stream with this exact transport peer/id binding.
    CancelNearbyTransfer {
        target: EndpointId,
        transfer_id: [u8; 16],
    },
    /// Stream one immutable spool object to an exact certified personal
    /// device from the receiver-confirmed checkpoint. A second request for
    /// the same peer and transfer replaces the running stream.
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

/// A piece of a record's content on its way to the device of the room that
/// asked: over the direct plane when that device is a live peer of the
/// room's topic, else through the room's workspace hosts, which hand it to
/// that device alone and keep nothing, else on the topic itself.
#[derive(Debug)]
pub struct RecordContentDelivery {
    pub ws: WorkspaceId,
    pub target: Option<EndpointId>,
    pub msg: NeighbourMsg,
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
    /// The host of the joined room on topic `ws` admits this device's
    /// person no longer: they were removed (a public room prunes guests it
    /// has not seen for long). The shell joins again through the room's
    /// door, where it has one.
    NotAdmitted {
        ws: WorkspaceId,
    },
    /// A member of `ws` this device reaches through the workspace host
    /// `host` joined (`active`) or left the host's live plane.
    HostMember {
        ws: WorkspaceId,
        host: PeerId,
        device: PeerId,
        active: bool,
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
    NearbyTransferProgress {
        target: EndpointId,
        transfer_id: [u8; 16],
        sent_bytes: u64,
        total_bytes: u64,
        status: PersonalTransferStatus,
    },
}

impl NetEvent {
    /// What the event is, for timing: the message kind for a message.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Live { .. } => "Live",
            Self::NativeInput(_) => "NativeInput",
            Self::PeerJoined { .. } => "PeerJoined",
            Self::PeerLeft { .. } => "PeerLeft",
            Self::NotAdmitted { .. } => "NotAdmitted",
            Self::HostMember { .. } => "HostMember",
            Self::HostReady { .. } => "HostReady",
            Self::Msg { msg, .. } | Self::OwnReplay { msg, .. } => msg.kind(),
            Self::PersonalFabric { .. } => "PersonalFabric",
            Self::NearbyTransferProgress { .. } => "NearbyTransferProgress",
        }
    }
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

/// Where one outgoing personal-fabric transfer stream stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonalTransferStatus {
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
        result: Box<Result<RelayCoeditSession, tilo_joined_room::session::DialError>>,
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
        generation: u64,
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
    /// A personal fabric connection with `peer` came up, dialled or
    /// accepted, or a dial for one failed (`None`).
    PersonalFabricConnection {
        peer: EndpointId,
        connection: Option<iroh::endpoint::Connection>,
    },
    PersonalTransferProgress {
        target: EndpointId,
        transfer_id: [u8; 16],
        generation: u64,
        sent_bytes: u64,
    },
    PersonalTransferFinished {
        target: EndpointId,
        transfer_id: [u8; 16],
        generation: u64,
        outcome: PersonalTransferOutcome,
        sent_bytes: u64,
    },
    Closed {
        ws: WorkspaceId,
    },
}

async fn deliver_surface_frame(
    endpoint: Endpoint,
    addr: EndpointAddr,
    cached: Option<iroh::endpoint::Connection>,
    bytes: Vec<u8>,
) -> SurfaceFanoutOutcome {
    let peer = addr.id;
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
}

struct SurfaceFanoutOutcome {
    peer: EndpointId,
    connection: Option<iroh::endpoint::Connection>,
    delivered: bool,
}

enum HostEvent {
    NativeInput(native_ingress::Input),
    Ready,
    Voice(tilo_relay_session::coedit::VoiceEvent),
    Live {
        from: [u8; 32],
        frame: Vec<u8>,
    },
    /// A member joined or left the host's live plane.
    Member {
        device: [u8; 32],
        active: bool,
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
    /// Fingerprint of the identity material last announced on the topic, so
    /// a renewed certificate or a new revocation goes out the tick it lands
    /// and an unchanged one never does.
    identity_sent: Option<[u8; 32]>,
    /// When this device last announced its identity to the topic's
    /// neighbours ([`IDENTITY_ANSWER_INTERVAL`]).
    identity_announced: Option<Instant>,
    /// Whether gossip has had a neighbour on the topic: a `NeighborUp`, or a
    /// lag that may have hidden one. Until then nothing is handed to gossip
    /// (see [`Self::broadcast`]).
    linked: bool,
}

impl TopicRuntime {
    /// Whether this device announced its identity here within
    /// [`IDENTITY_ANSWER_INTERVAL`].
    fn announced_identity_lately(&self) -> bool {
        self.identity_announced
            .is_some_and(|announced| announced.elapsed() < IDENTITY_ANSWER_INTERVAL)
    }

    fn new(sender: GossipSender, pump: tokio::task::JoinHandle<()>) -> Self {
        Self {
            sender,
            pump,
            identity_sent: None,
            identity_announced: None,
            linked: false,
        }
    }

    /// Broadcast `bytes` to the topic's swarm, once it has had a neighbour.
    ///
    /// Before that gossip has nobody to send it to, and a neighbour that
    /// comes later is never offered an older message: the shell syncs it
    /// when it joins (`NetEvent::PeerJoined`). All a broadcast into an
    /// empty topic would do is start the topic's cache sweep
    /// ([`GOSSIP_CACHE_SWEEP`]), which then runs until the message expires
    /// (`gossip_caches_drain_within`), so a desktop alone with its rooms
    /// would wake for every local change. Only a linked topic broadcasts.
    async fn broadcast(&self, bytes: Bytes) -> Result<(), iroh_gossip::api::ApiError> {
        if !self.linked {
            return Ok(());
        }
        self.sender.broadcast(bytes).await
    }

    /// [`Self::broadcast`] to the topic's direct neighbours only.
    async fn broadcast_neighbors(&self, bytes: Bytes) -> Result<(), iroh_gossip::api::ApiError> {
        if !self.linked {
            return Ok(());
        }
        self.sender.broadcast_neighbors(bytes).await
    }
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

/// Early boot of another of the person's devices asking for its sibling
/// half. The peer is not authenticated by transport: the boot key
/// certificate and the challenge are what the sibling checks.
#[derive(Clone, Debug)]
struct DeviceUnlockProtocol;

impl ProtocolHandler for DeviceUnlockProtocol {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        tokio::spawn(crate::shell::device_unlock_runtime::serve_connection(
            connection,
        ));
        Ok(())
    }
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
                        | NeighbourMsg::SurfaceStreamStart { .. }
                        | NeighbourMsg::SurfaceStreamEnd { .. }
                        | NeighbourMsg::PermissionGrant { .. }
                        | NeighbourMsg::RemoteInput { .. }
                        | NeighbourMsg::SurfaceKeyframeRequest { .. }
                        | NeighbourMsg::SurfaceViewRequest { .. }
                        | NeighbourMsg::RemoteAction { .. }
                        | NeighbourMsg::RecordContentPiece { .. }
                        | NeighbourMsg::RecordContentLacks { .. }
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
    // Remote-action frames are requests and answers, not media, and a
    // stream's start, end and grants are its control: they take the
    // reliable queue with input.
    let is_input = matches!(
        &event,
        InternalEvent::SurfaceFrame {
            msg: NeighbourMsg::RemoteInput { .. }
                | NeighbourMsg::SurfaceKeyframeRequest { .. }
                | NeighbourMsg::SurfaceViewRequest { .. }
                | NeighbourMsg::RemoteAction { .. }
                | NeighbourMsg::RecordContentPiece { .. }
                | NeighbourMsg::RecordContentLacks { .. }
                | NeighbourMsg::SurfaceStreamStart { .. }
                | NeighbourMsg::SurfaceStreamEnd { .. }
                | NeighbourMsg::PermissionGrant { .. },
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
        read_personal_fabric(
            connection.clone(),
            self.internal_tx.clone(),
            &self.next_connection_incarnation,
        );
        // The peer's newest connection carries this device's frames to it
        // too: it reaches the peer however the peer reached here, where a
        // dial of this device's own may still try paths the peer had before
        // it restarted.
        let _ = self
            .internal_tx
            .send(InternalEvent::PersonalFabricConnection {
                peer,
                connection: Some(connection),
            })
            .await;
        Ok(())
    }
}

/// A personal fabric connection and when it last carried a frame.
struct PersonalFabricLink {
    connection: iroh::endpoint::Connection,
    used: Instant,
}

/// Read the personal fabric frames `connection` brings, whichever side
/// opened it, into the actor, until it closes.
fn read_personal_fabric(
    connection: iroh::endpoint::Connection,
    tx: mpsc::Sender<InternalEvent>,
    incarnations: &AtomicU64,
) {
    let peer = connection.remote_id();
    let connection_incarnation = incarnations.fetch_add(1, Ordering::Relaxed).max(1);
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
}

/// Why `command` means a remote party is involved, so this device must be
/// reachable for a while (`crate::network::reachability`): it dials a peer
/// or host, enrols a device, administers a relay room, joins a call or
/// sends to a named one of the person's devices. `None` for everything a
/// lone desktop does by itself: joining and publishing to a topic nobody
/// else is on (its rooms, its fabric broadcast) sends nothing, and a lone
/// desktop does that on every local change.
fn remote_party_command(command: &NetCmd) -> Option<&'static str> {
    Some(match command {
        NetCmd::Connect { .. } => "dialling a peer",
        NetCmd::ConnectHost { .. } | NetCmd::NativeConnect(..) => "dialling a host",
        NetCmd::GovernorAction { .. } | NetCmd::RoomGovernance(_) => "talking to a room's host",
        NetCmd::NativeEnrollment(_) => "enrolling a device",
        NetCmd::NativeMedia(_) => "a call",
        NetCmd::PublishPersonalFabricTarget { .. } | NetCmd::PublishNearbyTransfer { .. } => {
            "sending to one of the person's devices"
        }
        NetCmd::PublishSurfaceTo { .. } => "streaming a window to a peer",
        _ => return None,
    })
}

/// Why `event` means a remote party reached this device (on a path that
/// needed no relay, such as the local network), so it should be reachable
/// through its relays too.
fn remote_party_event(event: &InternalEvent) -> Option<&'static str> {
    match event {
        InternalEvent::Topic {
            event: GossipEvent::NeighborUp(_),
            ..
        } => Some("a peer joined a topic"),
        InternalEvent::PersonalFabric { .. } => Some("one of the person's devices connected"),
        InternalEvent::SurfaceFrame { .. } => Some("a peer streamed a window"),
        _ => None,
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
        Self::spawn_deferring_relays(
            endpoint,
            lookup,
            host_identity,
            listen_addrs,
            Reachability::default(),
        )
    }

    /// [`Self::spawn_reporting_addrs`] for an endpoint whose relays wait in
    /// `reachability` until something needs this device reachable: the
    /// actor wakes them when it dials a remote party or one reaches it
    /// (`remote_party_command`, `remote_party_event`).
    pub fn spawn_deferring_relays(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        host_identity: Option<HostIdentity>,
        listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
        reachability: Reachability,
    ) -> (mpsc::Sender<NetCmd>, tokio::task::JoinHandle<()>) {
        Self::spawn_with_heartbeat(
            endpoint,
            lookup,
            host_identity,
            listen_addrs,
            HEARTBEAT_INTERVAL,
            reachability,
            None,
        )
    }

    /// [`Self::spawn_deferring_relays`] for a device that may govern rooms:
    /// with `governing`, its endpoint also answers the workspace-join
    /// exchange for them ([`super::governor_join::GoverningRooms`]), as a
    /// relay's does.
    pub fn spawn_governing(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        host_identity: Option<HostIdentity>,
        listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
        reachability: Reachability,
        governing: Option<super::governor_join::GoverningRooms>,
    ) -> (mpsc::Sender<NetCmd>, tokio::task::JoinHandle<()>) {
        Self::spawn_with_heartbeat(
            endpoint,
            lookup,
            host_identity,
            listen_addrs,
            HEARTBEAT_INTERVAL,
            reachability,
            governing,
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
        reachability: Reachability,
        governing: Option<super::governor_join::GoverningRooms>,
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
            reachability,
            governing,
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
    private_messages: super::private_messages::Transport,
    native_media: super::native_media::Registry,
    native_enrollment: super::invite::enrollment_driver::Registry,
    native_routes: native_ingress::Routes,
    /// Governance invocations waiting for their room's relay to answer.
    room_governance: room_governance::Waiting,
    /// The endpoint's relays, held back until a remote party appears.
    reachability: Reachability,
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
    /// Approver sessions that put this person's agents' questions on the
    /// sheet, one per connected host.
    agent_questions: super::agent_questions::Answering,
    peers_seen: HashMap<(WorkspaceId, EndpointId), PeerMembership>,
    next_membership_epoch: u64,
    /// Inbound ALPN connections are bound once to the membership active on
    /// their first frame. `None` is a tombstone retained until that concrete
    /// connection closes, so an old connection cannot rebind after reconnect.
    personal_fabric_ingress: HashMap<(EndpointId, u64), Option<(WorkspaceId, MembershipEpoch)>>,
    recent: HashSet<(WorkspaceId, [u8; 32])>,
    recent_order: VecDeque<(WorkspaceId, [u8; 32])>,
    /// When each recent remote-input message arrived (see [`mark_new_input`]).
    recent_input: HashMap<(WorkspaceId, [u8; 32]), (EndpointId, Instant)>,
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
    /// At most one direct fanout per shared window may be dialing or
    /// writing. Pending encoded units retain their reference order within
    /// the existing frame byte budget, without stalling the actor. On loss,
    /// wait for the encoder's periodic keyframe before resuming delivery.
    /// Windows are independent, so their units never displace each other.
    surface_fanout_tasks: SurfaceFanoutTasks,
    next_surface_fanout_generation: u64,
    pending_surface_frames: HashMap<(WorkspaceId, SurfaceId), SurfaceFrameBacklog>,
    /// Windows streamed to named viewers only: their frames go to exactly
    /// these devices and never fall back to the workspace's gossip topic.
    surface_fanout_viewers: HashMap<(WorkspaceId, SurfaceId), Vec<EndpointId>>,
    /// Each of the person's devices' fabric connection, while it carries
    /// frames ([`PERSONAL_FABRIC_IDLE`]).
    personal_fabric_connections: HashMap<EndpointId, PersonalFabricLink>,
    /// Fabric connections a newer one replaced, kept until idle too.
    personal_fabric_replaced: Vec<PersonalFabricLink>,
    /// Frames for a peer whose dial is under way, newest last, sent once it
    /// connects ([`PERSONAL_FABRIC_FRAMES_WAITING`] at most).
    personal_fabric_dials: HashMap<EndpointId, std::collections::VecDeque<(WorkspaceId, Vec<u8>)>>,
    /// Numbers each personal fabric connection, dialled or accepted, so a
    /// frame is bound to the one it came on.
    personal_fabric_incarnations: Arc<AtomicU64>,
    active_personal_transfers: HashMap<(EndpointId, [u8; 16]), ActivePersonalTransfer>,
    next_personal_transfer_generation: u64,
    /// Every application message peers delivered here (see [`WireTally`]).
    tally: Arc<WireTally>,
    /// Fingerprint of the codec advert last announced, so a change goes out
    /// and an unchanged advert never does.
    caps_sent: Option<[u8; 32]>,
    /// Remote-action requests that found neither a direct nor a relayed
    /// route. Sent when that peer is next seen on the workspace.
    pending_remote_actions: PendingRemoteActions,
    /// Pointer moves held back from gossip, per slot, to the rate the relay
    /// path writes them (see [`ActorState::pace_gossip`]).
    gossip_pointers: PointerPacer<GossipPointer>,
}

/// An encoded pointer move waiting for its room's gossip topic.
struct GossipPointer {
    ws: WorkspaceId,
    bytes: Bytes,
}

/// Every application message this device received from its peers, by kind:
/// gossip, workspace-host and direct-stream traffic alike, counted where it
/// arrives and before any deduplication. Transport keep-alives (QUIC PINGs,
/// gossip membership upkeep) never reach the application and are not here.
/// What an idle session costs its peers and hosts is read from this.
#[derive(Debug, Default)]
pub struct WireTally {
    counts: Mutex<std::collections::BTreeMap<&'static str, u64>>,
}

impl WireTally {
    fn note(&self, kind: &'static str) {
        self.counts
            .lock_recover()
            .entry(kind)
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
    }

    /// The counts so far, by message kind.
    pub fn snapshot(&self) -> std::collections::BTreeMap<&'static str, u64> {
        self.counts.lock_recover().clone()
    }
}

/// One running outgoing transfer stream. Dropping `cancel` also stops it.
struct ActivePersonalTransfer {
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

/// The actor-side bindings one streaming task reports through.
struct PersonalTransferTask {
    internal: mpsc::Sender<InternalEvent>,
    sent_bytes: std::sync::Arc<std::sync::atomic::AtomicU64>,
    generation: u64,
    cancel: tokio::sync::watch::Receiver<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersonalTransferOutcome {
    Completed,
    Failed,
    Cancelled,
}

fn take_personal_transfer_generation(
    active: &mut HashMap<(EndpointId, [u8; 16]), ActivePersonalTransfer>,
    key: (EndpointId, [u8; 16]),
    generation: u64,
) -> Option<ActivePersonalTransfer> {
    active
        .get(&key)
        .is_some_and(|entry| entry.generation == generation)
        .then(|| active.remove(&key))
        .flatten()
}

#[derive(Clone, Copy)]
struct PeerMembership {
    /// When the membership lapses unless the peer is heard from again;
    /// `None` while its gossip link is up, when the transport is the
    /// liveness check (see [`DEPARTURE_GRACE`]).
    expires: Option<Instant>,
    epoch: MembershipEpoch,
    /// Fingerprint of the last codec advert taken from the peer during this
    /// membership: a repeated advert changes nothing, but one repeated after
    /// the peer was evicted and came back is new again.
    caps_advert: Option<[u8; 32]>,
}

impl PeerMembership {
    fn new(epoch: MembershipEpoch, now: Instant) -> Self {
        Self {
            expires: Some(later(now, PEER_TIMEOUT)),
            epoch,
            caps_advert: None,
        }
    }

    /// The peer was heard from. A linked peer needs no deadline; one known
    /// only by its traffic stays a member for [`PEER_TIMEOUT`] after it.
    fn heard(&mut self, now: Instant) {
        if let Some(expires) = &mut self.expires {
            *expires = (*expires).max(later(now, PEER_TIMEOUT));
        }
    }

    /// Take codec advert `fingerprint`, answering whether it is news from
    /// this peer.
    fn take_caps_advert(&mut self, fingerprint: [u8; 32]) -> bool {
        self.caps_advert.replace(fingerprint) != Some(fingerprint)
    }
}

const MAX_PENDING_REMOTE_ACTIONS: usize = 8;
/// A request older than this is dropped rather than run long after the
/// person who sent it has moved on.
const PENDING_REMOTE_ACTION_TTL: Duration = Duration::from_secs(30);

struct PendingRemoteAction {
    ws: WorkspaceId,
    target: EndpointId,
    frame: Vec<u8>,
    queued_at: Instant,
}

#[derive(Default)]
struct PendingRemoteActions {
    items: VecDeque<PendingRemoteAction>,
}

impl PendingRemoteActions {
    fn hold(&mut self, ws: WorkspaceId, target: EndpointId, frame: Vec<u8>, now: Instant) {
        self.expire(now);
        if self.items.len() >= MAX_PENDING_REMOTE_ACTIONS {
            self.items.pop_front();
        }
        self.items.push_back(PendingRemoteAction {
            ws,
            target,
            frame,
            queued_at: now,
        });
    }

    fn expire(&mut self, now: Instant) {
        self.items.retain(|item| {
            now.saturating_duration_since(item.queued_at) < PENDING_REMOTE_ACTION_TTL
        });
    }

    /// The frames held for `target` on `ws`, oldest first.
    fn take_for(&mut self, ws: WorkspaceId, target: EndpointId, now: Instant) -> Vec<Vec<u8>> {
        self.expire(now);
        let (ready, kept) = std::mem::take(&mut self.items)
            .into_iter()
            .partition::<VecDeque<_>, _>(|item| item.ws == ws && item.target == target);
        self.items = kept;
        ready.into_iter().map(|item| item.frame).collect()
    }

    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn targets(&self) -> Vec<(WorkspaceId, EndpointId)> {
        let mut seen = Vec::new();
        for item in &self.items {
            if !seen.contains(&(item.ws, item.target)) {
                seen.push((item.ws, item.target));
            }
        }
        seen
    }
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
        let now = Instant::now();
        if let Some(membership) = self.peers_seen.get_mut(&(ws, peer)) {
            membership.heard(now);
            return (membership.epoch, false);
        }
        self.next_membership_epoch = self.next_membership_epoch.saturating_add(1).max(1);
        let epoch = MembershipEpoch(self.next_membership_epoch);
        self.peers_seen
            .insert((ws, peer), PeerMembership::new(epoch, now));
        (epoch, true)
    }

    fn touch_existing_membership(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
    ) -> Option<MembershipEpoch> {
        let membership = self.peers_seen.get_mut(&(ws, peer))?;
        membership.heard(Instant::now());
        Some(membership.epoch)
    }

    /// The gossip link to `peer` on `ws` came up (`up`) or went down. While
    /// it is up the transport vouches for the peer: QUIC keep-alives flow
    /// under it and its idle timeout ([`PEER_TIMEOUT`]) ends it. Its going
    /// down does not end the membership at once: gossip names no connection
    /// incarnation on a down edge, so a reconnect racing it must not lose the
    /// peer. The membership lapses [`DEPARTURE_GRACE`] later unless the link
    /// comes back or the peer is heard from.
    fn set_link(&mut self, ws: WorkspaceId, peer: EndpointId, up: bool) {
        let now = Instant::now();
        let Some(membership) = self.peers_seen.get_mut(&(ws, peer)) else {
            return;
        };
        membership.expires = (!up).then_some(later(now, DEPARTURE_GRACE));
    }

    /// The soonest a membership lapses, if any can.
    fn next_eviction(&self) -> Option<Instant> {
        self.peers_seen
            .values()
            .filter_map(|membership| membership.expires)
            .min()
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
        self.cancel_personal_transfer_route(ws, peer);
        revoke_personal_fabric_ingress(&mut self.personal_fabric_ingress, ws, epoch);
        if let Some(link) = self.personal_fabric_connections.remove(&peer) {
            link.connection.close(0_u32.into(), b"membership departed");
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
        if let Some(why) = command.as_ref().and_then(remote_party_command) {
            self.reachability.wake(why);
        }
        match command {
            Some(NetCmd::PrivateMessages(reply)) => {
                let _ = reply.send(self.private_messages.clone());
            }
            Some(NetCmd::WireTally(reply)) => {
                let _ = reply.send(Arc::clone(&self.tally));
            }
            Some(NetCmd::NativeMedia(command)) => {
                let sessions = &self.host_sessions;
                self.native_media
                    .handle(command, self.host_identity.as_ref(), &|ws| {
                        native_ingress::native_session(sessions, ws)
                    });
            }
            Some(NetCmd::NativeConnect(connect, https)) => self.native_connect(connect, https),
            Some(NetCmd::GovernorAction { ws, room, request }) => {
                self.governor_action(ws, room, request);
            }
            Some(NetCmd::NativePublish { ws }) => self.drive_native_publication(ws),
            Some(NetCmd::NativeEnrollment(command)) => self.native_enrollment.handle(
                command,
                self.endpoint.clone(),
                self.host_identity.as_ref(),
            ),
            Some(NetCmd::RoomGovernance(request)) => self.send_room_governance(*request).await,
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
                self.pending_surface_frames
                    .retain(|(frame_ws, _), _| *frame_ws != ws);
                self.surface_fanout_viewers
                    .retain(|(frame_ws, _), _| *frame_ws != ws);
                self.surface_fanout_tasks.retain(|(task_ws, _), task| {
                    if *task_ws == ws {
                        task.1.abort();
                    }
                    *task_ws != ws
                });
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
            Some(NetCmd::AnnounceIdentity { ws }) => {
                if self
                    .topics
                    .get(&ws)
                    .is_some_and(|topic| !topic.announced_identity_lately())
                {
                    self.announce_identity(ws).await;
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
            Some(NetCmd::NativeReplay { ws }) => {
                if let Some(session) = native_ingress::native_session(&self.host_sessions, ws)
                    && let Err(error) = session.resubscribe()
                {
                    tracing::debug!(ws, %error, "joined room replay not asked: not connected");
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
                self.observe_surface_stream(ws, &msg);
                remember_governance(&mut self.governance, ws, &msg);
                if self.native_routes.contains(ws)
                    && matches!(&msg, NeighbourMsg::AppDocument { envelope, .. } if envelope.encryption.is_some())
                {
                    // A sealed write in a room whose records are sealed
                    // channels: the governor never admits it, the relay
                    // retains it for the record's channel members.
                    self.publish_sealed_native(ws, &msg);
                    return true;
                }
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
                    self.surface_fanout_viewers
                        .remove(&(ws, fanout_surface(&msg)));
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
                    self.publish_paced(ws, msg).await;
                }
            }
            Some(NetCmd::SetWake(LoopWake(wake))) => self.wake = Some(wake),
            Some(NetCmd::PublishToHosts { ws, msg }) => {
                self.hold_for_host(ws, &msg, false);
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
            Some(NetCmd::PublishHostedTopology { ws, msg }) => {
                if !self.host_sessions.keys().any(|(topic, _)| *topic == ws) {
                    tracing::debug!(
                        ws,
                        "no host session to publish this session's windows on yet"
                    );
                }
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
            Some(NetCmd::PublishSurfaceTo { ws, msg, viewers }) => {
                self.publish_surface_to(ws, msg, &viewers).await;
            }
            Some(NetCmd::PublishRemoteAction { ws, target, msg }) => {
                self.publish_remote_action(ws, target, msg).await;
            }
            Some(NetCmd::PublishRecordContent(delivery)) => {
                self.publish_record_content(delivery).await;
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
            Some(NetCmd::CancelNearbyTransfer {
                target,
                transfer_id,
            }) => {
                self.cancel_personal_transfer((target, transfer_id));
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
                self.agent_questions
                    .start(&self.endpoint, addr.clone(), identity);
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
        self.agent_questions.stop(id);
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
        let key = (ws, fanout_surface(&msg));
        if self.surface_fanout_tasks.contains_key(&key) {
            self.pending_surface_frames
                .entry(key)
                .or_default()
                .push(msg);
            return true;
        }
        if !self
            .pending_surface_frames
            .entry(key)
            .or_default()
            .admit(&msg)
        {
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
            self.pending_surface_frames
                .entry(key)
                .or_default()
                .lose_references();
            tracing::warn!(ws, bytes = bytes.len(), "surface frame exceeds wire limit");
            return false;
        }
        let only = self.surface_fanout_viewers.get(&key);
        let targets: Vec<_> = self
            .peers
            .values()
            .filter(|addr| self.peers_seen.contains_key(&(ws, addr.id)))
            .filter(|addr| only.is_none_or(|viewers| viewers.contains(&addr.id)))
            .cloned()
            .collect();
        let workspace_peer_count = match only {
            Some(_) => targets.len(),
            None => self
                .peers_seen
                .keys()
                .filter(|(peer_ws, _)| *peer_ws == ws)
                .count(),
        };
        // Each viewer is an independent QUIC route. Dial/write them
        // concurrently so one unreachable peer cannot add 300ms in front of
        // every healthy viewer (or block later collaboration commands once
        // per peer). The wall-clock bound stays one dial timeout plus one
        // write timeout regardless of room size.
        let cached = self.surface_connections.clone();
        let endpoint = self.endpoint.clone();
        let internal_tx = self.internal_tx.clone();
        let Some(generation) = self.next_surface_fanout_generation.checked_add(1) else {
            tracing::warn!(ws, "surface fanout generation exhausted");
            return false;
        };
        self.next_surface_fanout_generation = generation;
        let task = tokio::spawn(async move {
            let mut pending = FuturesUnordered::new();
            for addr in targets {
                let peer = addr.id;
                let cached = cached.get(&peer).cloned();
                let endpoint = endpoint.clone();
                let bytes = bytes.clone();
                pending.push(deliver_surface_frame(endpoint, addr, cached, bytes));
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
                    generation,
                    msg,
                    outcomes,
                    all_delivered: workspace_peer_count > 0 && sent == workspace_peer_count,
                })
                .await;
        });
        self.surface_fanout_tasks.insert(key, (generation, task));
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
        // Every member of the topic, dialled by id: the endpoint knows the
        // paths its gossip link to the peer uses, where an address kept from
        // an earlier dial may name a port a restarted peer no longer has.
        let targets: Vec<EndpointId> = self
            .peers_seen
            .keys()
            .filter(|(topic, peer)| {
                *topic == ws && exact_target.is_none_or(|target| *peer == target)
            })
            .map(|(_, peer)| *peer)
            .collect();
        for peer in targets {
            // A connection either side let go of since is dialled afresh.
            self.personal_fabric_connections
                .retain(|_, link| link.connection.close_reason().is_none());
            if let Some(link) = self.personal_fabric_connections.get_mut(&peer) {
                link.used = Instant::now();
                let connection = link.connection.clone();
                self.send_personal_fabric_frame(ws, peer, &connection, &bytes)
                    .await;
                continue;
            }
            // Dialled in the background, as long as a peer takes to answer:
            // a restarted device is reached only once the endpoint has let
            // go of the paths it had, and a dial cut short every frame never
            // gets that far. Its frames wait meanwhile.
            if let Some(waiting) = self.personal_fabric_dials.get_mut(&peer) {
                if waiting.len() == PERSONAL_FABRIC_FRAMES_WAITING {
                    waiting.pop_front();
                }
                waiting.push_back((ws, bytes.clone()));
                continue;
            }
            self.personal_fabric_dials.insert(
                peer,
                std::collections::VecDeque::from([(ws, bytes.clone())]),
            );
            let endpoint = self.endpoint.clone();
            let tx = self.internal_tx.clone();
            let incarnations = Arc::clone(&self.personal_fabric_incarnations);
            tokio::spawn(async move {
                let connection = tokio::time::timeout(
                    PEER_TIMEOUT,
                    endpoint.connect(peer, PERSONAL_FABRIC_ALPN),
                )
                .await
                .ok()
                .and_then(Result::ok);
                if let Some(connection) = &connection {
                    read_personal_fabric(connection.clone(), tx.clone(), &incarnations);
                }
                let _ = tx
                    .send(InternalEvent::PersonalFabricConnection { peer, connection })
                    .await;
            });
        }
    }

    /// [`InternalEvent::PersonalFabricConnection`]: keep the connection,
    /// the peer's newest, and send what waited for it.
    async fn personal_fabric_connected(
        &mut self,
        peer: EndpointId,
        connection: Option<iroh::endpoint::Connection>,
    ) {
        let waiting = self.personal_fabric_dials.remove(&peer).unwrap_or_default();
        let Some(connection) = connection else {
            tracing::debug!(peer = %peer, "personal fabric peer did not answer");
            return;
        };
        let replaced = self.personal_fabric_connections.insert(
            peer,
            PersonalFabricLink {
                connection: connection.clone(),
                used: Instant::now(),
            },
        );
        // The connection it replaces may still bring frames the peer sent on
        // it: it goes once idle, as any other.
        if let Some(replaced) =
            replaced.filter(|link| link.connection.stable_id() != connection.stable_id())
        {
            self.personal_fabric_replaced.push(replaced);
        }
        for (ws, bytes) in waiting {
            self.send_personal_fabric_frame(ws, peer, &connection, &bytes)
                .await;
        }
    }

    async fn send_personal_fabric_frame(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
        connection: &iroh::endpoint::Connection,
        bytes: &[u8],
    ) {
        let send = async {
            let mut stream = connection.open_uni().await.map_err(|_| ())?;
            stream.write_all(bytes).await.map_err(|_| ())?;
            stream.finish().map_err(|_| ())?;
            Ok::<(), ()>(())
        };
        if !matches!(
            tokio::time::timeout(Duration::from_millis(100), send).await,
            Ok(Ok(()))
        ) {
            tracing::debug!(ws, peer = %peer, "personal fabric frame not sent; dialling afresh next time");
            self.personal_fabric_connections.remove(&peer);
        }
    }

    /// Report one outgoing transfer's progress. A terminal status is
    /// delivered even when the subscriber is momentarily full: losing it
    /// would leave the sender waiting on a stream that has ended.
    fn report_personal_transfer(
        &self,
        workspace: WorkspaceId,
        key: (EndpointId, [u8; 16]),
        sent_bytes: u64,
        total_bytes: u64,
        status: PersonalTransferStatus,
    ) {
        let Some(progress) = self.subscribers.get(&workspace).cloned() else {
            return;
        };
        let event = NetEvent::NearbyTransferProgress {
            target: key.0,
            transfer_id: key.1,
            sent_bytes: sent_bytes.min(total_bytes),
            total_bytes,
            status,
        };
        if matches!(
            status,
            PersonalTransferStatus::Started | PersonalTransferStatus::Progress
        ) {
            let _ = progress.try_send(event);
        } else {
            tokio::spawn(async move {
                let _ = progress.send(event).await;
            });
        }
    }

    fn cancel_personal_transfer(&mut self, key: (EndpointId, [u8; 16])) -> bool {
        let Some(active) = self.active_personal_transfers.remove(&key) else {
            return false;
        };
        let _ = active.cancel.send(true);
        self.report_personal_transfer(
            active.workspace,
            key,
            active.sent_bytes.load(std::sync::atomic::Ordering::Relaxed),
            active.total_bytes,
            PersonalTransferStatus::Cancelled,
        );
        true
    }

    fn cancel_personal_transfer_route(&mut self, workspace: WorkspaceId, target: EndpointId) {
        let keys = self
            .active_personal_transfers
            .iter()
            .filter_map(|(key, active)| {
                (key.0 == target && active.workspace == workspace).then_some(*key)
            })
            .collect::<Vec<_>>();
        for key in keys {
            self.cancel_personal_transfer(key);
        }
    }

    fn publish_nearby_transfer(
        &mut self,
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
        let key = (target, transfer_id);
        // A fresh checkpoint for the same object supersedes the running
        // stream, whose stale completion the generation then ignores.
        self.active_personal_transfers.remove(&key);
        let failed = |actor: &Self| {
            actor.report_personal_transfer(
                ws,
                key,
                resume_offset,
                total_bytes,
                PersonalTransferStatus::Failed,
            );
        };
        if transfer_id == [0; 16]
            || request_id == [0; 16]
            || resume_offset > total_bytes
            || !self.peers_seen.contains_key(&(ws, target))
        {
            failed(self);
            return;
        }
        let Some(addr) = self.peers.get(&target).cloned() else {
            failed(self);
            return;
        };
        if self.active_personal_transfers.len() >= MAX_ACTIVE_PERSONAL_TRANSFERS {
            tracing::warn!(peer = %target, "refusing transfer while source streams are full");
            failed(self);
            return;
        }
        self.next_personal_transfer_generation =
            self.next_personal_transfer_generation.wrapping_add(1);
        let generation = self.next_personal_transfer_generation;
        let sent_bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(resume_offset));
        let (cancel, cancel_rx) = tokio::sync::watch::channel(false);
        self.active_personal_transfers.insert(
            key,
            ActivePersonalTransfer {
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
        let internal = self.internal_tx.clone();
        self.report_personal_transfer(
            ws,
            key,
            resume_offset,
            total_bytes,
            PersonalTransferStatus::Started,
        );
        tokio::spawn(async move {
            let outcome = stream_nearby_transfer(
                route,
                transfer,
                file,
                PersonalTransferTask {
                    internal: internal.clone(),
                    sent_bytes: std::sync::Arc::clone(&sent_bytes),
                    generation,
                    cancel: cancel_rx,
                },
            )
            .await;
            let _ = internal
                .send(InternalEvent::PersonalTransferFinished {
                    target,
                    transfer_id,
                    generation,
                    outcome,
                    sent_bytes: sent_bytes.load(std::sync::atomic::Ordering::Relaxed),
                })
                .await;
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
        if !matches!(
            msg,
            NeighbourMsg::RemoteInput { .. }
                | NeighbourMsg::SurfaceKeyframeRequest { .. }
                | NeighbourMsg::SurfaceViewRequest { .. }
        ) {
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

    fn observe_surface_stream(&mut self, ws: WorkspaceId, msg: &NeighbourMsg) {
        match msg {
            NeighbourMsg::SurfaceStreamStart { surface, codec, .. } => {
                if let Some((_, task)) = self.surface_fanout_tasks.remove(&(ws, *surface)) {
                    task.abort();
                }
                let backlog = self
                    .pending_surface_frames
                    .entry((ws, *surface))
                    .or_default();
                backlog.codec = media::StreamCodec::from_wire_label(codec);
                backlog.lose_references();
            }
            NeighbourMsg::SurfaceStreamEnd { surface, .. } => {
                self.pending_surface_frames.remove(&(ws, *surface));
                if let Some((_, task)) = self.surface_fanout_tasks.remove(&(ws, *surface)) {
                    task.abort();
                }
            }
            _ => {}
        }
    }

    /// [`NetCmd::PublishSurfaceTo`]: one window's stream to exactly its
    /// viewers. Frames take the per-window media fanout restricted to them;
    /// the stream's start, end and control grants go to each directly.
    async fn publish_surface_to(&mut self, ws: WorkspaceId, msg: NeighbourMsg, viewers: &[u64]) {
        self.observe_surface_stream(ws, &msg);
        let targets = self
            .peers
            .keys()
            .filter(|peer| self.peers_seen.contains_key(&(ws, **peer)))
            .filter(|peer| viewers.contains(&crate::network::behaviour::peer_liveness_handle(peer)))
            .copied()
            .collect::<Vec<_>>();
        match msg {
            NeighbourMsg::SurfaceFrame { .. } => {
                let key = (ws, fanout_surface(&msg));
                self.surface_fanout_viewers.insert(key, targets);
                let _ = self.start_surface_fanout(ws, msg);
            }
            NeighbourMsg::SurfaceStreamStart { .. }
            | NeighbourMsg::SurfaceStreamEnd { .. }
            | NeighbourMsg::PermissionGrant { .. } => {
                if let NeighbourMsg::SurfaceStreamEnd { surface, .. } = &msg {
                    self.surface_fanout_viewers.remove(&(ws, *surface));
                }
                for target in targets {
                    let _ = self.publish_direct(ws, target, msg.clone()).await;
                }
            }
            _ => tracing::warn!(ws, "refused a non-stream message on the viewer route"),
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
            tracing::warn!(ws, peer = %target, "no direct or relayed route for a remote action; held until the peer is seen");
            self.pending_remote_actions
                .hold(ws, target, frame, Instant::now());
        }
    }

    /// Send `request` to whoever it is addressed to ([`room_governance`]): a
    /// room's relay device on any session of the room, or a host on its own
    /// session of the workspace once that session is subscribed.
    async fn send_room_governance(&mut self, request: room_governance::Request) {
        let (ws, to) = (request.ws, request.to);
        match to {
            room_governance::Addressee::Device(host) => {
                if let Some(frame) = self.room_governance.send(request, host, Instant::now()) {
                    self.publish_remote_action(ws, host, NeighbourMsg::RemoteAction { frame })
                        .await;
                }
            }
            room_governance::Addressee::Host(endpoint) => {
                let subscribed = self
                    .host_sessions
                    .get(&(ws, endpoint))
                    .is_some_and(|runtime| {
                        !runtime.replaying.load(std::sync::atomic::Ordering::Acquire)
                    });
                if subscribed {
                    self.send_to_host(request, endpoint);
                } else {
                    // Its session is dialled with every topic this device
                    // joined; the request goes once it is subscribed.
                    self.room_governance.hold(request, Instant::now());
                }
            }
        }
    }

    /// The host at `peer` subscribed this device to `ws`: refresh what its
    /// native session holds and send the requests that waited for it.
    fn host_ready(&mut self, ws: WorkspaceId, peer: EndpointId) -> NetEvent {
        // The room's own host caught this device up: the room holds what it
        // holds. Another host on the topic (a personal relay, a peer) says
        // nothing about the room's host.
        if !self.native_routes.contains(ws)
            || self
                .host_sessions
                .get(&(ws, peer))
                .is_some_and(|runtime| runtime.native_scope.is_some())
        {
            crate::shell::room_sync::synced(ws);
        }
        self.request_native_refresh(ws);
        for request in self.room_governance.ready_for(ws, peer) {
            self.send_to_host(request, peer);
        }
        NetEvent::HostReady { ws, peer }
    }

    /// Send `request` on the session of its workspace with the host at
    /// `endpoint`, to the device that session's handshake authenticated.
    fn send_to_host(&mut self, request: room_governance::Request, endpoint: EndpointId) {
        let Some(session) = self
            .host_sessions
            .get(&(request.ws, endpoint))
            .map(|runtime| Arc::clone(&runtime.session))
        else {
            let _ = request.reply.send(Err("the host's session closed".into()));
            return;
        };
        let device = session.relay_device_public_key();
        let Ok(host) = EndpointId::from_bytes(&device) else {
            let _ = request
                .reply
                .send(Err("the host named an invalid device".into()));
            return;
        };
        if let Some(frame) = self.room_governance.send(request, host, Instant::now())
            && let Err(error) = session.publish_remote_action(device, frame)
        {
            // Its caller hears nothing and gives up in its own time.
            tracing::warn!(%error, host = %endpoint, "a request to a host was not sent");
        }
    }

    async fn flush_pending_remote_actions(&mut self, ws: WorkspaceId, peer: EndpointId) {
        for frame in self
            .pending_remote_actions
            .take_for(ws, peer, Instant::now())
        {
            self.publish_remote_action(ws, peer, NeighbourMsg::RemoteAction { frame })
                .await;
        }
    }

    async fn publish_record_content(&mut self, delivery: RecordContentDelivery) {
        let RecordContentDelivery { ws, target, msg } = delivery;
        if !matches!(
            msg,
            NeighbourMsg::RecordContentPiece { .. } | NeighbourMsg::RecordContentLacks { .. }
        ) {
            tracing::warn!(
                ws,
                "refused a non-content message on the record-content route"
            );
            return;
        }
        if let Some(target) = target
            && self.peers_seen.contains_key(&(ws, target))
            && self.publish_direct(ws, target, msg.clone()).await
        {
            return;
        }
        // The workspace hosts route it to that device alone when it is
        // connected to them.
        if let Some(target) = target
            && let Ok(frame) = postcard::to_allocvec(&msg)
            && self
                .host_sessions
                .iter()
                .filter(|((session_ws, _), _)| *session_ws == ws)
                .any(|(_, runtime)| {
                    runtime
                        .session
                        .publish_record_content(*target.as_bytes(), frame.clone())
                        .is_ok()
                })
        {
            return;
        }
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
        let budget = if matches!(
            msg,
            NeighbourMsg::RemoteAction { .. }
                | NeighbourMsg::RecordContentPiece { .. }
                | NeighbourMsg::RecordContentLacks { .. }
        ) {
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
        if self
            .surface_connections
            .get(&target)
            .is_some_and(|connection| connection.close_reason().is_some())
        {
            self.surface_connections.remove(&target);
        }
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
        let delivered = tokio::select! {
            biased;
            _ = connection.closed() => false,
            result = tokio::time::timeout(budget, send) => matches!(result, Ok(Ok(()))),
        };
        if delivered {
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
        self.subscribe_topic(ws).await;
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

    /// Subscribe to `ws`'s gossip topic unless a subscription is running.
    /// A subscribe that failed, or one whose stream has closed, is retried
    /// from the heartbeat while the workspace is still wanted.
    async fn subscribe_topic(&mut self, ws: WorkspaceId) {
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
                    e.insert(TopicRuntime::new(sender, pump));
                }
                Err(error) => tracing::warn!(ws, %error, "could not join iroh workspace topic"),
            }
        }
    }

    /// Workspaces that are joined but have no running gossip subscription.
    async fn resubscribe_missing_topics(&mut self) {
        let missing: Vec<_> = self
            .subscribers
            .keys()
            .copied()
            .filter(|ws| !self.topics.contains_key(ws))
            .collect();
        for ws in missing {
            self.subscribe_topic(ws).await;
        }
    }

    /// Known direct peers are joined when they are first learned. A device
    /// whose network came up after that attempt never appears, so ask again
    /// on each topic until the peer is actually seen there.
    async fn retry_direct_peers(&self) {
        let local = self.endpoint.id();
        for (ws, topic) in &self.topics {
            let absent: Vec<_> = self
                .peers
                .keys()
                .copied()
                .filter(|id| *id != local && !self.peers_seen.contains_key(&(*ws, *id)))
                .collect();
            if absent.is_empty() {
                continue;
            }
            if let Err(error) = topic.sender.join_peers(absent).await {
                tracing::debug!(ws, %error, "direct peer join will be retried");
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
        if let Some(why) = internal.as_ref().and_then(remote_party_event) {
            self.reachability.wake(why);
        }
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
                result,
            }) => self.native_host_connected(ws, serial, *hint, certificate, result),
            Some(InternalEvent::Host {
                ws,
                peer,
                session,
                event,
            }) => {
                // A room's governance answering this device is taken here:
                // its caller waits off the thread other answers go to.
                if self.host_sessions.get(&(ws, peer)).is_some_and(|runtime| {
                    std::sync::Weak::ptr_eq(&session, &Arc::downgrade(&runtime.session))
                }) && !self.room_governance.takes(ws, &event)
                {
                    self.handle_host_event(ws, peer, event).await;
                }
            }
            Some(InternalEvent::SurfaceFrame {
                ws,
                peer,
                fingerprint,
                msg,
            }) => {
                self.tally.note(msg.kind());
                self.handle_surface_frame(ws, peer, fingerprint, msg).await;
            }
            Some(InternalEvent::SurfaceFanoutComplete {
                ws,
                generation,
                msg,
                outcomes,
                all_delivered,
            }) => {
                self.handle_surface_fanout_complete(ws, generation, msg, outcomes, all_delivered)
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
            Some(InternalEvent::PersonalFabricConnection { peer, connection }) => {
                self.personal_fabric_connected(peer, connection).await;
            }
            Some(InternalEvent::PersonalFabricClosed {
                peer,
                connection_incarnation,
            }) => {
                self.personal_fabric_ingress
                    .remove(&(peer, connection_incarnation));
            }
            Some(InternalEvent::PersonalTransferProgress {
                target,
                transfer_id,
                generation,
                sent_bytes,
            }) => {
                let key = (target, transfer_id);
                if let Some((workspace, total_bytes)) = self
                    .active_personal_transfers
                    .get(&key)
                    .filter(|active| active.generation == generation)
                    .map(|active| (active.workspace, active.total_bytes))
                {
                    self.report_personal_transfer(
                        workspace,
                        key,
                        sent_bytes,
                        total_bytes,
                        PersonalTransferStatus::Progress,
                    );
                }
            }
            Some(InternalEvent::PersonalTransferFinished {
                target,
                transfer_id,
                generation,
                outcome,
                sent_bytes,
            }) => {
                self.handle_personal_transfer_finished(
                    (target, transfer_id),
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
        generation: u64,
        msg: NeighbourMsg,
        outcomes: Vec<SurfaceFanoutOutcome>,
        all_delivered: bool,
    ) {
        let key = (ws, fanout_surface(&msg));
        if !finish_surface_fanout(&mut self.surface_fanout_tasks, key, generation) {
            return;
        }
        if !self.subscribers.contains_key(&ws) {
            self.pending_surface_frames.remove(&key);
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
        // A window streamed to named viewers only never falls back to the
        // workspace topic: a frame that missed them is superseded by the next.
        let targeted = self.surface_fanout_viewers.contains_key(&key);
        if !all_delivered {
            self.pending_surface_frames
                .entry(key)
                .or_default()
                .lose_references();
        }
        if !all_delivered && !targeted {
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
        if let Some(next) = self
            .pending_surface_frames
            .get_mut(&key)
            .and_then(SurfaceFrameBacklog::pop)
            && !self.start_surface_fanout(ws, next.clone())
            && !targeted
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
        self.tally.note("PersonalFabric");
        if let Some(link) = self.personal_fabric_connections.get_mut(&peer) {
            link.used = Instant::now();
        }
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

    /// [`InternalEvent::PersonalTransferFinished`] arm of [`Self::handle_internal`],
    /// split out to keep that dispatcher short. Only the stream that still
    /// owns its binding reports; a replaced or cancelled one is silent.
    fn handle_personal_transfer_finished(
        &mut self,
        key: (EndpointId, [u8; 16]),
        generation: u64,
        outcome: PersonalTransferOutcome,
        sent_bytes: u64,
    ) {
        if let Some(active) =
            take_personal_transfer_generation(&mut self.active_personal_transfers, key, generation)
        {
            let status = match outcome {
                PersonalTransferOutcome::Completed => PersonalTransferStatus::Completed,
                PersonalTransferOutcome::Failed => PersonalTransferStatus::Failed,
                PersonalTransferOutcome::Cancelled => PersonalTransferStatus::Cancelled,
            };
            self.report_personal_transfer(
                active.workspace,
                key,
                sent_bytes,
                active.total_bytes,
                status,
            );
        }
    }

    fn joined_gossip_peer(&mut self, ws: WorkspaceId, peer: EndpointId) -> (NetEvent, bool) {
        let (membership_epoch, fresh) = self.touch_or_mint_membership(ws, peer);
        self.set_link(ws, peer, true);
        (
            NetEvent::PeerJoined {
                ws,
                peer,
                membership_epoch,
            },
            fresh,
        )
    }

    /// A neighbour on `ws`, traffic only a neighbour delivers, or a lag that
    /// may have hidden one: from here on the topic broadcasts
    /// ([`TopicRuntime::broadcast`]).
    fn note_topic_linked(&mut self, ws: WorkspaceId, event: &GossipEvent) {
        if !matches!(event, GossipEvent::NeighborDown(_))
            && let Some(topic) = self.topics.get_mut(&ws)
        {
            topic.linked = true;
        }
    }

    /// [`InternalEvent::Topic`] arm of [`Self::handle_internal`], split out
    /// to keep that dispatcher short.
    /// Whether a message `peer` sent on `ws` is news. A codec advert
    /// describes its sender rather than reporting an event: it is news when
    /// it differs from the last one this membership took, so a peer evicted
    /// and back is heard again though it says the same thing. An identity
    /// hello is always news: it comes once per link or as an answer
    /// ([`IDENTITY_ANSWER_INTERVAL`]), and the session binds the peer's
    /// current membership to the identity each time.
    fn is_news(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
        decoded: Option<&NeighbourMsg>,
        fingerprint: [u8; 32],
    ) -> bool {
        match decoded {
            Some(NeighbourMsg::IdentityHello { .. }) => true,
            Some(NeighbourMsg::MediaCapabilities { .. }) => self
                .peers_seen
                .get_mut(&(ws, peer))
                .is_some_and(|membership| membership.take_caps_advert(fingerprint)),
            _ => mark_new_message(ws, fingerprint, &mut self.recent, &mut self.recent_order),
        }
    }

    async fn handle_topic_event(&mut self, ws: WorkspaceId, event: GossipEvent) {
        self.note_topic_linked(ws, &event);
        if matches!(event, GossipEvent::NeighborUp(_)) {
            // Authenticate this device, and say what it can decode, as soon
            // as the link is usable: once per link, never on a timer. State
            // sync can otherwise arrive before the identity and be rejected
            // by the peer.
            self.announce_identity(ws).await;
            self.announce_media_caps(ws).await;
        }
        let Some(tx) = self.subscribers.get(&ws).cloned() else {
            return;
        };
        let mut joined_anew = None;
        let event = match event {
            GossipEvent::NeighborUp(peer) => {
                let (joined, fresh) = self.joined_gossip_peer(ws, peer);
                joined_anew = fresh.then_some(peer);
                Some(joined)
            }
            GossipEvent::NeighborDown(peer) => {
                // Gossip does not attach a connection incarnation to a down
                // edge. Revoking the current epoch here could let a delayed
                // Down(N) kill a newer Up(N+1): the membership only starts
                // its departure grace, which a new link cancels.
                tracing::debug!(ws, peer = %peer, "gossip neighbour down; membership lapses unless it returns");
                self.set_link(ws, peer, false);
                None
            }
            GossipEvent::Received(message) => {
                let peer = message.delivered_from;
                let (membership_epoch, rediscovered) = self.touch_or_mint_membership(ws, peer);
                if rediscovered {
                    joined_anew = Some(peer);
                }
                let decoded = postcard::from_bytes::<NeighbourMsg>(&message.content);
                self.tally
                    .note(decoded.as_ref().map_or("Undecodable", NeighbourMsg::kind));
                // An identity announcement from another peer protocol version
                // may not decode at this one; its frozen prefix still says
                // which, and so which of the two devices has to update.
                if decoded.is_err()
                    && let Some(theirs) = neighbour_wire::identity_hello_version(&message.content)
                {
                    crate::shell::collaborator_versions::note(
                        peer.to_string(),
                        String::new(),
                        theirs,
                    );
                }
                let fingerprint = message_fingerprint(&message.content);
                if matches!(decoded, Ok(NeighbourMsg::IdentityHello { .. }))
                    && self
                        .topics
                        .get(&ws)
                        .is_some_and(|topic| !topic.announced_identity_lately())
                {
                    self.announce_identity(ws).await;
                }
                if !self.is_news(ws, peer, decoded.as_ref().ok(), fingerprint) {
                    // A restarted device repeats a message already seen, such
                    // as its identity hello. The payload is old but the
                    // membership is new, and without a join the peer stays
                    // out of the live set.
                    if rediscovered {
                        let _ = tx
                            .send(NetEvent::PeerJoined {
                                ws,
                                peer,
                                membership_epoch,
                            })
                            .await;
                        self.flush_pending_remote_actions(ws, peer).await;
                    }
                    return;
                }
                match decoded {
                    // Liveness is the transport's: a heartbeat says only that
                    // its sender is here.
                    Ok(NeighbourMsg::Heartbeat { .. }) => {
                        rediscovered.then_some(NetEvent::PeerJoined {
                            ws,
                            peer,
                            membership_epoch,
                        })
                    }
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
        if let Some(peer) = joined_anew {
            self.flush_pending_remote_actions(ws, peer).await;
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
                self.announce_identity_to_new_host(ws, &session);
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

    /// The host `host` of `ws` reports another member device joined
    /// (`active`) or left its live plane; `None` for this device itself.
    fn host_member(
        &self,
        ws: WorkspaceId,
        host: EndpointId,
        device: [u8; 32],
        active: bool,
    ) -> Option<NetEvent> {
        EndpointId::from_bytes(&device)
            .ok()
            .filter(|device| !self.is_own_device(device))
            .map(|device| NetEvent::HostMember {
                ws,
                host,
                device,
                active,
            })
    }

    /// [`InternalEvent::Host`] arm of [`Self::handle_internal`], split out
    /// to keep that dispatcher short.
    async fn handle_host_event(&mut self, ws: WorkspaceId, peer: EndpointId, event: HostEvent) {
        self.tally.note(match &event {
            HostEvent::Message { msg, .. } => msg.kind(),
            HostEvent::NativeInput(_) => "HostNativeInput",
            HostEvent::Ready => "HostReady",
            HostEvent::Live { .. } => "HostLive",
            HostEvent::Member { .. } => "HostMember",
            HostEvent::Voice(_) => "HostVoice",
            HostEvent::Closed => "HostClosed",
        });
        let event = self.admit_native_governance(ws, event).await;
        let mapped = match event {
            HostEvent::NativeInput(input) => Some(NetEvent::NativeInput(input)),
            HostEvent::Ready => Some(self.host_ready(ws, peer)),
            HostEvent::Live { from, frame } => Some(NetEvent::Live { ws, from, frame }),
            HostEvent::Member { device, active } => self.host_member(ws, peer, device, active),
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
                    NeighbourMsg::RemoteInput { .. } => mark_new_input(
                        ws,
                        peer,
                        fingerprint,
                        Instant::now(),
                        &mut self.recent_input,
                    ),
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
                        | NeighbourMsg::SurfaceKeyframeRequest { .. }
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

    /// Publish `msg` to the room's hosts and its gossip topic, pacing pointer
    /// moves on the direct path as the session writer paces them to a relay
    /// (`tilo_relay_session::pointer_pacing`): a pointer slot goes to gossip
    /// at most every `POINTER_GAP`, the first move after a pause at once and
    /// the latest when the gap ends, so the resting position always goes.
    /// The hosts get every message at once; their session writers pace it.
    ///
    /// Only the message's own slot decides, never its author: a person's and
    /// an agent's pointer travel alike. Carets, selections and typing pass a
    /// held pointer; anything else is broadcast after every held pointer, so
    /// an app's `Left` never overtakes its last move.
    async fn publish_paced(&mut self, ws: WorkspaceId, msg: NeighbourMsg) {
        let pacing = slot_pacing(ws, msg.presence_slot());
        if let Pacing::Pointer(key) = pacing
            && self.topics.contains_key(&ws)
        {
            let bytes = match postcard::to_allocvec(&msg) {
                Ok(bytes) => Bytes::from(bytes),
                Err(error) => {
                    tracing::warn!(ws, %error, "collaboration message encode failed");
                    return;
                }
            };
            // Hosts first, as `publish` does: the direct mesh never delays
            // the hosted path.
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
            let now = tokio::time::Instant::now();
            self.gossip_pointers
                .hold(key, GossipPointer { ws, bytes }, now);
            let due = self.gossip_pointers.take_due(now);
            self.broadcast_pointers(due).await;
            return;
        }
        if pacing == Pacing::Ordered {
            let held = self.gossip_pointers.take_all(tokio::time::Instant::now());
            self.broadcast_pointers(held).await;
        }
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

    /// Broadcast the held pointer moves whose gap has ended (the run loop
    /// wakes for [`PointerPacer::next_due`], armed only while one is held).
    async fn broadcast_due_pointers(&mut self) {
        let due = self.gossip_pointers.take_due(tokio::time::Instant::now());
        self.broadcast_pointers(due).await;
    }

    async fn broadcast_pointers(&self, pointers: Vec<GossipPointer>) {
        for GossipPointer { ws, bytes } in pointers {
            // A room left since the move was held has nobody to tell.
            let Some(topic) = self.topics.get(&ws) else {
                continue;
            };
            if let Err(error) = topic.broadcast(bytes).await {
                tracing::warn!(ws, %error, "iroh gossip publish failed");
            }
        }
    }

    /// Drop the peers whose membership lapsed: their gossip link went down
    /// and stayed down, or they were known only by traffic that stopped.
    /// Runs at [`Self::next_eviction`], never on a fixed tick.
    async fn run_eviction_tick(&mut self) {
        let now = Instant::now();
        let stale: Vec<_> = self
            .peers_seen
            .iter()
            .filter_map(|(&(ws, peer), membership)| {
                membership
                    .expires
                    .is_some_and(|expires| expires <= now)
                    .then_some((ws, peer, membership.epoch))
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

    /// Whether [`Self::run_heartbeat_tick`] has anything to do: a peer to
    /// keep, retry or tell about a change, a workspace host or a governed
    /// room's relay to keep connected, a remote action held for a peer, or a
    /// topic whose subscription failed. A desktop alone with its rooms has
    /// none of these, so its runtime sleeps until one appears ([`run`]).
    fn needs_upkeep(&self) -> bool {
        !self.peers.is_empty()
            || !self.personal_fabric_connections.is_empty()
            || !self.personal_fabric_replaced.is_empty()
            || !self.peers_seen.is_empty()
            || !self.hosts.is_empty()
            || !self.host_sessions.is_empty()
            || !self.native_routes.is_empty()
            || !self.pending_remote_actions.is_empty()
            || self
                .subscribers
                .keys()
                .any(|ws| !self.topics.contains_key(ws))
    }

    /// The remote party this actor is connected to or keeps dialling, if
    /// any, which holds this device's relays up ([`Need::Transport`]): a
    /// peer it dials, a peer on one of its topics, a workspace host, a
    /// governed room's relay, one of the person's devices, a window streamed
    /// with a peer, a remote action waiting for its peer, a device being
    /// enrolled or a Messages account its contacts text. A peer that left
    /// stops counting once its membership lapses.
    fn remote_party(&self) -> Option<&'static str> {
        Some(if !self.peers.is_empty() {
            "a peer this device dials"
        } else if !self.peers_seen.is_empty() {
            "a peer on one of its topics"
        } else if !self.hosts.is_empty()
            || !self.connecting_hosts.is_empty()
            || !self.host_sessions.is_empty()
        {
            "a workspace host"
        } else if !self.native_routes.is_empty() {
            "a relay-governed room"
        } else if !self.personal_fabric_connections.is_empty()
            || !self.active_personal_transfers.is_empty()
        {
            "one of the person's devices"
        } else if !self.surface_connections.is_empty() || !self.surface_fanout_viewers.is_empty() {
            "a window streamed with a peer"
        } else if !self.pending_remote_actions.is_empty() {
            "a remote action waiting for its peer"
        } else if self.native_enrollment.is_busy() {
            "a device being enrolled"
        } else if self.private_messages.is_bound() {
            "a Messages account receives texts"
        } else {
            return None;
        })
    }

    /// Retry any topic subscription, direct peer or workspace host not yet
    /// connected, and announce this device's identity and codecs where they
    /// changed.
    /// Runs every [`HEARTBEAT_INTERVAL`] while [`Self::needs_upkeep`].
    async fn run_heartbeat_tick(&mut self) {
        self.close_idle_personal_fabric(Instant::now());
        for (ws, peer) in self.pending_remote_actions.targets() {
            if self.peers_seen.contains_key(&(ws, peer)) {
                self.flush_pending_remote_actions(ws, peer).await;
            }
        }
        self.resubscribe_missing_topics().await;
        self.retry_direct_peers().await;
        self.retry_native_routes();
        let rooms: Vec<_> = self.host_sessions.keys().map(|(ws, _)| *ws).collect();
        for ws in rooms {
            self.request_native_refresh(ws);
            self.drive_native_publication(ws);
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
        // Neighbours heard this device's identity and codecs when their link
        // came up; from here on they hear them again only when they change
        // (a renewed certificate, a new revocation, an H.264-only viewer
        // opening). A converged, idle desktop says nothing.
        let workspaces: Vec<_> = self.topics.keys().copied().collect();
        for ws in workspaces {
            self.announce_identity_if_changed(ws).await;
        }
        self.announce_media_caps_if_changed().await;
    }

    /// Let go of the personal fabric connections that carried no frame for
    /// [`PERSONAL_FABRIC_IDLE`] as of `now`, and of those already closed.
    fn close_idle_personal_fabric(&mut self, now: Instant) {
        let keep = |link: &PersonalFabricLink| {
            if link.connection.close_reason().is_some() {
                return false;
            }
            if now.saturating_duration_since(link.used) < PERSONAL_FABRIC_IDLE {
                return true;
            }
            link.connection.close(0_u32.into(), b"idle");
            false
        };
        self.personal_fabric_connections
            .retain(|_, link| keep(link));
        self.personal_fabric_replaced.retain(keep);
    }

    /// The identity announcement this device makes on `ws`: its certificate
    /// for the room and the revocations and successions that come with it.
    fn identity_hello(&self, ws: WorkspaceId) -> Option<NeighbourMsg> {
        let identity = self.host_identity.as_ref()?;
        let (cert, revocations) = identity.material_for(ws);
        Some(NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert),
            revocations,
            successions: identity.successions_for(ws),
        })
    }

    /// Send this device's certificate to its direct neighbors on `ws`.
    ///
    /// Identity announcements are deliberately neighbor-only: receivers bind
    /// the certificate to the endpoint that delivered it, so forwarding one
    /// through another gossip peer would make that check fail.
    async fn announce_identity(&mut self, ws: WorkspaceId) {
        let Some(hello) = self.identity_hello(ws) else {
            return;
        };
        let bytes = match postcard::to_allocvec(&hello) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(ws, %error, "could not encode collaboration identity announcement");
                return;
            }
        };
        let Some(topic) = self.topics.get_mut(&ws) else {
            return;
        };
        topic.identity_sent = Some(message_fingerprint(&bytes));
        topic.identity_announced = Some(Instant::now());
        if let Err(error) = topic.broadcast_neighbors(Bytes::from(bytes)).await {
            tracing::warn!(ws, %error, "could not send collaboration identity announcement");
        }
    }

    /// Announce this device's identity on `ws` again, to its neighbours and
    /// its workspace hosts, when it changed since it was last announced
    /// there: a new revocation reaches the room the tick it is read from
    /// disk, as when every tick announced it.
    async fn announce_identity_if_changed(&mut self, ws: WorkspaceId) {
        let Some(hello) = self.identity_hello(ws) else {
            return;
        };
        let Ok(bytes) = postcard::to_allocvec(&hello) else {
            return;
        };
        let fingerprint = message_fingerprint(&bytes);
        let Some(topic) = self.topics.get_mut(&ws) else {
            return;
        };
        // Nothing announced here yet: the first link or host session will.
        let Some(sent) = topic.identity_sent.replace(fingerprint) else {
            return;
        };
        if sent == fingerprint {
            return;
        }
        tracing::info!(ws, "device identity material changed; announcing it again");
        topic.identity_announced = Some(Instant::now());
        if let Err(error) = topic.broadcast_neighbors(Bytes::from(bytes)).await {
            tracing::warn!(ws, %error, "could not send collaboration identity announcement");
        }
        for ((session_ws, _), runtime) in &self.host_sessions {
            if *session_ws == ws {
                Self::announce_identity_to_host(ws, &hello, &runtime.session);
            }
        }
    }

    /// This machine's codec advert, encoded.
    fn media_caps_advert(&self) -> Option<Vec<u8>> {
        let advert = crate::network::media_caps::local_advert(
            crate::network::behaviour::peer_liveness_handle(&self.endpoint.id()),
        );
        postcard::to_allocvec(&advert).ok()
    }

    /// Tell `ws`'s neighbours what this machine can encode and decode, so a
    /// sharer picks a codec every viewer takes. Neighbour scoped like the
    /// identity: receivers take an advert only from the device it describes.
    async fn announce_media_caps(&mut self, ws: WorkspaceId) {
        let Some(bytes) = self.media_caps_advert() else {
            return;
        };
        self.caps_sent = Some(message_fingerprint(&bytes));
        if let Some(topic) = self.topics.get(&ws) {
            let _ = topic.broadcast_neighbors(Bytes::from(bytes)).await;
        }
    }

    /// Announce this machine's codecs again on every topic when they changed
    /// since they were last announced.
    async fn announce_media_caps_if_changed(&mut self) {
        let Some(bytes) = self.media_caps_advert() else {
            return;
        };
        let fingerprint = message_fingerprint(&bytes);
        let Some(sent) = self.caps_sent.replace(fingerprint) else {
            return;
        };
        if sent == fingerprint {
            return;
        }
        for topic in self.topics.values() {
            let _ = topic.broadcast_neighbors(Bytes::from(bytes.clone())).await;
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
        self.hold_backlog(ws, channel, kind, bytes);
    }

    /// Keep one message for room `ws`'s host until its session is up, the
    /// oldest dropped past the room's bounds.
    fn hold_backlog(
        &mut self,
        ws: WorkspaceId,
        channel: [u8; 32],
        kind: Retention,
        bytes: Vec<u8>,
    ) {
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

    /// Tell a workspace host session of `ws` that just started who this
    /// device is; what the host holds then is what a later change is judged
    /// by ([`Self::announce_identity_if_changed`]).
    fn announce_identity_to_new_host(&mut self, ws: WorkspaceId, session: &RelayCoeditSession) {
        let Some(hello) = self.identity_hello(ws) else {
            return;
        };
        Self::announce_identity_to_host(ws, &hello, session);
        if let (Some(topic), Ok(bytes)) = (self.topics.get_mut(&ws), postcard::to_allocvec(&hello))
        {
            topic
                .identity_sent
                .get_or_insert_with(|| message_fingerprint(&bytes));
        }
    }

    /// Send identity announcement `hello` to the workspace host of `ws`.
    fn announce_identity_to_host(
        ws: WorkspaceId,
        hello: &NeighbourMsg,
        session: &RelayCoeditSession,
    ) {
        let (Some((channel, _)), Ok(bytes)) =
            (retention(hello, false), postcard::to_allocvec(hello))
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
    reason = "one audited streaming state machine keeps prefix verification, bounded chunks, cancellation, progress, and the authenticated finish together"
)]
async fn stream_nearby_transfer(
    route: PersonalStreamRoute,
    transfer: NearbyTransferCheckpoint,
    file: std::fs::File,
    task: PersonalTransferTask,
) -> PersonalTransferOutcome {
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
    let PersonalTransferTask {
        internal,
        sent_bytes,
        generation,
        mut cancel,
    } = task;
    // File reads go through the I/O pool, never a network worker: the
    // resumed prefix can be most of a large file.
    let file = Arc::new(file);
    let prefix = Arc::clone(&file);
    let verify = tilo_rt::io::run(move || {
        use std::io::Read as _;
        let mut reader = &*prefix;
        let mut hasher = blake3::Hasher::new();
        let mut verify = vec![0_u8; PERSONAL_TRANSFER_CHUNK_BYTES];
        let mut verified = 0_u64;
        while verified < resume_offset {
            let want = usize::try_from(
                resume_offset
                    .saturating_sub(verified)
                    .min(PERSONAL_TRANSFER_CHUNK_BYTES as u64),
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
    });
    let mut hasher = tokio::select! {
        result = verify => match result {
            Ok(hasher) => hasher,
            Err(_) => return PersonalTransferOutcome::Failed,
        },
        () = personal_transfer_cancelled(&mut cancel) => return PersonalTransferOutcome::Cancelled,
    };
    if hasher.finalize().as_bytes() != &prefix_hash {
        return PersonalTransferOutcome::Failed;
    }
    let connect = tokio::time::timeout(
        Duration::from_secs(10),
        endpoint.connect(addr, PERSONAL_FABRIC_ALPN),
    );
    let connection = tokio::select! {
        result = connect => match result {
            Ok(Ok(connection)) => connection,
            _ => return PersonalTransferOutcome::Failed,
        },
        () = personal_transfer_cancelled(&mut cancel) => return PersonalTransferOutcome::Cancelled,
    };
    let streamed: Result<(), PersonalTransferOutcome> = async {
        let mut offset = resume_offset;
        while offset < total_bytes {
            let want = usize::try_from(
                total_bytes
                    .saturating_sub(offset)
                    .min(PERSONAL_TRANSFER_CHUNK_BYTES as u64),
            )
            .map_err(|_| PersonalTransferOutcome::Failed)?;
            let bytes = tokio::select! {
                result = tilo_rt::io::read_at(Arc::clone(&file), offset, want) => result.map_err(|_| PersonalTransferOutcome::Failed)?,
                () = personal_transfer_cancelled(&mut cancel) => return Err(PersonalTransferOutcome::Cancelled),
            };
            if bytes.is_empty() {
                return Err(PersonalTransferOutcome::Failed);
            }
            let read = bytes.len();
            hasher.update(&bytes);
            let send = send_nearby_transfer_frame(
                &connection,
                workspace,
                user_public_key,
                PersonalNearbyTransferFrame::Chunk {
                    transfer_id,
                    request_id,
                    offset,
                    bytes,
                },
            );
            tokio::select! {
                result = send => result.map_err(|()| PersonalTransferOutcome::Failed)?,
                () = personal_transfer_cancelled(&mut cancel) => return Err(PersonalTransferOutcome::Cancelled),
            }
            offset = offset
                .saturating_add(u64::try_from(read).map_err(|_| PersonalTransferOutcome::Failed)?);
            sent_bytes.store(offset, std::sync::atomic::Ordering::Relaxed);
            let _ = internal.try_send(InternalEvent::PersonalTransferProgress {
                target,
                transfer_id,
                generation,
                sent_bytes: offset,
            });
        }
        let tail = tokio::select! {
            result = tilo_rt::io::read_at(Arc::clone(&file), offset, 1) => result.map_err(|_| PersonalTransferOutcome::Failed)?,
            () = personal_transfer_cancelled(&mut cancel) => return Err(PersonalTransferOutcome::Cancelled),
        };
        if !tail.is_empty() || hasher.finalize().as_bytes() != &content_hash {
            return Err(PersonalTransferOutcome::Failed);
        }
        let finish = send_nearby_transfer_frame(
            &connection,
            workspace,
            user_public_key,
            PersonalNearbyTransferFrame::Finish {
                transfer_id,
                request_id,
                content_hash,
            },
        );
        tokio::select! {
            result = finish => result.map_err(|()| PersonalTransferOutcome::Failed),
            () = personal_transfer_cancelled(&mut cancel) => Err(PersonalTransferOutcome::Cancelled),
        }
    }
    .await;
    match streamed {
        Ok(()) => PersonalTransferOutcome::Completed,
        Err(outcome) => {
            // The receiver keeps its last durable prefix; the abort only
            // tells it this stream will send nothing more.
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                send_nearby_transfer_frame(
                    &connection,
                    workspace,
                    user_public_key,
                    PersonalNearbyTransferFrame::Abort {
                        transfer_id,
                        request_id,
                    },
                ),
            )
            .await;
            outcome
        }
    }
}

async fn personal_transfer_cancelled(cancel: &mut tokio::sync::watch::Receiver<bool>) {
    if *cancel.borrow() {
        return;
    }
    // A changed value and a dropped sender both end the wait.
    let _ = cancel.changed().await;
}

#[allow(
    clippy::too_many_lines,
    reason = "the actor's state is built in one literal so every field's starting value reads together"
)]
async fn run(
    endpoint: Endpoint,
    lookup: MemoryLookup,
    host_identity: Option<HostIdentity>,
    listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
    mut cmd_rx: mpsc::Receiver<NetCmd>,
    heartbeat_interval: Duration,
    reachability: Reachability,
    governing: Option<super::governor_join::GoverningRooms>,
) {
    let (internal_tx, mut internal_rx) = mpsc::channel(256);
    let personal_fabric_incarnations = Arc::new(AtomicU64::new(1));
    let (input_tx, mut input_rx) = mpsc::channel(64);
    let gossip = Gossip::builder()
        .max_message_size(MAX_GOSSIP_MESSAGE_BYTES)
        .broadcast_config(broadcast_config())
        .spawn(endpoint.clone());
    let private_messages = super::private_messages::Registry::new(endpoint.id());
    let router = Router::builder(endpoint.clone())
        .accept(GOSSIP_ALPN, gossip.clone())
        .accept(super::private_messages::ALPN, private_messages.clone())
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
                next_connection_incarnation: Arc::clone(&personal_fabric_incarnations),
            },
        )
        .accept(tilo_unlock_protocol::UNLOCK_ALPN, DeviceUnlockProtocol);
    // Answered only where this device may govern rooms; a dial elsewhere
    // finds no such protocol, as at any device that governs none.
    let router = match governing {
        Some(governing) => router.accept(
            tilo_delivery_protocol::workspace_join::WORKSPACE_JOIN_ALPN,
            tilo_relay_session::workspace_join::WorkspaceJoinProtocol::new(governing),
        ),
        None => router,
    }
    .spawn();
    let reporting_addrs = report_addrs(&endpoint, listen_addrs);
    let mut state = ActorState {
        private_messages: super::private_messages::Transport::new(
            endpoint.clone(),
            private_messages,
            reachability.clone(),
        ),
        reachability,
        endpoint,
        lookup,
        host_identity,
        native_media: super::native_media::Registry::default(),
        native_enrollment: super::invite::enrollment_driver::Registry::default(),
        native_routes: native_ingress::Routes::default(),
        room_governance: room_governance::Waiting::default(),
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
        agent_questions: super::agent_questions::Answering::default(),
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
        next_surface_fanout_generation: 0,
        surface_fanout_viewers: HashMap::new(),
        pending_surface_frames: HashMap::new(),
        personal_fabric_connections: HashMap::new(),
        personal_fabric_replaced: Vec::new(),
        personal_fabric_dials: HashMap::new(),
        personal_fabric_incarnations,
        active_personal_transfers: HashMap::new(),
        next_personal_transfer_generation: 0,
        tally: Arc::default(),
        caps_sent: None,
        pending_remote_actions: PendingRemoteActions::default(),
        gossip_pointers: PointerPacer::default(),
    };
    let mut upkeep = None;
    let mut host_checkpoint = None;
    let mut reported_party = None;

    loop {
        // The relays stay up while a remote party lasts, and rest a while
        // after the last one is gone (`crate::network::reachability`).
        let party = state.remote_party();
        if party != reported_party {
            reported_party = party;
            state.reachability.need(Need::Transport, party);
        }
        // Evict when the soonest membership lapses, not on a polling tick:
        // linked peers have no deadline at all.
        let eviction = state.next_eviction();
        // Upkeep every `heartbeat_interval` while there is someone to keep
        // up with; a desktop alone with its rooms never wakes for it.
        upkeep = next_upkeep(upkeep, state.needs_upkeep(), heartbeat_interval);
        host_checkpoint = next_upkeep(
            host_checkpoint,
            !state.host_sessions.is_empty(),
            HOST_CHECKPOINT_INTERVAL,
        );
        // A held pointer move falls due; nothing is armed while none is held.
        let pointer_due = state.gossip_pointers.next_due();
        tokio::select! {
            biased;
            input = input_rx.recv() => {
                if !state.handle_internal(input).await {
                    break;
                }
            }
            // Commands before a held pointer's deadline: a newer move already
            // queued replaces the held one before the gap's end sends it.
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
            () = sleep_until_due(pointer_due) => state.broadcast_due_pointers().await,
            () = sleep_until_lapse(eviction) => state.run_eviction_tick().await,
            () = sleep_until_lapse(upkeep) => {
                upkeep = None;
                state.run_heartbeat_tick().await;
            }
            () = sleep_until_lapse(host_checkpoint) => {
                host_checkpoint = None;
                state.run_host_checkpoint_tick().await;
            }
        }
    }

    for (_, topic) in state.topics {
        topic.pump.abort();
    }
    for (_, host) in state.host_sessions {
        host.pump.abort();
    }
    for (_, (_, task)) in state.surface_fanout_tasks {
        task.abort();
    }
    reporting_addrs.abort();
    let _ = router.shutdown().await;
}

/// Sleep until `at` on the runtime's clock, or forever when nothing is due.
async fn sleep_until_due(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// When the next upkeep tick is due, given when it was due (`armed`) and
/// whether the actor still [needs one](ActorState::needs_upkeep): kept while
/// it is needed, `interval` from now once it becomes needed, and none while
/// nothing needs it.
fn next_upkeep(armed: Option<Instant>, needed: bool, interval: Duration) -> Option<Instant> {
    if !needed {
        return None;
    }
    Some(armed.unwrap_or_else(|| later(Instant::now(), interval)))
}

/// Keep `listen_addrs` at the endpoint's current address, which invites
/// carry and [`crate::shell::network::ShellNetwork::dial_addr`] reads: a
/// new direct address, or the home relay once [`Reachability::wake`] adds
/// the relays. iroh says when the address changes, so this waits rather
/// than re-reading it on a timer.
fn report_addrs(
    endpoint: &Endpoint,
    listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
) -> tokio::task::JoinHandle<()> {
    use iroh::Watcher as _;
    let mut addr = endpoint.watch_addr();
    let current = addr.get();
    let report = move |current: EndpointAddr| {
        if let Ok(mut out) = listen_addrs.lock() {
            out.clear();
            out.push(current);
        }
        // A new address or home relay: the network came back or moved, and
        // a certificate renewal that failed meanwhile tries again.
        tilo_identity_host::provisioning::RENEWAL_CAUSES.changed();
    };
    report(current);
    tokio::spawn(async move {
        while let Ok(current) = addr.updated().await {
            report(current);
        }
    })
}

/// `span` after `now`; `now` itself in the unreachable case that overflows.
fn later(now: Instant, span: Duration) -> Instant {
    now.checked_add(span).unwrap_or(now)
}

/// Sleep until `at`, or forever when nothing can lapse.
async fn sleep_until_lapse(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

/// How often gossip sweeps a topic's message caches. iroh-gossip's 1 s
/// sweep was a timer per topic that never stopped, the one thing that woke
/// an idle, converged session several times a second; swept every
/// [`GOSSIP_CACHE_SWEEP`] instead, a topic with messages wakes its runtime
/// a tenth as often. The vendored iroh-gossip (`vendor/iroh-gossip/
/// TILO-PATCH.md`) arms it on a topic's first cached message and stops it
/// once the caches are empty (`gossip_caches_drain_within`); a topic that
/// never had a neighbour caches nothing ([`TopicRuntime::broadcast`]).
const GOSSIP_CACHE_SWEEP: Duration = Duration::from_secs(10);
/// How long gossip keeps a message to answer a neighbour's graft for it.
/// iroh-gossip's 30 s less the longer sweep, so a message is still gone
/// within 30 s of arriving: a graft is asked for within a few round trips,
/// and this cache holds whole messages of up to
/// [`MAX_GOSSIP_MESSAGE_BYTES`].
const GOSSIP_MESSAGE_RETENTION: Duration = Duration::from_secs(20);

/// How long after its last message a topic stops sweeping: the message id
/// outlives the message itself (iroh-gossip's 90 s, to refuse a late
/// duplicate), and the sweep that finds both gone is the last.
#[cfg(test)]
pub(crate) fn gossip_caches_drain_within() -> Duration {
    let config = broadcast_config();
    config
        .message_id_retention
        .max(config.message_cache_retention)
        .saturating_add(config.cache_evict_interval)
}

/// Plumtree broadcast settings: iroh-gossip's, with the cache swept every
/// [`GOSSIP_CACHE_SWEEP`] instead of every second.
fn broadcast_config() -> iroh_gossip::proto::PlumtreeConfig {
    iroh_gossip::proto::PlumtreeConfig {
        cache_evict_interval: GOSSIP_CACHE_SWEEP,
        message_cache_retention: GOSSIP_MESSAGE_RETENTION,
        ..iroh_gossip::proto::PlumtreeConfig::default()
    }
}

/// The QUIC transport collaboration endpoints use: iroh's, with the
/// connection ending after `PEER_TIMEOUT` without a packet from the peer
/// rather than iroh's 30 s. iroh's keep-alives (one after 5 s of silence)
/// keep a live peer's connection from ever idling; a peer that vanished
/// misses three and its links go down. This is the liveness check the
/// 5 s application heartbeat used to be, without an application message.
pub fn transport_config() -> iroh::endpoint::QuicTransportConfig {
    let millis = u32::try_from(PEER_TIMEOUT.as_millis()).unwrap_or(u32::MAX);
    iroh::endpoint::QuicTransportConfig::builder()
        .max_idle_timeout(Some(iroh::endpoint::VarInt::from_u32(millis).into()))
        .build()
}

/// The window a direct fanout carries: each shared window is fanned out on
/// its own, so its frames are coalesced only with its own.
fn fanout_surface(msg: &NeighbourMsg) -> SurfaceId {
    match msg {
        NeighbourMsg::SurfaceFrame { surface, .. } => *surface,
        _ => 0,
    }
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
            if let Err(error) = topic.broadcast(Bytes::from(bytes)).await {
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
            | NeighbourMsg::MlsRoutedAccessRequest { .. }
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
/// byte-identical to the first. Only a copy that another host session
/// delivers within [`INPUT_COPY`] (the same event over another route to this
/// room) is dropped; one session delivers each event once, in order, so
/// identical events over it (a doubled letter in one batch) are all new.
fn mark_new_input(
    ws: WorkspaceId,
    host: EndpointId,
    fingerprint: [u8; 32],
    now: Instant,
    recent: &mut HashMap<(WorkspaceId, [u8; 32]), (EndpointId, Instant)>,
) -> bool {
    const INPUT_COPY: Duration = Duration::from_millis(30);
    if recent.len() > 256 {
        recent.retain(|_, (_, seen)| now.saturating_duration_since(*seen) < INPUT_COPY);
    }
    let copy = recent.get(&(ws, fingerprint)).is_some_and(|(via, seen)| {
        *via != host && now.saturating_duration_since(*seen) < INPUT_COPY
    });
    if !copy {
        recent.insert((ws, fingerprint), (host, now));
    }
    !copy
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
                .and_then(|result| result.map_err(|error| relay_error(&host, &error)))
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
            .and_then(|result| result.map_err(|error| relay_error(&host, &error)))
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
async fn register_content_peer(
    registration: tilo_relay_protocol::transport::ContentPeerRegistration,
) -> bool {
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
    noted_content_peer_removals().lock_recover().push(relay);
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

/// How long a workspace host session goes without hearing from the host
/// before it asks the host for a keep-alive. A session that carries traffic
/// is never pinged; an idle one once per this long, rather than every 5 s.
const HOST_PING_AFTER: Duration = Duration::from_secs(15);
/// How long a host may stay silent, keep-alives included, before its
/// session is dropped and dialled again. A connection whose host session
/// ended without closing it (or whose host stopped reading) otherwise
/// looks alive forever. The bound is what it was with a ping every 5 s and a
/// 20 s limit read at those pings: a host gone quiet is dropped 25 s after
/// it was last heard.
const HOST_SILENCE_LIMIT: Duration = Duration::from_secs(25);

/// Wait until host `session` is due a keep-alive check, then make it: a
/// host silent for [`HOST_PING_AFTER`] is pinged once (`pinged` notes it),
/// and one silent for [`HOST_SILENCE_LIMIT`] is not worth keeping (`false`).
async fn host_keepalive(session: &RelayCoeditSession, pinged: &mut bool) -> bool {
    let silent = session.silent_for();
    if silent < HOST_PING_AFTER {
        *pinged = false;
    }
    let check = if *pinged {
        HOST_SILENCE_LIMIT.saturating_sub(silent)
    } else {
        HOST_PING_AFTER.saturating_sub(silent)
    };
    tokio::time::sleep(check).await;
    let silent = session.silent_for();
    if silent >= HOST_SILENCE_LIMIT {
        return false;
    }
    if silent >= HOST_PING_AFTER && !*pinged {
        let _ = session.ping();
        *pinged = true;
    }
    true
}

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
        // Whether this silence was already answered with a ping.
        let mut pinged = false;
        loop {
            let inbound = tokio::select! {
                inbound = session.recv() => inbound,
                alive = host_keepalive(&session, &mut pinged) => {
                    if !alive {
                        tracing::warn!(ws, host = %host, "workspace host stopped answering; reconnecting");
                        break;
                    }
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
                HostInbound::Member { device, active } => HostEvent::Member { device, active },
                HostInbound::Voice(event) => HostEvent::Voice(event),
                // The shell opens no on-demand record yet.
                HostInbound::RecordOpened { .. } => continue,
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
                HostInbound::RecordContent { from, frame } => {
                    let Some(event) = record_content_event(&from, &frame) else {
                        continue;
                    };
                    event
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

/// A piece of a record's content, or word that a device keeps none, a
/// workspace host handed this device from `from`; `None` for anything else.
fn record_content_event(from: &[u8; 32], frame: &[u8]) -> Option<HostEvent> {
    let author = EndpointId::from_bytes(from).ok()?;
    let msg = postcard::from_bytes(frame).ok()?;
    matches!(
        msg,
        NeighbourMsg::RecordContentPiece { .. } | NeighbourMsg::RecordContentLacks { .. }
    )
    .then(|| HostEvent::Message {
        author,
        fingerprint: message_fingerprint(frame),
        replayed: false,
        msg,
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

    #[tokio::test]
    async fn encoded_backlog_stale_completion_cannot_finish_a_restarted_stream() {
        let key = (42, 31);
        let mut tasks = SurfaceFanoutTasks::new();
        tasks.insert(key, (1, tokio::spawn(std::future::pending())));
        let (_, old) = tasks.remove(&key).expect("old stream task");
        old.abort();
        let current = tokio::spawn(std::future::pending());
        let current_abort = current.abort_handle();
        tasks.insert(key, (2, current));
        assert!(!finish_surface_fanout(&mut tasks, key, 1));
        assert!(
            tasks.contains_key(&key),
            "old completion retains the new task"
        );
        assert!(finish_surface_fanout(&mut tasks, key, 2));
        current_abort.abort();
        assert!(tasks.is_empty());
        assert!(!finish_surface_fanout(&mut tasks, key, 2));
    }

    #[test]
    fn encoded_backlog_accounts_for_retained_allocation_after_pop_and_refill() {
        let mut backlog = SurfaceFrameBacklog::default();
        for cycle in 0..4 {
            for ts in 0..100 {
                backlog.push(NeighbourMsg::SurfaceFrame {
                    surface: 31,
                    source_peer: 1,
                    ts,
                    encoded_h264: vec![0x41; 1024],
                    seq: ts,
                });
                let allocated = backlog
                    .frames
                    .capacity()
                    .saturating_mul(std::mem::size_of::<NeighbourMsg>())
                    .saturating_add(
                        backlog
                            .frames
                            .iter()
                            .map(|msg| match msg {
                                NeighbourMsg::SurfaceFrame { encoded_h264, .. } => {
                                    encoded_h264.capacity()
                                }
                                _ => 0,
                            })
                            .sum::<usize>(),
                    );
                assert!(
                    allocated <= backlog.bytes && allocated <= MAX_SURFACE_FRAME_BYTES,
                    "cycle={cycle} accounted={} allocated={allocated}",
                    backlog.bytes
                );
            }
            for _ in 0..75 {
                assert!(backlog.pop().is_some());
                let allocated = backlog
                    .frames
                    .capacity()
                    .saturating_mul(std::mem::size_of::<NeighbourMsg>())
                    .saturating_add(
                        backlog
                            .frames
                            .iter()
                            .map(|msg| match msg {
                                NeighbourMsg::SurfaceFrame { encoded_h264, .. } => {
                                    encoded_h264.capacity()
                                }
                                _ => 0,
                            })
                            .sum::<usize>(),
                    );
                assert!(allocated <= backlog.bytes && allocated <= MAX_SURFACE_FRAME_BYTES);
            }
        }
    }

    #[test]
    fn encoded_backlog_preserves_order_and_recovers_at_a_keyframe_after_loss() {
        let frame = |ts, encoded_h264| NeighbourMsg::SurfaceFrame {
            surface: 31,
            source_peer: 1,
            encoded_h264,
            ts,
            seq: ts,
        };
        let mut backlog = SurfaceFrameBacklog::default();
        for ts in 1..4 {
            backlog.push(frame(ts, vec![0, 0, 1, 0x41]));
        }
        for expected in 1..4 {
            assert!(
                matches!(backlog.pop(), Some(NeighbourMsg::SurfaceFrame { ts, .. }) if ts == expected)
            );
        }
        assert_eq!(backlog.bytes, 0);
        // Fill the same production byte budget, including queue storage.
        let payload = vec![
            0x41;
            MAX_SURFACE_FRAME_BYTES.saturating_sub(
                std::mem::size_of::<NeighbourMsg>().saturating_mul(4)
            )
        ];
        backlog.push(frame(4, payload));
        assert_eq!(backlog.bytes, MAX_SURFACE_FRAME_BYTES);
        backlog.push(frame(5, vec![0, 0, 1, 0x41]));
        assert!(backlog.frames.is_empty());
        assert_eq!(backlog.bytes, 0);
        assert!(backlog.awaiting_keyframe);
        backlog.push(frame(6, vec![0, 0, 1, 0x41]));
        assert!(backlog.pop().is_none(), "lost references cannot be used");
        backlog.push(frame(7, vec![0, 0, 1, 0x65]));
        assert!(matches!(
            backlog.pop(),
            Some(NeighbourMsg::SurfaceFrame { ts: 7, .. })
        ));
        assert!(!backlog.awaiting_keyframe);
        // A failed send uses this same production recovery transition.
        backlog.lose_references();
        backlog.codec = Some(media::StreamCodec::Vp9);
        assert!(
            !backlog.admit(&frame(8, vec![0, 0, 1, 0x65])),
            "H264 is not a VP9 keyframe"
        );
        assert!(backlog.admit(&frame(9, vec![0x82, 0x49, 0x83, 0x42])));
        backlog.codec = None;
        backlog.lose_references();
        assert!(
            !backlog.awaiting_keyframe,
            "independent pictures need no keyframe"
        );
        backlog.push(frame(10, vec![0xff, 0xd8]));
        backlog.push(frame(11, vec![0xff, 0xd8]));
        assert!(matches!(
            backlog.pop(),
            Some(NeighbourMsg::SurfaceFrame { ts: 11, .. })
        ));
        assert!(
            backlog.pop().is_none(),
            "independent pictures keep newest only"
        );
    }

    /// A dial wakes the relays; what a lone desktop does on every local
    /// change (join its own topic, publish to it) does not.
    #[test]
    fn only_a_remote_party_needs_this_device_reachable() {
        let addr = EndpointAddr::new(iroh::SecretKey::generate().public());
        assert!(remote_party_command(&NetCmd::Connect { addr: addr.clone() }).is_some());
        assert!(
            remote_party_command(&NetCmd::ConnectHost {
                addr,
                expectation: None,
            })
            .is_some()
        );
        let (on_event, _events) = mpsc::channel(1);
        assert!(remote_party_command(&NetCmd::JoinWorkspace { ws: 7, on_event }).is_none());
        assert!(remote_party_command(&NetCmd::LeaveWorkspace { ws: 7 }).is_none());
    }

    #[test]
    fn a_remote_action_with_no_route_waits_for_that_peer_and_expires() {
        let mut pending = PendingRemoteActions::default();
        let peer = iroh::SecretKey::generate().public();
        let other = iroh::SecretKey::generate().public();
        let start = Instant::now();
        pending.hold(1, peer, b"open".to_vec(), start);
        pending.hold(1, other, b"other".to_vec(), start);
        pending.hold(1, peer, b"again".to_vec(), start);
        assert_eq!(pending.targets(), vec![(1, peer), (1, other)]);
        assert_eq!(
            pending.take_for(1, peer, start),
            vec![b"open".to_vec(), b"again".to_vec()]
        );
        assert!(
            pending.take_for(2, other, start).is_empty(),
            "held per workspace"
        );
        assert_eq!(pending.take_for(1, other, start), vec![b"other".to_vec()]);

        for index in 0..MAX_PENDING_REMOTE_ACTIONS {
            pending.hold(2, peer, vec![u8::try_from(index).unwrap()], start);
        }
        pending.hold(2, peer, b"newest".to_vec(), start);
        let kept = pending.take_for(2, peer, start);
        assert_eq!(kept.len(), MAX_PENDING_REMOTE_ACTIONS);
        assert_eq!(kept[0], vec![1]);
        assert_eq!(kept.last().expect("kept"), b"newest");

        pending.hold(3, peer, b"stale".to_vec(), start);
        assert!(
            pending
                .take_for(3, peer, start + PENDING_REMOTE_ACTION_TTL)
                .is_empty(),
            "a request is not run long after it was sent"
        );
    }

    #[test]
    fn upkeep_is_armed_only_while_something_needs_it() {
        const EVERY: Duration = Duration::from_secs(5);
        let before = Instant::now();
        assert_eq!(
            next_upkeep(None, false, EVERY),
            None,
            "alone, it never ticks"
        );
        let armed = next_upkeep(None, true, EVERY).expect("a peer arms it");
        assert!(
            armed >= later(before, EVERY),
            "the first tick is a whole interval away"
        );
        assert_eq!(
            next_upkeep(Some(armed), true, EVERY),
            Some(armed),
            "an armed tick keeps its deadline"
        );
        assert_eq!(
            next_upkeep(Some(armed), false, EVERY),
            None,
            "the last peer gone disarms it"
        );
    }

    /// A topic hands nothing to gossip until it has had a neighbour, so a
    /// desktop alone with its rooms never arms gossip's per-topic cache
    /// sweep, which would otherwise run for the rest of the session. Once
    /// linked, it broadcasts and the sweep runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_topic_that_never_had_a_neighbour_never_arms_the_gossip_sweep() {
        const SWEEP: Duration = Duration::from_millis(20);
        let endpoint = Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let gossip = Gossip::builder()
            .broadcast_config(iroh_gossip::proto::PlumtreeConfig {
                cache_evict_interval: SWEEP,
                ..broadcast_config()
            })
            .spawn(endpoint.clone());
        let (sender, _receiver) = gossip
            .subscribe(topic_id(42), Vec::new())
            .await
            .unwrap()
            .split();
        let mut topic = TopicRuntime::new(sender, tokio::spawn(std::future::pending()));
        let timers = || gossip.metrics().actor_tick_timers.get();

        for _ in 0..3 {
            topic
                .broadcast(Bytes::from_static(b"a local change"))
                .await
                .unwrap();
            topic
                .broadcast_neighbors(Bytes::from_static(b"an identity"))
                .await
                .unwrap();
        }
        tokio::time::sleep(SWEEP.saturating_mul(10)).await;
        assert_eq!(timers(), 0, "gossip woke for a topic nobody else is on");

        topic.linked = true;
        topic
            .broadcast(Bytes::from_static(b"a change a neighbour hears"))
            .await
            .unwrap();
        tilo_test_env::within("a linked topic's cache sweep", async {
            while timers() < 2 {
                tokio::time::sleep(SWEEP).await;
            }
        })
        .await;
        topic.pump.abort();
        endpoint.close().await;
    }
    use getrandom::{SysRng, rand_core::UnwrapErr};

    #[test]
    fn a_repeated_click_is_new_but_its_copy_over_another_route_is_not() {
        let mut recent = HashMap::new();
        let host = iroh::SecretKey::from_bytes(&[0xA1; 32]).public();
        let other = iroh::SecretKey::from_bytes(&[0xA2; 32]).public();
        let start = Instant::now();
        assert!(mark_new_input(1, host, [7; 32], start, &mut recent));
        assert!(
            !mark_new_input(
                1,
                other,
                [7; 32],
                start + Duration::from_millis(1),
                &mut recent
            ),
            "the same event arriving over a second relay session"
        );
        assert!(
            mark_new_input(
                1,
                host,
                [7; 32],
                start + Duration::from_millis(200),
                &mut recent
            ),
            "a second, byte-identical click"
        );
        assert!(mark_new_input(
            2,
            host,
            [7; 32],
            start + Duration::from_millis(200),
            &mut recent
        ));
        // "ll" typed in one batch: both strokes come over one session at once.
        for _ in 0..2 {
            assert!(
                mark_new_input(
                    1,
                    host,
                    [9; 32],
                    start + Duration::from_millis(300),
                    &mut recent
                ),
                "a repeated key over the same session"
            );
        }
    }

    fn active_personal_transfer(generation: u64) -> ActivePersonalTransfer {
        let (cancel, _receiver) = tokio::sync::watch::channel(false);
        ActivePersonalTransfer {
            generation,
            workspace: 7,
            total_bytes: 8,
            sent_bytes: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            cancel,
        }
    }

    #[test]
    fn personal_bindings_are_exact_generation_and_membership_scoped() {
        // Phase `personal_transfer_generation_is_exact_and_stale_completion_cannot_remove_replacement`.
        {
            let first = iroh::SecretKey::from_bytes(&[0xB1; 32]).public();
            let sibling = iroh::SecretKey::from_bytes(&[0xB2; 32]).public();
            let transfer_id = [0xB3; 16];
            let key = (first, transfer_id);
            let sibling_key = (sibling, transfer_id);
            let mut active = HashMap::from([
                (key, active_personal_transfer(12)),
                (sibling_key, active_personal_transfer(4)),
            ]);

            assert!(take_personal_transfer_generation(&mut active, key, 11).is_none());
            assert!(
                active.contains_key(&key),
                "old worker cannot remove replacement"
            );
            assert!(
                take_personal_transfer_generation(&mut active, sibling_key, 12).is_none(),
                "same id on another target is isolated"
            );
            assert!(take_personal_transfer_generation(&mut active, key, 12).is_some());
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
                        bytes: vec![7; PERSONAL_TRANSFER_CHUNK_BYTES],
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
            assert_eq!(bytes.len(), PERSONAL_TRANSFER_CHUNK_BYTES);
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn personal_fabric_unknown_extension_preserves_following_streams() {
        let (a, _) = endpoint().await;
        let (b, _) = endpoint().await;
        let (internal_tx, mut events) = mpsc::channel(8);
        let router = Router::builder(b.clone())
            .accept(
                PERSONAL_FABRIC_ALPN,
                PersonalFabricProtocol {
                    internal_tx,
                    next_connection_incarnation: Arc::new(AtomicU64::new(1)),
                },
            )
            .spawn();
        let connection = a.connect(b.addr(), PERSONAL_FABRIC_ALPN).await.unwrap();
        let frame = PersonalFabricWireFrame {
            workspace: 43,
            user_public_key: [0xA5; 32],
            payload: PersonalFabricPayload::Snapshot(vec![1, 2, 3]),
        };
        let valid = postcard::to_allocvec(&frame).unwrap();
        assert_eq!(valid[33], 1, "the legacy Snapshot payload tag stays fixed");
        let mut future = valid.clone();
        future[33] = 9; // First unsupported tag after optional connector observations.
        assert!(postcard::from_bytes::<PersonalFabricWireFrame>(&future).is_err());
        for bytes in [&future, &valid] {
            let mut stream = connection.open_uni().await.unwrap();
            stream.write_all(bytes).await.unwrap();
            stream.finish().unwrap();
        }
        let event = tilo_test_env::within("valid checkpoint after unknown extension", async {
            loop {
                match events.recv().await {
                    Some(InternalEvent::PersonalFabricConnection { .. }) => {}
                    other => return other,
                }
            }
        })
        .await
        .unwrap();
        assert!(matches!(event, InternalEvent::PersonalFabric {
            ws: 43, payload: PersonalFabricPayload::Snapshot(bytes), ..
        } if bytes == vec![1, 2, 3]));
        connection.close(0u32.into(), b"fixture complete");
        router.shutdown().await.unwrap();
        a.close().await;
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
                        nonce: 0,
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
                        seq: 0,
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

    /// What this actor's peers delivered to it so far, by kind.
    async fn tally_of(cmd: &mpsc::Sender<NetCmd>) -> std::collections::BTreeMap<&'static str, u64> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        cmd.send(NetCmd::WireTally(reply)).await.unwrap();
        answer.await.unwrap().snapshot()
    }

    /// An identified actor that runs its upkeep tick every `tick`, joined
    /// to workspace 42.
    async fn idle_test_actor(
        root: &ed25519_dalek::SigningKey,
        label: &str,
        tick: Duration,
    ) -> (
        mpsc::Sender<NetCmd>,
        mpsc::Receiver<NetEvent>,
        EndpointAddr,
        tokio::task::JoinHandle<()>,
    ) {
        let (key, cert) = enrol(root, DeviceCapabilities::primary(), label);
        let (endpoint, lookup) = identified_endpoint(&key).await;
        let addr = endpoint.addr();
        let identity = HostIdentity {
            device_cert: cert,
            device_signing_key: key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let (cmd, task) = Actor::spawn_with_heartbeat(
            endpoint,
            lookup,
            Some(identity),
            Arc::new(Mutex::new(Vec::new())),
            tick,
            Reachability::default(),
            None,
        );
        let (events, rx) = mpsc::channel(64);
        cmd.send(NetCmd::JoinWorkspace {
            ws: 42,
            on_event: events,
        })
        .await
        .unwrap();
        (cmd, rx, addr, task)
    }

    /// Wait on `rx` until the neighbour has said who it is and what it can
    /// decode, as it does once when the link comes up.
    async fn await_identity_and_codecs(rx: &mut mpsc::Receiver<NetEvent>) {
        let (mut identity, mut codecs) = (false, false);
        while !(identity && codecs) {
            match tilo_test_env::within("the neighbour's identity and codecs", rx.recv())
                .await
                .unwrap()
            {
                NetEvent::Msg {
                    msg: NeighbourMsg::IdentityHello { .. },
                    ..
                } => identity = true,
                NetEvent::Msg {
                    msg: NeighbourMsg::MediaCapabilities { .. },
                    ..
                } => codecs = true,
                _ => {}
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn linked_actors_keep_liveness_on_the_transport_and_say_nothing_while_idle() {
        // The upkeep tick runs every 50 ms here, a hundred times its pace in
        // a session, so a second of idling is a hundred ticks.
        const TICK: Duration = Duration::from_millis(50);
        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let (cmd_a, mut rx_a, _, task_a) = idle_test_actor(&root, "a", TICK).await;
        let (cmd_b, mut rx_b, addr_b, task_b) = idle_test_actor(&root, "b", TICK).await;
        cmd_a.send(NetCmd::Connect { addr: addr_b }).await.unwrap();

        // Phase `a_new_neighbour_hears_identity_and_codecs_once_per_link`.
        await_identity_and_codecs(&mut rx_a).await;
        await_identity_and_codecs(&mut rx_b).await;

        // Phase `a_linked_idle_pair_sends_no_application_message`. Before
        // the transport carried liveness every tick sent a heartbeat, an
        // identity hello and a codec advert per topic.
        tokio::time::sleep(TICK * 4).await;
        let (before_a, before_b) = (tally_of(&cmd_a).await, tally_of(&cmd_b).await);
        tokio::time::sleep(TICK * 20).await;
        assert_eq!(
            tally_of(&cmd_a).await,
            before_a,
            "a heard nothing while idle"
        );
        assert_eq!(
            tally_of(&cmd_b).await,
            before_b,
            "b heard nothing while idle"
        );
        assert!(
            std::iter::from_fn(|| rx_a.try_recv().ok())
                .chain(std::iter::from_fn(|| rx_b.try_recv().ok()))
                .all(|event| !matches!(event, NetEvent::PeerLeft { .. })),
            "a silent, linked peer stays a member"
        );

        // Phase `a_departed_peer_is_dropped_within_the_departure_grace`. A
        // peer that shuts down closes its links; it is dropped that grace
        // later, not after a heartbeat timeout.
        drop(cmd_b);
        task_b.await.unwrap();
        let gone = Instant::now();
        let left = tilo_test_env::within("the departed peer to be dropped", async {
            loop {
                if let Some(NetEvent::PeerLeft { cause, .. }) = rx_a.recv().await {
                    return cause;
                }
            }
        })
        .await;
        assert_eq!(left, PeerDepartureCause::LivenessTimedOut);
        let took = gone.elapsed();
        assert!(
            took <= DEPARTURE_GRACE + Duration::from_secs(3),
            "dropped {took:?} after it left"
        );
        drop(rx_b);
        drop(cmd_a);
        task_a.await.unwrap();
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
            // A dial to the dead viewer takes up to its 300 ms timeout. Inside
            // the actor loop every attempt would wait it out; outside it, an
            // attempt on a loaded runner is late only now and then. Each
            // attempt names a new surface, so each dials the viewer anew.
            let mut fastest = Duration::MAX;
            for attempt in 0..5_u32 {
                let started = Instant::now();
                cmd_a
                    .send(NetCmd::Publish {
                        ws: 44,
                        msg: NeighbourMsg::SurfaceFrame {
                            surface: 9 + u64::from(attempt),
                            source_peer,
                            encoded_h264: vec![0, 0, 0, 1, 0x65],
                            ts: 1,
                            seq: 0,
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
                            nonce: u64::from(attempt),
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
                fastest = fastest.min(started.elapsed());
            }
            assert!(
                fastest < Duration::from_millis(250),
                "every attempt waited for the dead viewer's dial: {fastest:?}"
            );
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
        // Phase `personal_transfer_streams_in_order_with_a_verified_finish_hash`.
        {
            let (a, lookup_a) = endpoint().await;
            let (b, lookup_b) = endpoint().await;
            let a_id = a.id();
            let b_id = b.id();
            let b_addr = b.addr();
            let (cmd_a, _a_task) = Actor::spawn(a, lookup_a);
            let (cmd_b, _b_task) = Actor::spawn(b, lookup_b);
            // Progress is best-effort; leave room for every chunk's report
            // while the receiver side is drained first.
            let (event_a, mut rx_a) = mpsc::channel(256);
            let (event_b, mut rx_b) = mpsc::channel(256);
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
            let bytes = vec![0x9a; PERSONAL_TRANSFER_CHUNK_BYTES.saturating_add(31)];
            std::fs::write(&path, &bytes).unwrap();
            let file = std::fs::File::open(&path).unwrap();
            let transfer_id = [0x45; 16];
            let request_id = [0x46; 16];
            cmd_a
                .send(NetCmd::PublishNearbyTransfer {
                    ws: 45,
                    user_public_key: [0xA6; 32],
                    target: b_id,
                    transfer: NearbyTransferCheckpoint {
                        transfer_id,
                        request_id,
                        total_bytes: u64::try_from(bytes.len()).unwrap(),
                        resume_offset: 0,
                        prefix_hash: *blake3::hash(&[]).as_bytes(),
                        content_hash: *blake3::hash(&bytes).as_bytes(),
                    },
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
                    payload: PersonalFabricPayload::NearbyTransfer(frame),
                    ..
                } = event
                else {
                    continue;
                };
                match frame {
                    PersonalNearbyTransferFrame::Chunk {
                        transfer_id: id,
                        request_id: request,
                        offset,
                        bytes: chunk,
                    } => {
                        assert_eq!((id, request), (transfer_id, request_id));
                        assert_eq!(offset, u64::try_from(received.len()).unwrap());
                        received.extend_from_slice(&chunk);
                    }
                    PersonalNearbyTransferFrame::Finish {
                        transfer_id: id,
                        request_id: request,
                        content_hash,
                    } => {
                        assert_eq!((id, request), (transfer_id, request_id));
                        assert_eq!(content_hash, *blake3::hash(&received).as_bytes());
                        break;
                    }
                    PersonalNearbyTransferFrame::Abort { .. } => panic!("transfer aborted"),
                }
            }
            assert_eq!(received, bytes);

            let mut statuses = Vec::new();
            loop {
                let event = tilo_test_env::within("a message on rx_a", rx_a.recv())
                    .await
                    .unwrap();
                let NetEvent::NearbyTransferProgress {
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
                if status == PersonalTransferStatus::Completed {
                    break;
                }
            }
            assert_eq!(
                statuses.first(),
                Some(&(PersonalTransferStatus::Started, 0))
            );
            assert!(statuses.iter().any(|(status, sent)| {
                *status == PersonalTransferStatus::Progress
                    && *sent == u64::try_from(bytes.len()).unwrap()
            }));
            assert_eq!(
                statuses.last(),
                Some(&(
                    PersonalTransferStatus::Completed,
                    u64::try_from(bytes.len()).unwrap()
                ))
            );

            // Phase `personal_transfer_cancel_stops_only_the_exact_stream`.
            let large = vec![0x3c; PERSONAL_TRANSFER_CHUNK_BYTES.saturating_mul(64)];
            let large_path = dir.path().join("film.mkv");
            std::fs::write(&large_path, &large).unwrap();
            let cancelled_id = [0x47; 16];
            cmd_a
                .send(NetCmd::PublishNearbyTransfer {
                    ws: 45,
                    user_public_key: [0xA6; 32],
                    target: b_id,
                    transfer: NearbyTransferCheckpoint {
                        transfer_id: cancelled_id,
                        request_id,
                        total_bytes: u64::try_from(large.len()).unwrap(),
                        resume_offset: 0,
                        prefix_hash: *blake3::hash(&[]).as_bytes(),
                        content_hash: *blake3::hash(&large).as_bytes(),
                    },
                    file: std::fs::File::open(&large_path).unwrap(),
                })
                .await
                .unwrap();
            cmd_a
                .send(NetCmd::CancelNearbyTransfer {
                    target: a_id,
                    transfer_id: cancelled_id,
                })
                .await
                .unwrap();
            cmd_a
                .send(NetCmd::CancelNearbyTransfer {
                    target: b_id,
                    transfer_id: cancelled_id,
                })
                .await
                .unwrap();
            loop {
                let event = tilo_test_env::within("a message on rx_a", rx_a.recv())
                    .await
                    .unwrap();
                let NetEvent::NearbyTransferProgress {
                    target,
                    transfer_id: id,
                    status,
                    ..
                } = event
                else {
                    continue;
                };
                if id != cancelled_id {
                    // Late progress of the finished stream.
                    continue;
                }
                assert_eq!(target, b_id);
                assert_ne!(
                    status,
                    PersonalTransferStatus::Completed,
                    "a cancelled stream never completes"
                );
                if status == PersonalTransferStatus::Cancelled {
                    break;
                }
            }
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
        spawn_pinned_relay_for_members_on(root, 42, a_pub, b_pub).await
    }

    async fn spawn_pinned_relay_for_members_on(
        root: &ed25519_dalek::SigningKey,
        ws: WorkspaceId,
        a_pub: [u8; 32],
        b_pub: [u8; 32],
    ) -> (Relay, tempfile::TempDir, DeviceCert) {
        let (host_key, host_cert) = enrol(root, DeviceCapabilities::host(), "host");
        let state = tempfile::tempdir().unwrap();
        let config = RelayConfig {
            state_root: state.path().to_path_buf(),
            workspaces: vec![WorkspaceBinding {
                id: ws,
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
            .set_workspace_membership(ws, true, vec![a_pub, b_pub], state.path().to_path_buf())
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
                        nonce: 0,
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
                    .lock_recover()
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
                            .lock_recover()
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
            Reachability::default(),
            None,
        );
        let (cmd_b, _task_b) = Actor::spawn_with_heartbeat(
            b,
            lookup_b,
            Some(identity_b),
            Arc::new(Mutex::new(Vec::new())),
            heartbeat,
            Reachability::default(),
            None,
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
            collab_types::OpKind::Change,
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
            collab_types::OpKind::Change,
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

    /// The person's always-on host keeps a record of their own room that one
    /// of their devices published to it, and hands it to another of their
    /// devices that connects after the first went away; only to hosts, on
    /// the person's continuity topic.
    #[allow(
        clippy::too_many_lines,
        reason = "a host, two of one person's devices and the first leaving before the second arrives in one scenario"
    )]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_persons_host_keeps_their_records_for_a_device_that_comes_later() {
        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let user = root.verifying_key().to_bytes();
        let continuity = tilo_relay_protocol::personal_continuity_workspace_id(&user);
        let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "laptop");
        let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "phone");
        let (relay, _state, _host_cert) = spawn_pinned_relay_for_members_on(
            &root,
            continuity,
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
        let cluster = collab_types::person_room(&user).to_string();
        let mut op = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Snapshot(b"watched one video".to_vec())
                .encode()
                .unwrap(),
            collab_types::document_artifact_id("so.tilo.tube", &cluster, "history"),
            collab_types::Ed25519PublicKey::from_bytes(a_key.verifying_key().to_bytes()),
            collab_types::document_attestation_id("so.tilo.tube", &cluster, "history"),
            collab_types::OpKind::Change,
            std::time::SystemTime::now(),
            1,
        );
        collab_types::sign_op(&mut op, &a_key).unwrap();

        let (a, lookup_a) = identified_endpoint(&a_key).await;
        let (cmd_a, task_a) = Actor::spawn_reporting_addrs(
            a,
            lookup_a,
            Some(identity(a_key, a_cert)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (events_a, mut rx_a) = mpsc::channel(16);
        cmd_a
            .send(NetCmd::JoinWorkspace {
                ws: continuity,
                on_event: events_a,
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
        cmd_a
            .send(NetCmd::PublishToHosts {
                ws: continuity,
                msg: NeighbourMsg::AppDocument {
                    peer: 7,
                    app_id: "so.tilo.tube".into(),
                    cluster_id: cluster.clone(),
                    record_id: "history".into(),
                    envelope: Box::new(op.clone()),
                },
            })
            .await
            .unwrap();
        tilo_test_env::within("the laptop's host session is ready", async {
            while !matches!(rx_a.recv().await, Some(NetEvent::HostReady { .. })) {}
        })
        .await;
        // The host has it once the session is up; the laptop goes away.
        tilo_test_env::within("the host retains the laptop's record", async {
            while relay
                .room_records()
                .retained(continuity)
                .await
                .unwrap_or_default()
                .is_empty()
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        drop(cmd_a);
        task_a.abort();

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
                ws: continuity,
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
        tilo_test_env::within(
            "the phone receives the laptop's record from the host",
            async {
                loop {
                    if let Some(NetEvent::Msg {
                        ws,
                        msg:
                            NeighbourMsg::AppDocument {
                                envelope,
                                cluster_id,
                                ..
                            },
                        ..
                    }) = rx_b.recv().await
                        && *envelope == op
                    {
                        assert_eq!(ws, continuity);
                        assert_eq!(cluster_id, cluster);
                        break;
                    }
                }
            },
        )
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
                place: None,
                chat: None,
                reaction: None,
                wave: None,
                seq: 1,
                sent_at_ms: 1,
                focus: None,
                pointer: Some(tilo_relay_protocol::live::LivePointer::Strip {
                    strip: 1,
                    at: [3.0, 4.0],
                }),
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
                    nonce: 0,
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
                        inbound.push(Inbound::AccessRequest {
                            room,
                            request,
                            route: None,
                        });
                    }
                    NeighbourMsg::MlsRoutedAccessRequest { request, route, .. } => {
                        inbound.push(Inbound::AccessRequest {
                            room,
                            request,
                            route: Some(route),
                        });
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
            collab_types::OpKind::Change,
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
