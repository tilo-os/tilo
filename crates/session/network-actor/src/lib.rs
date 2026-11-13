//! The iroh actor for workspace collaboration.
//!
//! Direct paths, NAT traversal and stateless relay fallback are selected by
//! iroh. Tilo only names authenticated endpoint ids and workspaces. Every
//! workspace travels on one protocol, the session a workspace host serves: a
//! device reaches the devices that hold a workspace through the hosts that
//! store it, each device serving its own ([`local_host`]). Two of the
//! person's own devices keep one session of their own between them
//! ([`device_sessions`]), which carries what is for the person's devices
//! alone: their device fabric, nearby transfers, codec adverts and their
//! workspace list. Only what is for one exact device of a workspace (input,
//! window streams, remote actions) goes directly to it, on the
//! authenticated device plane.
//! The actor is the desktop's transport, below the compositor: what arrives
//! on it that is the desktop's business goes to the services the session
//! installs ([`services`]), so a compositor change does not rebuild it and
//! its tests run on their own.

pub mod behaviour;
mod device_sessions;
pub mod fabric;
mod fabric_link;
pub mod local_host;
pub mod media_caps;
pub mod native_ingress;
pub mod services;
#[cfg(any(test, feature = "test-support"))]
pub mod siblings;
pub mod workspace_sync;

/// The workspace a message has none for.
pub const DEFAULT_WS_TOPIC: tilo_desktop_model::WorkspaceId =
    tilo_desktop_model::WorkspaceId::nil();

/// A workspace host connection's error, as the actor reports it. A host at
/// another peer protocol version is also the person's to know about: one of
/// the two devices has to update ([`services::Services::peer_version`]).
fn relay_error(host: &iroh::EndpointId, error: &tilo_relay_session::ClientError) -> String {
    if let Some(theirs) = error.peer_version() {
        services::peer_version(host.to_string(), theirs);
    }
    error.to_string()
}

mod workspace_lane;
#[cfg(test)]
mod workspace_lane_tests;
mod workspace_members;
use workspace_members::Admission;
pub use workspace_members::RosterDevice;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::address_lookup::memory::MemoryLookup;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_relay_session::{Inbound as HostInbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::mpsc;

use neighbour_wire::NeighbourMsg;
use neighbour_wire::{Retention, retention};
use tilo_endpoint_reachability::{Need, Reachability};
use tilo_lock::MutexRecover as _;

/// How often the actor retries what is missing (a session with one of the
/// person's devices, a workspace host). Armed only while something is
/// ([`ActorState::needs_upkeep`]): a converged desktop never wakes for it.
/// A change of this device's identity material or codecs is announced when
/// it happens, not found by a tick.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// Peer liveness, kept by the transport rather than by messages: iroh
/// sends a QUIC keep-alive after 5 s without traffic, and a connection that
/// hears nothing for this long ends ([`transport_config`]).
const PEER_TIMEOUT: Duration = Duration::from_secs(15);
/// How often each workspace host session is asked for a fresh checkpoint
/// ([`ActorState::run_host_checkpoint_tick`]); armed only while there is a
/// host session to ask.
const HOST_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(600);
/// Largest message the direct control route reads: a window's control
/// messages and a record's content piece are far smaller.
const MAX_SURFACE_FRAME_BYTES: usize = 64 * 1024 * 1024;
/// How long a device whose direct route could not be opened is reached
/// through the workspace's host before a dial is tried again.
const DIRECT_ROUTE_RETRY: Duration = Duration::from_secs(30);
const SURFACE_STREAM_ALPN: &[u8] = b"tilo-surface-stream/1";
/// Largest byte chunk one personal-fabric transfer frame carries.
pub const PERSONAL_TRANSFER_CHUNK_BYTES: usize = 256 * 1024;
const MAX_ACTIVE_PERSONAL_TRANSFERS: usize = 8;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SurfaceWireFrame {
    workspace: WorkspaceId,
    message: NeighbourMsg,
}

/// Versioned private-device traffic. It travels directly between two of
/// the person's devices, each frame on a bidirectional stream of the
/// connection their session runs on ([`device_sessions`],
/// `read_personal_fabric`), and leaves the legacy Postcard enum untouched.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PersonalFabricPayload {
    Request,
    Snapshot(Vec<u8>),
    /// Ephemeral, exact-target command encoded independently of snapshot
    /// schema so it is never persisted or relayed through workspace history.
    Action(Vec<u8>),
    /// The one lane for file bytes between a person's devices: Nearby sends
    /// and media handoffs alike. Offers and resume checkpoints travel as
    /// authenticated actions; this lane carries bounded bytes only.
    NearbyTransfer(PersonalNearbyTransferFrame),
    /// One signed mutation of a record in the person's own store. Its scope is
    /// implied by the route: the fabric only ever joins one person's devices.
    /// `record_id` is the sealed wire name, as on a workspace topic.
    Document {
        app_id: String,
        record_id: String,
        envelope: Box<collab_types::SignedOp>,
    },
    /// A device of the person asking for content one of `app_id`'s person
    /// records references. Its scope is the person's, implied by the route.
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
        stamp: crate::fabric::FabricStamp,
        outputs: Vec<crate::fabric::DisplayConnectorPlacement>,
    },
    /// Rich output observations follow the same certified, opt-in route.
    DisplayOutputs {
        stamp: crate::fabric::FabricStamp,
        outputs: Vec<crate::fabric::DisplayOutputObservation>,
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
    user_public_key: [u8; 32],
    payload: PersonalFabricPayload,
}

pub use tilo_desktop_model::WorkspaceId;
pub type PeerId = EndpointId;
pub use tilo_identity_host::custody::HostIdentity;

/// Enrollment-bound identity expected behind a distinct relay service key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomePeerExpectation {
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
}

#[derive(Debug)]
pub enum NetCmd {
    /// Relay between the devices that take part in this device's workspaces
    /// through it, as their workspace's host, and the rest of each workspace:
    /// what they write reaches this device's apps and the workspace's own
    /// host, and what reaches this device of the workspace reaches them.
    ServeMembers(tilo_peer_state::direct::Owning),
    /// Ask the host of the joined workspace on `ws` to replay what it retains:
    /// something an app message let go earlier waited for has arrived, and
    /// the message is decided again when it comes back.
    NativeReplay {
        ws: WorkspaceId,
    },
    /// Ask the hosts of `ws` for the record on `channel` (a retained route's
    /// channel, `neighbour_wire::app_document_channel`): what they hold of it,
    /// and their changes to it from then on. A workspace that sends records
    /// when they are opened sends nothing of a record before this.
    OpenRecord {
        ws: WorkspaceId,
        channel: [u8; 32],
    },
    /// Hold an enrolled workspace's relay session over the iroh endpoint its
    /// hint names.
    NativeConnect(native_ingress::Connect),
    /// This device's own workspace host opened: join the workspaces it hosts that
    /// wait for it.
    LocalHostReady,
    NativePublish {
        ws: WorkspaceId,
    },
    /// What this device has received from its peers, by kind.
    WireTally(tokio::sync::oneshot::Sender<Arc<WireTally>>),
    /// Take part in workspace `ws`: its events reach `on_event`, and its
    /// workspace hosts are dialled. A workspace's traffic travels over its host
    /// sessions alone.
    JoinWorkspace {
        ws: WorkspaceId,
        on_event: mpsc::Sender<NetEvent>,
    },
    /// Hear the person's other devices at `on_event`: each whose session is
    /// up ([`NetEvent::Sibling`]), what it sends there, and when it ends.
    WatchSiblings {
        on_event: mpsc::Sender<NetEvent>,
    },
    /// Send `msg` to each of the person's devices whose session is up.
    PublishToSiblings {
        msg: NeighbourMsg,
    },
    LeaveWorkspace {
        ws: WorkspaceId,
    },
    /// Workspace `ws`'s admitted roster changed: `devices` are every device its
    /// governor admitted, by endpoint id, with its certificate's expiry.
    /// They alone send and receive the workspace's window streams; a device that
    /// left, or whose certificate expired, has its connections closed.
    SetWorkspaceRoster {
        ws: WorkspaceId,
        devices: Vec<RosterDevice>,
    },
    /// Publish `msg` on every session of `ws`.
    Publish {
        ws: WorkspaceId,
        msg: NeighbourMsg,
    },
    /// Say one shared window's control message (a viewer's ask, input or
    /// keyframe request, a page's announcement) to `target` alone: over the
    /// authenticated direct route while one is up, and otherwise through the
    /// workspace's host to that device's connection only. It is never fanned
    /// out to the workspace's other members.
    PublishWorkspaceControl {
        ws: WorkspaceId,
        msg: NeighbourMsg,
        target: EndpointId,
    },
    /// Send one piece of a record's content to the device that asked for it.
    PublishRecordContent(RecordContentDelivery),
    /// Send same-person fabric state to each of the person's devices that
    /// proved a certificate of `user_public_key` on its session.
    PublishPersonalFabric {
        user_public_key: [u8; 32],
        payload: PersonalFabricPayload,
    },
    /// Send an ephemeral personal-fabric action to one authenticated device.
    PublishPersonalFabricTarget {
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
        user_public_key: [u8; 32],
        target: EndpointId,
        transfer: NearbyTransferCheckpoint,
        file: std::fs::File,
    },
    /// One of the person's own devices, where it is reached: the session
    /// between the two is kept up ([`device_sessions`]).
    Connect {
        addr: EndpointAddr,
    },
    /// Connect to an always-on Tilo host, which adds persistence and offline
    /// delivery while the person's devices are away.
    ConnectHost {
        addr: EndpointAddr,
        expectation: Option<HomePeerExpectation>,
    },
}

/// A piece of a record's content on its way to the device of the workspace that
/// asked: over the direct plane when that device is a live member of the
/// workspace, else through the workspace's hosts, which hand it to that
/// device alone and keep nothing.
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

#[derive(Debug, Clone)]
pub enum NetEvent {
    NativeInput(Box<native_ingress::Input>),
    /// One of the person's devices proved itself on the session between the
    /// two ([`device_sessions`]), or proved itself anew there (a renewed
    /// certificate, a new revocation): `certs` are its certificates of the
    /// roots this device acts for too. What is for that device alone is
    /// bound to `membership_epoch` while the session lasts.
    Sibling {
        peer: PeerId,
        membership_epoch: MembershipEpoch,
        certs: Vec<DeviceCert>,
        revocations: Vec<tilo_user_identity::RevocationEntry>,
    },
    /// The host of workspace `ws` said its owner deleted it: it is gone for
    /// good, and nothing dials it again.
    WorkspaceDeleted {
        ws: WorkspaceId,
    },
    /// The session of membership `membership_epoch` with one of the
    /// person's devices ended.
    SiblingLeft {
        peer: PeerId,
        membership_epoch: MembershipEpoch,
    },
    /// One of the person's devices sent `msg` on its session.
    SiblingMsg {
        peer: PeerId,
        membership_epoch: MembershipEpoch,
        msg: NeighbourMsg,
    },
    PeerLeft {
        ws: WorkspaceId,
        peer: PeerId,
        cause: PeerDepartureCause,
        membership_epoch: Option<MembershipEpoch>,
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
    /// `records_when_opened`: this host sends a device only the workspace's
    /// governance, its shared desktop and the heads of its chained records,
    /// and every other record once the device opens it
    /// ([`NetCmd::OpenRecord`]), so a record that is not here is not new
    /// until the host answered it holds none of it
    /// ([`Self::HostRecordOpened`]).
    HostReady {
        ws: WorkspaceId,
        peer: PeerId,
        records_when_opened: bool,
    },
    /// A host sent everything it retains of a record this device opened, as
    /// that record's frames before this event: `frames` of them, none when
    /// the workspace has none of the record.
    HostRecordOpened {
        ws: WorkspaceId,
        channel: [u8; 32],
        frames: u32,
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
        peer: PeerId,
        membership_epoch: MembershipEpoch,
        user_public_key: [u8; 32],
        payload: PersonalFabricPayload,
        /// When the frame reached this device, read where it was taken off
        /// the wire: a clock exchange is only worth what its timestamps are.
        received: Instant,
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
            Self::NativeInput(_) => "NativeInput",
            Self::Sibling { .. } => "Sibling",
            Self::SiblingLeft { .. } => "SiblingLeft",
            Self::WorkspaceDeleted { .. } => "WorkspaceDeleted",
            Self::PeerLeft { .. } => "PeerLeft",
            Self::HostMember { .. } => "HostMember",
            Self::HostReady { .. } => "HostReady",
            Self::HostRecordOpened { .. } => "HostRecordOpened",
            Self::Msg { msg, .. } | Self::OwnReplay { msg, .. } | Self::SiblingMsg { msg, .. } => {
                msg.kind()
            }
            Self::PersonalFabric { .. } => "PersonalFabric",
            Self::NearbyTransferProgress { .. } => "NearbyTransferProgress",
        }
    }
}

/// Actor-minted identity of one endpoint's membership incarnation: of a
/// workspace's roster, or of a session with one of the person's devices. This
/// value never crosses the wire: it is authority only for local consumers
/// of [`NetEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MembershipEpoch(u64);

impl MembershipEpoch {
    #[cfg(any(test, feature = "test-support"))]
    pub const fn for_test(value: u64) -> Self {
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
    /// The session that reached the peer closed.
    HostSessionClosed,
}

enum InternalEvent {
    /// A workspace's hold reached a host: the link runs until the actor lets
    /// go of `held`.
    NativeLinkConnected {
        ws: WorkspaceId,
        linked: Box<tilo_peer_state::owner::session::Linked>,
        scope: Arc<native_ingress::Scope>,
        held: tokio::sync::oneshot::Sender<()>,
    },
    /// A workspace's link brought a frame into this device's owner.
    NativeHeard {
        ws: WorkspaceId,
        heard: tilo_peer_state::direct::Heard,
    },
    /// A device that takes part in workspace `ws` through this one brought a
    /// frame into this device's owner over its link.
    MemberHeard {
        ws: WorkspaceId,
        workspace: tilo_desktop_model::WorkspaceId,
        heard: tilo_peer_state::direct::Heard,
    },
    /// A workspace's link has the host's retained history in hand.
    NativeReady { ws: WorkspaceId },
    /// A workspace's link ended; `deleted` when its host said the
    /// workspace's owner deleted it.
    NativeLinkClosed { ws: WorkspaceId, deleted: bool },
    /// A workspace's hold failed to dial `via`, its `governor` or another host.
    NativeDialFailed {
        ws: WorkspaceId,
        via: Box<tilo_peer_state::join::TransportHint>,
        governor: bool,
        reason: String,
    },
    Host {
        ws: WorkspaceId,
        session: std::sync::Weak<RelayCoeditSession>,
        peer: EndpointId,
        event: HostEvent,
    },
    /// A device dialled this one for the session between two of the
    /// person's devices ([`device_sessions`]).
    SiblingConnected(iroh::endpoint::Connection),
    /// Both sides of such a session said who they are.
    SiblingOpened(Box<device_sessions::Opened>),
    /// Dialling one of the person's devices failed.
    SiblingFailed(EndpointId),
    /// One of the person's devices sent something on its session.
    SiblingHeard {
        peer: EndpointId,
        epoch: MembershipEpoch,
        heard: device_sessions::Heard,
    },
    /// The session of membership `epoch` with one of the person's devices
    /// ended.
    SiblingClosed {
        peer: EndpointId,
        epoch: MembershipEpoch,
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
    /// The direct route to `peer` came up, or could not be opened (`None`).
    SurfaceRouteOpened {
        peer: EndpointId,
        connection: Option<iroh::endpoint::Connection>,
    },
    PersonalFabric {
        peer: EndpointId,
        epoch: MembershipEpoch,
        user_public_key: [u8; 32],
        payload: PersonalFabricPayload,
        /// When the frame reached this device, read where it was taken off
        /// the wire: a clock exchange is only worth what its timestamps are.
        received: Instant,
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
    /// A direct send queued on a workspace's lane went, or did not.
    DirectSent {
        ws: WorkspaceId,
        outcome: Box<workspace_lane::DirectOutcome>,
    },
}

enum HostEvent {
    Ready,
    /// The host sent everything it retains of a record this device opened
    /// ([`RelayCoeditSession::open_record`]), as that record's frames before
    /// this event: `frames` of them.
    RecordOpened {
        channel: [u8; 32],
        frames: u32,
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

/// A topic only the person's hosts carry, which this device joined
#[derive(Default)]
struct HostedTopic {
    /// Fingerprint of the identity material last announced on the topic, so
    /// a renewed certificate or a new revocation goes out the tick it lands
    /// and an unchanged one never does.
    identity_sent: Option<[u8; 32]>,
}

struct HostRuntime {
    session: Arc<RelayCoeditSession>,
    pump: tokio::task::JoinHandle<()>,
}

/// Early boot of another of the person's devices asking for its sibling
/// half. The peer is not authenticated by transport: the boot key
/// certificate and the challenge are what the sibling checks.
#[derive(Clone, Debug)]
struct DeviceUnlockProtocol;

impl ProtocolHandler for DeviceUnlockProtocol {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        services::device_unlock(connection);
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
                    NeighbourMsg::SurfaceStreamStart { .. }
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

/// Read the personal fabric frames the connection of a session with one of
/// the person's devices brings, whichever side opened it, into the actor,
/// bound to the session's membership `epoch`, until it closes. Each frame
/// comes on a bidirectional stream of its own after the session's first;
/// the return direction carries nothing.
fn read_personal_fabric(
    connection: iroh::endpoint::Connection,
    tx: mpsc::Sender<InternalEvent>,
    epoch: MembershipEpoch,
) {
    let peer = connection.remote_id();
    tokio::spawn(async move {
        loop {
            let Ok((_, mut recv)) = connection.accept_bi().await else {
                return;
            };
            let bytes = match recv
                .read_to_end(crate::fabric::MAX_FABRIC_CHECKPOINT_BYTES + 128)
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
            // Read here, off the wire, before anything queues behind it: a
            // clock report's delay is measured from this moment.
            let received = Instant::now();
            if tx
                .send(InternalEvent::PersonalFabric {
                    peer,
                    epoch,
                    user_public_key: frame.user_public_key,
                    payload: frame.payload,
                    received,
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
/// reachable for a while (`tilo_endpoint_reachability`): it dials a peer
/// or host, enrols a device, administers a relay workspace, joins a call or
/// sends to a named one of the person's devices. `None` for everything a
/// lone desktop does by itself: joining and publishing to a topic nobody
/// else is on (its workspaces, its fabric broadcast) sends nothing, and a lone
/// desktop does that on every local change.
fn remote_party_command(command: &NetCmd) -> Option<&'static str> {
    Some(match command {
        NetCmd::Connect { .. } => "dialling a peer",
        NetCmd::ConnectHost { .. } | NetCmd::NativeConnect(..) => "dialling a host",
        NetCmd::PublishPersonalFabricTarget { .. } | NetCmd::PublishNearbyTransfer { .. } => {
            "sending to one of the person's devices"
        }
        _ => return None,
    })
}

/// Why `event` means a remote party reached this device (on a path that
/// needed no relay, such as the local network), so it should be reachable
/// through its relays too.
fn remote_party_event(event: &InternalEvent) -> Option<&'static str> {
    match event {
        InternalEvent::PersonalFabric { .. } => Some("one of the person's devices connected"),
        InternalEvent::SurfaceFrame { .. } => Some("a peer streamed a window"),
        _ => None,
    }
}

pub struct Actor;

impl Actor {
    /// Spawn the actor for `endpoint`, also serving `served` — protocol
    /// handlers the session's own features answer on, beside the actor's: a
    /// device's action plane, say. Each names its own ALPN and serves it for
    /// as long as the actor runs.
    pub fn spawn_serving(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        host_identity: Option<HostIdentity>,
        listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
        reachability: Reachability,
        served: crate::local_host::Protocols,
    ) -> (mpsc::Sender<NetCmd>, tokio::task::JoinHandle<()>) {
        Self::spawn_with_heartbeat(
            endpoint,
            lookup,
            host_identity,
            listen_addrs,
            HEARTBEAT_INTERVAL,
            reachability,
            None,
            served,
        )
    }

    /// [`Self::spawn_serving`] with an explicit liveness and host-retry
    /// cadence, so a test can observe several heartbeats without waiting out
    /// the production interval.
    fn spawn_with_heartbeat(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        host_identity: Option<HostIdentity>,
        listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
        heartbeat: Duration,
        reachability: Reachability,
        hosting: Option<Hosting>,
        served: crate::local_host::Protocols,
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
            Serving { hosting, served },
        ));
        (cmd_tx, task)
    }
}

/// What this endpoint serves beside the actor's own protocols: this
/// device's own host's handlers, when it hosts the workspaces placed on it,
/// and the session's own (its action plane, say) — all on the one endpoint
/// both reach this device at.
struct Serving {
    hosting: Option<Hosting>,
    served: crate::local_host::Protocols,
}

/// This device's workspace host, for the actor: the handle the native owner
/// joins the workspaces hosted here through, and the handlers serving the host
/// on the router ([`crate::local_host`]).
pub struct Hosting {
    pub host: crate::local_host::LocalHost,
    pub protocols: crate::local_host::Protocols,
}

/// The channel governance a device published to one workspace: each message's
/// retention channel and its encoded frame.
type GovernanceLog = VecDeque<([u8; 32], Vec<u8>)>;

/// Mutable state shared across every `select!` branch of [`run`]'s event
/// loop, bundled into a struct so each branch's handling can be split into
/// its own method instead of one long function body.
struct ActorState {
    endpoint: Endpoint,
    lookup: MemoryLookup,
    host_identity: Option<HostIdentity>,
    native_routes: native_ingress::Routes,
    /// This device's workspace host, through which the native owner joins the
    /// workspaces hosted here.
    local_host: Option<crate::local_host::LocalHost>,
    /// Governance invocations waiting for their workspace's relay to answer.
    /// The endpoint's relays, held back until a remote party appears.
    reachability: Reachability,
    internal_tx: mpsc::Sender<InternalEvent>,
    /// The topics only the person's hosts carry that this device joined.
    topics: HashMap<WorkspaceId, HostedTopic>,
    /// Each workspace's direct sends, so one workspace's slow devices hold up no
    /// other workspace ([`workspace_lane`]).
    lanes: workspace_lane::Lanes,
    subscribers: HashMap<WorkspaceId, mpsc::Sender<NetEvent>>,
    /// The person's other devices ([`NetCmd::Connect`]), where each is
    /// reached.
    own_devices: HashMap<EndpointId, EndpointAddr>,
    /// The person's devices whose session with this one is up
    /// ([`device_sessions`]).
    siblings: HashMap<EndpointId, device_sessions::Sibling>,
    /// The person's devices being dialled for that session.
    sibling_dialling: HashSet<EndpointId>,
    /// Since when this device waits to be dialled by each of the person's
    /// devices with a lower endpoint id.
    sibling_waiting: HashMap<EndpointId, Instant>,
    /// Fingerprint of the hello last said to the person's devices.
    sibling_hello_sent: Option<[u8; 32]>,
    /// Where the person's devices are heard ([`NetCmd::WatchSiblings`]).
    sibling_events: Option<mpsc::Sender<NetEvent>>,
    hosts: HashMap<EndpointId, (EndpointAddr, Option<HomePeerExpectation>)>,
    host_sessions: HashMap<(WorkspaceId, EndpointId), HostRuntime>,
    /// Each joined workspace's link, on the direct lane.
    native_links: HashMap<WorkspaceId, native_ingress::NativeLink>,
    /// The devices that take part in this device's workspaces through it
    /// ([`NetCmd::ServeMembers`]).
    members: Option<tilo_peer_state::direct::Owning>,
    /// What this device's apps take the members' frames of each workspace under while no link of
    /// it is up, as they take a link's: a workspace this device hosts itself has none.
    member_scopes: HashMap<WorkspaceId, Arc<native_ingress::Scope>>,
    connecting_hosts: HashSet<(WorkspaceId, EndpointId)>,
    content_discovery_tasks: HashMap<(WorkspaceId, EndpointId), tokio::task::JoinHandle<()>>,
    /// Approver sessions that put this person's agents' questions on the
    /// sheet, one per connected host.
    agent_questions: Box<dyn services::Approvers>,
    next_membership_epoch: u64,
    /// Each workspace's admitted roster ([`NetCmd::SetWorkspaceRoster`]), and the
    /// window-stream connections peers opened to this device, bound to it.
    workspace_members: workspace_members::WorkspaceMembers,
    recent: HashSet<(WorkspaceId, [u8; 32])>,
    recent_order: VecDeque<(WorkspaceId, [u8; 32])>,
    /// When each recent remote-input message arrived (see [`mark_new_input`]).
    recent_input: HashMap<(WorkspaceId, [u8; 32]), (EndpointId, Instant)>,
    /// Channel governance this device published to each workspace, by retention
    /// channel (see [`remember_governance`]).
    governance: HashMap<WorkspaceId, GovernanceLog>,
    /// Retained state published to a workspace while none of its hosts had a
    /// session (see [`ActorState::hold_for_host`]).
    host_backlog: HashMap<WorkspaceId, HostBacklog>,

    /// Long-lived direct control connections. One unidirectional QUIC stream
    /// per message avoids head-of-line blocking between messages.
    surface_connections: HashMap<EndpointId, iroh::endpoint::Connection>,
    /// Direct routes being opened. A dial runs until it connects or fails;
    /// nothing waits on it: what goes to that device meanwhile goes through
    /// the workspace's host.
    surface_routes_opening: HashMap<EndpointId, tokio::task::JoinHandle<()>>,
    /// When a direct route to a device last failed to open: it is reached
    /// through the workspace's host until [`DIRECT_ROUTE_RETRY`] has passed.
    surface_routes_failed: HashMap<EndpointId, Instant>,
    active_personal_transfers: HashMap<(EndpointId, [u8; 16]), ActivePersonalTransfer>,
    next_personal_transfer_generation: u64,
    /// Every application message peers delivered here (see [`WireTally`]).
    tally: Arc<WireTally>,
    /// Fingerprint of the codec advert last announced, so a change goes out
    /// and an unchanged advert never does.
    caps_sent: Option<[u8; 32]>,
}

/// Every application message this device received from its peers, by kind:
/// host-session and direct-stream traffic alike, counted where it arrives
/// and before any deduplication. Transport keep-alives (QUIC PINGs) never
/// reach the application and are not here.
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
    total_bytes: u64,
    sent_bytes: std::sync::Arc<std::sync::atomic::AtomicU64>,
    cancel: tokio::sync::watch::Sender<bool>,
}

/// The exact certified device a personal-fabric stream goes to, the
/// connection of its session, and the user key its frames are authenticated
/// under.
struct PersonalStreamRoute {
    connection: iroh::endpoint::Connection,
    target: EndpointId,
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

impl Drop for HostRuntime {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

impl ActorState {
    /// Whether `peer` is a member of `ws` for window streams: on the workspace's
    /// admitted roster, or, on a topic without one, one of the person's
    /// devices whose session is up.
    fn is_surface_member(&self, ws: WorkspaceId, peer: EndpointId) -> bool {
        if self.workspace_members.has_roster(ws) {
            self.workspace_members
                .member(ws, peer, tilo_clock::unix_now_ns())
                .is_some()
        } else {
            self.siblings.contains_key(&peer)
        }
    }

    /// Take workspace `ws`'s admitted roster as `devices` (none, and not `keep`,
    /// once this device left it): the connections bound to a membership that
    /// ended close, and so does this device's own to a device no workspace holds
    /// any more.
    fn set_workspace_roster(&mut self, ws: WorkspaceId, devices: Vec<RosterDevice>, keep: bool) {
        let local = self.endpoint.id();
        let next = &mut self.next_membership_epoch;
        let removed = self.workspace_members.set_roster(
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
    /// and this device's own to a device no workspace holds any more.
    fn close_removed(&mut self, removed: workspace_members::Removed, reason: &[u8]) {
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
            self.surface_routes_failed.remove(&device);
        }
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
            Some(NetCmd::WireTally(reply)) => {
                let _ = reply.send(Arc::clone(&self.tally));
            }
            Some(NetCmd::NativeConnect(connect)) => self.native_connect(connect),
            Some(NetCmd::ServeMembers(members)) => self.serve_members(members),
            Some(NetCmd::LocalHostReady) => self.retry_native_routes(),
            Some(NetCmd::NativePublish { ws }) => self.drive_native_publication(ws),
            Some(NetCmd::JoinWorkspace { ws, on_event }) => {
                self.handle_join_workspace(ws, on_event);
            }
            Some(NetCmd::WatchSiblings { on_event }) => self.watch_siblings(on_event).await,
            Some(NetCmd::PublishToSiblings { msg }) => self.publish_to_siblings(msg),
            Some(NetCmd::SetWorkspaceRoster { ws, devices }) => {
                self.set_workspace_roster(ws, devices, true);
            }
            Some(NetCmd::LeaveWorkspace { ws }) => {
                self.topics.remove(&ws);
                self.native_routes.remove(ws);
                self.native_links.remove(&ws);
                self.subscribers.remove(&ws);
                self.set_workspace_roster(ws, Vec::new(), false);
                self.lanes.close(ws);
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
            // A workspace's link brings what its host retains when it starts,
            // and every frame after it live. A write this device let go
            // unapplied waits in that history: once what it waited for lands,
            // the link reads the history again from where the workspace stands.
            Some(NetCmd::NativeReplay { ws }) => {
                if let Some(link) = self.native_links.get(&ws) {
                    link.link.ask_replay();
                } else {
                    tracing::debug!(
                        ws = %ws,
                        "no link of the workspace is up; the next one replays as it starts"
                    );
                }
            }
            // Every host of the workspace that retains records opens this one
            // for this device: what it holds of it, then its changes. Asking
            // a session that already opened it asks nothing again.
            Some(NetCmd::OpenRecord { ws, channel }) => {
                let sessions: Vec<_> = self
                    .host_sessions
                    .iter()
                    .filter(|((session_ws, _), _)| *session_ws == ws)
                    .map(|(_, runtime)| Arc::clone(&runtime.session))
                    .collect();
                if sessions.is_empty() {
                    tracing::debug!(
                        ws = %ws,
                        "no host session of the workspace is up; the record is asked for again when one is"
                    );
                }
                for session in sessions {
                    if let Err(error) = session.open_record(channel) {
                        tracing::debug!(ws = %ws, %error, "could not ask a host for a record");
                    }
                }
            }
            Some(NetCmd::Publish { ws, msg }) => {
                remember_governance(&mut self.governance, ws, &msg);
                self.carry_own(&msg);
                if self.native_routes.contains(ws) {
                    match native_ingress::carried(&msg) {
                        Some(native_ingress::Carried::Link) => {
                            self.publish_over_link(ws, &msg);
                            return true;
                        }
                        Some(native_ingress::Carried::Publication) => {
                            self.drive_native_publication(ws);
                            return true;
                        }
                        Some(native_ingress::Carried::Nowhere) => return true,
                        None => {}
                    }
                }
                self.hold_for_host(ws, &msg);
                publish(&self.host_sessions, ws, &msg);
            }
            Some(NetCmd::PublishWorkspaceControl { ws, msg, target }) => {
                self.publish_workspace_control(ws, msg, target);
            }
            Some(NetCmd::PublishRecordContent(delivery)) => {
                self.publish_record_content(delivery);
            }
            Some(NetCmd::PublishPersonalFabric {
                user_public_key,
                payload,
            }) => {
                self.publish_personal_fabric_to(user_public_key, payload, None);
            }
            Some(NetCmd::PublishPersonalFabricTarget {
                user_public_key,
                target,
                payload,
            }) => {
                self.publish_personal_fabric_to(user_public_key, payload, Some(target));
            }
            Some(NetCmd::CancelNearbyTransfer {
                target,
                transfer_id,
            }) => {
                self.cancel_personal_transfer((target, transfer_id));
            }
            Some(NetCmd::PublishNearbyTransfer {
                user_public_key,
                target,
                transfer,
                file,
            }) => {
                self.publish_nearby_transfer(user_public_key, target, transfer, file);
            }
            Some(NetCmd::Connect { addr }) => {
                if addr.id == self.endpoint.id() {
                    return true;
                }
                self.lookup.add_endpoint_info(addr.clone());
                self.own_devices.insert(addr.id, addr);
                self.dial_siblings();
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
                    self.teardown_host(id);
                }
                self.lookup.add_endpoint_info(addr.clone());
                self.hosts.insert(id, (addr.clone(), expectation));
                let Some(identity) = self.host_identity.as_ref() else {
                    tracing::warn!(peer = %id, "cannot connect workspace host without a device certificate");
                    return true;
                };
                self.agent_questions.start(&self.endpoint, addr, identity);
                self.schedule_all_topic_sessions();
            }
            None => return false,
        }
        true
    }

    /// Stop relay sessions and discovery tasks for one host. An address
    /// refresh must never revoke a still-valid Files hydration source, so
    /// this keeps the host's content registrations.
    fn teardown_host(&mut self, id: EndpointId) {
        self.agent_questions.stop(id);
        self.hosts.remove(&id);
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
    }

    /// Open the direct route to `peer` unless it is up, being opened, or
    /// failed to open within [`DIRECT_ROUTE_RETRY`]. The dial runs as long as
    /// connecting takes; its outcome comes back as
    /// [`InternalEvent::SurfaceRouteOpened`].
    fn open_surface_route(&mut self, peer: EndpointId) {
        if self.surface_routes_opening.contains_key(&peer)
            || self
                .surface_routes_failed
                .get(&peer)
                .is_some_and(|failed| failed.elapsed() < DIRECT_ROUTE_RETRY)
        {
            return;
        }
        let addr = self
            .own_devices
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
                    tracing::debug!(peer = %peer, %error, "a direct route failed to open");
                })
                .ok();
            let _ = internal_tx
                .send(InternalEvent::SurfaceRouteOpened { peer, connection })
                .await;
        });
        self.surface_routes_opening.insert(peer, task);
    }

    /// [`InternalEvent::SurfaceRouteOpened`]: what goes to `peer` from now on
    /// takes the route that came up, or the workspace's host for a while when
    /// none could.
    fn surface_route_opened(
        &mut self,
        peer: EndpointId,
        connection: Option<iroh::endpoint::Connection>,
    ) {
        self.surface_routes_opening.remove(&peer);
        match connection {
            Some(connection) => {
                self.surface_routes_failed.remove(&peer);
                self.surface_connections.insert(peer, connection);
            }
            None => {
                self.surface_routes_failed.insert(peer, Instant::now());
            }
        }
    }

    /// Whether a direct route to `peer` is up.
    fn live_surface_route(&mut self, peer: EndpointId) -> bool {
        let Some(connection) = self.surface_connections.get(&peer) else {
            return false;
        };
        if connection.close_reason().is_some() {
            self.surface_connections.remove(&peer);
            return false;
        }
        true
    }

    /// Send `payload` to each of the person's devices that proved a
    /// certificate of `user_public_key` on its session, or to
    /// `exact_target` alone, on the session's connection ([`fabric_link`]).
    fn publish_personal_fabric_to(
        &self,
        user_public_key: [u8; 32],
        payload: PersonalFabricPayload,
        exact_target: Option<EndpointId>,
    ) {
        if matches!(payload, PersonalFabricPayload::Action(ref bytes) if bytes.len() > crate::fabric::MAX_FABRIC_ACTION_BYTES)
        {
            tracing::warn!("personal fabric action exceeds wire limit");
            return;
        }
        let wire = PersonalFabricWireFrame {
            user_public_key,
            payload,
        };
        let Ok(bytes) = postcard::to_allocvec(&wire) else {
            return;
        };
        if bytes.len() > crate::fabric::MAX_FABRIC_CHECKPOINT_BYTES + 128 {
            tracing::warn!(
                bytes = bytes.len(),
                "personal fabric frame exceeds wire limit"
            );
            return;
        }
        // A device whose session is down misses the frame: when its session
        // is back, each side asks the other for its fabric anew.
        for (peer, sibling) in &self.siblings {
            if exact_target.is_none_or(|target| *peer == target) && sibling.proved(&user_public_key)
            {
                sibling.fabric.send(bytes.clone());
            }
        }
    }

    /// Report one outgoing transfer's progress. A terminal status is
    /// delivered even when the subscriber is momentarily full: losing it
    /// would leave the sender waiting on a stream that has ended.
    fn report_personal_transfer(
        &self,
        key: (EndpointId, [u8; 16]),
        sent_bytes: u64,
        total_bytes: u64,
        status: PersonalTransferStatus,
    ) {
        let Some(progress) = self.sibling_events.clone() else {
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
            key,
            active.sent_bytes.load(std::sync::atomic::Ordering::Relaxed),
            active.total_bytes,
            PersonalTransferStatus::Cancelled,
        );
        true
    }

    fn cancel_personal_transfer_route(&mut self, target: EndpointId) {
        let keys = self
            .active_personal_transfers
            .keys()
            .filter(|key| key.0 == target)
            .copied()
            .collect::<Vec<_>>();
        for key in keys {
            self.cancel_personal_transfer(key);
        }
    }

    fn publish_nearby_transfer(
        &mut self,
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
                key,
                resume_offset,
                total_bytes,
                PersonalTransferStatus::Failed,
            );
        };
        if transfer_id == [0; 16] || request_id == [0; 16] || resume_offset > total_bytes {
            failed(self);
            return;
        }
        let Some(connection) = self
            .siblings
            .get(&target)
            .filter(|sibling| sibling.proved(&user_public_key))
            .map(|sibling| sibling.fabric.connection())
        else {
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
                total_bytes,
                sent_bytes: std::sync::Arc::clone(&sent_bytes),
                cancel,
            },
        );
        let route = PersonalStreamRoute {
            connection,
            target,
            user_public_key,
        };
        let internal = self.internal_tx.clone();
        self.report_personal_transfer(
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

    /// Say one window's control message to `target` alone: on the direct
    /// route while it is up, and through the workspace's host to that device's
    /// connection otherwise, while a direct route is opened for what follows.
    /// A device that takes no connection of its own (a machine behind a NAT,
    /// a hosted environment) is reached that way, and nothing of it is fanned
    /// out to the workspace's other members.
    fn publish_workspace_control(
        &mut self,
        ws: WorkspaceId,
        msg: NeighbourMsg,
        target: EndpointId,
    ) {
        if !matches!(
            msg,
            NeighbourMsg::RemoteInput { .. }
                | NeighbourMsg::SurfaceKeyframeRequest { .. }
                | NeighbourMsg::SurfaceViewRequest { .. }
                | NeighbourMsg::SurfaceStreamStart { .. }
                | NeighbourMsg::SurfaceStreamEnd { .. }
        ) {
            tracing::warn!(ws = %ws, "refused a non-control message on the control route");
            return;
        }
        if self.live_surface_route(target) {
            // Input lands within its frame budget or not at all: what fails
            // goes through the host ([`Self::direct_sent`]).
            let _ = self.publish_direct(ws, target, msg);
            return;
        }
        if self.is_surface_member(ws, target) {
            self.open_surface_route(target);
        }
        if !self.relay_to_device(ws, Some(target), &msg) {
            // No host carries the workspace: the direct route is the only one.
            let _ = self.publish_direct(ws, target, msg);
        }
    }

    /// The host at `peer` subscribed this device to `ws`: refresh what its
    /// native session holds. Only a workspace host's is news to the
    /// shell: this device's own host and the person's devices are no
    /// durable hosts it seeds.
    fn host_ready(&self, ws: WorkspaceId, peer: EndpointId, records_when_opened: bool) -> NetEvent {
        // The workspace's own host caught this device up: the workspace holds what it
        // holds. Another host on the topic (a personal relay, a peer) says
        // nothing about the workspace's host, whose link has its own word.
        if !self.native_routes.contains(ws) {
            crate::workspace_sync::synced(ws);
        }
        NetEvent::HostReady {
            ws,
            peer,
            records_when_opened,
        }
    }

    fn publish_record_content(&mut self, delivery: RecordContentDelivery) {
        let RecordContentDelivery { ws, target, msg } = delivery;
        if !matches!(
            msg,
            NeighbourMsg::RecordContentPiece { .. } | NeighbourMsg::RecordContentLacks { .. }
        ) {
            tracing::warn!(
                ws = %ws,
                "refused a non-content message on the record-content route"
            );
            return;
        }
        // The device that asked is the workspace's host on this device's link: the
        // ask came over that link, so the answer goes back the same way.
        if let Some(target) = target
            && let Some(link) = self.native_links.get(&ws)
            && link.scope.host().host.device_public_key == *target.as_bytes()
        {
            if let Err(error) = link.link.send(msg) {
                tracing::debug!(ws = %ws, %error, "a record's content waits for the workspace's next link");
            }
            return;
        }
        let msg = match target {
            Some(target) if self.is_surface_member(ws, target) => {
                match self.publish_direct(ws, target, msg) {
                    // How it went comes back from the workspace's lane.
                    Ok(()) => return,
                    Err(msg) => *msg,
                }
            }
            _ => msg,
        };
        if !self.relay_to_device(ws, target, &msg) {
            tracing::debug!(ws = %ws, "record content has no route to the device that asked");
        }
    }

    /// Send `msg` through the workspace hosts to `target` alone: a record's
    /// content, or a window's control message, that cannot go directly.
    /// Whether a host took it.
    fn relay_to_device(
        &self,
        ws: WorkspaceId,
        target: Option<EndpointId>,
        msg: &NeighbourMsg,
    ) -> bool {
        // The workspace hosts route it to that device alone when it is
        // connected to them.
        target.is_some_and(|target| {
            postcard::to_allocvec(msg).is_ok_and(|frame| {
                self.host_sessions
                    .iter()
                    .filter(|((session_ws, _), _)| *session_ws == ws)
                    .any(|(_, runtime)| {
                        runtime
                            .session
                            .publish_record_content(*target.as_bytes(), frame.clone())
                            .is_ok()
                    })
            })
        })
    }

    /// Queue one message for exactly `target` on the authenticated direct
    /// surface/control plane, on the workspace's lane: the peer must be live on
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
            tracing::warn!(ws = %ws, peer = %target, "direct route is no longer live");
            return Err(Box::new(msg));
        }
        // Input must land within an interactive frame budget or not at all;
        // an action frame (up to a catalogue page) is worth waiting for.
        let budget = if matches!(
            msg,
            NeighbourMsg::RecordContentPiece { .. } | NeighbourMsg::RecordContentLacks { .. }
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
        let direct = workspace_lane::Direct {
            addr: self
                .own_devices
                .get(&target)
                .cloned()
                .unwrap_or_else(|| EndpointAddr::new(target)),
            cached: self.surface_connections.get(&target).cloned(),
            msg: wire.message,
            bytes,
            budget,
        };
        self.lanes.queue_direct(ws, direct).inspect_err(|_| {
            tracing::warn!(ws = %ws, peer = %target, "workspace's remote work is backed up; not sent directly");
        })
    }

    /// A direct send queued on `ws`'s lane went (or did not): keep its
    /// connection, and send what failed the way it would have gone without
    /// a direct route.
    fn direct_sent(&mut self, ws: WorkspaceId, outcome: workspace_lane::DirectOutcome) {
        let workspace_lane::DirectOutcome {
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
            NeighbourMsg::RecordContentPiece { .. }
            | NeighbourMsg::RecordContentLacks { .. }
            | NeighbourMsg::RemoteInput { .. }
            | NeighbourMsg::SurfaceKeyframeRequest { .. }
            | NeighbourMsg::SurfaceViewRequest { .. }
            | NeighbourMsg::SurfaceStreamStart { .. }
            | NeighbourMsg::SurfaceStreamEnd { .. } => {
                let _ = self.relay_to_device(ws, Some(target), &msg);
            }
            _ => {}
        }
    }

    /// [`NetCmd::JoinWorkspace`] arm of [`Self::handle_command`], split out
    /// to keep that dispatcher short.
    fn handle_join_workspace(&mut self, ws: WorkspaceId, on_event: mpsc::Sender<NetEvent>) {
        self.subscribers.insert(ws, on_event);
        self.schedule_topic_sessions(ws);
    }

    /// [`Self::schedule_topic_sessions`] for every hosted topic.
    fn schedule_all_topic_sessions(&mut self) {
        let workspaces: Vec<WorkspaceId> = self.topics.keys().copied().collect();
        for ws in workspaces {
            self.schedule_topic_sessions(ws);
        }
    }

    /// Open whatever session of hosted topic `ws` with a host of the
    /// person's is missing and not being opened.
    fn schedule_topic_sessions(&mut self, ws: WorkspaceId) {
        if !self.topics.contains_key(&ws) || self.native_routes.contains(ws) {
            return;
        }
        let Some(identity) = self.host_identity.clone() else {
            return;
        };
        let mut routes = HostRouteScheduler {
            endpoint: &self.endpoint,
            identity: &identity,
            internal_tx: &self.internal_tx,
            sessions: &self.host_sessions,
            connecting: &mut self.connecting_hosts,
            content_discovery_tasks: &mut self.content_discovery_tasks,
        };
        for (addr, expectation) in self.hosts.values().cloned() {
            routes.schedule(ws, addr, expectation);
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
            Some(InternalEvent::DirectSent { ws, outcome }) => self.direct_sent(ws, *outcome),
            Some(InternalEvent::SiblingConnected(connection)) => {
                self.sibling_connected(connection);
            }
            Some(InternalEvent::SiblingOpened(opened)) => self.sibling_opened(*opened).await,
            Some(InternalEvent::SiblingFailed(peer)) => {
                self.sibling_dialling.remove(&peer);
            }
            Some(InternalEvent::SiblingHeard { peer, epoch, heard }) => {
                self.sibling_heard(peer, epoch, heard).await;
            }
            Some(InternalEvent::SiblingClosed { peer, epoch }) => {
                self.sibling_closed(peer, epoch).await;
            }
            Some(InternalEvent::HostConnected {
                ws,
                peer,
                addr,
                result,
            }) => {
                self.handle_host_connected(ws, peer, &addr, result);
            }
            Some(InternalEvent::NativeLinkConnected {
                ws,
                linked,
                scope,
                held,
            }) => self.native_link_connected(ws, *linked, scope, held),
            Some(InternalEvent::NativeHeard { ws, heard }) => {
                self.native_heard(ws, heard).await;
            }
            Some(InternalEvent::MemberHeard {
                ws,
                workspace,
                heard,
            }) => self.member_heard(ws, workspace, heard).await,
            Some(InternalEvent::NativeReady { ws }) => self.native_ready(ws).await,
            Some(InternalEvent::NativeLinkClosed { ws, deleted }) => {
                self.native_link_closed(ws, deleted).await;
            }
            Some(InternalEvent::NativeDialFailed {
                ws,
                via,
                governor,
                reason,
            }) => self.native_dial_failed(ws, &via, governor, &reason),
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
                self.workspace_members
                    .connection(peer, connection_incarnation, connection);
            }
            Some(InternalEvent::SurfaceRouteOpened { peer, connection }) => {
                self.surface_route_opened(peer, connection);
            }
            Some(InternalEvent::PersonalFabric {
                peer,
                epoch,
                user_public_key,
                payload,
                received,
            }) => {
                self.handle_personal_fabric(peer, epoch, user_public_key, payload, received)
                    .await;
            }
            Some(InternalEvent::IdentityMaterialChanged) => {
                self.identity_material_changed();
            }
            Some(InternalEvent::PersonalTransferProgress {
                target,
                transfer_id,
                generation,
                sent_bytes,
            }) => {
                let key = (target, transfer_id);
                if let Some(total_bytes) = self
                    .active_personal_transfers
                    .get(&key)
                    .filter(|active| active.generation == generation)
                    .map(|active| active.total_bytes)
                {
                    self.report_personal_transfer(
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
        // peer to be a member: of the workspace's admitted roster, or, on a topic
        // without one, one of the person's devices whose session is up.
        // ShellNetwork performs the capability and claimed-publisher checks
        // before dispatch.
        let membership = if self.workspace_members.has_roster(ws) {
            match self.workspace_members.admit(
                ws,
                peer,
                connection_incarnation,
                tilo_clock::unix_now_ns(),
            ) {
                Admission::Member(epoch) => Some(epoch),
                Admission::Refused => None,
                Admission::Expired(removed) => {
                    tracing::info!(ws = %ws, peer = %peer, "surface frame from a device whose certificate expired");
                    self.close_removed(removed, b"certificate expired");
                    return;
                }
            }
        } else {
            self.siblings.get(&peer).map(|sibling| sibling.epoch)
        };
        let Some(membership_epoch) = membership else {
            tracing::warn!(ws = %ws, peer = %peer, "surface frame from a device that is no member");
            return;
        };
        // Input has no event id or timestamp, so two deliberate
        // identical key/button events must not be content-deduped.
        // A remote-action retry is byte-identical on purpose; the
        // receiving host deduplicates it by invocation id instead.
        if (matches!(&msg, NeighbourMsg::RemoteInput { .. })
            || said_again_on_purpose(&msg)
            || mark_new_message(ws, fingerprint, &mut self.recent, &mut self.recent_order))
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

    /// [`InternalEvent::PersonalFabric`] arm of [`Self::handle_internal`], split
    /// out to keep that dispatcher short: a frame `peer` sent on the session
    /// of membership `epoch`, taken while that session lasts. `ShellNetwork`
    /// still binds the frame to the certificate the device proved there.
    async fn handle_personal_fabric(
        &self,
        peer: EndpointId,
        epoch: MembershipEpoch,
        user_public_key: [u8; 32],
        payload: PersonalFabricPayload,
        received: Instant,
    ) {
        self.tally.note("PersonalFabric");
        if self
            .siblings
            .get(&peer)
            .is_none_or(|sibling| sibling.epoch != epoch)
        {
            tracing::warn!(peer = %peer, "personal fabric frame from an ended session");
            return;
        }
        if let Some(tx) = &self.sibling_events {
            let _ = tx
                .send(NetEvent::PersonalFabric {
                    peer,
                    membership_epoch: epoch,
                    user_public_key,
                    payload,
                    received,
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
            self.report_personal_transfer(key, sent_bytes, active.total_bytes, status);
        }
    }

    /// Whether a message on hosted topic `ws` is news. An identity hello is
    /// always news: the shell takes the device's identity anew each time.
    fn is_news(&mut self, ws: WorkspaceId, msg: &NeighbourMsg, fingerprint: [u8; 32]) -> bool {
        matches!(msg, NeighbourMsg::IdentityHello { .. })
            || said_again_on_purpose(msg)
            || mark_new_message(ws, fingerprint, &mut self.recent, &mut self.recent_order)
    }

    /// [`InternalEvent::HostConnected`] arm of [`Self::handle_internal`],
    /// split out to keep that dispatcher short. `peer` is a host of the
    /// person's.
    fn handle_host_connected(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
        attempted_addr: &EndpointAddr,
        result: Box<Result<RelayCoeditSession, String>>,
    ) {
        self.connecting_hosts.remove(&(ws, peer));
        if self.native_routes.contains(ws) || !self.topics.contains_key(&ws) {
            return;
        }
        if self
            .hosts
            .get(&peer)
            .is_none_or(|(current, _)| current != attempted_addr)
        {
            return;
        }
        match *result {
            Ok(session) => {
                tracing::info!(ws = %ws, host = %peer, "workspace host session started");
                let session = Arc::new(session);
                self.announce_identity_to_new_host(ws, &session);
                for (channel, bytes) in self.governance.get(&ws).into_iter().flatten() {
                    if let Err(error) = session.publish_checkpoint(*channel, bytes.clone()) {
                        tracing::warn!(ws = %ws, %error, "could not send channel governance to the workspace host");
                    }
                }
                for (channel, retention, bytes) in self.host_backlog.remove(&ws).unwrap_or_default()
                {
                    let sent = match retention {
                        Retention::Checkpoint => session.publish_checkpoint(channel, bytes),
                        Retention::Delta => session.publish_retained_delta(channel, bytes),
                    };
                    if let Err(error) = sent {
                        tracing::warn!(ws = %ws, %error, "could not send held collaboration state to the workspace host");
                    }
                }
                if let Err(error) = session.live_join() {
                    tracing::warn!(ws = %ws, %error, "could not join the workspace host's live plane");
                }
                let replaying = Arc::new(std::sync::atomic::AtomicBool::new(true));
                let pump = spawn_host_pump(
                    ws,
                    peer,
                    Arc::clone(&session),
                    Arc::clone(&replaying),
                    self.internal_tx.clone(),
                    true,
                );
                self.host_sessions
                    .insert((ws, peer), HostRuntime { session, pump });
            }
            Err(error) => {
                tracing::warn!(ws = %ws, peer = %peer, %error, "workspace host connection failed");
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
        self.tally.note(host_event_kind(&event));
        if let Some(event) = self.map_host_event(ws, peer, event) {
            self.publish_to_subscriber(ws, event).await;
        }
    }

    /// What a host session's event means to this device: `None` when it
    /// changes nothing here.
    fn map_host_event(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
        event: HostEvent,
    ) -> Option<NetEvent> {
        match event {
            HostEvent::Ready => {
                let when_opened = self
                    .host_sessions
                    .get(&(ws, peer))
                    .is_some_and(|runtime| runtime.session.records_when_opened());
                Some(self.host_ready(ws, peer, when_opened))
            }
            HostEvent::RecordOpened { channel, frames } => Some(NetEvent::HostRecordOpened {
                ws,
                channel,
                frames,
            }),
            HostEvent::Member { device, active } => self.host_member(ws, peer, device, active),
            HostEvent::Message {
                author,
                replayed: true,
                msg: msg @ NeighbourMsg::AppDocument { .. },
                ..
            } if self.is_own_device(&author) => Some(NetEvent::OwnReplay { ws, msg }),
            HostEvent::Message { author, .. } if self.is_own_device(&author) => {
                // The host replays retained state to every subscriber,
                // including what this device published earlier; nothing
                // downstream holds this device's own certificate to
                // authenticate them.
                // Only its replayed documents come back (above).
                None
            }
            HostEvent::Message {
                author,
                fingerprint,
                replayed,
                msg,
            } => self.host_message(ws, peer, author, fingerprint, replayed, msg),
            HostEvent::Closed => Some(self.host_session_closed(ws, peer)),
        }
    }

    /// The session of `ws` with the host at `peer` ended.
    fn host_session_closed(&mut self, ws: WorkspaceId, peer: EndpointId) -> NetEvent {
        tracing::info!(ws = %ws, host = %peer, "workspace host session ended; the next heartbeat dials it again");
        if let Some(runtime) = self.host_sessions.remove(&(ws, peer)) {
            runtime.pump.abort();
        }
        NetEvent::PeerLeft {
            ws,
            peer,
            cause: PeerDepartureCause::HostSessionClosed,
            membership_epoch: None,
        }
    }

    /// Hand `event` to the workspace's subscriber and wake the loop for the
    /// recorded streams, which are written out of band.
    async fn publish_to_subscriber(&self, ws: WorkspaceId, event: NetEvent) {
        let Some(tx) = self.subscribers.get(&ws) else {
            return;
        };
        let _ = tx.send(event).await;
    }

    /// A message a host session delivered: `None` when it repeats one this
    /// device already handled, unless the host was asked to replay it.
    fn host_message(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
        author: EndpointId,
        fingerprint: [u8; 32],
        replayed: bool,
        msg: NeighbourMsg,
    ) -> Option<NetEvent> {
        if matches!(
            msg,
            NeighbourMsg::RemoteInput { .. } | NeighbourMsg::IdentityHello { .. }
        ) {
            tracing::debug!(ws = %ws, author = %author, kind = ?std::mem::discriminant(&msg), "host session message");
        }
        // A replay the host was asked for repeats what this device saw
        // before on purpose: the broker drops what it holds.
        let new = match msg {
            NeighbourMsg::RemoteInput { .. } => mark_new_input(
                ws,
                peer,
                fingerprint,
                Instant::now(),
                &mut self.recent_input,
            ),
            _ if self.topics.contains_key(&ws) => self.is_news(ws, &msg, fingerprint),
            _ if said_again_on_purpose(&msg) => true,
            _ => mark_new_message(ws, fingerprint, &mut self.recent, &mut self.recent_order),
        };
        if !new && !replayed {
            return None;
        }
        Some(NetEvent::Msg {
            ws,
            peer: author,
            membership_epoch: None,
            replayed,
            msg,
        })
    }

    /// Whether [`Self::run_heartbeat_tick`] has anything to do: one of the
    /// person's devices without a session, a workspace host or a governed
    /// workspace's relay to keep connected, or a remote action held for a peer.
    /// A converged pair of desktops has none of these, so its runtime sleeps
    /// until one appears ([`run`]).
    fn needs_upkeep(&self) -> bool {
        self.own_devices
            .keys()
            .any(|device| !self.siblings.contains_key(device))
            || !self.hosts.is_empty()
            || self.remote_host_sessions()
            || self.native_routes.any_remote()
            // A room this device hosts is joined in process; a join not yet
            // made is retried here as a remote dial is, so nothing waits on it
            // with nothing to wake it.
            || self
                .native_routes
                .local()
                .any(|ws| !self.native_links.contains_key(&ws))
    }

    /// Whether a host session goes to another machine: sessions with this
    /// device's own host keep nothing up.
    fn remote_host_sessions(&self) -> bool {
        !self.host_sessions.is_empty()
    }

    /// Whether `command` stays in this process: it goes to this device's
    /// own host, so it needs no relay up.
    fn stays_local(&self, command: &NetCmd) -> bool {
        matches!(
            command,
            NetCmd::NativeConnect(connect)
                if self.host_identity.as_ref().is_some_and(|identity| {
                    connect.material(identity).is_ok_and(|(hint, cert, _)| {
                        tilo_peer_state::owner::session::hosted_by(&hint, &cert.device_public_key)
                    })
                })
        )
    }

    /// The remote party this actor is connected to or keeps dialling, if
    /// any, which holds this device's relays up ([`Need::Transport`]): a
    /// peer it dials, a peer on one of its topics, a workspace host, a
    /// governed workspace's relay, one of the person's devices, a window streamed
    /// with a peer, a remote action waiting for its peer, a device being
    /// enrolled or a Messages account its contacts text. A peer that left
    /// stops counting once its membership lapses.
    fn remote_party(&self) -> Option<&'static str> {
        // This device's own host is joined in process and needs no relay.
        let own = self.endpoint.id();
        Some(if !self.own_devices.is_empty() {
            "one of the person's devices this device dials"
        } else if !self.siblings.is_empty() {
            "one of the person's devices dialled this one"
        } else if !self.hosts.is_empty()
            || self.connecting_hosts.iter().any(|(_, host)| *host != own)
            || self.remote_host_sessions()
        {
            "a workspace host"
        } else if self.native_routes.any_remote() {
            "a workspace hosted elsewhere"
        } else if !self.active_personal_transfers.is_empty() {
            "one of the person's devices"
        } else if !self.surface_connections.is_empty() || !self.surface_routes_opening.is_empty() {
            "a window streamed with a peer"
        } else {
            return None;
        })
    }

    /// Retry any topic subscription, direct peer or workspace host not yet
    /// connected. Runs every [`HEARTBEAT_INTERVAL`] while
    /// [`Self::needs_upkeep`].
    fn run_heartbeat_tick(&mut self) {
        self.retry_native_routes();
        // A session with this device's own host loses nothing between
        // heartbeats: it publishes as the owner commits.
        let workspaces: Vec<_> = self.native_links.keys().copied().collect();
        for ws in workspaces {
            self.drive_native_publication(ws);
        }
        self.schedule_all_topic_sessions();
        self.dial_siblings();
    }

    /// Each session heard this device's identity when it started; from here
    /// on it hears it again only when it changes (a renewed certificate, a
    /// new revocation), which the files it is read from say.
    fn identity_material_changed(&mut self) {
        let workspaces: Vec<_> = self.topics.keys().copied().collect();
        for ws in workspaces {
            self.announce_identity_if_changed(ws);
        }
        self.hello_siblings_if_changed();
    }

    /// The identity announcement this device makes on `ws`: its certificate
    /// for the workspace and the revocations that come with it.
    fn identity_hello(&self, ws: WorkspaceId) -> Option<NeighbourMsg> {
        let identity = self.host_identity.as_ref()?;
        let (cert, revocations) = identity.material_for(ws);
        Some(NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert),
            revocations,
        })
    }

    /// Announce this device's identity on every session of `ws` again: a
    /// device there could not place something of this one's without it.
    fn announce_identity(&mut self, ws: WorkspaceId) {
        let Some(hello) = self.identity_hello(ws) else {
            return;
        };
        let Ok(bytes) = postcard::to_allocvec(&hello) else {
            return;
        };
        let Some(topic) = self.topics.get_mut(&ws) else {
            return;
        };
        topic.identity_sent = Some(message_fingerprint(&bytes));
        for ((session_ws, _), runtime) in &self.host_sessions {
            if *session_ws == ws {
                Self::announce_identity_to_host(ws, &hello, &runtime.session);
            }
        }
    }

    /// Announce this device's identity on `ws` again, on each of its
    /// sessions, when it changed since it was last announced there: a new
    /// revocation reaches the workspace as soon as it is on disk.
    fn announce_identity_if_changed(&mut self, ws: WorkspaceId) {
        let Some(hello) = self.identity_hello(ws) else {
            return;
        };
        let Ok(bytes) = postcard::to_allocvec(&hello) else {
            return;
        };
        let fingerprint = message_fingerprint(&bytes);
        let Some(topic) = self.topics.get(&ws) else {
            return;
        };
        // Nothing announced here yet: the first session will.
        if topic.identity_sent.is_none_or(|sent| sent == fingerprint) {
            return;
        }
        tracing::info!(ws = %ws, "device identity material changed; announcing it again");
        self.announce_identity(ws);
    }

    /// This machine's codec advert: what it can encode and decode, so a
    /// sharer picks a codec every viewer takes. Receivers take an advert
    /// only from the device it describes.
    fn media_caps_message(&self) -> NeighbourMsg {
        crate::media_caps::local_advert(crate::behaviour::peer_liveness_handle(&self.endpoint.id()))
    }

    /// Announce this machine's codecs again to the person's devices when
    /// they changed since they were last announced.
    fn announce_media_caps_if_changed(&mut self) {
        let advert = self.media_caps_message();
        let Ok(bytes) = postcard::to_allocvec(&advert) else {
            return;
        };
        let fingerprint = message_fingerprint(&bytes);
        let Some(sent) = self.caps_sent.replace(fingerprint) else {
            return;
        };
        if sent == fingerprint {
            return;
        }
        self.publish_to_siblings(advert);
    }

    /// Whether a host-relayed message was authored by this device: the host
    /// names authors by the device key it authenticated them with.
    fn is_own_device(&self, author: &EndpointId) -> bool {
        self.host_identity
            .as_ref()
            .is_some_and(|identity| *author.as_bytes() == identity.device_public_key())
            || *author == self.endpoint.id()
    }

    /// Keep `msg` for the workspace's host when the host should retain it but no
    /// session to a configured host is up: a session is replaced (a pinned
    /// home peer takes over an unpinned dial) or redialled, and what a device
    /// writes meanwhile (a record it creates at launch, say) would otherwise
    /// reach no host, and every later change of it would depend on changes
    /// the host's other devices never get. The next session sends it first.
    fn hold_for_host(&mut self, ws: WorkspaceId, msg: &NeighbourMsg) {
        if self.hosts.is_empty()
            || self
                .host_sessions
                .keys()
                .any(|(session_ws, _)| *session_ws == ws)
        {
            return;
        }
        let (Some((channel, kind)), Ok(bytes)) =
            (retention(msg, false), postcard::to_allocvec(msg))
        else {
            return;
        };
        self.hold_backlog(ws, channel, kind, bytes);
    }

    /// Keep one message for workspace `ws`'s host until its session is up, the
    /// oldest dropped past the workspace's bounds.
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
                ws = %ws,
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
            tracing::warn!(ws = %ws, %error, "could not send the identity announcement to the workspace host");
        }
    }

    /// Re-announce every live workspace-host session so a late-joining
    /// subscriber still sees `HostReady`.
    async fn run_host_checkpoint_tick(&self) {
        for (&(ws, peer), runtime) in &self.host_sessions {
            if let Some(tx) = self.subscribers.get(&ws) {
                let _ = tx
                    .send(NetEvent::HostReady {
                        ws,
                        peer,
                        records_when_opened: runtime.session.records_when_opened(),
                    })
                    .await;
            }
        }
    }
}

async fn send_nearby_transfer_frame(
    connection: &iroh::endpoint::Connection,
    user_public_key: [u8; 32],
    frame: PersonalNearbyTransferFrame,
) -> Result<(), ()> {
    let wire = PersonalFabricWireFrame {
        user_public_key,
        payload: PersonalFabricPayload::NearbyTransfer(frame),
    };
    let bytes = postcard::to_allocvec(&wire).map_err(|_| ())?;
    if bytes.len() > crate::fabric::MAX_FABRIC_CHECKPOINT_BYTES + 128 {
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
    Serving { hosting, served }: Serving,
) {
    let (internal_tx, mut internal_rx) = mpsc::channel(256);
    let (input_tx, mut input_rx) = mpsc::channel(64);
    let connection_incarnations = Arc::new(AtomicU64::new(1));
    let router = Router::builder(endpoint.clone())
        .accept(
            SURFACE_STREAM_ALPN,
            SurfaceStreamProtocol {
                internal_tx: internal_tx.clone(),
                input_tx,
                next_connection_incarnation: Arc::clone(&connection_incarnations),
            },
        )
        .accept(tilo_unlock_protocol::UNLOCK_ALPN, DeviceUnlockProtocol)
        .accept(
            tilo_relay_protocol::transport::SIBLING_ALPN,
            device_sessions::SiblingProtocol(internal_tx.clone()),
        );
    // This device's workspace host answers here once it opens: until then its
    // handlers refuse, as a device hosting nothing does.
    let (local_host, mut protocols) = match hosting {
        Some(Hosting { host, protocols }) => (Some(host), protocols),
        None => (None, Vec::new()),
    };
    // What the session serves beside the actor's own protocols: its action
    // plane, say, on the one endpoint both reach this device at.
    protocols.extend(served);
    let router = protocols
        .into_iter()
        .fold(router, |router, (alpn, handler)| {
            router.accept(alpn, handler)
        })
        .spawn();
    let reporting_addrs = report_addrs(&endpoint, listen_addrs);
    let lanes = workspace_lane::Lanes::new(endpoint.clone(), internal_tx.clone());
    let mut state = ActorState {
        reachability,
        endpoint,
        lookup,
        host_identity,
        native_routes: native_ingress::Routes::default(),
        local_host,
        lanes,
        internal_tx,
        topics: HashMap::new(),
        subscribers: HashMap::new(),
        own_devices: HashMap::new(),
        siblings: HashMap::new(),
        sibling_dialling: HashSet::new(),
        sibling_waiting: HashMap::new(),
        sibling_hello_sent: None,
        sibling_events: None,
        hosts: HashMap::new(),
        host_sessions: HashMap::new(),
        native_links: HashMap::new(),
        members: None,
        member_scopes: HashMap::new(),
        connecting_hosts: HashSet::new(),
        content_discovery_tasks: HashMap::new(),
        agent_questions: services::approvers(),
        next_membership_epoch: 0,
        workspace_members: workspace_members::WorkspaceMembers::default(),
        recent: HashSet::new(),
        recent_order: VecDeque::new(),
        recent_input: HashMap::new(),
        governance: HashMap::new(),
        host_backlog: HashMap::new(),

        surface_connections: HashMap::new(),
        surface_routes_opening: HashMap::new(),
        surface_routes_failed: HashMap::new(),
        active_personal_transfers: HashMap::new(),
        next_personal_transfer_generation: 0,
        tally: Arc::default(),
        caps_sent: None,
    };
    let _identity_watch = state
        .host_identity
        .as_ref()
        .and_then(|identity| watch_identity_material(identity, state.internal_tx.clone()));
    let mut media_caps = crate::media_caps::changes();
    let mut upkeep = None;
    let mut host_checkpoint = None;
    let mut reported_party = None;

    loop {
        // The relays stay up while a remote party lasts, and rest a while
        // after the last one is gone (`tilo_endpoint_reachability`).
        let party = state.remote_party();
        if party != reported_party {
            reported_party = party;
            state.reachability.need(Need::Transport, party);
        }
        // Upkeep every `heartbeat_interval` while something waits for a
        // retry; a converged desktop never wakes for it.
        upkeep = next_upkeep(upkeep, state.needs_upkeep(), heartbeat_interval);
        host_checkpoint = next_upkeep(
            host_checkpoint,
            state.remote_host_sessions(),
            HOST_CHECKPOINT_INTERVAL,
        );
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

    for host in state.host_sessions.values() {
        host.pump.abort();
    }
    for dial in state.surface_routes_opening.values() {
        dial.abort();
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
/// carry and `ShellNetwork::dial_addr` (the shell's) reads: a
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

/// An endpoint for a test of the network's own peers: on loopback, with no
/// relay, no address directory and no gateway port mapping, so its peers
/// reach each other directly and a run never depends on public relays, DNS
/// or the local network being up and quick. `key`
/// fixes its identity; `None` makes a fresh one.
#[cfg(any(test, feature = "test-support"))]
#[expect(
    clippy::expect_used,
    reason = "a test without its loopback endpoint has nothing to run on"
)]
pub async fn loopback_endpoint(
    key: Option<iroh::SecretKey>,
) -> (iroh::Endpoint, iroh::address_lookup::memory::MemoryLookup) {
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .portmapper_config(iroh::endpoint::PortmapperConfig::Disabled)
        .address_lookup(lookup.clone())
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("a loopback address");
    let builder = match key {
        Some(key) => builder.secret_key(key),
        None => builder,
    };
    (
        builder.bind().await.expect("bind a loopback endpoint"),
        lookup,
    )
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

/// Publish `msg` on every session of `ws` with its workspace hosts. Each
/// session's client enqueues it into its already authenticated QUIC pump at
/// once: none waits for another, nor for any other workspace.
fn publish(
    host_sessions: &HashMap<(WorkspaceId, EndpointId), HostRuntime>,
    ws: WorkspaceId,
    msg: &NeighbourMsg,
) {
    let mut sessions = host_sessions
        .iter()
        .filter(|((session_ws, _), _)| *session_ws == ws)
        .peekable();
    // Live traffic (pointers, carets, presence) is for whoever hears it now:
    // with no session up there is no one, and nothing keeps it.
    if sessions.peek().is_none() {
        return;
    }
    let Some((bytes, retention)) = encode_for_hosts(ws, msg) else {
        return;
    };
    for (_, runtime) in sessions {
        send_encoded(ws, &runtime.session, bytes.clone(), retention);
    }
}

/// What a host keeps a message as: a checkpoint or delta on its channel, or
/// live traffic it only hands on (`None`).
type HostRetention = Option<([u8; 32], Retention)>;

/// `msg` encoded for a host, with what the host keeps it as.
fn encode_for_hosts(ws: WorkspaceId, msg: &NeighbourMsg) -> Option<(Vec<u8>, HostRetention)> {
    match postcard::to_allocvec(msg) {
        Ok(bytes) => Some((bytes, retention(msg, false))),
        Err(error) => {
            tracing::warn!(ws = %ws, %error, "collaboration message encode failed");
            None
        }
    }
}

/// Send encoded `bytes` on `session` as `retention` says.
fn send_encoded(
    ws: WorkspaceId,
    session: &RelayCoeditSession,
    bytes: Vec<u8>,
    retention: HostRetention,
) {
    let result = match retention {
        Some((channel, Retention::Checkpoint)) => session.publish_checkpoint(channel, bytes),
        Some((channel, Retention::Delta)) => session.publish_retained_delta(channel, bytes),
        None => session.publish_presence(bytes),
    };
    if let Err(error) = result {
        tracing::warn!(ws = %ws, %error, "workspace host publish failed");
    }
}

/// Channel governance kept per workspace for the host sessions that start later.
const MAX_REMEMBERED_GOVERNANCE: usize = 512;

/// Bounds of what one workspace holds for its host while no session is up.
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
    if !native_ingress::is_channel_governance(msg) {
        return;
    }
    let (Some((channel, Retention::Checkpoint)), Ok(bytes)) =
        (retention(msg, false), postcard::to_allocvec(msg))
    else {
        return;
    };
    let workspace = remembered.entry(ws).or_default();
    workspace.retain(|(kept, _)| *kept != channel);
    if workspace.len() >= MAX_REMEMBERED_GOVERNANCE {
        workspace.pop_front();
    }
    workspace.push_back((channel, bytes));
}

fn message_fingerprint(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// Whether `msg` is said again byte for byte on purpose, so that every saying of it is news. A
/// device says which windows of another it views every few seconds for as long as it views them,
/// and the device hosting them stops naming a viewer it no longer hears: an ask dropped as a
/// repeat of the first would leave the viewer unnamed from then on. It says what the viewer wants
/// now, so a copy that came over another route changes nothing.
const fn said_again_on_purpose(msg: &NeighbourMsg) -> bool {
    matches!(msg, NeighbourMsg::SurfaceViewRequest { .. })
}

/// Remote input is not deduplicated for good: a second click or key press is
/// byte-identical to the first. Only a copy that another host session
/// delivers within [`INPUT_COPY`] (the same event over another route to this
/// workspace) is dropped; one session delivers each event once, in order, so
/// identical events over it (a doubled letter in one batch) are all new.
/// The tally name of a host session's event.
fn host_event_kind(event: &HostEvent) -> &'static str {
    match event {
        HostEvent::Message { msg, .. } => msg.kind(),
        HostEvent::Ready => "HostReady",
        HostEvent::RecordOpened { .. } => "HostRecordOpened",
        HostEvent::Member { .. } => "HostMember",
        HostEvent::Closed => "HostClosed",
    }
}

/// The tally name of a frame a workspace's link admitted.
fn heard_kind(input: &native_ingress::Input) -> &'static str {
    match input.message() {
        NeighbourMsg::AppDocument { envelope, .. } if envelope.encryption.is_some() => {
            if input.retained() {
                "HostSealedReplay"
            } else {
                "HostSealedLive"
            }
        }
        NeighbourMsg::SyncResp { .. } => "HostGovernance",
        _ => "HostNativeInput",
    }
}

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
            let discovery_key = Arc::clone(&identity.signer);
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
                        tracing::warn!(ws = %ws, "workspace identity has an invalid revocation entry");
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
                            discovery_key.clone(),
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
                            tracing::debug!(%error, ws = %ws, "relay content discovery is not ready");
                            retry = retry.saturating_mul(2).min(Duration::from_secs(10));
                        }
                        Err(_) => {
                            tracing::debug!(ws = %ws, "relay content discovery timed out");
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
        let key = Arc::clone(&identity.signer);
        let (device_cert, revocation_entries) = identity.material_for(ws);
        // A member's device reaches a workspace host over the v2 stream; a
        // device that can act as a host itself uses the v1 stream.
        let connect_v2 = !device_cert.capabilities.can_act_as_host;
        let mut revocations = RevocationList::new();
        for entry in revocation_entries {
            if revocations.insert(entry).is_err() {
                tracing::warn!(ws = %ws, "workspace identity has an invalid revocation entry");
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
                        key,
                        revocations,
                    ),
                )
                .await
                .map_err(|_| "workspace host connection timed out".to_string())
                .and_then(|result| result.map_err(|error| relay_error(&host, &error)))
                .and_then(|client| {
                    RelayCoeditSession::join(client, ws, identity.device_public_key())
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
                if connect_v2 {
                    RelayClient::connect_v2_to_enrolled_home_peer(
                        endpoint,
                        addr.clone(),
                        expectation.user_public_key,
                        expectation.device_public_key,
                        device_cert,
                        key,
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
                        key,
                        revocations,
                    )
                    .await
                }
            })
            .await
            .map_err(|_| "workspace host connection timed out".to_string())
            .and_then(|result| result.map_err(|error| relay_error(&host, &error)))
            .and_then(|client| {
                RelayCoeditSession::join(client, ws, identity.device_public_key())
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
    if cfg!(any(test, feature = "test-support"))
        && std::env::var_os("TILO_FS_CONTENT_PEER_PUBLISHER").is_none()
    {
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

/// The event a host's `inbound` frame is to the actor, `None` for one that
/// is dropped: `replay` is whether the session was replaying when it came.
fn host_event(
    inbound: HostInbound,
    ws: WorkspaceId,
    host: EndpointId,
    replaying: &std::sync::atomic::AtomicBool,
    replay: bool,
) -> Option<HostEvent> {
    use std::sync::atomic::Ordering;
    Some(match inbound {
        HostInbound::Ready { .. } => {
            replaying.store(false, Ordering::Release);
            HostEvent::Ready
        }
        HostInbound::Member { device, active } => HostEvent::Member { device, active },
        // A record this device opened: the host sent what it holds of it, and
        // how much. The records are the workspace's state, applied as they
        // come; the count is what says a record the workspace has none of is
        // new here.
        HostInbound::RecordOpened { channel, frames } => {
            HostEvent::RecordOpened { channel, frames }
        }
        // Nothing here holds these: a call's audio is the session's, carried
        // by the relay session the call itself holds, and a host keeps what a
        // link's store kept.
        HostInbound::Voice(_) | HostInbound::Committed { .. } => return None,
        HostInbound::CommitRefused { reason, .. } => {
            tracing::warn!(ws = %ws, host = %host, %reason, "the host did not keep a sealed write");
            return None;
        }
        HostInbound::RemoteAction { from, frame } => {
            let Ok(author) = EndpointId::from_bytes(&from) else {
                return None;
            };
            HostEvent::Message {
                author,
                // Never deduplicated; see `handle_host_event`.
                fingerprint: [0; 32],
                replayed: false,
                msg: NeighbourMsg::RemoteAction { frame },
            }
        }
        HostInbound::RecordContent { from, frame } => record_content_event(&from, &frame)?,
        HostInbound::Delta { author, data }
        | HostInbound::Presence {
            author,
            payload: data,
        } => {
            let Ok(author) = EndpointId::from_bytes(&author) else {
                return None;
            };
            let fingerprint = message_fingerprint(&data);
            let Ok(msg) = postcard::from_bytes(&data) else {
                return None;
            };
            HostEvent::Message {
                author,
                fingerprint,
                replayed: replay,
                msg,
            }
        }
    })
}

/// Whether `data`, a delta this device is receiving, is of a record the
/// device opened ([`RelayCoeditSession::open_record`]): the host sends it in
/// the record's order, its history first, which a device that restarts
/// reads its records from. Each write is decided by its place in that
/// order, never by when it arrived.
fn opened_record(data: &[u8], session: &RelayCoeditSession) -> bool {
    tilo_relay_session::retention_metadata(data)
        .is_some_and(|(channel, _)| session.opened_record(&channel))
}

fn spawn_host_pump(
    ws: WorkspaceId,
    host: EndpointId,
    session: Arc<RelayCoeditSession>,
    replaying: Arc<std::sync::atomic::AtomicBool>,
    tx: mpsc::Sender<InternalEvent>,
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
                // remote host is kept up, so a device hosting its workspaces with
                // nobody connected wakes for nothing.
                alive = host_keepalive(&session, &mut pinged), if keepalive => {
                    if !alive {
                        tracing::warn!(ws = %ws, host = %host, "workspace host stopped answering; reconnecting");
                        break;
                    }
                    continue;
                }
            };
            let Some(inbound) = inbound else {
                break;
            };
            let replay = replaying.load(Ordering::Acquire)
                || matches!(&inbound, HostInbound::Delta { data, .. } if opened_record(data, &session));
            let Some(event) = host_event(inbound, ws, host, &replaying, replay) else {
                continue;
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

/// Run a workspace's link and report what it brings: each frame it admits
/// into the workspace's owner, its catch-up, and its end.
fn spawn_direct_pump(
    ws: WorkspaceId,
    linked: tilo_peer_state::owner::session::Linked,
    link: Arc<tilo_peer_state::direct::DirectSync>,
    tx: mpsc::Sender<InternalEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = link.reader();
        let mut caught_up = link.caught_up();
        let running = linked.run();
        tokio::pin!(running);
        let waiting = async move {
            loop {
                if *caught_up.borrow_and_update() {
                    return;
                }
                if caught_up.changed().await.is_err() {
                    return;
                }
            }
        };
        tokio::pin!(waiting);
        let mut announced = false;
        let mut deleted = false;
        loop {
            // The catch-up's frames are told before it is said to be over: a channel service
            // founds no group before then.
            tokio::select! {
                biased;
                // A reader that fell behind the link takes what it missed
                // from the workspace's history before it reads on.
                received = reader.recv() => match received {
                    Some(heard) => {
                        if tx.send(InternalEvent::NativeHeard { ws, heard }).await.is_err() {
                            return;
                        }
                    }
                    None => break,
                },
                result = &mut running => {
                    if let Err(reason) = result {
                        deleted = reason == tilo_relay_protocol::refusal::WORKSPACE_DELETED;
                        tracing::debug!(ws = %ws, %reason, "a workspace's link ended");
                    }
                    break;
                }
                () = &mut waiting, if !announced => {
                    announced = true;
                    if tx.send(InternalEvent::NativeReady { ws }).await.is_err() {
                        return;
                    }
                }
            }
        }
        let _ = tx
            .send(InternalEvent::NativeLinkClosed { ws, deleted })
            .await;
    })
}

/// A piece of a record's content, or word that a device keeps none, a
/// workspace host handed this device from `from`; `None` for anything else.
/// What another device sent this one alone through the host: a piece of a
/// record's content, or a window's control message from a device that has no
/// direct route here.
fn record_content_event(from: &[u8; 32], frame: &[u8]) -> Option<HostEvent> {
    let author = EndpointId::from_bytes(from).ok()?;
    let msg = postcard::from_bytes(frame).ok()?;
    let fingerprint = match msg {
        NeighbourMsg::RecordContentPiece { .. } | NeighbourMsg::RecordContentLacks { .. } => {
            message_fingerprint(frame)
        }
        // A viewer's input repeats itself (the same key pressed twice): it is
        // never deduplicated, as a remote action is not.
        NeighbourMsg::RemoteInput { .. }
        | NeighbourMsg::SurfaceKeyframeRequest { .. }
        | NeighbourMsg::SurfaceViewRequest { .. }
        | NeighbourMsg::SurfaceStreamStart { .. }
        | NeighbourMsg::SurfaceStreamEnd { .. } => [0; 32],
        _ => return None,
    };
    Some(HostEvent::Message {
        author,
        fingerprint,
        replayed: false,
        msg,
    })
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "a test's device keeps its owner and its links for the whole run"
)]
mod member_apps_tests;
