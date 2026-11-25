//! The iroh actor for workspace collaboration.
//!
//! Direct paths, NAT traversal and stateless relay fallback are selected by
//! iroh. Tilo only names authenticated endpoint ids and workspaces. A
//! workspace's records travel the device link its host serves
//! ([`native_ingress`]): a device reaches the devices that hold a workspace
//! through its host, each device serving its own ([`own_service`]). Two of the
//! person's own devices keep one session of their own between them
//! ([`device_sessions`]), which carries what is for the person's devices
//! alone: their device fabric, codec adverts and their workspace list. Only
//! what is for one exact device of a workspace (input,
//! window streams, remote actions) goes directly to it, on the
//! authenticated device action service.
//! The actor is the desktop's transport, below the compositor: what arrives
//! on it that is the desktop's business goes to the services the session
//! installs ([`services`]), so a compositor change does not rebuild it and
//! its tests run on their own.

pub mod behaviour;
mod i18n {
    ui::embedded_catalog!(pub(crate) fn, ["en-US", "de-DE"]);
}
mod device_sessions;
pub mod fabric;
mod fabric_link;
pub mod native_ingress;
pub mod own_service;
pub mod services;
#[cfg(any(test, feature = "test-support"))]
pub mod siblings;
pub mod workspace_sync;

/// The workspace a message has none for.
pub const DEFAULT_WS_TOPIC: tilo_desktop_model::WorkspaceId =
    tilo_desktop_model::WorkspaceId::nil();

mod workspace_members;
mod workspace_worker;
#[cfg(test)]
mod workspace_worker_tests;
use workspace_members::Admission;
pub use workspace_members::RosterDevice;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::address_lookup::memory::MemoryLookup;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_user_identity::DeviceCert;
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
/// Largest message the direct control route reads: a window's control
/// messages and a record's content piece are far smaller.
const MAX_SURFACE_FRAME_BYTES: usize = 64 * 1024 * 1024;
const SURFACE_STREAM_ALPN: &[u8] = b"tilo-surface-stream/1";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SurfaceWireFrame {
    workspace: WorkspaceId,
    message: NeighbourMsg,
}

impl SurfaceWireFrame {
    /// The frame `bytes` holds, its message read as [`NeighbourMsg::decode`] reads one, so a
    /// device one peer version behind is heard too.
    fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        let (workspace, message) = postcard::take_from_bytes(bytes)?;
        Ok(Self {
            workspace,
            message: NeighbourMsg::decode(message)?,
        })
    }
}

/// Versioned private-device traffic. It travels directly between two of
/// the person's devices, each frame on a bidirectional stream of the
/// connection their session runs on ([`device_sessions`],
/// `read_personal_fabric`), and leaves the legacy Postcard enum untouched.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PersonalFabricPayload {
    /// That the sending device is there: sent when its session with another device of the
    /// person's starts, and never kept.
    Here,
    /// The copies the sending device offers, as it offers them now: sent when
    /// its session with a sibling starts and whenever they change.
    Clipboard(Vec<crate::fabric::ClipboardEntry>),
    /// Ephemeral, exact-target command encoded independently of snapshot
    /// schema so it is never persisted or relayed through workspace history.
    Action(Vec<u8>),
    /// One signed mutation of a record in the person's own store. Its scope is
    /// implied by the route: the fabric only ever joins one person's devices.
    /// `document_id` is the sealed wire name, as on a workspace topic.
    Document {
        app_id: String,
        document_id: String,
        envelope: Box<collab_types::SignedOp>,
    },
    /// A device of the person asking for content one of `app_id`'s person
    /// records references. Its scope is the person's, implied by the route.
    DocumentContentWant { app_id: String, hash: String },
    /// One bounded piece of that content, for the device that asked alone.
    DocumentContentPiece {
        app_id: String,
        hash: String,
        total: u64,
        offset: u64,
        bytes: Vec<u8>,
    },
    /// Word to the device that asked ([`Self::DocumentContentWant`]) that this
    /// one keeps none of that content.
    DocumentContentLacks { app_id: String, hash: String },
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PersonalFabricWireFrame {
    user_public_key: [u8; 32],
    payload: PersonalFabricPayload,
}

pub use tilo_desktop_model::WorkspaceId;
pub type PeerId = EndpointId;
pub use tilo_identity::identity_store::DeviceIdentity;

#[derive(Debug)]
pub enum NetCmd {
    /// Ask the host of the joined workspace on `ws` to replay what it retains:
    /// something an app message let go earlier waited for has arrived, and
    /// the message is decided again when it comes back.
    NativeReplay {
        ws: WorkspaceId,
    },
    /// Ask the holders of `ws` for the record on `channel` (a retained route's
    /// channel, `neighbour_wire::app_document_group`): what they hold of it,
    /// and their changes to it from then on. A workspace that sends records
    /// when they are opened sends nothing of a record before this.
    OpenDocument {
        ws: WorkspaceId,
        channel: [u8; 32],
    },
    /// Hold an enrolled workspace's relay session over the iroh endpoint its
    /// hint names.
    NativeConnect(native_ingress::Connect),
    /// This device's own workspace service opened, or failed to open
    /// ([`own_service::OwnService::failed_to_open`]): join the workspaces it
    /// holds that wait for it, or say they cannot be served here.
    OwnServiceSettled,
    /// The person asked to dial the holders of workspace `ws` again now.
    RedialWorkspace {
        ws: WorkspaceId,
    },
    NativePublish {
        ws: WorkspaceId,
    },
    /// What this device has received from its peers, by kind.
    WireTally(tokio::sync::oneshot::Sender<Arc<WireTally>>),
    /// Take part in workspace `ws`: its events reach `on_event`. The workspace's state travels its
    /// device link; this actor carries what is the live network's.
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
    /// One of the person's own devices, where it is reached: the session
    /// between the two is kept up ([`device_sessions`]).
    Connect {
        addr: EndpointAddr,
    },
    /// Where an always-on Tilo host is reached, which adds persistence and
    /// offline delivery while the person's devices are away.
    ConnectHost {
        addr: EndpointAddr,
    },
}

/// A piece of a record's content on its way to the device of the workspace that
/// asked: over the direct peer network when that device is a live member of the
/// workspace, else through the workspace's holders, which hand it to that
/// device alone and keep nothing.
#[derive(Debug)]
pub struct RecordContentDelivery {
    pub ws: WorkspaceId,
    pub target: Option<EndpointId>,
    pub msg: NeighbourMsg,
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
    /// An authenticated durable host finished subscribing to this workspace.
    /// Distinct from a direct peer arrival: the shell uses this edge to seed
    /// transport-level checkpoints for future first-time/offline devices.
    /// `documents_when_opened`: this host sends a device only the workspace's
    /// governance, its shared desktop and the heads of its chained records,
    /// and every other record once the device opens it, so a record that is not here is not new
    /// until the host answered it holds none of it.
    HostReady {
        ws: WorkspaceId,
        peer: PeerId,
        documents_when_opened: bool,
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
            Self::HostReady { .. } => "HostReady",
            Self::Msg { msg, .. } | Self::OwnReplay { msg, .. } | Self::SiblingMsg { msg, .. } => {
                msg.kind()
            }
            Self::PersonalFabric { .. } => "PersonalFabric",
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
        heard: Box<tilo_peer_state::direct::Heard>,
    },
    /// A workspace's link has the host's retained history in hand.
    NativeReady { ws: WorkspaceId },
    /// The device a workspace's link reaches answered this device's writes.
    NativeAnswered {
        ws: WorkspaceId,
        answers: Vec<tilo_peer_state::direct::Answered>,
    },
    /// This device let answered writes of workspace `ws` go from its ownership: what waited behind
    /// them goes now.
    NativeRetired { ws: WorkspaceId },
    /// A workspace's link ended; `deleted` when its host said the
    /// workspace's owner deleted it.
    NativeLinkClosed { ws: WorkspaceId, deleted: bool },
    /// A workspace's hold failed to dial `via`, one of its holders.
    NativeDialFailed {
        ws: WorkspaceId,
        via: Box<tilo_workspace_join::TransportHint>,
        /// Why the holder turned this device away, typed: a workspace its
        /// owner deleted is one this device forgets, not one to dial again.
        reason: tilo_server_protocol::refusal::Refusal,
    },
    /// A workspace's hold dials none of its holders now, for `cause`.
    NativeCannotDial {
        ws: WorkspaceId,
        cause: tilo_peer_state::owner::session::CannotDial,
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
    /// A direct send queued on a workspace's worker went, or did not.
    DirectSent {
        ws: WorkspaceId,
        outcome: Box<workspace_worker::DirectOutcome>,
    },
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

/// Another of the person's devices asking for this one to enrol its boot key.
/// The peer is the device its connection names: its fleet key.
#[derive(Clone, Debug)]
struct DeviceEnrolProtocol;

impl ProtocolHandler for DeviceEnrolProtocol {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        services::device_enrol(connection);
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
                let Ok(frame) = SurfaceWireFrame::decode(&bytes) else {
                    tracing::warn!(peer = %peer, "invalid dedicated surface frame");
                    continue;
                };
                if !matches!(
                    frame.message,
                    NeighbourMsg::PermissionGrant { .. }
                        | NeighbourMsg::RemoteAction { .. }
                        | NeighbourMsg::DocumentContentPiece { .. }
                        | NeighbourMsg::DocumentContentLacks { .. }
                        | NeighbourMsg::DocumentContentRefused { .. }
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
            msg: NeighbourMsg::RemoteAction { .. }
                | NeighbourMsg::DocumentContentPiece { .. }
                | NeighbourMsg::DocumentContentLacks { .. }
                | NeighbourMsg::DocumentContentRefused { .. }
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
                .read_to_end(crate::fabric::MAX_FABRIC_FRAME_BYTES + 128)
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
/// or host, enrols a device, administers a server workspace, joins a call or
/// sends to a named one of the person's devices. `None` for everything a
/// lone desktop does by itself: joining and publishing to a topic nobody
/// else is on (its workspaces, its fabric broadcast) sends nothing, and a lone
/// desktop does that on every local change.
fn remote_party_command(command: &NetCmd) -> Option<&'static str> {
    Some(match command {
        NetCmd::Connect { .. } => "dialling a peer",
        NetCmd::ConnectHost { .. } | NetCmd::NativeConnect(..) => "dialling a host",
        NetCmd::PublishPersonalFabricTarget { .. } => "sending to one of the person's devices",
        _ => return None,
    })
}

/// Why `event` means a remote party reached this device (on a path that
/// needed no server, such as the local network), so it should be reachable
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
    /// Spawn the actor for `endpoint`, serving this device's workspace service
    /// (`serving_own`) and `served`: protocol handlers the session's own features
    /// answer on, beside the actor's, such as a device's action service. Each
    /// names its own ALPN and serves it while the actor is up; one of `served`
    /// replaces the service's handler of the same ALPN.
    pub fn spawn_serving(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        host_identity: Option<DeviceIdentity>,
        listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
        reachability: Reachability,
        serving_own: Option<OwnServing>,
        served: crate::own_service::Protocols,
    ) -> (mpsc::Sender<NetCmd>, tokio::task::JoinHandle<()>) {
        Self::spawn_with_heartbeat(
            endpoint,
            lookup,
            host_identity,
            listen_addrs,
            HEARTBEAT_INTERVAL,
            reachability,
            serving_own,
            served,
        )
    }

    /// [`Self::spawn_serving`] with an explicit liveness and host-retry
    /// cadence, so a test can observe several heartbeats without waiting out
    /// the production interval.
    fn spawn_with_heartbeat(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        host_identity: Option<DeviceIdentity>,
        listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
        heartbeat: Duration,
        reachability: Reachability,
        serving_own: Option<OwnServing>,
        served: crate::own_service::Protocols,
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
            Serving {
                serving_own,
                served,
            },
        ));
        (cmd_tx, task)
    }
}

/// What this endpoint serves beside the actor's own protocols: this
/// device's own host's handlers, when it hosts the workspaces placed on it,
/// and the session's own (its action service, say) — all on the one endpoint
/// both reach this device at.
struct Serving {
    serving_own: Option<OwnServing>,
    served: crate::own_service::Protocols,
}

/// This device's own workspace service, for the actor: the handle the native
/// owner joins the workspaces it holds through, and the handlers serving it
/// on the router ([`crate::own_service`]).
pub struct OwnServing {
    pub service: crate::own_service::OwnService,
    pub protocols: crate::own_service::Protocols,
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
    host_identity: Option<DeviceIdentity>,
    native_routes: native_ingress::Routes,
    /// This device's workspace host, through which the native owner joins the
    /// workspaces hosted here.
    own_service: Option<crate::own_service::OwnService>,
    /// Governance invocations waiting for their workspace's relay to answer.
    /// The endpoint's relays, held back until a remote party appears.
    reachability: Reachability,
    internal_tx: mpsc::Sender<InternalEvent>,
    /// Each workspace's direct sends, so one workspace's slow devices hold up no
    /// other workspace ([`workspace_worker`]).
    workers: workspace_worker::Workers,
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
    /// Each joined workspace's link, on the direct connection.
    native_links: HashMap<WorkspaceId, native_ingress::NativeLink>,
    next_membership_epoch: u64,
    /// Each workspace's admitted roster ([`NetCmd::SetWorkspaceRoster`]), and the
    /// window-stream connections peers opened to this device, bound to it.
    workspace_members: workspace_members::WorkspaceMembers,
    recent: HashSet<(WorkspaceId, [u8; 32])>,
    recent_order: VecDeque<(WorkspaceId, [u8; 32])>,
    /// Channel governance this device published to each workspace, by retention
    /// channel (see [`remember_governance`]).
    governance: HashMap<WorkspaceId, GovernanceLog>,

    /// Long-lived direct control connections. One unidirectional QUIC stream
    /// per message avoids head-of-line blocking between messages.
    surface_connections: HashMap<EndpointId, iroh::endpoint::Connection>,
    /// Every application message peers delivered here (see [`WireTally`]).
    tally: Arc<WireTally>,
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
            Some(NetCmd::OwnServiceSettled) => self.retry_native_routes(),
            Some(NetCmd::RedialWorkspace { ws }) => self.redial_native_route(ws),
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
                self.native_routes.remove(ws);
                self.native_links.remove(&ws);
                self.subscribers.remove(&ws);
                self.set_workspace_roster(ws, Vec::new(), false);
                self.workers.close(ws);
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
            Some(NetCmd::OpenDocument { ws, channel }) => {
                // A record is opened over the workspace's link, which holds it as it lands: a
                // desktop takes the records its workspace's store keeps eagerly, so there is
                // nothing to ask a session for.
                tracing::debug!(
                    ws = %ws,
                    channel = %hex::encode(channel),
                    "a record is opened over the workspace's link; nothing is asked of a host session"
                );
            }
            Some(NetCmd::Publish { ws, msg }) => {
                remember_governance(&mut self.governance, ws, &msg);
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
            Some(NetCmd::Connect { addr }) => {
                if addr.id == self.endpoint.id() {
                    return true;
                }
                self.lookup.add_endpoint_info(addr.clone());
                self.own_devices.insert(addr.id, addr);
                self.dial_siblings();
            }
            Some(NetCmd::ConnectHost { addr }) => {
                self.lookup.add_endpoint_info(addr);
            }
            None => return false,
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
        if bytes.len() > crate::fabric::MAX_FABRIC_FRAME_BYTES + 128 {
            tracing::warn!(
                bytes = bytes.len(),
                "personal fabric frame exceeds wire limit"
            );
            return;
        }
        // A device whose session is down misses the frame: when its session
        // is back, each side says itself to the other again.
        for (peer, sibling) in &self.siblings {
            if exact_target.is_none_or(|target| *peer == target) && sibling.proved(&user_public_key)
            {
                sibling.fabric.send(bytes.clone());
            }
        }
    }

    fn publish_record_content(&mut self, delivery: RecordContentDelivery) {
        let RecordContentDelivery { ws, target, msg } = delivery;
        if !matches!(
            msg,
            NeighbourMsg::DocumentContentPiece { .. }
                | NeighbourMsg::DocumentContentLacks { .. }
                | NeighbourMsg::DocumentContentRefused { .. }
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
            && link.scope.host().host.is_of_device(target.as_bytes())
        {
            if let Err(error) = link.link.send(msg) {
                tracing::debug!(ws = %ws, %error, "a record's content waits for the workspace's next link");
            }
            return;
        }
        let msg = match target {
            Some(target) if self.is_surface_member(ws, target) => {
                match self.publish_direct(ws, target, msg) {
                    // How it went comes back from the workspace's worker.
                    Ok(()) => return,
                    Err(msg) => *msg,
                }
            }
            _ => msg,
        };
        if !Self::relay_to_device(ws, target, &msg) {
            tracing::debug!(ws = %ws, "record content has no route to the device that asked");
        }
    }

    /// A record's content, or a window's control message, that cannot go directly: this device
    /// has no session with a workspace host to carry it, and the workspace's own host hands a
    /// member's asks on over the link each device holds to it. Nothing goes this way.
    fn relay_to_device(_ws: WorkspaceId, _target: Option<EndpointId>, _msg: &NeighbourMsg) -> bool {
        false
    }

    /// Queue one message for exactly `target` on the authenticated direct
    /// surface/control peer network, on the workspace's worker: the peer must be live on
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
            NeighbourMsg::DocumentContentPiece { .. }
                | NeighbourMsg::DocumentContentLacks { .. }
                | NeighbourMsg::DocumentContentRefused { .. }
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
        let direct = workspace_worker::Direct {
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
        self.workers.queue_direct(ws, direct).inspect_err(|_| {
            tracing::warn!(ws = %ws, peer = %target, "workspace's remote work is backed up; not sent directly");
        })
    }

    /// A direct send queued on `ws`'s worker went (or did not): keep its
    /// connection, and send what failed the way it would have gone without
    /// a direct route.
    fn direct_sent(&mut self, ws: WorkspaceId, outcome: workspace_worker::DirectOutcome) {
        let workspace_worker::DirectOutcome {
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
            NeighbourMsg::DocumentContentPiece { .. }
            | NeighbourMsg::DocumentContentLacks { .. }
            | NeighbourMsg::DocumentContentRefused { .. } => {
                let _ = Self::relay_to_device(ws, Some(target), &msg);
            }
            _ => {}
        }
    }

    /// [`NetCmd::JoinWorkspace`] arm of [`Self::handle_command`], split out
    /// to keep that dispatcher short.
    fn handle_join_workspace(&mut self, ws: WorkspaceId, on_event: mpsc::Sender<NetEvent>) {
        // What the workspace brings reaches the subscriber from its link, the one path its state
        // travels between this device and its host.
        self.subscribers.insert(ws, on_event);
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
            Some(InternalEvent::NativeLinkConnected {
                ws,
                linked,
                scope,
                held,
            }) => self.native_link_connected(ws, *linked, scope, held),
            Some(InternalEvent::NativeHeard { ws, heard }) => {
                self.native_heard(ws, *heard).await;
            }
            Some(InternalEvent::NativeReady { ws }) => self.native_ready(ws).await,
            Some(InternalEvent::NativeAnswered { ws, answers }) => {
                self.native_answered(ws, &answers);
            }
            Some(InternalEvent::NativeRetired { ws }) => self.drive_native_publication(ws),
            Some(InternalEvent::NativeLinkClosed { ws, deleted }) => {
                self.native_link_closed(ws, deleted).await;
            }
            Some(InternalEvent::NativeDialFailed { ws, via, reason }) => {
                self.native_dial_failed(ws, &via, &reason).await;
            }
            Some(InternalEvent::NativeCannotDial { ws, cause }) => {
                self.native_cannot_dial(ws, cause);
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
        if mark_new_message(ws, fingerprint, &mut self.recent, &mut self.recent_order)
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

    /// Hand `event` to the workspace's subscriber and wake the loop for the
    /// recorded streams, which are written out of band.
    async fn publish_to_subscriber(&self, ws: WorkspaceId, event: NetEvent) {
        let Some(tx) = self.subscribers.get(&ws) else {
            return;
        };
        let _ = tx.send(event).await;
    }

    /// Whether [`Self::run_heartbeat_tick`] has anything to do: one of the
    /// person's devices without a session, or a workspace hosted elsewhere
    /// whose link is not up. A converged pair of desktops has neither, so its
    /// runtime sleeps until one appears ([`run`]).
    fn needs_upkeep(&self) -> bool {
        self.own_devices
            .keys()
            .any(|device| !self.siblings.contains_key(device))
            || self.native_routes.any_remote()
            // A workspace this device hosts is joined in process; a join not yet
            // made is retried here as a remote dial is, so nothing waits on it
            // with nothing to wake it.
            || self
                .native_routes
                .local()
                .any(|ws| !self.native_links.contains_key(&ws))
    }

    /// Whether `command` stays in this process: it goes to this device's
    /// own host, so it needs no server up.
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
    /// peer it dials, one of the person's devices, a window streamed with a peer,
    /// a workspace hosted elsewhere or a device being enrolled. A peer that
    /// left stops counting once its membership lapses.
    fn remote_party(&self) -> Option<&'static str> {
        Some(if !self.own_devices.is_empty() {
            "one of the person's devices this device dials"
        } else if !self.siblings.is_empty() {
            "one of the person's devices dialled this one"
        } else if self.native_routes.any_remote() {
            "a workspace hosted elsewhere"
        } else if !self.surface_connections.is_empty() {
            "a direct route to a peer"
        } else {
            return None;
        })
    }

    /// Retry any direct peer or workspace link not yet connected. Runs every
    /// [`HEARTBEAT_INTERVAL`] while [`Self::needs_upkeep`].
    fn run_heartbeat_tick(&mut self) {
        self.retry_native_routes();
        // A session with this device's own host loses nothing between
        // heartbeats: it publishes as the owner commits.
        let workspaces: Vec<_> = self.native_links.keys().copied().collect();
        for ws in workspaces {
            self.drive_native_publication(ws);
        }
        self.dial_siblings();
    }

    /// The person's devices hear this device's identity again when it changes
    /// (a renewed certificate, a new revocation), which the files it is read
    /// from say.
    fn identity_material_changed(&mut self) {
        self.hello_siblings_if_changed();
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the actor's state is built in one literal so every field's starting value reads together"
)]
async fn run(
    endpoint: Endpoint,
    lookup: MemoryLookup,
    host_identity: Option<DeviceIdentity>,
    listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
    mut cmd_rx: mpsc::Receiver<NetCmd>,
    heartbeat_interval: Duration,
    reachability: Reachability,
    Serving {
        serving_own,
        served,
    }: Serving,
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
        .accept(tilo_unlock_protocol::ENROL_ALPN, DeviceEnrolProtocol)
        .accept(
            tilo_server_protocol::transport::SIBLING_ALPN,
            device_sessions::SiblingProtocol(internal_tx.clone()),
        );
    // This device's workspace host answers here once it opens: until then its
    // handlers refuse, as a device serving_own nothing does.
    let (own_service, mut protocols) = match serving_own {
        Some(OwnServing { service, protocols }) => (Some(service), protocols),
        None => (None, Vec::new()),
    };
    // What the session serves beside the actor's own protocols: its action
    // peer network, say, on the one endpoint both reach this device at.
    protocols.extend(served);
    let router = protocols
        .into_iter()
        .fold(router, |router, (alpn, handler)| {
            router.accept(alpn, handler)
        })
        .spawn();
    let reporting_addrs = report_addrs(&endpoint, listen_addrs);
    let workers = workspace_worker::Workers::new(endpoint.clone(), internal_tx.clone());
    let mut state = ActorState {
        reachability,
        endpoint,
        lookup,
        host_identity,
        native_routes: native_ingress::Routes::default(),
        own_service,
        workers,
        internal_tx,
        subscribers: HashMap::new(),
        own_devices: HashMap::new(),
        siblings: HashMap::new(),
        sibling_dialling: HashSet::new(),
        sibling_waiting: HashMap::new(),
        sibling_hello_sent: None,
        sibling_events: None,
        native_links: HashMap::new(),
        next_membership_epoch: 0,
        workspace_members: workspace_members::WorkspaceMembers::default(),
        recent: HashSet::new(),
        recent_order: VecDeque::new(),
        governance: HashMap::new(),

        surface_connections: HashMap::new(),
        tally: Arc::default(),
    };
    let _identity_watch = state
        .host_identity
        .as_ref()
        .and_then(|identity| watch_identity_material(identity, state.internal_tx.clone()));
    let mut upkeep = None;
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
            () = sleep_until_lapse(upkeep) => {
                upkeep = None;
                state.run_heartbeat_tick();
            }
        }
    }

    drop(state);
    reporting_addrs.abort();
    let _ = router.shutdown().await;
}

/// Tell the actor whenever a file `identity`'s material is read from
/// changes. `None` for an identity kept in memory, or one that cannot be
/// watched: it is then announced as it was when each link came up.
fn watch_identity_material(
    identity: &DeviceIdentity,
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
        tilo_identity::provisioning::RENEWAL_CAUSES.changed();
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
/// server, no address directory and no gateway port mapping, so its peers
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

/// Channel governance kept per workspace for the host sessions that start later.
const MAX_REMEMBERED_GOVERNANCE: usize = 512;

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
    if !native_ingress::is_governance(msg) {
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
        let mut answers = link.answers();
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
                        if tx.send(InternalEvent::NativeHeard {
                            ws,
                            heard: Box::new(heard),
                        }).await.is_err() {
                            return;
                        }
                    }
                    None => break,
                },
                // What the host answered of this device's writes, gathered while more wait.
                answered = answers.recv() => match answered {
                    Ok(first) => {
                        let mut answered = vec![first];
                        while let Ok(more) = answers.try_recv() {
                            answered.push(more);
                        }
                        let event = InternalEvent::NativeAnswered { ws, answers: answered };
                        if tx.send(event).await.is_err() {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!(ws = %ws, missed, "answers to this device's writes were missed; they leave ownership at the next link");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {}
                },
                result = &mut running => {
                    if let Err(reason) = result {
                        deleted = reason == tilo_server_protocol::refusal::Refusal::WorkspaceDeleted;
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

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests;
