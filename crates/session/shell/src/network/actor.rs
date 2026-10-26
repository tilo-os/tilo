//! Iroh-gossip actor for workspace collaboration.
//!
//! Direct paths, NAT traversal and stateless relay fallback are selected by
//! iroh. Tilo only names authenticated endpoint ids and workspace topics.

pub(crate) mod governor_lane;
pub(crate) mod native_ingress;
pub(crate) mod own_device_host;

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
mod room_lane;
#[cfg(test)]
mod room_lane_tests;
mod room_members;
use room_members::Admission;
pub(crate) use room_members::RosterDevice;

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
use iroh_gossip::api::Event as GossipEvent;
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
/// direct peer, a workspace host). Armed only while something is
/// ([`ActorState::needs_upkeep`]): a converged desktop never wakes for it.
/// A change of this device's identity material or codecs is announced when
/// it happens, not found by a tick.
pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
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
/// A stream's start travels in the same queue, ahead of its frames, and
/// is never superseded by them.
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

    /// Drop the queued frames (they reference what is lost) and wait for
    /// the next keyframe. The stream's start stays queued.
    fn lose_references(&mut self) {
        self.frames
            .retain(|msg| !matches!(msg, NeighbourMsg::SurfaceFrame { .. }));
        if self.frames.is_empty() {
            self.frames = VecDeque::new();
        }
        self.bytes = self
            .frames
            .iter()
            .fold(0, |bytes, msg| bytes.saturating_add(Self::frame_bytes(msg)));
        self.awaiting_keyframe = self.codec.is_some();
    }

    fn admit(&mut self, msg: &NeighbourMsg) -> bool {
        let keyframe = match msg {
            NeighbourMsg::SurfaceFrame { encoded_h264, .. } => self
                .codec
                .is_none_or(|codec| media::realtime::is_keyframe(codec, encoded_h264)),
            // The stream's start references nothing and is always kept.
            _ => return true,
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
/// Largest byte chunk one personal-fabric transfer frame carries.
pub const PERSONAL_TRANSFER_CHUNK_BYTES: usize = 256 * 1024;
const MAX_ACTIVE_PERSONAL_TRANSFERS: usize = 8;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SurfaceWireFrame {
    workspace: WorkspaceId,
    message: NeighbourMsg,
}

/// Versioned private-device traffic. It travels on the bidirectional
/// streams of the gossip connection two devices already share (gossip uses
/// unidirectional streams only, [`read_personal_fabric`]), and leaves the
/// legacy Postcard enum untouched.
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
    /// This device's own room host opened: join the rooms it hosts that
    /// wait for it.
    LocalHostReady,
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
    /// Tell `ended` once workspace `ws` has no session with a host left: at
    /// once when it has none now. What waits on a room's host lasts as long
    /// as the room's sessions do.
    WatchHostSessions {
        ws: u64,
        ended: tokio::sync::oneshot::Sender<()>,
    },
    /// Ask another of the person's devices for one of its host's own
    /// operations, on a session opened for it.
    OwnDeviceHost(Box<own_device_host::Request>),
    /// Take part in workspace `ws`: its events reach `on_event`, and its
    /// workspace hosts are dialled. A room's traffic travels over its host
    /// sessions alone.
    JoinWorkspace {
        ws: WorkspaceId,
        on_event: mpsc::Sender<NetEvent>,
    },
    /// [`NetCmd::JoinWorkspace`] for the topic of the person's own devices
    /// (their continuity), which also travels directly between those
    /// devices over gossip.
    JoinPersonalTopic {
        ws: WorkspaceId,
        on_event: mpsc::Sender<NetEvent>,
    },
    LeaveWorkspace {
        ws: WorkspaceId,
    },
    /// Room `ws`'s admitted roster changed: `devices` are every device its
    /// governor admitted, by endpoint id, with its certificate's expiry.
    /// They alone send and receive the room's window streams; a device that
    /// left, or whose certificate expired, has its connections closed.
    SetRoomRoster {
        ws: WorkspaceId,
        devices: Vec<RosterDevice>,
    },
    Publish {
        ws: WorkspaceId,
        msg: NeighbourMsg,
        /// When the message was published, which pointer pacing measures
        /// its gap from: a command the actor reaches late still paces as the
        /// moves happened, not as the actor got to them.
        at: tokio::time::Instant,
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
        /// The host dialled: the governor `hint` names, or a host of the
        /// room while the governor is out of reach.
        via: Box<tilo_peer_state::join::TransportHint>,
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
        connection_incarnation: u64,
        fingerprint: [u8; 32],
        msg: NeighbourMsg,
    },
    /// A peer opened (`Some`) or closed (`None`) a window-stream connection
    /// to this device.
    SurfaceIngress {
        peer: EndpointId,
        connection_incarnation: u64,
        connection: Option<iroh::endpoint::Connection>,
    },
    SurfaceFanoutComplete {
        ws: WorkspaceId,
        generation: u64,
        msg: NeighbourMsg,
        outcomes: Vec<SurfaceFanoutOutcome>,
        all_delivered: bool,
    },
    /// The window-stream route to `peer` came up, or could not be opened
    /// (`None`).
    SurfaceRouteOpened {
        peer: EndpointId,
        connection: Option<iroh::endpoint::Connection>,
    },
    /// A unit held for a viewer while its route opened was written, or not.
    HeldSurfaceUnitWritten {
        key: (WorkspaceId, SurfaceId, EndpointId),
        generation: u64,
        delivered: bool,
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
    /// A file this device's identity material is read from changed.
    IdentityMaterialChanged,
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
    /// A room's lane subscribed to its topic, or failed to (`None`).
    Subscribed {
        ws: WorkspaceId,
        serial: u64,
        receiver: Option<Box<iroh_gossip::api::GossipReceiver>>,
    },
    /// A direct send queued on a room's lane went, or did not.
    DirectSent {
        ws: WorkspaceId,
        outcome: Box<room_lane::DirectOutcome>,
    },
}

/// Write one unit of a window's stream on `peer`'s open route. Routes are
/// opened apart from this ([`ActorState::open_surface_route`]); what is
/// for a viewer whose route is still opening waits for it there.
async fn deliver_surface_frame(
    peer: EndpointId,
    connection: iroh::endpoint::Connection,
    bytes: Vec<u8>,
) -> SurfaceFanoutOutcome {
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
    SurfaceFanoutOutcome { peer, delivered }
}

struct SurfaceFanoutOutcome {
    peer: EndpointId,
    delivered: bool,
}

/// A window's units for one viewer whose route is being opened, or whose
/// route came up and is being given them in order. While this exists the
/// viewer's further units join it rather than overtake it.
struct HeldSurfaceUnits {
    backlog: SurfaceFrameBacklog,
    /// One of the units is being written now.
    writing: bool,
    /// Tells a write's completion from one for a stream since restarted.
    generation: u64,
    /// The viewer's route failed to open or broke: until it is up again, a
    /// window streamed to the whole workspace also sends its units to the
    /// viewer on gossip.
    unreachable: bool,
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
    /// Which subscription this is: one asked for before the room was left
    /// and joined again is not it.
    serial: u64,
    /// Pumps the subscription's events, once the room's lane made it.
    pump: Option<tokio::task::JoinHandle<()>>,
    /// Fingerprint of the identity material last announced on the topic, so
    /// a renewed certificate or a new revocation goes out the tick it lands
    /// and an unchanged one never does.
    identity_sent: Option<[u8; 32]>,
    /// When this device last announced its identity to the topic's
    /// neighbours ([`IDENTITY_ANSWER_INTERVAL`]).
    identity_announced: Option<Instant>,
}

impl TopicRuntime {
    /// Whether this device announced its identity here within
    /// [`IDENTITY_ANSWER_INTERVAL`].
    fn announced_identity_lately(&self) -> bool {
        self.identity_announced
            .is_some_and(|announced| announced.elapsed() < IDENTITY_ANSWER_INTERVAL)
    }

    fn new(serial: u64) -> Self {
        Self {
            serial,
            pump: None,
            identity_sent: None,
            identity_announced: None,
        }
    }
}

struct HostRuntime {
    native_scope: Option<Arc<native_ingress::Scope>>,
    /// The session goes to this device's own host, in this process: no
    /// remote party, nothing to keep up.
    local: bool,
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

/// Another of the person's devices routing through this one; the exit
/// decides whether it may (`crate::shell::device_vpn`).
#[cfg(target_os = "linux")]
#[derive(Debug)]
struct DeviceVpnProtocol;

#[cfg(target_os = "linux")]
impl ProtocolHandler for DeviceVpnProtocol {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        tokio::spawn(crate::shell::device_vpn::serve_connection(connection));
        Ok(())
    }
}

#[derive(Clone)]
struct SurfaceStreamProtocol {
    internal_tx: mpsc::Sender<InternalEvent>,
    input_tx: mpsc::Sender<InternalEvent>,
    next_connection_incarnation: Arc<AtomicU64>,
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
        let connection_incarnation = self
            .next_connection_incarnation
            .fetch_add(1, Ordering::Relaxed)
            .max(1);
        let _ = tx
            .send(InternalEvent::SurfaceIngress {
                peer,
                connection_incarnation,
                connection: Some(connection.clone()),
            })
            .await;
        tokio::spawn(async move {
            loop {
                let Ok(mut recv) = connection.accept_uni().await else {
                    let _ = tx
                        .send(InternalEvent::SurfaceIngress {
                            peer,
                            connection_incarnation,
                            connection: None,
                        })
                        .await;
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
                    connection_incarnation,
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

/// Read the personal fabric frames each connection gossip runs brings,
/// dialled or accepted, until gossip stops.
fn read_personal_fabric_on_gossip_connections(
    mut connections: mpsc::UnboundedReceiver<iroh::endpoint::Connection>,
    tx: mpsc::Sender<InternalEvent>,
) {
    tokio::spawn(async move {
        let incarnations = AtomicU64::new(1);
        while let Some(connection) = connections.recv().await {
            read_personal_fabric(connection, tx.clone(), &incarnations);
        }
    });
}

/// Read the personal fabric frames `connection` brings, whichever side
/// opened it, into the actor, until it closes. Each frame comes on a
/// bidirectional stream of its own, which gossip never opens or accepts on
/// the connection it shares; the return direction carries nothing.
fn read_personal_fabric(
    connection: iroh::endpoint::Connection,
    tx: mpsc::Sender<InternalEvent>,
    incarnations: &AtomicU64,
) {
    let peer = connection.remote_id();
    let connection_incarnation = incarnations.fetch_add(1, Ordering::Relaxed).max(1);
    tokio::spawn(async move {
        loop {
            let Ok((_, mut recv)) = connection.accept_bi().await else {
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
        NetCmd::OwnDeviceHost(_) => "asking one of the person's devices",
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

    /// [`Self::spawn_deferring_relays`] for a device that hosts the rooms
    /// placed on it: with `hosting`, its endpoint also serves this device's
    /// room host's protocols (refusing them until the host opens), and the
    /// native owner joins the rooms hosted here through that host.
    pub fn spawn_hosting(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        host_identity: Option<HostIdentity>,
        listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
        reachability: Reachability,
        hosting: Option<Hosting>,
    ) -> (mpsc::Sender<NetCmd>, tokio::task::JoinHandle<()>) {
        Self::spawn_with_heartbeat(
            endpoint,
            lookup,
            host_identity,
            listen_addrs,
            HEARTBEAT_INTERVAL,
            reachability,
            hosting,
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
        hosting: Option<Hosting>,
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
            hosting,
        ));
        (cmd_tx, task)
    }
}

/// This device's room host, for the actor: the handle the native owner
/// joins the rooms hosted here through, and the handlers serving the host
/// on the router ([`super::local_host`]).
pub struct Hosting {
    pub host: super::local_host::LocalHost,
    pub protocols: super::local_host::Protocols,
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
    /// This device's room host, through which the native owner joins the
    /// rooms hosted here.
    local_host: Option<super::local_host::LocalHost>,
    /// Governance invocations waiting for their room's relay to answer.
    room_governance: room_governance::Waiting,
    /// The endpoint's relays, held back until a remote party appears.
    reachability: Reachability,
    internal_tx: mpsc::Sender<InternalEvent>,
    /// The person's own topics, the only ones carried over gossip too.
    gossip_topics: HashSet<WorkspaceId>,
    topics: HashMap<WorkspaceId, TopicRuntime>,
    /// Each room's remote sends, so one room's slow devices hold up no
    /// other room ([`room_lane`]).
    lanes: room_lane::Lanes,
    /// Gossip itself, whose connection to a member the personal fabric
    /// shares (`Gossip::connection`).
    gossip: Gossip,
    next_topic_serial: u64,
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
    /// Each room's admitted roster ([`NetCmd::SetRoomRoster`]), and the
    /// window-stream connections peers opened to this device, bound to it.
    room_members: room_members::RoomMembers,
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
    /// Window-stream routes being opened. A dial runs until it connects or
    /// fails; nothing waits on it but the units held for that viewer.
    surface_routes_opening: HashMap<EndpointId, tokio::task::JoinHandle<()>>,
    /// Units for viewers whose route is not up yet, per window and viewer.
    held_surface_units: HashMap<(WorkspaceId, SurfaceId, EndpointId), HeldSurfaceUnits>,
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

/// The exact certified device a personal-fabric stream goes to, the gossip
/// connection it shares with it, and the personal workspace and user key
/// its frames are authenticated under.
struct PersonalStreamRoute {
    connection: iroh::endpoint::Connection,
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

    /// Whether `peer` is a member of `ws` for window streams: on the room's
    /// admitted roster, or, on the person's own topic, a member there.
    fn is_surface_member(&self, ws: WorkspaceId, peer: EndpointId) -> bool {
        if self.room_members.has_roster(ws) {
            self.room_members
                .member(ws, peer, tilo_clock::unix_now_ns())
                .is_some()
        } else {
            self.peers_seen.contains_key(&(ws, peer))
        }
    }

    /// Every device a window of `ws` streams to ([`Self::is_surface_member`]).
    fn surface_members(&self, ws: WorkspaceId) -> Vec<EndpointId> {
        if self.room_members.has_roster(ws) {
            return self.room_members.members(ws, tilo_clock::unix_now_ns());
        }
        self.peers_seen
            .keys()
            .filter(|(peer_ws, _)| *peer_ws == ws)
            .map(|(_, peer)| *peer)
            .collect()
    }

    /// Take room `ws`'s admitted roster as `devices` (none, and not `keep`,
    /// once this device left it): the connections bound to a membership that
    /// ended close, and so does this device's own to a device no room holds
    /// any more.
    fn set_room_roster(&mut self, ws: WorkspaceId, devices: Vec<RosterDevice>, keep: bool) {
        let local = self.endpoint.id();
        let next = &mut self.next_membership_epoch;
        let removed = self.room_members.set_roster(
            ws,
            local,
            devices,
            || {
                *next = next.saturating_add(1).max(1);
                MembershipEpoch(*next)
            },
            keep,
        );
        self.close_removed(removed, b"membership removed");
    }

    /// Close what ended memberships removed: the connections bound to them,
    /// and this device's own to a device no room holds any more.
    fn close_removed(&mut self, removed: room_members::Removed, reason: &[u8]) {
        for connection in removed.connections {
            connection.close(0_u32.into(), reason);
        }
        for device in removed.devices {
            if let Some(connection) = self.surface_connections.remove(&device) {
                connection.close(0_u32.into(), reason);
            }
            if let Some(dial) = self.surface_routes_opening.remove(&device) {
                dial.abort();
            }
            self.held_surface_units
                .retain(|(_, _, peer), _| *peer != device);
        }
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
        true
    }

    /// Handle one [`NetCmd`]. Returns `false` once the command channel has
    /// closed, so [`run`]'s loop should stop.
    #[allow(
        clippy::too_many_lines,
        reason = "the actor command match is the single auditable transport mutation boundary"
    )]
    async fn handle_command(&mut self, command: Option<NetCmd>) -> bool {
        if let Some(why) = command.as_ref().and_then(remote_party_command)
            && !command
                .as_ref()
                .is_some_and(|command| self.stays_local(command))
        {
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
            Some(NetCmd::LocalHostReady) => self.retry_native_routes(),
            Some(NetCmd::OwnDeviceHost(request)) => self.ask_own_device_host(*request),
            Some(NetCmd::GovernorAction { ws, room, request }) => {
                self.governor_action(ws, room, request);
            }
            Some(NetCmd::NativePublish { ws }) => self.drive_native_publication(ws),
            Some(NetCmd::NativeEnrollment(command)) => self.native_enrollment.handle(
                command,
                self.endpoint.clone(),
                self.host_identity.as_ref(),
            ),
            Some(NetCmd::RoomGovernance(request)) => self.send_room_governance(*request),
            Some(NetCmd::WatchHostSessions { ws, ended }) => {
                let now = self.sessions_of(ws);
                self.room_governance.watch_sessions(ws, now, ended);
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
                // The sessions already up take the broker's private channel
                // as they are: a session does not depend on the broker, so
                // none is ended and dialled again for it.
                let live: Vec<_> = self
                    .host_sessions
                    .iter()
                    .map(|(&(ws, peer), runtime)| (ws, peer, runtime.session.clone()))
                    .collect();
                for (ws, peer, session) in live {
                    let actions = self.serve_actions(ws, peer, &session);
                    if let Some(runtime) = self.host_sessions.get_mut(&(ws, peer))
                        && let Some(previous) = std::mem::replace(&mut runtime.actions, actions)
                    {
                        previous.abort();
                    }
                }
            }
            Some(NetCmd::JoinWorkspace { ws, on_event }) => {
                self.handle_join_workspace(ws, on_event);
            }
            Some(NetCmd::JoinPersonalTopic { ws, on_event }) => {
                self.gossip_topics.insert(ws);
                self.handle_join_workspace(ws, on_event);
            }
            Some(NetCmd::SetRoomRoster { ws, devices }) => {
                self.set_room_roster(ws, devices, true);
            }
            Some(NetCmd::LeaveWorkspace { ws }) => {
                self.gossip_topics.remove(&ws);
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
                self.set_room_roster(ws, Vec::new(), false);
                self.pending_surface_frames
                    .retain(|(frame_ws, _), _| *frame_ws != ws);
                self.held_surface_units
                    .retain(|(held_ws, _, _), _| *held_ws != ws);
                self.surface_fanout_viewers
                    .retain(|(frame_ws, _), _| *frame_ws != ws);
                self.surface_fanout_tasks.retain(|(task_ws, _), task| {
                    if *task_ws == ws {
                        task.1.abort();
                    }
                    *task_ws != ws
                });
                self.lanes.close(ws);
                if let Some(pump) = self.topics.remove(&ws).and_then(|topic| topic.pump) {
                    pump.abort();
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
                    self.announce_identity(ws);
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
            Some(NetCmd::Publish { ws, msg, at }) => {
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
                    // useful. Enqueue it before any direct-peer dial timeout.
                    publish(
                        &self.topics,
                        &self.host_sessions,
                        &mut self.lanes,
                        ws,
                        &msg,
                        false,
                        false,
                        true,
                    );
                    let _ = self.start_surface_fanout(ws, msg);
                } else {
                    self.hold_for_host(ws, &msg, false);
                    self.publish_paced(ws, &msg, at);
                }
            }
            Some(NetCmd::SetWake(LoopWake(wake))) => self.wake = Some(wake),
            Some(NetCmd::PublishToHosts { ws, msg }) => {
                self.hold_for_host(ws, &msg, false);
                publish(
                    &self.topics,
                    &self.host_sessions,
                    &mut self.lanes,
                    ws,
                    &msg,
                    false,
                    false,
                    true,
                );
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
                        &mut self.lanes,
                        ws,
                        &msg,
                        false,
                        false,
                        true,
                    );
                }
            }
            Some(NetCmd::PublishWorkspaceControl { ws, msg, route }) => {
                self.publish_workspace_control(ws, msg, route);
            }
            Some(NetCmd::PublishSurfaceTo { ws, msg, viewers }) => {
                self.publish_surface_to(ws, msg, &viewers);
            }
            Some(NetCmd::PublishRemoteAction { ws, target, msg }) => {
                self.publish_remote_action(ws, target, msg);
            }
            Some(NetCmd::PublishRecordContent(delivery)) => {
                self.publish_record_content(delivery);
            }
            Some(NetCmd::PublishCheckpoint { ws, msg }) => {
                if self.native_routes.contains(ws) {
                    return true;
                }
                self.hold_for_host(ws, &msg, true);
                publish(
                    &self.topics,
                    &self.host_sessions,
                    &mut self.lanes,
                    ws,
                    &msg,
                    true,
                    true,
                    true,
                );
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
                for ws in self.topics.keys().copied() {
                    self.lanes
                        .queue_or_shed(ws, room_lane::Work::JoinPeers(vec![id]));
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
                    routes.schedule(ws, addr.clone(), expectation.clone());
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

    /// Send an encoded surface frame (or a targeted stream's start) over the
    /// media ALPN to every participant of this workspace it is for. A viewer
    /// whose route is not up yet gets it held until the route is, in order
    /// with the rest of its units. A unit that did not reach every
    /// participant goes on gossip as a compatibility fallback, unless the
    /// window streams to named viewers only. Returns false when the unit
    /// cannot go on the media ALPN at all. The workspace host is still
    /// published to separately.
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
        let only = self.surface_fanout_viewers.get(&key).cloned();
        let targeted = only.is_some();
        let peers: Vec<_> = self
            .surface_members(ws)
            .into_iter()
            .filter(|peer| only.as_ref().is_none_or(|viewers| viewers.contains(peer)))
            .collect();
        // Units held for a device this window no longer streams to go.
        self.held_surface_units
            .retain(|(held_ws, surface, peer), _| {
                (*held_ws, *surface) != key || peers.contains(peer)
            });
        let workspace_peer_count = peers.len();
        let (targets, on_the_way) = self.surface_fanout_targets(ws, key.1, peers, &msg);
        if targets.is_empty() {
            if !targeted && (workspace_peer_count == 0 || on_the_way < workspace_peer_count) {
                publish(
                    &self.topics,
                    &self.host_sessions,
                    &mut self.lanes,
                    ws,
                    &msg,
                    false,
                    true,
                    false,
                );
            }
            return true;
        }
        let internal_tx = self.internal_tx.clone();
        let Some(generation) = self.next_surface_fanout_generation.checked_add(1) else {
            tracing::warn!(ws, "surface fanout generation exhausted");
            return false;
        };
        self.next_surface_fanout_generation = generation;
        let task = tokio::spawn(async move {
            let mut pending = FuturesUnordered::new();
            for (peer, connection) in targets {
                pending.push(deliver_surface_frame(peer, connection, bytes.clone()));
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
                    all_delivered: sent.saturating_add(on_the_way) == workspace_peer_count,
                })
                .await;
        });
        self.surface_fanout_tasks.insert(key, (generation, task));
        true
    }

    /// Split `peers` into those whose route is open and takes `msg` now,
    /// and those it is held for; the count is of the held ones whose route
    /// is opening, for which it is on its way.
    fn surface_fanout_targets(
        &mut self,
        ws: WorkspaceId,
        surface: SurfaceId,
        peers: Vec<EndpointId>,
        msg: &NeighbourMsg,
    ) -> (Vec<(EndpointId, iroh::endpoint::Connection)>, usize) {
        // Each viewer is an independent QUIC route: write on the open ones
        // concurrently, so one viewer whose route is still opening (or will
        // not open) never holds up the healthy ones. Its units wait for its
        // route instead, in their order.
        let mut targets = Vec::new();
        // Held for a viewer whose route is opening: on its way, not missed.
        let mut on_the_way = 0usize;
        for peer in peers {
            let held = (ws, surface, peer);
            match self.live_surface_route(peer) {
                Some(connection) if !self.held_surface_units.contains_key(&held) => {
                    targets.push((peer, connection));
                }
                _ => {
                    self.hold_surface_unit(held, msg);
                    if self
                        .held_surface_units
                        .get(&held)
                        .is_some_and(|held| !held.unreachable)
                    {
                        on_the_way = on_the_way.saturating_add(1);
                    }
                }
            }
        }
        (targets, on_the_way)
    }

    /// The open window-stream route to `peer`, if there is one.
    fn live_surface_route(&mut self, peer: EndpointId) -> Option<iroh::endpoint::Connection> {
        let connection = self.surface_connections.get(&peer)?;
        if connection.close_reason().is_some() {
            self.surface_connections.remove(&peer);
            return None;
        }
        Some(connection.clone())
    }

    /// Hold `msg` for the viewer of `key` until its route is up, and open
    /// that route if it is neither up nor being opened.
    fn hold_surface_unit(&mut self, key: (WorkspaceId, SurfaceId, EndpointId), msg: &NeighbourMsg) {
        let codec = self
            .pending_surface_frames
            .get(&(key.0, key.1))
            .map_or(Some(media::StreamCodec::H264), |backlog| backlog.codec);
        let generation = self.next_surface_fanout_generation.saturating_add(1);
        self.next_surface_fanout_generation = generation;
        self.held_surface_units
            .entry(key)
            .or_insert_with(|| HeldSurfaceUnits {
                backlog: SurfaceFrameBacklog {
                    codec,
                    ..SurfaceFrameBacklog::default()
                },
                writing: false,
                generation,
                unreachable: false,
            })
            .backlog
            .push(msg.clone());
        if self.live_surface_route(key.2).is_some() {
            self.write_held_surface_unit(key);
        } else {
            self.open_surface_route(key.2);
        }
    }

    /// Open the window-stream route to `peer` unless it is being opened.
    /// The dial runs as long as connecting takes; its outcome comes back as
    /// [`InternalEvent::SurfaceRouteOpened`].
    fn open_surface_route(&mut self, peer: EndpointId) {
        if self.surface_routes_opening.contains_key(&peer) {
            return;
        }
        let addr = self
            .peers
            .get(&peer)
            .cloned()
            .unwrap_or_else(|| EndpointAddr::new(peer));
        let endpoint = self.endpoint.clone();
        let internal_tx = self.internal_tx.clone();
        let task = tokio::spawn(async move {
            let connection = endpoint
                .connect(addr, SURFACE_STREAM_ALPN)
                .await
                .inspect_err(|error| {
                    tracing::debug!(peer = %peer, %error, "window-stream route failed to open");
                })
                .ok();
            let _ = internal_tx
                .send(InternalEvent::SurfaceRouteOpened { peer, connection })
                .await;
        });
        self.surface_routes_opening.insert(peer, task);
    }

    /// [`InternalEvent::SurfaceRouteOpened`]: give the viewer what was held
    /// for it, in order; or, when its route could not be opened, keep only
    /// what does not reference lost units, until its next unit opens it
    /// again.
    fn surface_route_opened(
        &mut self,
        peer: EndpointId,
        connection: Option<iroh::endpoint::Connection>,
    ) {
        self.surface_routes_opening.remove(&peer);
        let mut keys: Vec<_> = self
            .held_surface_units
            .keys()
            .filter(|(_, _, held_peer)| *held_peer == peer)
            .copied()
            .collect();
        keys.retain(|key| {
            let member = self.is_surface_member(key.0, peer);
            if !member {
                self.held_surface_units.remove(key);
            }
            member
        });
        let Some(connection) = connection else {
            for key in keys {
                self.surface_route_lost(key);
            }
            return;
        };
        if keys.is_empty() && !self.surface_connections.contains_key(&peer) {
            // Nothing is streamed to the device any more.
            connection.close(0_u32.into(), b"no window streamed");
            return;
        }
        self.surface_connections.insert(peer, connection);
        for key in keys {
            if let Some(held) = self.held_surface_units.get_mut(&key) {
                held.unreachable = false;
            }
            self.write_held_surface_unit(key);
        }
    }

    /// `key`'s viewer could not be reached on its route. What was held for
    /// it goes on gossip when the window streams to the whole workspace;
    /// otherwise the frames, which reference what is lost, go. The stream's
    /// start stays held, and the viewer's next unit opens a route again.
    fn surface_route_lost(&mut self, key: (WorkspaceId, SurfaceId, EndpointId)) {
        let targeted = self.surface_fanout_viewers.contains_key(&(key.0, key.1));
        let Some(held) = self.held_surface_units.get_mut(&key) else {
            return;
        };
        held.unreachable = true;
        if !targeted {
            while let Some(msg) = held.backlog.pop() {
                publish(
                    &self.topics,
                    &self.host_sessions,
                    &mut self.lanes,
                    key.0,
                    &msg,
                    false,
                    true,
                    false,
                );
            }
        }
        held.backlog.lose_references();
    }

    /// Write the next unit held for `key`'s viewer on its open route, one at
    /// a time; once none is left, the viewer takes the window's fanout.
    fn write_held_surface_unit(&mut self, key: (WorkspaceId, SurfaceId, EndpointId)) {
        let Some(connection) = self.live_surface_route(key.2) else {
            return;
        };
        let (bytes, held) = loop {
            let Some(held) = self.held_surface_units.get_mut(&key) else {
                return;
            };
            if held.writing {
                return;
            }
            let Some(message) = held.backlog.pop() else {
                self.held_surface_units.remove(&key);
                return;
            };
            let wire = SurfaceWireFrame {
                workspace: key.0,
                message,
            };
            match postcard::to_allocvec(&wire) {
                Ok(bytes) if bytes.len() <= MAX_SURFACE_FRAME_BYTES => break (bytes, held),
                _ => held.backlog.lose_references(),
            }
        };
        held.writing = true;
        let generation = held.generation;
        let internal_tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let outcome = deliver_surface_frame(key.2, connection, bytes).await;
            let _ = internal_tx
                .send(InternalEvent::HeldSurfaceUnitWritten {
                    key,
                    generation,
                    delivered: outcome.delivered,
                })
                .await;
        });
    }

    /// [`InternalEvent::HeldSurfaceUnitWritten`].
    fn held_surface_unit_written(
        &mut self,
        key: (WorkspaceId, SurfaceId, EndpointId),
        generation: u64,
        delivered: bool,
    ) {
        let Some(held) = self.held_surface_units.get_mut(&key) else {
            return;
        };
        if held.generation != generation {
            return;
        }
        held.writing = false;
        if !delivered {
            self.surface_connections.remove(&key.2);
            self.surface_route_lost(key);
            return;
        }
        self.write_held_surface_unit(key);
    }

    /// Forget what is held for `surface`'s viewers: its stream restarted or
    /// ended.
    fn drop_held_surface_units(&mut self, ws: WorkspaceId, surface: SurfaceId) {
        self.held_surface_units
            .retain(|(held_ws, held_surface, _), _| (*held_ws, *held_surface) != (ws, surface));
    }

    async fn publish_personal_fabric(
        &self,
        ws: WorkspaceId,
        user_public_key: [u8; 32],
        payload: PersonalFabricPayload,
    ) {
        self.publish_personal_fabric_to(ws, user_public_key, payload, None)
            .await;
    }

    async fn publish_personal_fabric_to(
        &self,
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
        // Every member of the topic, on the gossip connection this device
        // has with it (`Gossip::connection`): a member is a gossip
        // neighbour, and gossip holds the connection before it names one, so
        // a restarted peer's is the one it just reached this device on.
        let targets: Vec<EndpointId> = self
            .peers_seen
            .keys()
            .filter(|(topic, peer)| {
                *topic == ws && exact_target.is_none_or(|target| *peer == target)
            })
            .map(|(_, peer)| *peer)
            .collect();
        for peer in targets {
            // A member whose link just went down misses the frame: when the
            // link returns, each side asks the other for its fabric anew.
            let Some(connection) = self.gossip.connection(peer) else {
                tracing::debug!(ws, peer = %peer, "no gossip link to carry a personal fabric frame");
                continue;
            };
            send_personal_fabric_frame(ws, peer, &connection, &bytes).await;
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
        let Some(connection) = self.gossip.connection(target) else {
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
            connection,
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
    fn publish_workspace_control(
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
                // Input lands within its frame budget or not at all: a
                // backed-up room has nothing better to do with it.
                let _ = self.publish_direct(ws, target, msg);
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
                *backlog = SurfaceFrameBacklog {
                    codec: media::StreamCodec::from_wire_label(codec),
                    ..SurfaceFrameBacklog::default()
                };
                backlog.lose_references();
                self.drop_held_surface_units(ws, *surface);
            }
            NeighbourMsg::SurfaceStreamEnd { surface, .. } => {
                self.pending_surface_frames.remove(&(ws, *surface));
                self.drop_held_surface_units(ws, *surface);
                if let Some((_, task)) = self.surface_fanout_tasks.remove(&(ws, *surface)) {
                    task.abort();
                }
            }
            _ => {}
        }
    }

    /// [`NetCmd::PublishSurfaceTo`]: one window's stream to exactly its
    /// viewers. Its start and frames take the per-window media fanout
    /// restricted to them, in order, held for a viewer until its route is
    /// up; the stream's end and control grants go to each directly.
    fn publish_surface_to(&mut self, ws: WorkspaceId, msg: NeighbourMsg, viewers: &[u64]) {
        self.observe_surface_stream(ws, &msg);
        let targets = self
            .surface_members(ws)
            .into_iter()
            .filter(|peer| viewers.contains(&crate::network::behaviour::peer_liveness_handle(peer)))
            .collect::<Vec<_>>();
        match msg {
            NeighbourMsg::SurfaceFrame { .. } | NeighbourMsg::SurfaceStreamStart { .. } => {
                let key = (ws, fanout_surface(&msg));
                self.surface_fanout_viewers.insert(key, targets);
                let _ = self.start_surface_fanout(ws, msg);
            }
            NeighbourMsg::SurfaceStreamEnd { .. } | NeighbourMsg::PermissionGrant { .. } => {
                if let NeighbourMsg::SurfaceStreamEnd { surface, .. } = &msg {
                    self.surface_fanout_viewers.remove(&(ws, *surface));
                }
                for target in targets {
                    let _ = self.publish_direct(ws, target, msg.clone());
                }
            }
            _ => tracing::warn!(ws, "refused a non-stream message on the viewer route"),
        }
    }

    /// Send a remote-action frame to exactly `target`: directly when the
    /// device is live on this workspace's mesh, otherwise through an
    /// authenticated workspace host, which routes it to that device alone.
    fn publish_remote_action(&mut self, ws: WorkspaceId, target: EndpointId, msg: NeighbourMsg) {
        let NeighbourMsg::RemoteAction { frame } = msg else {
            tracing::warn!(
                ws,
                "refused a non-action message on the remote-action route"
            );
            return;
        };
        if self.is_surface_member(ws, target) {
            match self.publish_direct(ws, target, NeighbourMsg::RemoteAction { frame }) {
                // How it went comes back from the room's lane.
                Ok(()) => return,
                Err(msg) => {
                    if let NeighbourMsg::RemoteAction { frame } = *msg {
                        self.relay_remote_action(ws, target, frame);
                    }
                }
            }
            return;
        }
        self.relay_remote_action(ws, target, frame);
    }

    /// A remote action that could not go to `target` directly: through an
    /// authenticated workspace host, or held until the device is seen.
    fn relay_remote_action(&mut self, ws: WorkspaceId, target: EndpointId, frame: Vec<u8>) {
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
    fn send_room_governance(&mut self, request: room_governance::Request) {
        let (ws, to) = (request.ws, request.to);
        room_governance::trace::note(|| {
            format!(
                "send {:?} {} on ws {ws} to {to:?}: sessions {:?}, replaying {:?}",
                request.invoke.invocation,
                request.invoke.target.address.action,
                self.sessions_of(ws),
                self.host_sessions
                    .iter()
                    .filter(|((session_ws, _), _)| *session_ws == ws)
                    .map(|((_, host), runtime)| (
                        host.fmt_short().to_string(),
                        runtime.replaying.load(std::sync::atomic::Ordering::Acquire)
                    ))
                    .collect::<Vec<_>>()
            )
        });
        match to {
            room_governance::Addressee::Device(host) => {
                // Its answer comes on a session of its workspace: with none
                // up it waits for one being established, and with none of
                // those there is nothing to wait for.
                match self.sessions_of(ws) {
                    room_governance::Sessions::Open => {}
                    room_governance::Sessions::Connecting => {
                        self.room_governance.hold_for_session(request);
                        return;
                    }
                    room_governance::Sessions::Closed => {
                        let _ = request.reply.send(Err(
                            "this device holds no session with the room's host".into(),
                        ));
                        return;
                    }
                }
                if let Some(frame) = self.room_governance.send(request, host, Instant::now()) {
                    self.publish_to_host_sessions(ws, host, &frame);
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

    /// Send `frame`, addressed to the room host whose device is `host`, on a
    /// host session of `ws`: the one whose handshake authenticated that
    /// device, else any other, which routes it there. Never the direct
    /// surface route: a device hosting its own rooms is also a member of
    /// them under the same key, and its shell does not answer for its host.
    fn publish_to_host_sessions(&self, ws: WorkspaceId, host: EndpointId, frame: &[u8]) {
        let device = *host.as_bytes();
        let mut sessions: Vec<_> = self
            .host_sessions
            .iter()
            .filter(|((session_ws, _), runtime)| *session_ws == ws && reaches_governor(runtime))
            .map(|(_, runtime)| &runtime.session)
            .collect();
        sessions.sort_by_key(|session| session.relay_device_public_key() != device);
        let sent = sessions.into_iter().any(|session| {
            session
                .publish_remote_action(device, frame.to_vec())
                .is_ok()
        });
        room_governance::trace::note(|| format!("published to ws {ws} host {host}: sent {sent}"));
        if !sent {
            tracing::warn!(ws, %host, "no session with the room's host to send a request on");
        }
    }

    fn flush_pending_remote_actions(&mut self, ws: WorkspaceId, peer: EndpointId) {
        for frame in self
            .pending_remote_actions
            .take_for(ws, peer, Instant::now())
        {
            self.publish_remote_action(ws, peer, NeighbourMsg::RemoteAction { frame });
        }
    }

    fn publish_record_content(&mut self, delivery: RecordContentDelivery) {
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
        let msg = match target {
            Some(target) if self.is_surface_member(ws, target) => {
                match self.publish_direct(ws, target, msg) {
                    // How it went comes back from the room's lane.
                    Ok(()) => return,
                    Err(msg) => *msg,
                }
            }
            _ => msg,
        };
        self.relay_record_content(ws, target, &msg);
    }

    /// Record content that could not go to `target` directly: through the
    /// workspace hosts to that device alone, else to the whole room.
    fn relay_record_content(
        &mut self,
        ws: WorkspaceId,
        target: Option<EndpointId>,
        msg: &NeighbourMsg,
    ) {
        // The workspace hosts route it to that device alone when it is
        // connected to them.
        if let Some(target) = target
            && let Ok(frame) = postcard::to_allocvec(msg)
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
            &mut self.lanes,
            ws,
            msg,
            false,
            true,
            false,
        );
    }

    /// Queue one message for exactly `target` on the authenticated direct
    /// surface/control plane, on the room's lane: the peer must be live on
    /// this workspace. Gives the message back when it cannot go that way
    /// now; how a queued one went comes back as
    /// [`InternalEvent::DirectSent`] ([`Self::direct_sent`]).
    fn publish_direct(
        &mut self,
        ws: WorkspaceId,
        target: EndpointId,
        msg: NeighbourMsg,
    ) -> Result<(), Box<NeighbourMsg>> {
        if !self.is_surface_member(ws, target) {
            tracing::warn!(ws, peer = %target, "direct route is no longer live");
            return Err(Box::new(msg));
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
            return Err(Box::new(wire.message));
        };
        let direct = room_lane::Direct {
            addr: self
                .peers
                .get(&target)
                .cloned()
                .unwrap_or_else(|| EndpointAddr::new(target)),
            cached: self.surface_connections.get(&target).cloned(),
            msg: wire.message,
            bytes,
            budget,
        };
        self.lanes.queue_direct(ws, direct).inspect_err(|_| {
            tracing::warn!(ws, peer = %target, "room's remote work is backed up; not sent directly");
        })
    }

    /// A direct send queued on `ws`'s lane went (or did not): keep its
    /// connection, and send what failed the way it would have gone without
    /// a direct route.
    fn direct_sent(&mut self, ws: WorkspaceId, outcome: room_lane::DirectOutcome) {
        let room_lane::DirectOutcome {
            target,
            msg,
            connection,
            delivered,
        } = outcome;
        match connection {
            Some(connection) if delivered && self.is_surface_member(ws, target) => {
                self.surface_connections.insert(target, connection);
            }
            _ => {
                self.surface_connections.remove(&target);
            }
        }
        if delivered || !self.subscribers.contains_key(&ws) {
            return;
        }
        match msg {
            NeighbourMsg::RemoteAction { frame } => self.relay_remote_action(ws, target, frame),
            NeighbourMsg::RecordContentPiece { .. } | NeighbourMsg::RecordContentLacks { .. } => {
                self.relay_record_content(ws, Some(target), &msg);
            }
            _ => {}
        }
    }

    /// [`NetCmd::JoinWorkspace`] arm of [`Self::handle_command`], split out
    /// to keep that dispatcher short.
    fn handle_join_workspace(&mut self, ws: WorkspaceId, on_event: mpsc::Sender<NetEvent>) {
        self.subscribers.insert(ws, on_event);
        if self.gossip_topics.contains(&ws) {
            self.subscribe_topic(ws);
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
                routes.schedule(ws, addr.clone(), expectation);
            }
        }
    }

    /// Subscribe to `ws`'s gossip topic, on the room's lane, unless a
    /// subscription is running or being made. A subscribe that failed, or
    /// one whose stream has closed, is retried from the heartbeat while the
    /// workspace is still wanted.
    fn subscribe_topic(&mut self, ws: WorkspaceId) {
        if self.topics.contains_key(&ws) {
            return;
        }
        self.next_topic_serial = self.next_topic_serial.wrapping_add(1);
        let serial = self.next_topic_serial;
        let bootstrap = self.peers.keys().copied().collect();
        if self
            .lanes
            .queue(ws, room_lane::Work::Subscribe { serial, bootstrap })
        {
            self.topics.insert(ws, TopicRuntime::new(serial));
        } else {
            tracing::warn!(
                ws,
                "room's remote work is backed up; its topic is joined later"
            );
        }
    }

    /// `ws`'s lane subscribed to its topic (`receiver`) or failed to: pump
    /// the subscription's events, unless it is not the one wanted now.
    fn topic_subscribed(
        &mut self,
        ws: WorkspaceId,
        serial: u64,
        receiver: Option<Box<iroh_gossip::api::GossipReceiver>>,
    ) {
        let Some(topic) = self
            .topics
            .get_mut(&ws)
            .filter(|topic| topic.serial == serial && topic.pump.is_none())
        else {
            return;
        };
        let Some(mut receiver) = receiver else {
            self.topics.remove(&ws);
            return;
        };
        let tx = self.internal_tx.clone();
        topic.pump = Some(tokio::spawn(async move {
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
        }));
    }

    /// Personal topics that are joined but have no running gossip
    /// subscription.
    fn resubscribe_missing_topics(&mut self) {
        let missing: Vec<_> = self
            .gossip_topics
            .iter()
            .copied()
            .filter(|ws| self.subscribers.contains_key(ws) && !self.topics.contains_key(ws))
            .collect();
        for ws in missing {
            self.subscribe_topic(ws);
        }
    }

    /// Known direct peers are joined when they are first learned. A device
    /// whose network came up after that attempt never appears, so ask again
    /// on each topic until the peer is actually seen there.
    fn retry_direct_peers(&mut self) {
        let local = self.endpoint.id();
        for ws in self.topics.keys() {
            let absent: Vec<_> = self
                .peers
                .keys()
                .copied()
                .filter(|id| *id != local && !self.peers_seen.contains_key(&(*ws, *id)))
                .collect();
            if absent.is_empty() {
                continue;
            }
            self.lanes
                .queue_or_shed(*ws, room_lane::Work::JoinPeers(absent));
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
            Some(InternalEvent::Subscribed {
                ws,
                serial,
                receiver,
            }) => self.topic_subscribed(ws, serial, receiver),
            Some(InternalEvent::DirectSent { ws, outcome }) => self.direct_sent(ws, *outcome),
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
                via,
                certificate,
                result,
            }) => self.native_host_connected(ws, serial, (*hint, *via), certificate, result),
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
                connection_incarnation,
                fingerprint,
                msg,
            }) => {
                self.tally.note(msg.kind());
                self.handle_surface_frame(ws, peer, connection_incarnation, fingerprint, msg)
                    .await;
            }
            Some(InternalEvent::SurfaceIngress {
                peer,
                connection_incarnation,
                connection,
            }) => {
                self.room_members
                    .connection(peer, connection_incarnation, connection);
            }
            Some(InternalEvent::SurfaceFanoutComplete {
                ws,
                generation,
                msg,
                outcomes,
                all_delivered,
            }) => {
                self.handle_surface_fanout_complete(ws, generation, &msg, outcomes, all_delivered);
            }
            Some(InternalEvent::SurfaceRouteOpened { peer, connection }) => {
                self.surface_route_opened(peer, connection);
            }
            Some(InternalEvent::HeldSurfaceUnitWritten {
                key,
                generation,
                delivered,
            }) => {
                self.held_surface_unit_written(key, generation, delivered);
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
            Some(InternalEvent::IdentityMaterialChanged) => {
                self.identity_material_changed();
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
        connection_incarnation: u64,
        fingerprint: [u8; 32],
        msg: NeighbourMsg,
    ) {
        // A bare ALPN connection is not workspace admission. Require the
        // peer to be a member: of the room's admitted roster, or of the
        // person's own topic. ShellNetwork performs the capability and
        // claimed-publisher checks before dispatch.
        let membership = if self.room_members.has_roster(ws) {
            match self.room_members.admit(
                ws,
                peer,
                connection_incarnation,
                tilo_clock::unix_now_ns(),
            ) {
                Admission::Member(epoch) => Some(epoch),
                Admission::Refused => None,
                Admission::Expired(removed) => {
                    tracing::info!(ws, peer = %peer, "surface frame from a device whose certificate expired");
                    self.close_removed(removed, b"certificate expired");
                    return;
                }
            }
        } else {
            self.touch_existing_membership(ws, peer)
        };
        let Some(membership_epoch) = membership else {
            tracing::warn!(ws, peer = %peer, "surface frame from a device that is no member");
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
    fn handle_surface_fanout_complete(
        &mut self,
        ws: WorkspaceId,
        generation: u64,
        msg: &NeighbourMsg,
        outcomes: Vec<SurfaceFanoutOutcome>,
        all_delivered: bool,
    ) {
        let key = (ws, fanout_surface(msg));
        if !finish_surface_fanout(&mut self.surface_fanout_tasks, key, generation) {
            return;
        }
        if !self.subscribers.contains_key(&ws) {
            self.pending_surface_frames.remove(&key);
            return;
        }
        // A unit that failed on an open route broke the reference chain the
        // window's queued units continue. One held for a viewer whose route
        // is opening did not: it reaches that viewer once the route is up.
        let mut lost = false;
        for outcome in outcomes {
            if !outcome.delivered || !self.is_surface_member(ws, outcome.peer) {
                lost |= !outcome.delivered;
                self.surface_connections.remove(&outcome.peer);
            }
        }
        // A window streamed to named viewers only never falls back to the
        // workspace topic: a frame that missed them is superseded by the next.
        let targeted = self.surface_fanout_viewers.contains_key(&key);
        if lost {
            self.pending_surface_frames
                .entry(key)
                .or_default()
                .lose_references();
        }
        if !all_delivered && !targeted {
            publish(
                &self.topics,
                &self.host_sessions,
                &mut self.lanes,
                ws,
                msg,
                false,
                true,
                false,
            );
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
                &mut self.lanes,
                ws,
                &next,
                false,
                true,
                false,
            );
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
        if matches!(event, GossipEvent::NeighborUp(_)) {
            // Authenticate this device, and say what it can decode, as soon
            // as the link is usable: once per link, never on a timer. State
            // sync can otherwise arrive before the identity and be rejected
            // by the peer.
            self.announce_identity(ws);
            self.announce_media_caps(ws);
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
                    self.announce_identity(ws);
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
                        self.flush_pending_remote_actions(ws, peer);
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
            self.flush_pending_remote_actions(ws, peer);
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
                    true,
                );
                self.host_sessions.insert(
                    (ws, peer),
                    HostRuntime {
                        replaying,
                        native_scope: None,
                        local: false,
                        governor: None,
                        actions: self.serve_actions(ws, peer, &session),
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

    /// Serve the attached action broker's private channel for `ws` on the
    /// session with the workspace host `peer`, when the room is one of the
    /// broker's and the host is an enrolled home peer. A state-only or
    /// pinned host, a room's own native session and a certificate that can
    /// act as host never carry it.
    fn serve_actions(
        &self,
        ws: WorkspaceId,
        peer: EndpointId,
        session: &Arc<RelayCoeditSession>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let (queue, host, rooms) = self.action_broker.as_ref()?;
        if self.native_routes.contains(ws)
            || self
                .hosts
                .get(&peer)
                .and_then(|(_, expected)| expected.as_ref())
                .is_none()
            || self
                .host_identity
                .as_ref()
                .is_none_or(|identity| identity.material_for(ws).0.capabilities.can_act_as_host)
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
            HostEvent::NativeInput(input) => match input.message() {
                NeighbourMsg::AppDocument { envelope, .. } if envelope.encryption.is_some() => {
                    if input.retained() {
                        "HostSealedReplay"
                    } else {
                        "HostSealedLive"
                    }
                }
                _ => "HostNativeInput",
            },
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
            HostEvent::Closed => Some(self.host_session_closed(ws, peer)),
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

    /// The session of `ws` with the host at `peer` ended.
    fn host_session_closed(&mut self, ws: WorkspaceId, peer: EndpointId) -> NetEvent {
        tracing::info!(ws, host = %peer, "workspace host session ended; the next heartbeat dials it again");
        if let Some(runtime) = self.host_sessions.remove(&(ws, peer)) {
            runtime.pump.abort();
            if let Some(scope) = runtime.native_scope.clone() {
                self.native_session_closed(ws, &scope);
            }
        }
        NetEvent::PeerLeft {
            ws,
            peer,
            cause: PeerDepartureCause::HostSessionClosed,
            membership_epoch: None,
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
    ///
    /// A pointer move is held and judged due at `at`, when it was published,
    /// so its gap runs from when the move happened however late the actor
    /// reaches it; a move that fell due meanwhile goes at once.
    fn publish_paced(&mut self, ws: WorkspaceId, msg: &NeighbourMsg, at: tokio::time::Instant) {
        let pacing = slot_pacing(ws, msg.presence_slot());
        if let Pacing::Pointer(key) = pacing
            && self.topics.contains_key(&ws)
        {
            let bytes = match postcard::to_allocvec(msg) {
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
                &mut self.lanes,
                ws,
                msg,
                false,
                false,
                true,
            );
            self.gossip_pointers
                .hold(key, GossipPointer { ws, bytes }, at);
            let due = self.gossip_pointers.take_due(at);
            self.broadcast_pointers(due);
            return;
        }
        if pacing == Pacing::Ordered {
            let held = self.gossip_pointers.take_all(tokio::time::Instant::now());
            self.broadcast_pointers(held);
        }
        publish(
            &self.topics,
            &self.host_sessions,
            &mut self.lanes,
            ws,
            msg,
            false,
            true,
            true,
        );
    }

    /// Broadcast the held pointer moves whose gap has ended (the run loop
    /// wakes for [`PointerPacer::next_due`], armed only while one is held).
    fn broadcast_due_pointers(&mut self) {
        let due = self.gossip_pointers.take_due(tokio::time::Instant::now());
        self.broadcast_pointers(due);
    }

    fn broadcast_pointers(&mut self, pointers: Vec<GossipPointer>) {
        for GossipPointer { ws, bytes } in pointers {
            // A room left since the move was held has nobody to tell.
            if self.topics.contains_key(&ws) {
                self.lanes
                    .queue_or_shed(ws, room_lane::Work::Broadcast(bytes));
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

    /// Whether [`Self::run_heartbeat_tick`] has anything to do: a direct
    /// peer not yet seen on one of the topics, a workspace host or a
    /// governed room's relay to keep connected, a remote action held for a
    /// peer, or a topic whose subscription failed. A converged pair of
    /// desktops has none of these, so its runtime sleeps until one appears
    /// ([`run`]).
    fn needs_upkeep(&self) -> bool {
        let local = self.endpoint.id();
        self.peers.keys().any(|peer| {
            *peer != local
                && self
                    .topics
                    .keys()
                    .any(|ws| !self.peers_seen.contains_key(&(*ws, *peer)))
        }) || !self.hosts.is_empty()
            || self.remote_host_sessions()
            || self.native_routes.any_remote()
            || !self.pending_remote_actions.is_empty()
            || self
                .gossip_topics
                .iter()
                .any(|ws| self.subscribers.contains_key(ws) && !self.topics.contains_key(ws))
    }

    /// Where workspace `ws` stands with its hosts, this device's own
    /// included: a session up, one being established, or neither.
    fn sessions_of(&self, ws: u64) -> room_governance::Sessions {
        // A session with a host standing in for a room's governor carries
        // none of its governance: only the governor's own counts.
        if self
            .host_sessions
            .iter()
            .any(|((session_ws, _), runtime)| *session_ws == ws && reaches_governor(runtime))
        {
            room_governance::Sessions::Open
        } else if self.native_routes.establishing(ws)
            || self
                .connecting_hosts
                .iter()
                .any(|(connecting_ws, _)| *connecting_ws == ws)
        {
            room_governance::Sessions::Connecting
        } else {
            room_governance::Sessions::Closed
        }
    }

    /// Settle what waits on where workspaces stand with their hosts
    /// ([`room_governance::Waiting::sessions_changed`]), and send what
    /// waited for a session now up. Every change to a workspace's sessions
    /// is made handling one event of [`run`]'s loop, which settles before
    /// it waits again, so a session ending with nothing else going on is
    /// settled at once. Costs nothing while nothing waits.
    fn settle_session_waits(&mut self) {
        if !self.room_governance.waits_on_sessions() {
            return;
        }
        let sessions: HashMap<u64, room_governance::Sessions> = self
            .room_governance
            .workspaces()
            .into_iter()
            .map(|ws| (ws, self.sessions_of(ws)))
            .collect();
        let ready = self.room_governance.sessions_changed(|ws| {
            sessions
                .get(&ws)
                .copied()
                .unwrap_or(room_governance::Sessions::Closed)
        });
        for request in ready {
            self.send_room_governance(request);
        }
    }

    /// Whether a host session goes to another machine: sessions with this
    /// device's own host keep nothing up.
    fn remote_host_sessions(&self) -> bool {
        self.host_sessions.values().any(|runtime| !runtime.local)
    }

    /// Whether `command` stays in this process: it goes to this device's
    /// own host, so it needs no relay up.
    fn stays_local(&self, command: &NetCmd) -> bool {
        let own = self.endpoint.id();
        match command {
            NetCmd::NativeConnect(connect, None) => {
                self.host_identity.as_ref().is_some_and(|identity| {
                    connect.material(identity).is_ok_and(|(hint, cert, _, _)| {
                        tilo_joined_room::session::hosted_by(&hint, &cert.device_public_key)
                    })
                })
            }
            NetCmd::RoomGovernance(request) => matches!(
                request.to,
                room_governance::Addressee::Device(host) | room_governance::Addressee::Host(host)
                    if host == own
            ),
            NetCmd::GovernorAction { ws, .. } => self.native_routes.is_local(*ws),
            _ => false,
        }
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
            || self.remote_host_sessions()
        {
            "a workspace host"
        } else if self.native_routes.any_remote() {
            "a room hosted elsewhere"
        } else if !self.active_personal_transfers.is_empty() {
            "one of the person's devices"
        } else if !self.surface_connections.is_empty()
            || !self.surface_fanout_viewers.is_empty()
            || !self.surface_routes_opening.is_empty()
            || !self.held_surface_units.is_empty()
        {
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
    /// connected. Runs every [`HEARTBEAT_INTERVAL`] while
    /// [`Self::needs_upkeep`].
    fn run_heartbeat_tick(&mut self) {
        for (ws, peer) in self.pending_remote_actions.targets() {
            if self.is_surface_member(ws, peer) {
                self.flush_pending_remote_actions(ws, peer);
            }
        }
        self.resubscribe_missing_topics();
        self.retry_direct_peers();
        self.retry_native_routes();
        // A session with this device's own host loses nothing between
        // heartbeats: it publishes as the owner commits.
        let rooms: Vec<_> = self
            .host_sessions
            .iter()
            .filter(|(_, runtime)| !runtime.local)
            .map(|((ws, _), _)| *ws)
            .collect();
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
                    routes.schedule(ws, addr, expectation);
                }
            }
        }
    }

    /// Neighbours heard this device's identity when their link came up; from
    /// here on they hear it again only when it changes (a renewed
    /// certificate, a new revocation), which the files it is read from say.
    fn identity_material_changed(&mut self) {
        let workspaces: Vec<_> = self.topics.keys().copied().collect();
        for ws in workspaces {
            self.announce_identity_if_changed(ws);
        }
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
    fn announce_identity(&mut self, ws: WorkspaceId) {
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
        self.lanes
            .queue_or_shed(ws, room_lane::Work::Neighbours(Bytes::from(bytes)));
    }

    /// Announce this device's identity on `ws` again, to its neighbours and
    /// its workspace hosts, when it changed since it was last announced
    /// there: a new revocation reaches the room as soon as it is on disk.
    fn announce_identity_if_changed(&mut self, ws: WorkspaceId) {
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
        self.lanes
            .queue_or_shed(ws, room_lane::Work::Neighbours(Bytes::from(bytes)));
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
    fn announce_media_caps(&mut self, ws: WorkspaceId) {
        let Some(bytes) = self.media_caps_advert() else {
            return;
        };
        self.caps_sent = Some(message_fingerprint(&bytes));
        if self.topics.contains_key(&ws) {
            self.lanes
                .queue_or_shed(ws, room_lane::Work::Neighbours(Bytes::from(bytes)));
        }
    }

    /// Announce this machine's codecs again on every topic when they changed
    /// since they were last announced.
    fn announce_media_caps_if_changed(&mut self) {
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
        for ws in self.topics.keys().copied() {
            self.lanes
                .queue_or_shed(ws, room_lane::Work::Neighbours(Bytes::from(bytes.clone())));
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
        for (&(ws, peer), _) in self
            .host_sessions
            .iter()
            .filter(|(_, runtime)| !runtime.local)
        {
            if let Some(tx) = self.subscribers.get(&ws) {
                let _ = tx.send(NetEvent::HostReady { ws, peer }).await;
            }
        }
    }
}

/// Whether `runtime` may carry its room's governance: any session but one
/// with a host standing in for the room's governor.
fn reaches_governor(runtime: &HostRuntime) -> bool {
    runtime
        .native_scope
        .as_ref()
        .is_none_or(|scope| scope.reaches_governor())
}

/// Send one personal fabric frame to `peer` on a bidirectional stream of
/// the gossip connection it shares ([`read_personal_fabric`]).
async fn send_personal_fabric_frame(
    ws: WorkspaceId,
    peer: EndpointId,
    connection: &iroh::endpoint::Connection,
    bytes: &[u8],
) {
    let send = async {
        let (mut stream, _) = connection.open_bi().await.map_err(|_| ())?;
        stream.write_all(bytes).await.map_err(|_| ())?;
        stream.finish().map_err(|_| ())?;
        Ok::<(), ()>(())
    };
    if !matches!(
        tokio::time::timeout(Duration::from_millis(100), send).await,
        Ok(Ok(()))
    ) {
        tracing::debug!(ws, peer = %peer, "personal fabric frame not sent");
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
        let (mut stream, _) = connection.open_bi().await.map_err(|_| ())?;
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
        connection,
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
    hosting: Option<Hosting>,
) {
    let (internal_tx, mut internal_rx) = mpsc::channel(256);
    let (input_tx, mut input_rx) = mpsc::channel(64);
    // Personal fabric frames share gossip's connections rather than open
    // their own: two linked devices keep one connection, with one set of
    // keep-alives and timers.
    let (gossip_connections, shared_connections) = mpsc::unbounded_channel();
    let gossip = Gossip::builder()
        .max_message_size(MAX_GOSSIP_MESSAGE_BYTES)
        .broadcast_config(broadcast_config())
        .connections(gossip_connections)
        .spawn(endpoint.clone());
    read_personal_fabric_on_gossip_connections(shared_connections, internal_tx.clone());
    let private_messages = super::private_messages::Registry::new(endpoint.id());
    let router = Router::builder(endpoint.clone())
        .accept(GOSSIP_ALPN, gossip.clone())
        .accept(super::private_messages::ALPN, private_messages.clone())
        .accept(
            SURFACE_STREAM_ALPN,
            SurfaceStreamProtocol {
                internal_tx: internal_tx.clone(),
                input_tx,
                next_connection_incarnation: Arc::new(AtomicU64::new(1)),
            },
        )
        .accept(tilo_unlock_protocol::UNLOCK_ALPN, DeviceUnlockProtocol);
    // This device's room host answers here once it opens: until then its
    // handlers refuse, as a device hosting nothing does.
    let (local_host, protocols) = match hosting {
        Some(Hosting { host, protocols }) => (Some(host), protocols),
        None => (None, Vec::new()),
    };
    #[cfg(target_os = "linux")]
    let router = router.accept(device_vpn::tunnel::ALPN, DeviceVpnProtocol);
    let router = protocols
        .into_iter()
        .fold(router, |router, (alpn, handler)| {
            router.accept(alpn, handler)
        })
        .spawn();
    let reporting_addrs = report_addrs(&endpoint, listen_addrs);
    let lanes = room_lane::Lanes::new(gossip.clone(), endpoint.clone(), internal_tx.clone());
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
        local_host,
        room_governance: room_governance::Waiting::default(),
        lanes,
        gossip,
        next_topic_serial: 0,
        internal_tx,
        gossip_topics: HashSet::new(),
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
        room_members: room_members::RoomMembers::default(),
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
        surface_routes_opening: HashMap::new(),
        held_surface_units: HashMap::new(),
        active_personal_transfers: HashMap::new(),
        next_personal_transfer_generation: 0,
        tally: Arc::default(),
        caps_sent: None,
        pending_remote_actions: PendingRemoteActions::default(),
        gossip_pointers: PointerPacer::default(),
    };
    let _identity_watch = state
        .host_identity
        .as_ref()
        .and_then(|identity| watch_identity_material(identity, state.internal_tx.clone()));
    let mut media_caps = crate::network::media_caps::changes();
    let mut upkeep = None;
    let mut host_checkpoint = None;
    let mut reported_party = None;

    loop {
        // What waited on a room's sessions follows them, however they
        // changed.
        state.settle_session_waits();
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
        // Upkeep every `heartbeat_interval` while something waits for a
        // retry; a converged desktop never wakes for it.
        upkeep = next_upkeep(upkeep, state.needs_upkeep(), heartbeat_interval);
        host_checkpoint = next_upkeep(
            host_checkpoint,
            state.remote_host_sessions(),
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
            () = sleep_until_due(pointer_due) => state.broadcast_due_pointers(),
            () = sleep_until_lapse(eviction) => state.run_eviction_tick().await,
            Ok(()) = media_caps.changed() => state.announce_media_caps_if_changed(),
            () = sleep_until_lapse(upkeep) => {
                upkeep = None;
                state.run_heartbeat_tick();
            }
            () = sleep_until_lapse(host_checkpoint) => {
                host_checkpoint = None;
                state.run_host_checkpoint_tick().await;
            }
        }
    }

    for pump in state
        .topics
        .values()
        .filter_map(|topic| topic.pump.as_ref())
    {
        pump.abort();
    }
    for host in state.host_sessions.values() {
        host.pump.abort();
    }
    for (_, task) in state.surface_fanout_tasks.values() {
        task.abort();
    }
    drop(state);
    reporting_addrs.abort();
    let _ = router.shutdown().await;
}

/// Tell the actor whenever a file `identity`'s material is read from
/// changes. `None` for an identity kept in memory, or one that cannot be
/// watched: it is then announced as it was when each link came up.
fn watch_identity_material(
    identity: &HostIdentity,
    internal_tx: mpsc::Sender<InternalEvent>,
) -> Option<tilo_files::native::Watched<()>> {
    let directories = identity.material_directories();
    if directories.is_empty() {
        return None;
    }
    let watched = tilo_files::native::Watched::start("identity-material", directories, || ())
        .inspect_err(|error| tracing::warn!(%error, "cannot watch this device's identity"))
        .ok()?;
    watched.on_change(move || {
        let _ = internal_tx.blocking_send(InternalEvent::IdentityMaterialChanged);
    });
    Some(watched)
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
/// once the caches are empty (`gossip_caches_drain_within`); a topic with
/// no neighbour caches nothing.
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
        NeighbourMsg::SurfaceFrame { surface, .. }
        | NeighbourMsg::SurfaceStreamStart { surface, .. }
        | NeighbourMsg::SurfaceStreamEnd { surface, .. } => *surface,
        _ => 0,
    }
}

fn publish(
    topics: &HashMap<WorkspaceId, TopicRuntime>,
    host_sessions: &HashMap<(WorkspaceId, EndpointId), HostRuntime>,
    lanes: &mut room_lane::Lanes,
    ws: WorkspaceId,
    msg: &NeighbourMsg,
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
    let retention = retention(msg, shell_checkpoint);
    // Live traffic (pointers, carets, presence) is for whoever hears it now:
    // in a room with no session up there is no one, and nothing keeps it.
    if retention.is_none()
        && !topics.contains_key(&ws)
        && host_sessions
            .keys()
            .all(|(session_ws, _)| *session_ws != ws)
    {
        return;
    }
    let bytes = match postcard::to_allocvec(msg) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(ws, %error, "collaboration message encode failed");
            return;
        }
    };

    // Host clients enqueue synchronously into their already-authenticated
    // QUIC pumps, and gossip goes on the room's lane: neither waits for the
    // other, nor for any other room. This also keeps collaboration live
    // through a transient failure to join the gossip topic, provided an
    // authenticated workspace host is ready.
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
        if topics.contains_key(&ws) {
            lanes.queue_or_shed(ws, room_lane::Work::Broadcast(Bytes::from(bytes)));
        } else if host_sessions
            .keys()
            .all(|(session_ws, _)| *session_ws != ws)
        {
            tracing::warn!(
                ws,
                kind = msg.kind(),
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
        // A member's device signs the private actions it sends, whether or
        // not the action broker is attached yet: the broker takes the
        // session as it is when it attaches.
        let action_v2 = !device_cert.capabilities.can_act_as_host;
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
    keepalive: bool,
) -> tokio::task::JoinHandle<()> {
    use std::sync::atomic::Ordering;
    tokio::spawn(async move {
        // Whether this silence was already answered with a ping.
        let mut pinged = false;
        loop {
            let inbound = tokio::select! {
                inbound = session.recv() => inbound,
                // A session with this device's own host runs in process: no
                // link can die under it, and its end ends `recv`. Only a
                // remote host is kept up, so a device hosting its rooms with
                // nobody connected wakes for nothing.
                alive = host_keepalive(&session, &mut pinged), if keepalive => {
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
mod tests;
