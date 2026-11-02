//! Who is here with this person: the session as a client of the
//! collaboration plane.
//!
//! The session takes part in the workspace each window shows. It says this
//! person's presence on the workspace's topic — that they are here, what they
//! say they are doing, how available they are, whose view they follow, where
//! their pointer is — and hears everyone else's from there, as one
//! [`PeerPresence`] under the workspace's scope. The wire is the collaboration
//! plane's own message ([`NeighbourMsg::AppPresenceV2`]), so a browser, an
//! agent and this session are present to each other alike, and the session
//! needs nothing of its own to be one of them.
//!
//! Nothing here is drawn. The roster it feeds is the desktop's
//! (`tilo_desktop::here`): the People panel, the bar's faces and the markers
//! on windows read the desktop, on this session and on every other.
//!
//! The transport is `tilo-network-actor`'s peer actor on an Iroh endpoint,
//! on the device's own collaboration key. The device certificate the actor
//! announces beside it names the device (the label everyone calls it by) and
//! the person it belongs to, so two devices of one person are one face.
//!
//! That endpoint keeps number 0's directory behind a gate and its relays
//! held back ([`tilo_endpoint_reachability`]): a desktop whose workspaces
//! have nobody else in them publishes nothing and keeps no relay, and the
//! first workspace shown with other live members in it starts both.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::fabric::{MediaContinuity, SessionFabric};
use collab_types::presence::{Availability, PointerSpace, PresenceKey};
use collab_types::{PeerPresence, PeerPresencePayload};
use iroh::{EndpointAddr, EndpointId};
use neighbour_wire::NeighbourMsg;
use tilo_desktop::chrome::WaveNotice;
use tilo_desktop::here::{Here, LiveMember, Wave};
use tilo_desktop_model::{DesktopTree, RoomId as DeskRoom, TabId, WorkspaceId as DeskWorkspace};
use tilo_desktop_surface::DesktopHandle;
use tilo_device_fabric::DeviceId;
use tilo_device_fabric::runtime::FabricRuntime;
use tilo_endpoint_reachability::{Need, Reachability};
use tilo_network_actor::{
    HostIdentity, NetCmd, NetEvent, PersonalFabricPayload, RecordContentDelivery, RosterDevice,
    WorkspaceControlRoute, WorkspaceId, native_ingress, workspace_topic_id,
};

use crate::channels::ChannelLink;
use crate::content;
use crate::device_actions::DeviceActions;
use crate::engine::Engine;

mod gate;
mod plane;

use plane::{Bound, Serving, run};

/// The app id the desktop of a workspace says its own presence under, as the
/// browser, the agents and every app reading a workspace expect it.
const DESKTOP_PRESENCE_APP: &str = "tilo-shell";

/// How often this person says again where they are, so someone whose host
/// session came up after the first word still hears them, and so a workspace never
/// mistakes nobody speaking for nobody being here.
const PRESENCE_AGAIN: Duration = Duration::from_secs(20);

/// How long redeeming an invitation to a workspace may take: the host is asked
/// three times, each exchange with its own bound.
const REDEEM_WITHIN: Duration = Duration::from_secs(120);

/// What this session says of itself here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnPresence {
    /// The person's name, as everyone here sees it.
    pub label: String,
    /// The person (user root, hex) this device is certified as.
    pub user: Option<String>,
    /// What they say they are doing, in their own words.
    pub status: Option<String>,
    /// How available they say they are.
    pub availability: Availability,
    /// The voice channel they are in here, if any: what everyone else's
    /// roster says beside their face.
    pub call: Option<ui::CallPresence>,
    /// Whose view they follow, by that member's publisher.
    pub following: Option<String>,
}

/// A call in progress in a workspace, as the people in it say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveCall {
    /// The workspace the channel is in, as its id travels.
    pub workspace: String,
    /// Chat's id of the voice channel.
    pub channel: String,
    /// What the channel is called, as the people in it say; empty when
    /// nobody named it.
    pub name: String,
    /// Who is in it, in the order they arrived.
    pub people: Vec<LiveCaller>,
}

/// Someone in a call, as their device's presence names them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveCaller {
    /// Their device's publisher.
    pub id: String,
    /// What to call the device.
    pub label: String,
    /// The person behind the device (user root, hex), once its certificate
    /// was heard.
    pub user: Option<String>,
}

impl LiveCall {
    /// `peer` is in this call, as their presence says: one more in it, and
    /// the name they give it when nobody named it first.
    fn join(&mut self, peer: &Peer, presence: &ui::CallPresence) {
        if self.name.trim().is_empty() {
            self.name.clone_from(&presence.name);
        }
        self.people.push(LiveCaller {
            id: peer.publisher.to_string(),
            label: peer.label.clone(),
            user: peer.user.clone(),
        });
    }
}

/// Someone else here, as their presence and their certificate tell it.
#[derive(Clone, Debug, PartialEq)]
struct Peer {
    /// Their device's publisher: the endpoint id every workspace member names it
    /// by.
    publisher: EndpointId,
    /// The person behind the device (user root, hex), once its certificate
    /// has been heard.
    user: Option<String>,
    /// What to call the device: its own label, from its certificate.
    label: String,
    /// What they say they are doing, in their own words.
    status: Option<String>,
    /// How available they say they are.
    availability: Availability,
    /// Whose view they follow, by that member's publisher.
    following: Option<String>,
    /// The last wave they sent at someone here.
    wave: Option<Wave>,
    /// The call they are in, as their presence says: the channel's id and
    /// what it is called.
    call: Option<ui::CallPresence>,
    /// Where their pointer is: the tab of this workspace its tile shows, and the
    /// position on that tile in logical pixels from its corner.
    pointer: Option<(u64, f64, f64)>,
    /// They said they are here and have not left: what makes them a member.
    here: bool,
}

impl Peer {
    fn new(publisher: EndpointId) -> Self {
        Self {
            publisher,
            user: None,
            label: String::new(),
            status: None,
            availability: Availability::default(),
            following: None,
            wave: None,
            call: None,
            pointer: None,
            here: false,
        }
    }

    /// What the desktop lists of them.
    fn member(&self) -> LiveMember {
        LiveMember {
            publisher: self.publisher.to_string(),
            kind: "person".into(),
            label: self.label.clone(),
            focus: None,
            activity: self.status.clone(),
            place: None,
            user: self.user.clone(),
            wave: self.wave.clone(),
            layout: None,
            following: self.following.clone(),
            availability: Some(self.availability.id().to_owned()),
            call: self.call.as_ref().map(ui::CallPresence::encode),
        }
    }
}

/// One workspace this session takes part in, and who it heard is here.
#[derive(Debug)]
struct Workspace {
    /// The workspace's topic.
    ws: WorkspaceId,
    /// The workspace, as its id travels: the uuid string every publish names.
    id: String,
    /// Everyone heard here, in the order they arrived.
    peers: Vec<Peer>,
    /// The devices a record's content of this workspace reaches this one
    /// from: those heard on its topic, and its hosts.
    content: content::Topology,
}

impl Workspace {
    /// Change what is known of `publisher`, starting their entry the first
    /// time they say anything.
    fn with(&mut self, publisher: EndpointId, change: impl FnOnce(&mut Peer)) {
        if let Some(index) = self
            .peers
            .iter()
            .position(|peer| peer.publisher == publisher)
            && let Some(peer) = self.peers.get_mut(index)
        {
            change(peer);
            return;
        }
        let mut peer = Peer::new(publisher);
        change(&mut peer);
        self.peers.push(peer);
    }

    /// They are here: this and what they say of themselves.
    fn hear(&mut self, from: EndpointId, presence: &PeerPresence) {
        let Ok(key) = postcard::from_bytes::<PresenceKey>(&presence.key) else {
            return;
        };
        match (&key, &presence.payload) {
            (PresenceKey::Workspace { id }, payload) if *id == self.id => {
                self.with(from, |peer| match payload {
                    PeerPresencePayload::Arrived => peer.here = true,
                    PeerPresencePayload::Left => peer.here = false,
                    PeerPresencePayload::Status { text, .. } => {
                        peer.status = Some(text.trim().to_owned()).filter(|text| !text.is_empty());
                    }
                    PeerPresencePayload::Availability { state, call } => {
                        peer.availability =
                            Availability::from_id(state).unwrap_or(Availability::Available);
                        peer.call = call.as_deref().and_then(ui::CallPresence::decode);
                    }
                    PeerPresencePayload::Following { target } => {
                        peer.following.clone_from(target);
                    }
                    PeerPresencePayload::Wave { seq, target } => {
                        peer.wave = Some(Wave {
                            seq: *seq,
                            target: target.clone(),
                        });
                    }
                    _ => {}
                });
            }
            (
                PresenceKey::Pointer {
                    space: PointerSpace::Tile { room, tab },
                },
                PeerPresencePayload::Pointer { x, y },
            ) if *room == self.id => {
                self.with(from, |peer| {
                    peer.pointer = Some((*tab, f64::from(*x), f64::from(*y)));
                });
            }
            _ => {}
        }
    }

    /// Where their pointer moved on the tile of a tab of this workspace.
    fn point(&mut self, from: EndpointId, key: &PresenceKey, point: (f64, f64)) {
        if let PresenceKey::Pointer {
            space: PointerSpace::Tile { room, tab },
        } = key
            && *room == self.id
        {
            self.with(from, |peer| peer.pointer = Some((*tab, point.0, point.1)));
        }
    }

    /// Their pointer left the tile it was on.
    fn clear_point(&mut self, from: EndpointId, key: &PresenceKey) {
        if let PresenceKey::Pointer {
            space: PointerSpace::Tile { .. },
        } = key
            && let Some(peer) = self.peers.iter_mut().find(|peer| peer.publisher == from)
        {
            peer.pointer = None;
        }
    }

    /// Their device's certificate arrived: it names the device and the
    /// person it belongs to. Only the sender's own, validly signed
    /// certificate says who is there: one naming another device, or signed by
    /// nobody, is refused, so nobody here can be named as somebody else.
    fn certified(&mut self, from: EndpointId, cert: &tilo_user_identity::DeviceCert) {
        if cert.device_public_key != *from.as_bytes() {
            tracing::warn!(
                peer = %from,
                device = %hex::encode(cert.device_public_key),
                "refusing a certificate that is not the sender's own device"
            );
            return;
        }
        if let Err(error) =
            tilo_user_identity::verify_device_cert_at(cert, tilo_clock::unix_now_ns())
        {
            tracing::warn!(peer = %from, %error, "refusing a device certificate");
            return;
        }
        self.with(from, |peer| {
            peer.user = Some(hex::encode(cert.user_public_key));
            let label = cert.device_label.trim();
            if !label.is_empty() {
                label.clone_into(&mut peer.label);
            }
        });
    }

    /// They left the workspace.
    fn depart(&mut self, publisher: EndpointId) {
        self.peers.retain(|peer| peer.publisher != publisher);
    }

    /// The calls in progress here, as those in them say: the people of one
    /// channel under its id and name, in the order they arrived.
    fn calls(&self) -> Vec<LiveCall> {
        let mut calls: Vec<LiveCall> = Vec::new();
        for peer in self.peers.iter().filter(|peer| peer.here) {
            let Some(presence) = &peer.call else {
                continue;
            };
            let at = calls
                .iter()
                .position(|known| known.channel == presence.channel);
            if let Some(at) = at {
                if let Some(call) = calls.get_mut(at) {
                    call.join(peer, presence);
                }
            } else {
                let mut call = LiveCall {
                    workspace: self.id.clone(),
                    channel: presence.channel.clone(),
                    name: presence.name.clone(),
                    people: Vec::new(),
                };
                call.join(peer, presence);
                calls.push(call);
            }
        }
        calls
    }

    /// Who is here, as the desktop lists them.
    fn members(&self) -> Vec<LiveMember> {
        self.peers
            .iter()
            .filter(|peer| peer.here)
            .map(Peer::member)
            .collect()
    }

    /// Everyone whose pointer is over `tab` now, by publisher.
    fn pointers(&self, tab: u64) -> Vec<(String, f64, f64)> {
        self.peers
            .iter()
            .filter(|peer| peer.here)
            .filter_map(|peer| match peer.pointer {
                Some((at, x, y)) if at == tab => Some((peer.publisher.to_string(), x, y)),
                _ => None,
            })
            .collect()
    }
}

/// The workspaces this session takes part in, and the one in front.
#[derive(Debug)]
struct Plane {
    /// This device's publisher.
    publisher: EndpointId,
    workspaces: Vec<Workspace>,
    /// The workspace in front: whose roster the desktop shows.
    active: Option<WorkspaceId>,
}

impl Plane {
    fn workspace(&mut self, ws: WorkspaceId) -> Option<&mut Workspace> {
        self.workspaces
            .iter_mut()
            .find(|workspace| workspace.ws == ws)
    }

    /// Take one event from the plane.
    fn apply(&mut self, event: NetEvent) {
        match event {
            NetEvent::Msg { ws, peer, msg, .. } => self.message(ws, peer, msg),
            NetEvent::PeerLeft { ws, peer, .. } => {
                if let Some(workspace) = self.workspace(ws) {
                    workspace.depart(peer);
                }
            }
            _ => {}
        }
    }

    /// Take one message a peer published on `ws`.
    fn message(&mut self, ws: WorkspaceId, from: EndpointId, msg: NeighbourMsg) {
        if from == self.publisher {
            return;
        }
        let Some(id) = self
            .workspaces
            .iter()
            .find(|workspace| workspace.ws == ws)
            .map(|workspace| workspace.id.clone())
        else {
            return;
        };
        let Some(workspace) = self.workspace(ws) else {
            return;
        };
        match msg {
            NeighbourMsg::AppPresenceV2 {
                app_id,
                cluster_id,
                presence,
                ..
            } if app_id == DESKTOP_PRESENCE_APP && cluster_id == id => {
                workspace.hear(from, &presence);
            }
            NeighbourMsg::PresenceMoved { key, point, .. } => {
                if let Some(key) = pointer_key(&key) {
                    workspace.point(from, &key, point);
                }
            }
            NeighbourMsg::PresenceCleared { key, .. } => {
                if let Some(key) = pointer_key(&key) {
                    workspace.clear_point(from, &key);
                }
            }
            NeighbourMsg::IdentityHello { cert, .. } => workspace.certified(from, &cert),
            _ => {}
        }
    }

    /// Everyone whose pointer is over `tab` of `workspace` now, by publisher, if
    /// this session takes part in that workspace.
    fn pointers(&self, workspace: &str, tab: u64) -> Vec<(String, f64, f64)> {
        self.workspaces
            .iter()
            .find(|held| held.id == workspace)
            .map(|held| held.pointers(tab))
            .unwrap_or_default()
    }
}

/// Where a moved pointer was, from the key a pointer message names it by:
/// the wire's own JSON of a [`PresenceKey`].
fn pointer_key(key: &str) -> Option<PresenceKey> {
    serde_json::from_str(key).ok()
}

/// One workspace's address, as a publish names it.
fn workspace_key(workspace: &str) -> Vec<u8> {
    postcard::to_stdvec(&PresenceKey::Workspace {
        id: workspace.to_owned(),
    })
    .unwrap_or_default()
}

/// One window stream's message as it reached this device: another member's
/// shared window, or a member's ask to view one of this device's.
#[derive(Clone, Debug)]
pub(crate) struct ReceivedSurface {
    /// The workspace's topic it came in on.
    pub(crate) ws: WorkspaceId,
    /// The device that published it.
    pub(crate) from: EndpointId,
    pub(crate) msg: NeighbourMsg,
}

/// What the sync thread is asked to do.
#[derive(Debug)]
pub(crate) enum Outgoing {
    /// Take part in the topic of a workspace.
    Join(WorkspaceId),
    /// Stop taking part in the topic of a workspace.
    Leave(WorkspaceId),
    /// Publish `msg` on the topic of a workspace.
    Publish {
        ws: WorkspaceId,
        msg: Box<NeighbourMsg>,
    },
    /// Send one piece of a record's content to the device of the workspace
    /// that asked for it, and nothing else.
    Content(RecordContentDelivery),
    /// Send one window's stream on the topic of the workspace it is shared in,
    /// to the members that asked to view it and no one else.
    PublishSurfaceTo {
        ws: WorkspaceId,
        msg: Box<NeighbourMsg>,
        viewers: Vec<neighbour_wire::PeerId>,
    },
    /// Say one workspace-control message to `target` alone, over the
    /// authenticated direct route: which of its windows this device shows.
    SendControl {
        ws: WorkspaceId,
        msg: Box<NeighbourMsg>,
        target: EndpointId,
    },
    /// Dial a workspace host: an always-on peer that keeps what this
    /// person's devices publish while the others are away.
    Connect(EndpointAddr),
    /// Redeem an invitation into the workspace the desk made for it.
    Enroll(Enrolling),
    /// Hear the person's other devices from now on.
    WatchSiblings,
    /// Say `msg` to each of the person's devices whose session is up.
    TellSiblings(Box<NeighbourMsg>),
    /// Dial one of the person's own devices: the session between the two
    /// carries their device fabric ([`tilo_network_actor::NetCmd::Connect`]).
    ConnectDevice(EndpointAddr),
    /// Send this device's fabric to the person's other devices, or to the one
    /// at `target`.
    Fabric {
        user_public_key: [u8; 32],
        target: Option<EndpointId>,
        payload: PersonalFabricPayload,
    },
    /// Ask for the lane this device's private texts travel on.
    PrivateMessages(tokio::sync::oneshot::Sender<tilo_network_actor::private_messages::Transport>),
    /// Ask `ws`'s hosts for the record on `channel`, which they replay a
    /// joining device only once it asks (an on-demand record, or a part of a
    /// chain behind its head).
    OpenRecord { ws: WorkspaceId, channel: [u8; 32] },
    /// Ask the host of the joined workspace on `ws` to replay what it retains:
    /// something a message let go earlier waited for has arrived, and the
    /// message is decided again when it comes back.
    Replay(WorkspaceId),
    /// Send this device's owner's outgoing custody of the workspace on `ws`
    /// to its peers: what the owner just committed leaves this device.
    PublishCustody(WorkspaceId),
    /// Say which devices workspace `ws` admits, and until when each of their
    /// certificates holds: they alone send and receive its window streams.
    Roster {
        ws: WorkspaceId,
        devices: Vec<RosterDevice>,
    },
    /// The session is going: stop the peer actor.
    Stop,
}

/// A workspace this session is joining: the invitation that admits this
/// device to its workspace, and the workspace on the desk made for it,
/// which is where what the workspace holds arrives.
#[derive(Debug)]
pub struct Enrolling {
    /// The workspace the desk made for it.
    pub workspace: DeskWorkspace,
    /// The workspace the invitation grants.
    pub workspace_id: DeskRoom,
    /// The invitation itself, verified for this person before it got here.
    pub invitation: String,
}

/// What became of an enrollment, and the workspace it was for.
#[derive(Debug)]
pub struct Enrolled {
    pub workspace: DeskWorkspace,
    pub workspace_id: DeskRoom,
    /// `Ok` once this device holds the workspace and takes part in it.
    pub result: Result<(), String>,
}

/// The session's client of the collaboration plane: the workspaces it shows,
/// this person's presence in them, and everyone else's.
pub struct Client {
    plane: RefCell<Plane>,
    own: RefCell<OwnPresence>,
    here: Here,
    /// When this person last said again where they are here.
    said_at: Cell<Instant>,
    /// The count this person's last wave carried here.
    wave_seq: Cell<u32>,
    /// The wave this person sent last, for their own row to say sent.
    sent_wave: RefCell<Option<Wave>>,
    /// The last wave at this person, for the bar to say.
    notice: RefCell<Option<WaveNotice>>,
    outgoing: tokio::sync::mpsc::UnboundedSender<Outgoing>,
    arrived: std::sync::mpsc::Receiver<NetEvent>,
    /// The window streams that reached this device: their frames, starts,
    /// ends and control, which the shares take ([`Self::take_surfaces`]).
    surfaces: RefCell<Vec<ReceivedSurface>>,
    /// What became of the enrollments asked for since the last call.
    enrolled: std::sync::mpsc::Receiver<Enrolled>,
    /// This device's workspaces, which a join enrolls into: `None` when the
    /// session keeps no state to hold them in.
    engine: Option<Arc<Engine>>,
    /// What the session's channels hear and say on this plane: the governance
    /// of the workspaces' sealed records, and the devices each workspace admits. What
    /// the engine holds, so a session that runs no channel service has none.
    link: Option<Arc<ChannelLink>>,
    /// The person this device belongs to, by their root key, as its
    /// certificate names them.
    user: Option<[u8; 32]>,
    /// The desk this session shows, whose workspaces' policies hold every
    /// member's messages to what the workspace grants them: `None` until the
    /// session hands it over ([`Self::attach_desk`]).
    desk: RefCell<Option<DesktopHandle>>,
    /// The records' content this session keeps and asks the workspaces for:
    /// `None` until the session hands its plane over
    /// ([`Self::attach_content`]).
    content: RefCell<Option<Arc<content::Plane>>>,
    /// The certificates each of the person's other devices was heard with,
    /// by its publisher.
    siblings: RefCell<std::collections::BTreeMap<EndpointId, Vec<tilo_user_identity::DeviceCert>>>,
    /// The workspace lists the person's other devices sent since the last
    /// call, each with the certificate its device was heard by.
    personal: RefCell<
        Vec<(
            tilo_user_identity::DirectoryUpdate,
            tilo_user_identity::DeviceCert,
        )>,
    >,
    /// Where this device's endpoint is reached, as other devices dial it.
    endpoint: EndpointAddr,
    /// This device's endpoint itself, once it has bound: what a call's media
    /// lanes run on, beside the plane's own traffic.
    endpoint_handle: Arc<std::sync::OnceLock<iroh::Endpoint>>,
    /// What answers a call's lanes on that endpoint: the carrier of the call
    /// this device is in, which a factory installs while it lasts.
    lane: crate::calls::LaneSlot,
    /// This device's fabric, when it is a certified device of the person's:
    /// `None` on a session that takes part as nobody.
    fabric: Option<Arc<SessionFabric>>,
    /// Where this device's actions are served, and how an app here reaches
    /// the person's other devices: `None` until the endpoint has bound.
    device_actions: Arc<std::sync::OnceLock<DeviceActions>>,
    /// This device's relays, held back until a workspace this session takes part
    /// in has someone else in it.
    reachability: Reachability,
    /// The thread running the peer actor, stopped when this drops.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let plane = self.plane.borrow();
        f.debug_struct("Client")
            .field("publisher", &plane.publisher)
            .field(
                "workspaces",
                &plane
                    .workspaces
                    .iter()
                    .map(|workspace| workspace.id.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.outgoing.send(Outgoing::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Client {
    /// Take part in the collaboration plane: bind this device's endpoint,
    /// start the peer actor, and from then on hear the workspaces the session
    /// joins.
    ///
    /// `identity` is this device's collaboration identity, on whose key the
    /// endpoint runs: with it, every workspace is told which device and which
    /// person is here. Without one — a test, a stand-in session — the
    /// session is a device nobody certified and stays anonymous in workspaces.
    ///
    /// `media` is what the person's settings say about what they play
    /// continuing on their other devices, kept current by the session's
    /// settings watch.
    ///
    /// The session's channels, when `engine` keeps them, hear the workspaces'
    /// governance on this plane and tell it which devices each workspace admits:
    /// what they say travels as the plane's own messages.
    ///
    /// # Errors
    /// The endpoint could not be bound.
    pub fn start(
        identity: Option<HostIdentity>,
        engine: Option<Arc<Engine>>,
        own: OwnPresence,
        hosts: Vec<EndpointAddr>,
        media: MediaContinuity,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<Self> {
        let (outgoing, commands) = tokio::sync::mpsc::unbounded_channel();
        let (arrivals, arrived) = std::sync::mpsc::channel();
        let (enrollments, enrolled) = std::sync::mpsc::channel();
        let (ready, bound) = std::sync::mpsc::channel();
        let user = identity
            .as_ref()
            .map(|identity| identity.device_cert.user_public_key);
        // This device's fabric before the endpoint runs, so a device that
        // proves itself at the actor's first tick is recorded with it: the
        // record it keeps is this device's, named by its certificate.
        let fabric = identity.as_ref().and_then(|identity| {
            FabricRuntime::restored(
                Some((
                    DeviceId::new(hex::encode(identity.device_cert.device_public_key)).ok()?,
                    identity.device_cert.device_label.clone(),
                )),
                None,
            )
            .ok()
            .map(|runtime| {
                SessionFabric::new(
                    runtime,
                    outgoing.clone(),
                    identity.device_cert.user_public_key,
                    media,
                )
            })
        });
        // The channels this session runs, if it runs any: their side of the
        // plane is the engine's, opened before the plane starts. The thread
        // takes its own handle, and this one tells them where the plane is.
        let link = engine.as_ref().and_then(|engine| engine.link());
        let plane_link = link.clone();
        let enrolling = engine.clone();
        let device_actions = Arc::new(std::sync::OnceLock::new());
        let serving = Arc::clone(&device_actions);
        let endpoint_handle = Arc::new(std::sync::OnceLock::new());
        let bound_endpoint = Arc::clone(&endpoint_handle);
        let lane = crate::calls::LaneSlot::default();
        let served_lane = lane.clone();
        let thread = std::thread::Builder::new()
            .name("tilo-sync".into())
            .spawn(move || {
                run(
                    identity,
                    enrolling,
                    commands,
                    arrivals,
                    enrollments,
                    ready,
                    wake,
                    Serving {
                        link: plane_link,
                        actions: serving,
                        bound: Bound {
                            endpoint: bound_endpoint,
                            lane: served_lane,
                        },
                    },
                );
            })?;
        let (publisher, endpoint, reachability) = bound
            .recv()
            .map_err(std::io::Error::other)?
            .map_err(std::io::Error::other)?;
        if let Some(link) = &link {
            link.attach(outgoing.clone());
        }
        let sync = Self {
            plane: RefCell::new(Plane {
                publisher,
                workspaces: Vec::new(),
                active: None,
            }),
            own: RefCell::new(own),
            here: Here::default(),
            said_at: Cell::new(Instant::now()),
            wave_seq: Cell::new(0),
            sent_wave: RefCell::new(None),
            notice: RefCell::new(None),
            outgoing,
            arrived,
            surfaces: RefCell::new(Vec::new()),
            enrolled,
            engine,
            link,
            user,
            desk: RefCell::new(None),
            content: RefCell::new(None),
            siblings: RefCell::new(std::collections::BTreeMap::new()),
            personal: RefCell::new(Vec::new()),
            endpoint,
            endpoint_handle,
            lane,
            fabric,
            device_actions,
            reachability,
            thread: Some(thread),
        };
        // The person's other devices are heard from the start, so a list one
        // of them published is taken even when this session shows none of
        // their workspaces yet.
        let _ = sync.outgoing.send(Outgoing::WatchSiblings);
        // The hosts next: a workspace joined before them is replayed to this
        // device when its host session comes up, never lost.
        for host in hosts {
            let _ = sync.outgoing.send(Outgoing::Connect(host));
        }
        Ok(sync)
    }

    /// This device's publisher here: the endpoint id every workspace names it by.
    #[must_use]
    pub fn publisher(&self) -> EndpointId {
        self.plane.borrow().publisher
    }

    /// The desktop's presence, shared with whatever draws who is here.
    #[must_use]
    pub fn here(&self) -> &Here {
        &self.here
    }

    /// This device's workspaces, when this session holds any.
    #[must_use]
    pub fn engine(&self) -> Option<&Arc<Engine>> {
        self.engine.as_ref()
    }

    /// Redeem `request`'s invitation off the loop: this device joins the
    /// workspace, takes in what it held before, and takes part in it. What became
    /// of it is handed back by [`Self::take_enrolled`].
    ///
    /// # Errors
    /// The plane takes no enrollment: this session holds no workspaces, or the
    /// plane has stopped.
    pub fn enroll(&self, request: Enrolling) -> Result<(), String> {
        if self.engine.is_none() {
            return Err("this session keeps no workspaces to enroll into".into());
        }
        self.outgoing
            .send(Outgoing::Enroll(request))
            .map_err(|_| "the collaboration plane is gone".to_owned())
    }

    /// Take what became of the enrollments asked for since the last call.
    pub fn take_enrolled(&self) -> Vec<Enrolled> {
        self.enrolled.try_iter().collect()
    }

    /// What this session says of itself here now.
    #[must_use]
    pub fn own(&self) -> OwnPresence {
        self.own.borrow().clone()
    }

    /// What this session says of itself: changed here, it is said again in
    /// the workspace in front. Whether it changed.
    pub fn set_own(&self, own: OwnPresence) -> bool {
        if *self.own.borrow() == own {
            return false;
        }
        *self.own.borrow_mut() = own;
        self.say_again();
        true
    }

    /// This device joined `call`, or left it (`None`): said again in the
    /// workspace in front, so everyone here reads which channel this person is
    /// in beside their face. Whether that changed anything.
    pub fn in_call(&self, call: Option<ui::CallPresence>) -> bool {
        let mut own = self.own.borrow().clone();
        if own.call == call {
            return false;
        }
        own.call = call;
        self.set_own(own)
    }

    /// Follow `tree`'s workspaces: take part in each, leave the
    /// ones gone, and remember which is in front. Whether that changed
    /// anything.
    pub fn follow_desktop(&self, tree: &DesktopTree) -> bool {
        let live: Vec<(WorkspaceId, String)> = tree
            .workspaces
            .iter()
            .filter(|workspace| !workspace.room().is_nil())
            .map(|workspace| {
                (
                    workspace_topic_id(workspace.room()),
                    workspace.room().to_string(),
                )
            })
            .collect();
        let active = tree
            .active_workspace()
            .filter(|workspace| !workspace.room().is_nil())
            .map(|workspace| workspace_topic_id(workspace.room()));
        let mut joined = Vec::new();
        let mut left = Vec::new();
        let mut changed = false;
        {
            let mut plane = self.plane.borrow_mut();
            for (ws, id) in &live {
                if plane.workspaces.iter().all(|workspace| workspace.ws != *ws) {
                    plane.workspaces.push(Workspace {
                        ws: *ws,
                        id: id.clone(),
                        peers: Vec::new(),
                        content: content::Topology::default(),
                    });
                    joined.push((*ws, id.clone()));
                }
            }
            for workspace in &plane.workspaces {
                if live.iter().all(|(ws, _)| *ws != workspace.ws) {
                    left.push(workspace.ws);
                }
            }
            plane.workspaces.retain(|held| !left.contains(&held.ws));
            if plane.active != active {
                plane.active = active;
                changed = true;
            }
        }
        for (ws, id) in joined {
            let _ = self.outgoing.send(Outgoing::Join(ws));
            self.say(ws, &id);
            changed = true;
        }
        for ws in left {
            let _ = self.outgoing.send(Outgoing::Leave(ws));
            changed = true;
        }
        self.note_reachability();
        if changed {
            self.show_here();
        }
        changed
    }

    /// Hear everything that reached the plane since the last call. Whether
    /// anything did.
    pub fn follow(&self) -> bool {
        let mut heard = false;
        while let Ok(event) = self.arrived.try_recv() {
            self.hear(event);
            heard = true;
        }
        // What arrived moved this device's fabric — a copy from another
        // device, a device that is gone: the person's other devices hear so
        // on the same pass.
        self.announce_fabric();
        if self.said_at.get().elapsed() >= PRESENCE_AGAIN {
            self.say_again();
        }
        heard
    }

    /// Take one thing the plane heard, and hand the desktop who is here now:
    /// who said they are here, what they said of themselves, where their
    /// pointer went. A session's loop calls this for everything the peer
    /// actor hands it.
    pub fn hear(&self, event: NetEvent) {
        // Every message from another member is the workspace's to admit
        // before anything here takes it: what a member the workspace's policy
        // narrowed may send is what that policy grants them, and nothing
        // else. A refused message is dropped where it arrives, with the
        // reason said here.
        if let Some(reason) = self.refusal(&event) {
            if let NetEvent::Msg { ws, peer, msg, .. } = &event {
                tracing::warn!(
                    ws,
                    peer = %peer,
                    kind = msg.kind(),
                    %reason,
                    "dropping a workspace message"
                );
            }
            return;
        }
        self.keep_personal(&event);
        // What a workspace says of its channels' governance is the session's
        // channels' to take in, wherever the message came from.
        if let (Some(link), NetEvent::Msg { msg, .. }) = (&self.link, &event) {
            link.heard(msg);
        }
        if let Some(fabric) = &self.fabric {
            match &event {
                NetEvent::Sibling {
                    peer,
                    certs,
                    revocations,
                    ..
                } => fabric.sibling(*peer, certs, revocations),
                NetEvent::SiblingLeft { peer, .. } => fabric.sibling_left(*peer),
                NetEvent::PersonalFabric { peer, payload, .. } => {
                    fabric.arrived(*peer, payload);
                }
                _ => {}
            }
        }
        // A window's stream is not presence: it goes to the shares, which
        // read it on the pass ([`Self::take_surfaces`]).
        if self.keep_surface(&event) {
            return;
        }
        // What a workspace's members say about a record's content is the
        // records' own plane's to take, and the devices it can be handed over
        // from follow them.
        self.follow_content_sources(&event);
        if self.keep_content(&event) {
            return;
        }
        self.plane.borrow_mut().apply(event);
        self.note_reachability();
        self.show_here();
        self.show_waves();
    }

    /// Hand the records' content plane its place on this client: the
    /// workspaces' topics are the records' routes, and what it asks for and
    /// answers there travels with this device's own messages. A session takes
    /// part in the plane of the client it was given ([`Self::attach_desk`]).
    pub(crate) fn attach_content(&self, content: Arc<content::Plane>) {
        content.bind(self.outgoing.clone(), self.publisher());
        self.content.replace(Some(content));
    }

    /// Keep what reached here of the devices a workspace's content can be
    /// handed over from: those that said anything on its topic, the hosts
    /// that serve it and the members they report, and those that left. A
    /// record's content follows them ([`content::Plane::sources_changed`]).
    fn follow_content_sources(&self, event: &NetEvent) {
        let Some(content) = self.content.borrow().clone() else {
            return;
        };
        let (ws, changed) = match event {
            NetEvent::Msg { ws, peer, .. } => (
                *ws,
                self.content_devices(*ws, |devices| devices.heard(*peer)),
            ),
            NetEvent::HostReady { ws, peer } => (
                *ws,
                self.content_devices(*ws, |devices| devices.host(*peer)),
            ),
            NetEvent::HostMember {
                ws,
                host,
                device,
                active,
            } => {
                let (host, device, active) = (*host, *device, *active);
                (
                    *ws,
                    self.content_devices(*ws, |devices| {
                        let served = devices.host(host);
                        let member = if active {
                            devices.heard(device)
                        } else {
                            devices.left(device)
                        };
                        served || member
                    }),
                )
            }
            NetEvent::PeerLeft { ws, peer, .. } => (
                *ws,
                self.content_devices(*ws, |devices| devices.left(*peer)),
            ),
            _ => return,
        };
        if !changed {
            return;
        }
        let sources = self
            .plane
            .borrow()
            .workspaces
            .iter()
            .find(|workspace| workspace.ws == ws)
            .map(|workspace| workspace.content.sources(self.publisher()))
            .unwrap_or_default();
        content.sources_changed(ws, sources);
    }

    /// Apply `change` to the devices this session reaches in the workspace on
    /// `ws`; whether that was news.
    fn content_devices(
        &self,
        ws: WorkspaceId,
        change: impl FnOnce(&mut content::Topology) -> bool,
    ) -> bool {
        self.plane
            .borrow_mut()
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.ws == ws)
            .is_some_and(|workspace| change(&mut workspace.content))
    }

    /// Hand `event` to the content plane when it is one of a record's content
    /// messages; whether it was, in which case nothing else here takes it.
    /// Every such message reached here through the workspace's policy, which
    /// admitted the device that sent it.
    fn keep_content(&self, event: &NetEvent) -> bool {
        let Some(content) = self.content.borrow().clone() else {
            return false;
        };
        let NetEvent::Msg { ws, peer, msg, .. } = event else {
            return false;
        };
        let workspace = self
            .plane
            .borrow()
            .workspaces
            .iter()
            .find(|held| held.ws == *ws)
            .map(|held| held.id.clone());
        let Some(workspace) = workspace else {
            return false;
        };
        content.heard(*ws, &workspace, *peer, msg)
    }

    /// Keep `event`'s message for the shares if it is one of a window's
    /// stream; whether it was.
    fn keep_surface(&self, event: &NetEvent) -> bool {
        let NetEvent::Msg { ws, peer, msg, .. } = event else {
            return false;
        };
        if !matches!(
            msg,
            NeighbourMsg::SurfaceStreamStart { .. }
                | NeighbourMsg::SurfaceFrame { .. }
                | NeighbourMsg::SurfaceStreamEnd { .. }
                | NeighbourMsg::RemoteInput { .. }
                | NeighbourMsg::SurfaceKeyframeRequest { .. }
                | NeighbourMsg::SurfaceViewRequest { .. }
        ) {
            return false;
        }
        self.surfaces.borrow_mut().push(ReceivedSurface {
            ws: *ws,
            from: *peer,
            msg: msg.clone(),
        });
        true
    }

    /// Take the window-stream messages that reached this device since the
    /// last call.
    #[must_use]
    pub(crate) fn take_surfaces(&self) -> Vec<ReceivedSurface> {
        std::mem::take(&mut *self.surfaces.borrow_mut())
    }

    /// Send one message of `surface`'s stream on the topic of the workspace it is
    /// shared in, to `viewers` alone: over the authenticated route to each
    /// of them, never fanned out to the workspace.
    pub(crate) fn publish_surface(
        &self,
        ws: WorkspaceId,
        msg: NeighbourMsg,
        viewers: &[neighbour_wire::PeerId],
    ) {
        let _ = self.outgoing.send(Outgoing::PublishSurfaceTo {
            ws,
            msg: Box::new(msg),
            viewers: viewers.to_vec(),
        });
    }

    /// Say `msg` to `target` alone, as a workspace-control message on the
    /// topic of the workspace `ws`.
    pub(crate) fn publish_control(&self, ws: WorkspaceId, msg: NeighbourMsg, target: EndpointId) {
        let _ = self.outgoing.send(Outgoing::SendControl {
            ws,
            msg: Box::new(msg),
            target,
        });
    }

    /// The topic of the workspace `workspace`, while this session takes part in it.
    #[must_use]
    pub(crate) fn workspace_topic(&self, workspace: &str) -> Option<WorkspaceId> {
        self.plane
            .borrow()
            .workspaces
            .iter()
            .find(|held| held.id == workspace)
            .map(|held| held.ws)
    }

    /// The devices heard in `workspace` beside this one, by publisher and the name
    /// each goes by: what shows which device a window of the workspace runs on.
    #[must_use]
    pub(crate) fn workspace_devices(&self, workspace: &str) -> Vec<(EndpointId, String)> {
        self.plane
            .borrow()
            .workspaces
            .iter()
            .find(|held| held.id == workspace)
            .map(|held| {
                held.peers
                    .iter()
                    .map(|peer| (peer.publisher, peer.label.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Keep what the person's other devices said of their workspaces: a
    /// device's certificates when its session comes up, and the lists it
    /// publishes with the certificate that signed them.
    fn keep_personal(&self, event: &NetEvent) {
        match event {
            NetEvent::Sibling { peer, certs, .. } => {
                self.siblings.borrow_mut().insert(*peer, certs.clone());
            }
            NetEvent::SiblingLeft { peer, .. } => {
                self.siblings.borrow_mut().remove(peer);
            }
            NetEvent::SiblingMsg {
                peer,
                msg: NeighbourMsg::PersonalWorkspaces { update, .. },
                ..
            } => {
                // The update names its author; the certificate the device was
                // heard by is the only one that says who it is, and the list
                // checks it against the person's root before it merges.
                let author = self
                    .siblings
                    .borrow()
                    .get(peer)
                    .and_then(|certs| author_of(certs, update));
                if let Some(author) = author {
                    self.personal
                        .borrow_mut()
                        .push(((**update).clone(), author));
                } else {
                    tracing::debug!(
                        device = %hex::encode(update.author_device_public_key),
                        "a device's workspaces arrived before its certificate"
                    );
                }
            }
            _ => {}
        }
    }

    /// Say this person's workspaces to their other devices: every one of
    /// them whose session is up hears the list, and the ones this device has
    /// and its sender does not are opened there.
    pub fn tell_personal(&self, update: &tilo_user_identity::DirectoryUpdate) {
        let Some(user) = self.user else {
            return;
        };
        let _ = self.outgoing.send(Outgoing::TellSiblings(Box::new(
            NeighbourMsg::PersonalWorkspaces {
                user_public_key: user,
                update: Box::new(update.clone()),
            },
        )));
    }

    /// Take the workspace lists the person's other devices sent since the
    /// last call, each with the certificate its device was heard by.
    pub fn take_personal(
        &self,
    ) -> Vec<(
        tilo_user_identity::DirectoryUpdate,
        tilo_user_identity::DeviceCert,
    )> {
        std::mem::take(&mut *self.personal.borrow_mut())
    }

    /// Tell the person's other devices what this device says of itself, when
    /// that moved since they last heard: the session's pass calls this once
    /// it offered the clipboard, so a copy leaves on the pass that made it.
    pub fn announce_fabric(&self) {
        if let Some(fabric) = &self.fabric {
            fabric.announce_if_changed();
        }
    }

    /// This device's fabric, when the person certified it: the link the
    /// session's services act on.
    #[must_use]
    pub fn fabric(&self) -> Option<tilo_device_fabric::link::SharedLink> {
        self.fabric
            .as_ref()
            .map(|fabric| Arc::clone(fabric) as tilo_device_fabric::link::SharedLink)
    }

    /// Where this device's actions are served, and how an app here runs its
    /// actions on the person's other devices: `None` on a session that takes
    /// part as nobody, which has no key of its own to ask with.
    #[must_use]
    pub(crate) fn device_actions(&self) -> Option<DeviceActions> {
        self.device_actions.get().cloned()
    }

    /// Ask the peer actor for this device's lane of private texts: the
    /// messages between people, carried by this endpoint. `None` when the
    /// actor is gone.
    #[must_use]
    pub fn private_messages(
        &self,
    ) -> Option<tokio::sync::oneshot::Receiver<tilo_network_actor::private_messages::Transport>>
    {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.outgoing.send(Outgoing::PrivateMessages(reply)).ok()?;
        Some(answer)
    }

    /// The actions one of the person's devices sent this one since the last
    /// call, for the session's pass to carry out.
    #[must_use]
    pub fn fabric_actions(&self) -> Vec<tilo_device_fabric::PersonalFabricActionEnvelope> {
        self.fabric
            .as_ref()
            .map(|fabric| fabric.take_actions())
            .unwrap_or_default()
    }

    /// Where other devices reach this one: the endpoint it bound, with the
    /// addresses it bound on.
    #[must_use]
    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.endpoint.clone()
    }

    /// This device's collaboration endpoint, once it has bound: where what a
    /// call carries runs, beside the rooms' own traffic. `None` on a client
    /// taking part in no transport.
    #[must_use]
    pub fn endpoint(&self) -> Option<iroh::Endpoint> {
        self.endpoint_handle.get().cloned()
    }

    /// Where that endpoint lands, for what starts before it has bound: a
    /// call joins the endpoint the moment the plane has it.
    #[must_use]
    pub(crate) fn endpoint_handle(&self) -> Arc<std::sync::OnceLock<iroh::Endpoint>> {
        Arc::clone(&self.endpoint_handle)
    }

    /// What answers a call's lanes on that endpoint.
    #[must_use]
    pub(crate) fn lane(&self) -> &crate::calls::LaneSlot {
        &self.lane
    }

    /// Dial one of the person's own devices, whose address this session was
    /// told: the session between the two carries their device fabric.
    pub fn connect(&self, addr: EndpointAddr) {
        // An action for this device is sent to where it was told to be,
        // without waiting for the two sessions to meet first.
        if let Some(actions) = self.device_actions.get() {
            actions.learn(addr.clone());
        }
        let _ = self.outgoing.send(Outgoing::ConnectDevice(addr));
    }

    /// Take the waves at this person heard since the last call: the bar
    /// says who waved, for a moment.
    fn show_waves(&self) {
        let heard = self.here.borrow_mut().take_waves();
        if let Some(last) = heard.last() {
            *self.notice.borrow_mut() = Some(WaveNotice {
                from: last.name.clone(),
                at: ui::time::now(),
            });
        }
    }

    /// The last wave at this person, for the bar to say.
    #[must_use]
    pub fn wave_notice(&self) -> Option<WaveNotice> {
        self.notice.borrow().clone()
    }

    /// Why what this session takes part in needs this device reachable, if
    /// it does: a workspace it shows has someone else here. A host is
    /// dialled at start and on every reconnect, and the peer actor holds
    /// the relays up for every remote party it is connected to or keeps
    /// dialling ([`tilo_endpoint_reachability::Need::Transport`]).
    fn reachability_need(&self) -> Option<&'static str> {
        let plane = self.plane.borrow();
        let own = plane.publisher;
        plane
            .workspaces
            .iter()
            .any(|held| {
                held.peers
                    .iter()
                    .any(|peer| peer.here && peer.publisher != own)
            })
            .then_some("a workspace is shown with someone else")
    }

    /// Tell the relays what this session has recorded: they stay up while a
    /// workspace it shows has someone else here, and may rest once none
    /// does ([`tilo_endpoint_reachability::REST_AFTER`]).
    fn note_reachability(&self) {
        self.reachability
            .need(Need::Session, self.reachability_need());
    }

    /// Hand the desktop this person and whoever is here with them in the
    /// workspace in front.
    fn show_here(&self) {
        let (own, members) = {
            let plane = self.plane.borrow();
            let members = plane
                .active
                .and_then(|ws| plane.workspaces.iter().find(|workspace| workspace.ws == ws))
                .map(Workspace::members)
                .unwrap_or_default();
            (self.own_member(), members)
        };
        self.here.borrow_mut().set_members(Some(own), Some(members));
    }

    /// Say this person's presence again in the workspace in front.
    fn say_again(&self) {
        let (ws, id) = {
            let plane = self.plane.borrow();
            let Some(ws) = plane.active else {
                return;
            };
            let Some(workspace) = plane.workspaces.iter().find(|workspace| workspace.ws == ws)
            else {
                return;
            };
            (ws, workspace.id.clone())
        };
        self.say(ws, &id);
        self.said_at.set(Instant::now());
    }

    /// Say all of this person's presence in the workspace `workspace`, on its topic
    /// `ws`.
    fn say(&self, ws: WorkspaceId, workspace: &str) {
        let own = self.own.borrow().clone();
        let mut payloads = vec![
            PeerPresencePayload::Arrived,
            PeerPresencePayload::Availability {
                state: own.availability.id().to_owned(),
                call: own.call.as_ref().map(ui::CallPresence::encode),
            },
        ];
        if let Some(status) = own.status {
            payloads.push(PeerPresencePayload::Status {
                text: status,
                automatic: false,
            });
        }
        if own.following.is_some() {
            payloads.push(PeerPresencePayload::Following {
                target: own.following,
            });
        }
        if let Some(wave) = self.sent_wave.borrow().clone() {
            payloads.push(PeerPresencePayload::Wave {
                seq: wave.seq,
                target: wave.target,
            });
        }
        for payload in payloads {
            let msg = NeighbourMsg::AppPresenceV2 {
                peer: self.peer_hash(),
                app_id: DESKTOP_PRESENCE_APP.into(),
                cluster_id: workspace.to_owned(),
                presence: PeerPresence {
                    key: workspace_key(workspace),
                    payload,
                },
                colour: None,
            };
            let _ = self.outgoing.send(Outgoing::Publish {
                ws,
                msg: Box::new(msg),
            });
        }
    }

    /// This device's presence handle, as a publish stamps it.
    fn peer_hash(&self) -> u64 {
        tilo_network_actor::behaviour::peer_liveness_handle(&self.plane.borrow().publisher)
    }

    /// This device's handle in a workspace, as a window's stream names it.
    #[must_use]
    pub(crate) fn peer(&self) -> neighbour_wire::PeerId {
        self.peer_hash()
    }

    /// Tell the workspace in front that this person's pointer is over `(x, y)` of
    /// the tile showing `tab`, in that tile's logical pixels.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the presence wire carries tile offsets as f32"
    )]
    pub fn pointer(&self, tab: u64, x: f64, y: f64) {
        let plane = self.plane.borrow();
        let Some(ws) = plane.active else {
            return;
        };
        let Some(workspace) = plane.workspaces.iter().find(|workspace| workspace.ws == ws) else {
            return;
        };
        let key = postcard::to_stdvec(&PresenceKey::Pointer {
            space: PointerSpace::Tile {
                room: workspace.id.clone(),
                tab,
            },
        })
        .unwrap_or_default();
        let msg = NeighbourMsg::AppPresenceV2 {
            peer: tilo_network_actor::behaviour::peer_liveness_handle(&plane.publisher),
            app_id: DESKTOP_PRESENCE_APP.into(),
            cluster_id: workspace.id.clone(),
            presence: PeerPresence {
                key,
                payload: PeerPresencePayload::Pointer {
                    x: x as f32,
                    y: y as f32,
                },
            },
            colour: None,
        };
        let _ = self.outgoing.send(Outgoing::Publish {
            ws,
            msg: Box::new(msg),
        });
    }

    /// Whether `publisher` is this person: this device, or any device of
    /// the person this session runs for.
    fn is_own_person(&self, publisher: &str) -> bool {
        let here = self.here.borrow();
        here.own_id() == Some(publisher)
            || self
                .own
                .borrow()
                .user
                .clone()
                .filter(|user| !user.is_empty())
                .is_some_and(|user| here.user_of(publisher) == Some(user.as_str()))
    }

    /// Wave at `target`, a member here by their live publisher: said in the
    /// workspace in front, where the person it is for hears it once.
    pub fn wave(&self, target: &str) {
        if target.is_empty() || self.is_own_person(target) {
            return;
        }
        let seq = self.wave_seq.get().wrapping_add(1);
        self.wave_seq.set(seq);
        let wave = Wave {
            seq,
            target: target.to_owned(),
        };
        *self.sent_wave.borrow_mut() = Some(wave.clone());
        let plane = self.plane.borrow();
        let Some(ws) = plane.active else {
            return;
        };
        let Some(workspace) = plane.workspaces.iter().find(|workspace| workspace.ws == ws) else {
            return;
        };
        let msg = NeighbourMsg::AppPresenceV2 {
            peer: tilo_network_actor::behaviour::peer_liveness_handle(&plane.publisher),
            app_id: DESKTOP_PRESENCE_APP.into(),
            cluster_id: workspace.id.clone(),
            presence: PeerPresence {
                key: workspace_key(&workspace.id),
                payload: PeerPresencePayload::Wave {
                    seq: wave.seq,
                    target: wave.target,
                },
            },
            colour: None,
        };
        let _ = self.outgoing.send(Outgoing::Publish {
            ws,
            msg: Box::new(msg),
        });
    }

    /// Everyone whose pointer is over `tab` of `workspace` now, by publisher. The
    /// desktop draws each over its own tile for that tab, above the windows.
    #[must_use]
    pub fn pointers(&self, workspace: &str, tab: u64) -> Vec<(String, f64, f64)> {
        self.plane.borrow().pointers(workspace, tab)
    }

    /// The calls in progress in every workspace this session takes part in, as
    /// those in them say: who to join, where.
    #[must_use]
    pub fn calls(&self) -> Vec<LiveCall> {
        self.plane
            .borrow()
            .workspaces
            .iter()
            .flat_map(Workspace::calls)
            .collect()
    }

    /// This session's own person, as everyone here reads them.
    fn own_member(&self) -> LiveMember {
        let own = self.own.borrow().clone();
        let publisher = self.plane.borrow().publisher.to_string();
        LiveMember {
            publisher,
            kind: "person".into(),
            label: own.label,
            focus: None,
            activity: own.status,
            place: None,
            user: own.user,
            wave: self.sent_wave.borrow().clone(),
            layout: None,
            following: own.following,
            availability: Some(own.availability.id().to_owned()),
            call: own.call.as_ref().map(ui::CallPresence::encode),
        }
    }

    /// Who is here, as the desktop lists them: `tree` places them where
    /// their focused record is shown, and `app_name` names an app.
    #[must_use]
    pub fn people(
        &self,
        tree: &DesktopTree,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<tilo_desktop::here::Person> {
        self.here.borrow().people(
            tree,
            &[] as &[(TabId, String)],
            app_name,
            ui::time::unix_now_ms(),
        )
    }

    /// Who is here, written into `tree`'s active workspace as its
    /// collaborators: the roster every surface that lists people reads.
    pub fn show(
        &self,
        tree: &mut DesktopTree,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<tilo_desktop::here::Person> {
        let people = self.people(tree, app_name);
        tilo_desktop::here::overlay_collaborators(tree, &people);
        people
    }

    /// A client taking part in no transport: it publishes nowhere and hears
    /// only what it is handed ([`Self::hear`]). A session given one
    /// (`Options::sync`) takes part in the plane this client does, so what a
    /// test hands it is what the desktop shows.
    #[must_use]
    pub fn untethered(publisher: EndpointId, own: OwnPresence) -> Self {
        Self::planeless(publisher, own, Vec::new(), Reachability::default()).0
    }

    /// A session taking part in a plane with no transport, for tests that
    /// hand it what it would have heard and read what it would have said.
    #[cfg(test)]
    fn offline(
        publisher: EndpointId,
        own: OwnPresence,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<Outgoing>) {
        Self::offline_at(publisher, own, Vec::new())
    }

    /// [`Self::offline`] with the hosts the real client would dial.
    #[cfg(test)]
    fn offline_at(
        publisher: EndpointId,
        own: OwnPresence,
        hosts: Vec<EndpointAddr>,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<Outgoing>) {
        Self::offline_reaching(publisher, own, hosts, Reachability::default())
    }

    /// [`Self::offline_at`] with the relays the real client holds back.
    #[cfg(test)]
    fn offline_reaching(
        publisher: EndpointId,
        own: OwnPresence,
        hosts: Vec<EndpointAddr>,
        reachability: Reachability,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<Outgoing>) {
        Self::planeless(publisher, own, hosts, reachability)
    }

    /// A client of a plane with no transport: what both the real client and
    /// [`Self::untethered`] are, at their middle. A test that reads what
    /// one plane would have sent to another takes the receiver.
    pub(crate) fn planeless(
        publisher: EndpointId,
        own: OwnPresence,
        hosts: Vec<EndpointAddr>,
        reachability: Reachability,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<Outgoing>) {
        let (outgoing, said) = tokio::sync::mpsc::unbounded_channel();
        let (_unheard, arrived) = std::sync::mpsc::channel();
        for host in hosts {
            let _ = outgoing.send(Outgoing::Connect(host));
        }
        (
            Self {
                plane: RefCell::new(Plane {
                    publisher,
                    workspaces: Vec::new(),
                    active: None,
                }),
                own: RefCell::new(own),
                here: Here::default(),
                said_at: Cell::new(Instant::now()),
                wave_seq: Cell::new(0),
                sent_wave: RefCell::new(None),
                notice: RefCell::new(None),
                outgoing,
                arrived,
                surfaces: RefCell::new(Vec::new()),
                enrolled: {
                    let (_landed, enrolled) = std::sync::mpsc::channel();
                    enrolled
                },
                engine: None,
                link: None,
                user: None,
                desk: RefCell::new(None),
                content: RefCell::new(None),
                siblings: RefCell::new(std::collections::BTreeMap::new()),
                personal: RefCell::new(Vec::new()),
                endpoint: EndpointAddr::new(publisher),
                endpoint_handle: Arc::new(std::sync::OnceLock::new()),
                lane: crate::calls::LaneSlot::default(),
                fabric: None,
                device_actions: Arc::new(std::sync::OnceLock::new()),
                reachability,
                thread: None,
            },
            said,
        )
    }

    /// The same plane, taken part in as `person` (their user root, hex): what
    /// a device's certificate says of it on a plane that has one, and what
    /// the workspaces' policies hold its members' messages to.
    #[cfg(test)]
    pub(crate) fn taking_part_as(mut self, person: &str) -> Self {
        let mut root = [0u8; 32];
        assert!(
            hex::decode_to_slice(person, &mut root).is_ok(),
            "a person is their root key"
        );
        self.user = Some(root);
        self
    }
}

/// This device's collaboration identity: the key and certificate this
/// session's endpoint runs on, when this machine has been provisioned.
///
/// A session on a machine without one takes part in no workspace as anybody: it
/// binds a fresh key and stays anonymous until the person sets a device up.
#[must_use]
pub fn device_identity() -> Option<HostIdentity> {
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    let key = tilo_user_identity::persist::load_device_key(&paths).ok()?;
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert).ok()?;
    if cert.device_public_key != key.verifying_key().to_bytes() {
        tracing::warn!("the collaboration device key and its certificate differ");
        return None;
    }
    Some(HostIdentity {
        device_cert: cert,
        device_signing_key: key,
        workspace_device_certs: HashMap::new(),
        workspace_revocations: HashMap::new(),
        identity_paths: Some(paths),
    })
}

/// The workspace hosts this session was started to dial: an always-on
/// peer that keeps what the person's devices publish while the others are
/// away, named by `TILO_COLLAB_HOST` (one address or endpoint id) or
/// `TILO_COLLAB_HOSTS` (a JSON array of addresses).
#[must_use]
pub fn configured_hosts() -> Vec<EndpointAddr> {
    let mut hosts = Vec::new();
    if let Some(value) = std::env::var_os("TILO_COLLAB_HOSTS") {
        match serde_json::from_str::<Vec<EndpointAddr>>(&value.to_string_lossy()) {
            Ok(parsed) => hosts.extend(parsed),
            Err(error) => tracing::warn!(
                %error,
                "TILO_COLLAB_HOSTS must be a JSON array of Iroh endpoint addresses"
            ),
        }
    }
    if let Some(value) = std::env::var_os("TILO_COLLAB_HOST") {
        if let Some(addr) = host_addr(&value.to_string_lossy()) {
            hosts.push(addr);
        } else {
            tracing::warn!("TILO_COLLAB_HOST is not an Iroh endpoint address or id");
        }
    }
    hosts.sort_by_key(|addr| addr.id.to_string());
    hosts.dedup_by_key(|addr| addr.id);
    hosts
}

/// A configured host, as its JSON address or as a bare endpoint id.
fn host_addr(value: &str) -> Option<EndpointAddr> {
    serde_json::from_str(value)
        .ok()
        .or_else(|| value.parse::<EndpointId>().ok().map(EndpointAddr::new))
}

/// The endpoint the peer actor binds, and the relays held back from it
/// until something needs this device dialable: number 0's for a device with
/// an identity, none for a session that takes part as nobody and stays on
/// this machine's loopback.
fn endpoint_for(
    identity: Option<&HostIdentity>,
) -> Result<(iroh::endpoint::Builder, iroh::RelayMap), String> {
    let Some(identity) = identity else {
        let builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .portmapper_config(iroh::endpoint::PortmapperConfig::Disabled)
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .map_err(|error| format!("no loopback address: {error}"))?;
        return Ok((builder, iroh::RelayMap::empty()));
    };
    let key = iroh::SecretKey::from_bytes(&identity.device_signing_key.to_bytes());
    // What `presets::N0` builds, with its publisher behind number 0's
    // directory gate and its public relays held back
    // ([`tilo_endpoint_reachability`]): binding with an empty relay map
    // keeps net report, address discovery and port mapping off the network
    // until a workspace this session shows has someone else in it.
    let builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(key)
        .address_lookup(tilo_endpoint_reachability::collaborating())
        .address_lookup(iroh::address_lookup::DnsAddressLookup::n0_dns())
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::empty()));
    Ok((builder, iroh::endpoint::default_relay_mode().relay_map()))
}

/// Hear the peer actor: what it says reaches the session, and what a workspace's
/// host says after this device joined it — the workspace's state as it stands, its
/// people and everything its apps recorded — is taken in where this device's
/// workspaces are kept.
///
/// The replay goes in on a thread of its own, in the order the host said it,
/// so it never holds up what the session shows, and each run of one session
/// in one commit, as a host replays a workspace in one go.
fn heard(
    identity: Option<HostIdentity>,
    arrivals: std::sync::mpsc::Sender<NetEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    link: Option<Arc<ChannelLink>>,
) -> (
    tokio::sync::mpsc::Sender<NetEvent>,
    tokio::task::JoinHandle<()>,
    Option<tokio::task::JoinHandle<()>>,
) {
    let (admissions, attending) = match identity {
        Some(identity) => {
            let (say, hear) = tokio::sync::mpsc::unbounded_channel();
            let attending = tokio::spawn(async move { take_in(hear, &identity, link).await });
            (Some(say), Some(attending))
        }
        None => (None, None),
    };
    let (events, mut heard) = tokio::sync::mpsc::channel::<NetEvent>(256);
    let carrying = tokio::spawn(async move {
        while let Some(event) = heard.recv().await {
            if let (NetEvent::NativeInput(input), Some(say)) = (&event, admissions.as_ref()) {
                let _ = say.send(input.clone());
            }
            if arrivals.send(event).is_err() {
                break;
            }
            (wake)();
        }
    });
    (events, carrying, attending)
}

/// Take what hosts replay into the workspaces this device holds, as it arrives:
/// each run of one session admitted together and in the order the host said
/// it, on a blocking thread so no replay holds up the collaboration plane.
async fn take_in(
    mut replayed: tokio::sync::mpsc::UnboundedReceiver<native_ingress::Input>,
    identity: &HostIdentity,
    link: Option<Arc<ChannelLink>>,
) {
    while let Some(input) = replayed.recv().await {
        let mut waiting = vec![input];
        while let Ok(next) = replayed.try_recv() {
            waiting.push(next);
        }
        let mut run: Vec<native_ingress::Input> = Vec::new();
        for input in waiting {
            // The channels' governance is between the workspace's devices: its
            // governor never admits it, and the channel service settles the
            // groups with it instead.
            if link
                .as_ref()
                .is_some_and(|link| link.dispatch(input.message()))
            {
                continue;
            }
            if run
                .first()
                .is_some_and(|first: &native_ingress::Input| !first.same_session(&input))
            {
                admit(run, identity, link.as_deref()).await;
                run = Vec::new();
            }
            run.push(input);
        }
        admit(run, identity, link.as_deref()).await;
    }
}

/// Admit one session's inputs, as one commit of the workspace they belong to.
async fn admit(
    inputs: Vec<native_ingress::Input>,
    identity: &HostIdentity,
    link: Option<&ChannelLink>,
) {
    let Some(workspace) = inputs.first().map(native_ingress::Input::workspace) else {
        return;
    };
    let identity = identity.clone();
    let admitted = match tokio::task::spawn_blocking(move || {
        native_ingress::Input::admit_batch(inputs, &identity)
    })
    .await
    {
        Ok(admitted) => admitted,
        Err(error) => {
            tracing::debug!(%error, %workspace, "a workspace's state was not taken in");
            return;
        }
    };
    let mut replay_wanted = false;
    for (_, report) in admitted.inputs {
        match report {
            Ok(report) => {
                replay_wanted |= report.replay_wanted;
            }
            Err(error) => {
                tracing::debug!(%error, %workspace, "a workspace's state was not taken in");
            }
        }
    }
    if let Some(link) = link {
        if replay_wanted {
            // A message waited on something the workspace's host sends of its own
            // accord as well, such as the checkpoint a sealed record is
            // founded on: the host replays what it retains, and the message
            // is decided again.
            link.ask_replay(workspace);
        }
        if admitted.changed {
            // The workspace's governance moved with it: its channels and the
            // devices it admits are read again, and the plane told what it
            // admits now.
            link.workspace_changed(workspace);
        }
    }
}

/// Give one command from the session to the plane: whether it was taken, or
/// `None` when it is the command that ends it.
async fn carry_out(
    plane: &tokio::sync::mpsc::Sender<NetCmd>,
    command: Outgoing,
    events: &tokio::sync::mpsc::Sender<NetEvent>,
) -> Option<bool> {
    Some(match command {
        Outgoing::Join(ws) => plane
            .send(NetCmd::JoinWorkspace {
                ws,
                on_event: events.clone(),
            })
            .await
            .is_ok(),
        Outgoing::Leave(ws) => plane.send(NetCmd::LeaveWorkspace { ws }).await.is_ok(),
        Outgoing::Publish { ws, msg } => {
            plane.send(NetCmd::Publish { ws, msg: *msg }).await.is_ok()
        }
        Outgoing::Content(delivery) => plane
            .send(NetCmd::PublishRecordContent(delivery))
            .await
            .is_ok(),
        Outgoing::PublishSurfaceTo { ws, msg, viewers } => plane
            .send(NetCmd::PublishSurfaceTo {
                ws,
                msg: *msg,
                viewers,
            })
            .await
            .is_ok(),
        Outgoing::SendControl { ws, msg, target } => plane
            .send(NetCmd::PublishWorkspaceControl {
                ws,
                msg: *msg,
                route: WorkspaceControlRoute::Direct { target },
            })
            .await
            .is_ok(),
        Outgoing::Connect(addr) => plane
            .send(NetCmd::ConnectHost {
                addr,
                expectation: None,
            })
            .await
            .is_ok(),
        Outgoing::WatchSiblings => plane
            .send(NetCmd::WatchSiblings {
                on_event: events.clone(),
            })
            .await
            .is_ok(),
        Outgoing::TellSiblings(msg) => plane
            .send(NetCmd::PublishToSiblings { msg: *msg })
            .await
            .is_ok(),
        Outgoing::ConnectDevice(addr) => plane.send(NetCmd::Connect { addr }).await.is_ok(),
        // A fabric frame goes to the one device it is for, or to each of the
        // person's devices whose session is up.
        Outgoing::Fabric {
            user_public_key,
            target,
            payload,
        } => {
            let command = match target {
                Some(target) => NetCmd::PublishPersonalFabricTarget {
                    user_public_key,
                    target,
                    payload,
                },
                None => NetCmd::PublishPersonalFabric {
                    user_public_key,
                    payload,
                },
            };
            plane.send(command).await.is_ok()
        }
        Outgoing::Replay(ws) => plane.send(NetCmd::NativeReplay { ws }).await.is_ok(),
        Outgoing::PublishCustody(ws) => plane.send(NetCmd::NativePublish { ws }).await.is_ok(),
        Outgoing::OpenRecord { ws, channel } => {
            plane.send(NetCmd::OpenRecord { ws, channel }).await.is_ok()
        }
        Outgoing::Roster { ws, devices } => plane
            .send(NetCmd::SetWorkspaceRoster { ws, devices })
            .await
            .is_ok(),
        // Redeemed beside this loop, which never waits on a host.
        Outgoing::Enroll(_) => true,
        Outgoing::PrivateMessages(reply) => plane
            .send(tilo_network_actor::NetCmd::PrivateMessages(reply))
            .await
            .is_ok(),
        Outgoing::Stop => return None,
    })
}

/// Redeem an invitation into the workspace the desk made for its workspace: this
/// device becomes a member of the workspace, takes in what it held before it
/// arrived, and takes part in it from then on.
///
/// The workspace's state arrives as the host's answer to the enrollment: a person
/// who joins late reads the whole history, not only what is recorded from
/// now on.
async fn redeem(
    endpoint: iroh::Endpoint,
    identity: HostIdentity,
    engine: Arc<Engine>,
    plane: &tokio::sync::mpsc::Sender<NetCmd>,
    events: &tokio::sync::mpsc::Sender<NetEvent>,
    request: Enrolling,
    link: Option<&ChannelLink>,
) -> Result<(), String> {
    let topic = workspace_topic_id(request.workspace_id);
    // The workspace is joined as this person, under the identity they keep for
    // it: a workspace they select another of their identities for shows that one.
    let (selected, _) = identity.current_native_material(topic)?;
    if identity.identity_paths.is_some() {
        let registry = tilo_identity_host::registry::IdentityRegistryStore::from_env()
            .map_err(|error| format!("this computer's identities cannot be read: {error}"))?;
        registry
            .bind_workspace(
                &request.workspace_id.to_string(),
                &hex::encode(selected.user_public_key),
            )
            .map_err(|error| format!("the workspace is not kept as this person's: {error}"))?;
        if identity.current_native_material(topic)?.0 != selected {
            return Err("this computer's identity for the workspace changed while joining".into());
        }
    }
    let owner = engine.owner().clone();
    let client = tilo_relay_session::workspace_join::WorkspaceJoinClient::new(endpoint);
    let enrollment = tokio::time::timeout(
        REDEEM_WITHIN,
        tilo_peer_state::owner::enroll::prepare_and_complete(
            client,
            identity.clone(),
            topic,
            Some(request.workspace),
            request.invitation,
            owner.clone(),
        ),
    )
    .await
    .map_err(|_| "the workspace's host did not answer".to_owned())??;
    owner.install(&enrollment, std::time::SystemTime::now())?;
    // The workspace is held here now: this device takes part in it, so what its
    // people say and record reaches it as it happens, and the devices it
    // admits are told to the plane.
    if let Some(link) = link {
        link.workspace_changed(request.workspace_id);
    }
    // The workspace's subscription comes before the session it carries: what the
    // host says the moment this device joins is the workspace as it stands, and a
    // workspace nobody listens to yet drops it.
    plane
        .send(NetCmd::JoinWorkspace {
            ws: topic,
            on_event: events.clone(),
        })
        .await
        .map_err(|_| "the collaboration plane is gone".to_owned())?;
    let connect = tilo_network_actor::native_ingress::Connect::new(owner, request.workspace_id)
        .map_err(|error| format!("the workspace was not taken in: {error}"))?;
    plane
        .send(NetCmd::NativeConnect(connect, None))
        .await
        .map_err(|_| "the collaboration plane is gone".to_owned())
}

/// The certificate among `certs` that says who authored `update`, if the
/// device was heard with one.
///
/// The update names its author, and a device it arrived from is only that
/// author if the certificate the device's session came up with is the
/// author's: a list is worth no more than the device that signed it.
fn author_of(
    certs: &[tilo_user_identity::DeviceCert],
    update: &tilo_user_identity::DirectoryUpdate,
) -> Option<tilo_user_identity::DeviceCert> {
    certs
        .iter()
        .find(|cert| cert.device_public_key == update.author_device_public_key)
        .cloned()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod custody_tests;
