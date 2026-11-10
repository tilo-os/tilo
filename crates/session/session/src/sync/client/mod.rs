//! This desktop's transport of the collaboration plane: the peer actor on an iroh endpoint, on the
//! device's own collaboration key, and what it carries between the session and the others.
//!
//! The device certificate the actor announces names the device and its person, so two devices of
//! one person are one face. The endpoint keeps number 0's directory behind a gate and its relays
//! held back ([`tilo_endpoint_reachability`]): a desktop with nobody else in its workspaces
//! publishes nothing and keeps no relay.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::{Duration, Instant};

use collab_types::presence::{Presence, ShellPresence, Shown};
use iroh::{EndpointAddr, EndpointId};
use neighbour_wire::NeighbourMsg;
use tilo_desktop::chrome::{ArrivalNotice, WaveNotice};
use tilo_desktop::here::{Here, LiveMember, Wave};
use tilo_desktop_model::{DesktopTree, TabId, WorkspaceId as DeskWorkspaceId};
use tilo_desktop_surface::DesktopHandle;
#[cfg(target_os = "linux")]
use tilo_live::live::Track;
use tilo_live::plane::Heard;
use tilo_live::presence;
use tilo_live::presence::Board;

#[cfg(target_os = "linux")]
use super::Entering;
use super::presence::{OwnPresence, Plane, Workspace, now_ms};
use super::{Enrolled, InCall, LiveCall};
use crate::platform::plane::{Cmd, Device, Event, SayPresence, Topic, Transport, topic_of};

#[cfg(target_os = "linux")]
use crate::channels::ChannelLink;
#[cfg(target_os = "linux")]
use crate::content;
#[cfg(target_os = "linux")]
use crate::device_actions::DeviceActions;
#[cfg(target_os = "linux")]
use crate::engine::Engine;
#[cfg(target_os = "linux")]
use crate::fabric::{Continuity, SessionFabric};
#[cfg(target_os = "linux")]
use crate::sync::Enrolling;
#[cfg(target_os = "linux")]
use tilo_action_broker::record_content::ContentSources;
#[cfg(target_os = "linux")]
use tilo_device_fabric::DeviceId;
#[cfg(target_os = "linux")]
use tilo_device_fabric::runtime::FabricRuntime;
#[cfg(target_os = "linux")]
use tilo_endpoint_reachability::{Need, Reachability};
#[cfg(target_os = "linux")]
use tilo_identity_host::custody::HostIdentity;
#[cfg(target_os = "linux")]
use tilo_network_actor::{NetCmd, NetEvent, WorkspaceControlRoute, WorkspaceId, native_ingress};

#[cfg(target_os = "linux")]
mod adopt;
mod arrivals;
mod gate;
#[cfg(target_os = "linux")]
mod plane;

#[cfg(target_os = "linux")]
use plane::{Serving, event_of, run};

/// How long the session waits for this device's endpoint to bind before it starts without one: a
/// desktop with no one else on it, rather than a session that never opens.
#[cfg(target_os = "linux")]
const BIND_WITHIN: Duration = Duration::from_secs(30);

/// The endpoint the thread bound, within [`BIND_WITHIN`]: what publishes on it, the endpoint
/// itself, and whether this device can be reached.
#[cfg(target_os = "linux")]
type Bound = (EndpointId, EndpointAddr, Reachability);

#[cfg(target_os = "linux")]
fn wait_for_bound(
    bound: &std::sync::mpsc::Receiver<Result<Bound, String>>,
) -> std::io::Result<Bound> {
    bound
        .recv_timeout(BIND_WITHIN)
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the collaboration endpoint did not bind",
            )
        })?
        .map_err(std::io::Error::other)
}

/// One window stream's message as it reached this device: another member's shared window, or a
/// member's ask to view one of this device's.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug)]
pub(crate) struct ReceivedSurface {
    /// The workspace's topic it came in on.
    pub(crate) ws: Topic,
    /// The device that published it.
    pub(crate) from: EndpointId,
    pub(crate) msg: NeighbourMsg,
}

/// The person's other devices, dialled as this device's siblings: the session between two of their
/// desks is where each tells the other the workspaces it holds, and what it hosts, so one can adopt
/// what the other keeps.
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub(crate) struct SiblingDials {
    /// This device's endpoint id: its own session is never dialled.
    ours: [u8; 32],
    outgoing: tokio::sync::mpsc::UnboundedSender<Cmd>,
}

#[cfg(target_os = "linux")]
impl SiblingDials {
    /// Dial from `ours`, the endpoint this device's sessions run on.
    pub(crate) fn new(ours: [u8; 32], outgoing: tokio::sync::mpsc::UnboundedSender<Cmd>) -> Self {
        Self { ours, outgoing }
    }

    /// Dial `device` as a sibling, unless it is this device. The plane dials again whatever has no
    /// session, for as long as that lasts.
    pub(crate) fn dial(&self, device: [u8; 32]) {
        if device == self.ours {
            return;
        }
        let Ok(id) = iroh::EndpointId::from_bytes(&device) else {
            tracing::warn!(device = %hex::encode(device), "a device of the person has no endpoint id");
            return;
        };
        let _ = self
            .outgoing
            .send(Cmd::ConnectDevice(EndpointAddr::new(id)));
    }
}

/// Where this person is on this desk, as presence says it: their pointer over a tile here, and
/// their shell — its face, its screen and the window they work in.
#[derive(Clone, Debug, Default)]
struct Where {
    pointer: Option<presence::Point>,
    shell: Option<Box<ShellPresence>>,
}

impl Where {
    /// Whether `other` says where this person is as `self` does: a shell is the same one while what
    /// it says is the same, never because it was restamped.
    fn same(&self, other: &Self) -> bool {
        self.pointer == other.pointer
            && self
                .shell
                .as_deref()
                .map(tilo_live::presence::without_clock)
                == other
                    .shell
                    .as_deref()
                    .map(tilo_live::presence::without_clock)
    }
}

/// The session's client of the collaboration plane: the workspaces it shows,
/// this person's presence in them, and everyone else's.
#[cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page's transport feeds these next")
)]
pub struct Client {
    plane: RefCell<Plane>,
    own: RefCell<OwnPresence>,
    /// Where this person is on this desk, as the surfaces here read it: the pointer over the tile
    /// it is on, and their screen as a shell. Said with the rest of their word, and said again only
    /// when it moves.
    says: RefCell<Where>,
    here: Here,
    /// The count this person's last wave carried here.
    wave_seq: Cell<u32>,
    /// What this device said while it held no plane to say it on, for the tests that read what a
    /// session would have published.
    #[cfg(test)]
    said: RefCell<Vec<Presence>>,
    /// The wave this person sent last, for their own row to say sent.
    sent_wave: RefCell<Option<Wave>>,
    /// The last wave at this person, for the bar to say.
    notice: RefCell<Option<WaveNotice>>,
    /// The workspace whose arrival the bar is saying, so a person who comes to a workspace where
    /// people are is shown arriving once, not on every roster change.
    arrived_in: RefCell<Option<String>>,
    /// The arrival the bar shows.
    arrival: RefCell<Option<ArrivalNotice>>,
    outgoing: tokio::sync::mpsc::UnboundedSender<Cmd>,
    arrived: std::sync::mpsc::Receiver<Event>,
    /// Where this device's word of itself goes on each workspace's presence track, as the transport
    /// it says on carries it: `None` on a host that says presence nowhere.
    saying: Option<std::rc::Rc<dyn SayPresence>>,
    /// The window streams that reached this device: their frames, starts,
    /// ends and control, which the shares take ([`Self::take_surfaces`]).
    #[cfg(target_os = "linux")]
    surfaces: RefCell<Vec<ReceivedSurface>>,
    /// The call audio the workspaces' live planes heard since the last call,
    /// each with the workspace it was heard in: what the calls playing it take
    /// ([`Self::take_call_audio`]).
    call_audio: RefCell<Vec<(String, Heard)>>,
    /// The call pictures the workspaces' live planes heard since the last call, each with the
    /// workspace it was heard in: what the calls playing there draw ([`Self::take_call_pictures`]).
    call_pictures: RefCell<Vec<(String, Heard)>>,
    /// What became of the enrollments asked for since the last call.
    enrolled: std::sync::mpsc::Receiver<Enrolled>,
    /// This device's workspaces, which a join enrolls into: `None` when the session keeps no state
    /// to hold them in.
    #[cfg(target_os = "linux")]
    engine: Option<Arc<Engine>>,
    /// What the session's channels hear and say on this plane: the governance of the workspaces'
    /// sealed records, and the devices each workspace admits. What the engine holds, so a session
    /// that runs no channel service has none.
    #[cfg(target_os = "linux")]
    link: Option<Arc<ChannelLink>>,
    /// The person this device belongs to, by their root key, as its certificate names them.
    user: Option<[u8; 32]>,
    /// What this device calls itself, from its certificate: the name its person's other devices are
    /// told apart by. Empty on a device that named none.
    device: String,
    /// The collaborator's devices the plane heard at another peer protocol version since the last
    /// call: what the person is answered.
    #[cfg(target_os = "linux")]
    versions: std::sync::mpsc::Receiver<crate::network::VersionMismatch>,
    /// The desk this session shows, whose workspaces' policies hold every member's messages to what
    /// the workspace grants them: `None` until the session hands it over ([`Self::attach_desk`]).
    desk: RefCell<Option<DesktopHandle>>,
    /// The records' content this session keeps and asks the workspaces for: `None` until the
    /// session hands its plane over ([`Self::attach_content`]).
    #[cfg(target_os = "linux")]
    content: RefCell<Option<Arc<content::Plane>>>,
    /// The devices each workspace's record content reaches this one from: those heard on its topic,
    /// and its hosts.
    #[cfg(target_os = "linux")]
    topologies: RefCell<std::collections::BTreeMap<WorkspaceId, content::Topology>>,
    /// The certificates each of the person's other devices was heard with,
    /// by its publisher.
    siblings: RefCell<std::collections::BTreeMap<EndpointId, Vec<tilo_user_identity::DeviceCert>>>,
    /// Whether one of the person's other devices came up since the last call:
    /// a device that just said hello has not heard this device's list yet,
    /// and a list said only when something changed here would never reach it.
    sibling_arrived: std::cell::Cell<bool>,
    /// Where the workspaces the person's other devices hold are hosted, by workspace id: the host
    /// hints they said with the list, so this device enters a workspace at the device hosting it
    /// rather than guessing.
    host_hints: RefCell<std::collections::BTreeMap<String, tilo_peer_state::join::TransportHint>>,
    /// The workspace lists the person's other devices sent since the last call, each with the
    /// certificate its device was heard by.
    personal: RefCell<
        Vec<(
            tilo_user_identity::DirectoryUpdate,
            tilo_user_identity::DeviceCert,
        )>,
    >,
    /// The person's own store this session keeps for their other devices: what the broker keeps
    /// their own records in as well, served on this device's endpoint from the pass it binds.
    /// `None` on a session that has no state to keep it in.
    #[cfg(target_os = "linux")]
    personal_store: RefCell<Option<crate::personal::Personal>>,
    /// Where this device's endpoint is reached, as other devices dial it.
    endpoint: EndpointAddr,
    /// This device's endpoint itself, once it has bound: what the session's traffic runs on, and
    /// what names this device.
    endpoint_handle: Arc<std::sync::OnceLock<iroh::Endpoint>>,
    /// This device's fabric, when it is a certified device of the person's: `None` on a session
    /// that takes part as nobody.
    #[cfg(target_os = "linux")]
    fabric: Option<Arc<SessionFabric>>,
    /// This session's Nearby Send, once the session made it: what a transfer one of the person's
    /// devices offers, streams or finishes means for what Files shows.
    #[cfg(target_os = "linux")]
    nearby: RefCell<Option<Arc<crate::nearby_transfer::NearbyTransfer>>>,
    /// Where this device's actions are served, and how an app here reaches the person's other
    /// devices: `None` until the endpoint has bound.
    #[cfg(target_os = "linux")]
    device_actions: Arc<std::sync::OnceLock<DeviceActions>>,
    /// This device's relays, held back until a workspace this session takes part in has someone
    /// else in it.
    #[cfg(target_os = "linux")]
    reachability: Reachability,
    /// The protocols this session was handed to serve beside its own, by ALPN: what it answers on
    /// this device's endpoint, so the session can say whether it serves one ([`Self::serves`]).
    beside: Vec<Vec<u8>>,
    /// The workspaces' live plane: their relays and this device's sessions in them, on a thread of
    /// its own.
    #[cfg(target_os = "linux")]
    live: Option<crate::live::Live>,
    /// The thread running the peer actor, stopped when this drops.
    #[cfg(target_os = "linux")]
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
        // The live planes dial through this endpoint: they end before it does,
        // and before the thread that holds it joins.
        #[cfg(target_os = "linux")]
        drop(self.live.take());
        let _ = self.outgoing.send(Cmd::Stop);
        #[cfg(target_os = "linux")]
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What this device brings as it enters the collaboration plane: the identity its endpoint runs on,
/// where it dials, and how it hears back.
#[cfg(target_os = "linux")]
pub struct Entry {
    /// This device's collaboration identity, on whose key the endpoint runs: with it, every
    /// workspace is told which device and which person is here. Without one — a test, a stand-in
    /// session — the session is a device nobody certified and stays anonymous in workspaces.
    pub identity: Option<HostIdentity>,
    /// What the person's settings say this device says of them here.
    pub own: OwnPresence,
    /// The hosts the person points this device at.
    pub hosts: Vec<EndpointAddr>,
    /// What the person's settings say may continue on their other devices,
    /// kept current by the session's settings watch.
    pub continuity: Continuity,
    /// What tells the peer actor the network speaks.
    pub wake: Arc<dyn Fn() + Send + Sync>,
}

#[cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page's transport feeds these next")
)]
impl Client {
    /// Take part in the collaboration plane: bind this device's endpoint, start the peer actor,
    /// and from then on hear the workspaces the session joins. `beside` is what else this session
    /// serves on that endpoint (its protocols are fixed once it binds), and `documents` is where
    /// what admission takes in from another device goes.
    ///
    /// # Errors
    /// The endpoint could not be bound.
    #[cfg(target_os = "linux")]
    pub fn start(
        entry: Entry,
        engine: Option<Arc<Engine>>,
        questions: Option<tilo_service::Questions>,
        mut beside: tilo_network_actor::local_host::Protocols,
        documents: Option<PeerDocuments>,
    ) -> std::io::Result<Self> {
        let identity = entry.identity;
        let wake = entry.wake;
        let (outgoing, commands) = tokio::sync::mpsc::unbounded_channel();
        let (arrivals, arrived) = std::sync::mpsc::channel();
        let (enrollments, enrolled) = std::sync::mpsc::channel();
        let (ready, bound) = std::sync::mpsc::channel();
        let (user, device) = certified_as(identity.as_ref());
        let fabric = restored_fabric(identity.as_ref(), outgoing.clone(), entry.continuity);
        // What the actor serves is installed before it starts: every actor built after answers it.
        let versions = crate::network::install(fabric.as_ref(), questions);
        // The channels' side of the plane is the engine's, opened before the plane starts.
        let link = engine.as_ref().and_then(|engine| engine.link());
        let plane_link = link.clone();
        let enrolling = engine.clone();
        let device_actions = Arc::new(std::sync::OnceLock::new());
        let serving = Arc::clone(&device_actions);
        let endpoint_handle = Arc::new(std::sync::OnceLock::new());
        let bound_endpoint = Arc::clone(&endpoint_handle);
        // The live plane's relay role, when this device may serve one, is one more protocol on the
        // endpoint this session binds.
        let mut live = engine
            .as_ref()
            .map(|engine| crate::live::Live::new(Arc::clone(engine), Arc::clone(&wake)));
        if let Some(live) = &mut live {
            beside.extend(live.protocols());
        }
        let served: Vec<Vec<u8>> = beside.iter().map(|(alpn, _)| alpn.clone()).collect();
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
                    Serving::new(bound_endpoint, plane_link, serving, beside, documents),
                );
            })?;
        let (publisher, endpoint, reachability) = wait_for_bound(&bound)?;
        if let (Some(live), Some(bound)) = (&mut live, endpoint_handle.get()) {
            live.attach(bound.clone());
        }
        if let Some(link) = &link {
            link.attach(outgoing.clone());
        }
        let saying = live
            .as_ref()
            .map(|live| std::rc::Rc::new(live.saying()) as std::rc::Rc<dyn SayPresence>);
        let mut sync = Self::over(
            Transport {
                outgoing,
                arrived,
                enrolled,
                saying,
            },
            Device {
                publisher,
                endpoint,
                person: user,
                name: device,
            },
            entry.own,
        );
        sync.engine = engine;
        sync.link = link;
        sync.endpoint_handle = endpoint_handle;
        sync.fabric = fabric;
        sync.device_actions = device_actions;
        sync.reachability = reachability;
        sync.beside = served;
        sync.live = live;
        sync.versions = versions;
        sync.thread = Some(thread);
        // The person's other devices are heard from the start, so a list one of them published is
        // taken even when this session shows none of their workspaces yet.
        let _ = sync.outgoing.send(Cmd::WatchSiblings);
        // The hosts next: a workspace joined before them is replayed to this device when its host
        // session comes up, never lost.
        for host in entry.hosts {
            let _ = sync.outgoing.send(Cmd::Connect(host));
        }
        Ok(sync)
    }

    /// A client on a transport another host brings: it says what the session says through `plane`,
    /// and reads on every [`Self::follow`] what the transport delivered there. What only a desktop
    /// keeps — its workspaces, their channels and this device's fabric — is absent until the
    /// session hands its own over.
    #[must_use]
    pub fn over(plane: Transport, device: Device, own: OwnPresence) -> Self {
        Self {
            plane: RefCell::new(Plane::new(device.publisher)),
            own: RefCell::new(own),
            says: RefCell::default(),
            here: Here::default(),
            wave_seq: Cell::new(0),
            #[cfg(test)]
            said: RefCell::new(Vec::new()),
            sent_wave: RefCell::new(None),
            notice: RefCell::new(None),
            arrived_in: RefCell::new(None),
            arrival: RefCell::new(None),
            outgoing: plane.outgoing,
            arrived: plane.arrived,
            saying: plane.saying,
            #[cfg(target_os = "linux")]
            surfaces: RefCell::new(Vec::new()),
            call_audio: RefCell::new(Vec::new()),
            call_pictures: RefCell::new(Vec::new()),
            enrolled: plane.enrolled,
            user: device.person,
            device: device.name,
            desk: RefCell::new(None),
            siblings: RefCell::new(std::collections::BTreeMap::new()),
            host_hints: RefCell::new(std::collections::BTreeMap::new()),
            sibling_arrived: std::cell::Cell::new(false),
            personal: RefCell::new(Vec::new()),
            endpoint: device.endpoint,
            endpoint_handle: Arc::new(std::sync::OnceLock::new()),
            beside: Vec::new(),
            #[cfg(target_os = "linux")]
            engine: None,
            #[cfg(target_os = "linux")]
            link: None,
            #[cfg(target_os = "linux")]
            versions: {
                let (_said, versions) = std::sync::mpsc::channel();
                versions
            },
            #[cfg(target_os = "linux")]
            content: RefCell::new(None),
            #[cfg(target_os = "linux")]
            topologies: RefCell::new(std::collections::BTreeMap::new()),
            #[cfg(target_os = "linux")]
            personal_store: RefCell::new(None),
            #[cfg(target_os = "linux")]
            fabric: None,
            #[cfg(target_os = "linux")]
            nearby: RefCell::new(None),
            #[cfg(target_os = "linux")]
            device_actions: Arc::new(std::sync::OnceLock::new()),
            #[cfg(target_os = "linux")]
            reachability: Reachability::default(),
            #[cfg(target_os = "linux")]
            live: None,
            #[cfg(target_os = "linux")]
            thread: None,
        }
    }

    /// This device's publisher here: the endpoint id every workspace names it by.
    #[must_use]
    pub fn publisher(&self) -> EndpointId {
        self.plane.borrow().publisher
    }

    /// Whether this session answers `alpn` on this device's endpoint: what it was handed beside its
    /// own planes, from the pass the endpoint bound.
    #[must_use]
    pub fn serves(&self, alpn: &[u8]) -> bool {
        self.beside.iter().any(|served| served == alpn)
    }

    /// The collaborator's devices heard on this plane at another peer protocol version since the
    /// last call, each once per device and version: one of the two devices must update before the
    /// two work together again.
    #[cfg(target_os = "linux")]
    pub(crate) fn take_version_mismatches(&self) -> Vec<crate::network::VersionMismatch> {
        self.versions.try_iter().collect()
    }

    /// The desktop's presence, shared with whatever draws who is here.
    #[must_use]
    pub fn here(&self) -> &Here {
        &self.here
    }

    /// This device's workspaces, when this session holds any.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn engine(&self) -> Option<&Arc<Engine>> {
        self.engine.as_ref()
    }

    /// The workspaces' live plane: what this session says into and hears from each of them, and the
    /// relay each one is carried by; `None` when this session holds no workspaces.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn live(&self) -> Option<&crate::live::Live> {
        self.live.as_ref()
    }

    /// Redeem `request`'s invitation off the loop: this device joins the workspace and takes in
    /// what it held before. [`Self::take_enrolled`] says what became of it.
    ///
    /// # Errors
    /// The plane takes no enrollment: this session holds no workspaces, or it has stopped.
    #[cfg(target_os = "linux")]
    pub fn enroll(&self, request: Enrolling) -> Result<(), String> {
        if self.engine.is_none() {
            return Err("this session keeps no workspaces to enroll into".into());
        }
        self.outgoing
            .send(Cmd::Enroll(request))
            .map_err(|_| "the collaboration plane is gone".to_owned())
    }

    /// Take `request`'s workspace in as this person's further device, off the loop: what the
    /// workspace holds before arrives with the host's answer. [`Self::take_enrolled`] says what
    /// became of it.
    ///
    /// # Errors
    /// The plane takes no entry: this session holds no workspaces, or it has stopped.
    #[cfg(target_os = "linux")]
    pub fn enter(&self, request: Entering) -> Result<(), String> {
        if self.engine.is_none() {
            return Err("this session keeps no workspaces to take in".into());
        }
        self.outgoing
            .send(Cmd::Enter(Box::new(request)))
            .map_err(|_| "the collaboration plane is gone".to_owned())
    }

    /// Take what became of the enrollments and entries asked for since the last call.
    pub fn take_enrolled(&self) -> Vec<Enrolled> {
        self.enrolled.try_iter().collect()
    }

    /// Where the workspace `workspace` is hosted, as a device of the person's that holds it said:
    /// the host hint the person's other devices send with their list. `None` while none of them
    /// said it yet.
    pub(crate) fn host_hint(
        &self,
        workspace: &str,
    ) -> Option<tilo_peer_state::join::TransportHint> {
        self.host_hints.borrow().get(workspace).cloned()
    }

    /// What this session says of itself here now.
    #[must_use]
    pub fn own(&self) -> OwnPresence {
        self.own.borrow().clone()
    }

    /// What this session says of itself: changed here, it is said again in the workspace in front.
    /// Whether it changed.
    pub fn set_own(&self, own: OwnPresence) -> bool {
        if *self.own.borrow() == own {
            return false;
        }
        *self.own.borrow_mut() = own;
        self.say_own();
        true
    }

    /// Where this person is on this desk now: their `pointer` over the tile it is on, and their
    /// `shell` as the others' surfaces draw it. Said with the rest of their word, and said again
    /// only when one of them moved. Whether that changed anything.
    pub fn set_where(
        &self,
        pointer: Option<presence::Point>,
        shell: Option<Box<ShellPresence>>,
    ) -> bool {
        let where_ = Where { pointer, shell };
        if self.says.borrow().same(&where_) {
            return false;
        }
        *self.says.borrow_mut() = where_;
        self.say_own();
        true
    }

    /// This device joined `call` , or left it (`None`): said again in the workspace in front, so
    /// everyone here reads which channel this person is in beside their face. Whether that changed
    /// anything.
    pub fn in_call(&self, call: Option<InCall>) -> bool {
        let mut own = self.own.borrow().clone();
        if own.call == call {
            return false;
        }
        own.call = call;
        self.set_own(own)
    }

    /// Whether this person is in the call in `channel` , on any of their devices: what a Join is
    /// not offered for.
    #[must_use]
    pub fn is_in_call(&self, channel: &str) -> bool {
        let own = self.own.borrow();
        if own
            .call
            .as_ref()
            .is_some_and(|call| call.call.channel == channel)
        {
            return true;
        }
        let user = own.user.clone();
        drop(own);
        self.calls()
            .iter()
            .any(|call| call.channel == channel && call.joined_by(user.as_deref()))
    }

    /// Follow `tree` 's workspaces: take part in each, leave the ones gone, and remember which is
    /// in front. Whether that changed anything.
    pub fn follow_desktop(&self, tree: &DesktopTree) -> bool {
        let live: Vec<(Topic, String)> = tree
            .workspaces
            .iter()
            .filter(|workspace| !workspace.id().is_nil())
            .map(|workspace| (topic_of(workspace.id()), workspace.id().to_string()))
            .collect();
        let active = tree
            .active_workspace()
            .filter(|workspace| !workspace.id().is_nil())
            .map(|workspace| topic_of(workspace.id()));
        self.follow_workspaces(&live, active)
    }

    /// Follow the workspaces a host names itself — a page's mounts, each showing one — as
    /// [`Self::follow_desktop`] follows a desk's: take part in each of `live`, leave the ones gone,
    /// and remember `active` as the one in front. Whether that changed anything.
    pub fn follow_workspaces(&self, live: &[(Topic, String)], active: Option<Topic>) -> bool {
        let mut joined = Vec::new();
        let mut left = Vec::new();
        let mut changed = false;
        let mut front_changed = false;
        {
            let mut plane = self.plane.borrow_mut();
            let publisher = *plane.publisher.as_bytes();
            for (ws, id) in live {
                if plane.workspaces.iter().all(|workspace| workspace.ws != *ws) {
                    plane
                        .workspaces
                        .push(Workspace::new(*ws, id.clone(), publisher));
                    #[cfg(target_os = "linux")]
                    self.topologies
                        .borrow_mut()
                        .insert(*ws, content::Topology::default());
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
                front_changed = true;
                changed = true;
            }
        }
        for (ws, id) in joined {
            let _ = self.outgoing.send(Cmd::Join(ws));
            if DeskWorkspaceId::parse(&id).is_some() {
                self.say_into(&id);
                // This device takes part in the workspace through its own custody session too, the
                // one that carries the workspace's app records: a device that was away comes back
                // with every record it missed, and a device that restarted into a workspace it
                // already held reaches its records again.
                #[cfg(target_os = "linux")]
                if let Some(connect) = DeskWorkspaceId::parse(&id).and_then(|desk| {
                    let engine = self.engine.as_ref()?;
                    native_ingress::Connect::new(engine.owner().clone(), desk).ok()
                }) {
                    let _ = self.outgoing.send(Cmd::TakePart(connect));
                }
            }
            changed = true;
        }
        for ws in left {
            let _ = self.outgoing.send(Cmd::Leave(ws));
            #[cfg(target_os = "linux")]
            self.topologies.borrow_mut().remove(&ws);
            changed = true;
        }
        if front_changed {
            // A workspace coming to the front has not heard this device yet: it says who it is
            // there at once.
            self.say_own();
        }
        #[cfg(target_os = "linux")]
        self.note_reachability();
        if changed {
            self.show_here();
        }
        changed
    }

    /// When this person next says again where they are, so the workspaces hear they are still here
    /// with no pass in between.
    pub fn next_deadline(&self) -> Option<Instant> {
        let now = now_ms();
        let due = self.plane.borrow().next_due(now)?;
        let wait = (due - now).max(0.0) / 1_000.0;
        Instant::now().checked_add(Duration::from_secs_f64(wait))
    }

    /// Say what `workspace`'s board owes now — the keepalives, the repeats and the loss repairs —
    /// as a host that shows one workspace at a time asks with its own timer. Whether anything left.
    pub fn due_in(&self, workspace: &str) -> bool {
        let now = now_ms();
        let Some((desk, frames)) = self.plane.borrow_mut().due_in(workspace, now) else {
            return false;
        };
        self.say_frames(desk, frames);
        true
    }

    /// When `workspace`'s board next owes a word, in Unix milliseconds: what a host with its own
    /// clock arms its timer with.
    #[must_use]
    pub fn next_due_in(&self, workspace: &str) -> Option<f64> {
        self.plane.borrow().next_due_in(workspace, now_ms())
    }

    /// Hear everything that reached the plane since the last call. Whether anything did.
    pub fn follow(&self) -> bool {
        let mut heard = false;
        while let Ok(event) = self.arrived.try_recv() {
            self.hear(event);
            heard = true;
        }
        #[cfg(target_os = "linux")]
        {
            if self.hear_live() {
                heard = true;
            }
            // What arrived moved this device's fabric — a copy from another device, a device that
            // is gone: the person's other devices hear so on the same pass.
            self.announce_fabric();
            // The clock of every peer something holds in sync: its probes leave here, so the audio
            // timed against them can play in time.
            if let Some(fabric) = &self.fabric {
                fabric.pump_clock();
            }
            // What no device acknowledged of a send this device made is offered again, and what the
            // person's devices offered here waits for their answer.
            if let Some(nearby) = self.nearby() {
                nearby.pump();
            }
        }
        if self.pump_presence() {
            heard = true;
        }
        heard
    }

    /// Take every object the workspaces' live planes heard since the last call: a member's presence
    /// frame, or a call's voice or pictures. Whether any did.
    #[cfg(target_os = "linux")]
    fn hear_live(&self) -> bool {
        let Some(live) = &self.live else {
            return false;
        };
        let own = self.plane.borrow().publisher;
        let topics: Vec<(DeskWorkspaceId, String)> = self
            .plane
            .borrow()
            .workspaces
            .iter()
            .filter_map(|workspace| {
                DeskWorkspaceId::parse(&workspace.id).map(|id| (id, workspace.id.clone()))
            })
            .collect();
        let mut heard = false;
        for (ws, workspace) in topics {
            while let Some(object) = live.try_recv(ws) {
                if object.track == Track::Audio {
                    // A call's voice, already opened: the call of that workspace plays it.
                    self.call_audio
                        .borrow_mut()
                        .push((workspace.clone(), object));
                    continue;
                }
                if matches!(object.track, Track::Camera(_) | Track::Screen(_)) {
                    // A call's pictures, already opened: the call of that workspace draws them.
                    self.call_pictures
                        .borrow_mut()
                        .push((workspace.clone(), object));
                    continue;
                }
                if object.track != Track::Presence {
                    continue;
                }
                let Ok(from) = EndpointId::from_bytes(&object.sender) else {
                    continue;
                };
                if from == own {
                    continue;
                }
                self.heard_body(&workspace, from, &object.body);
                heard = true;
            }
        }
        heard
    }

    /// One member's word about themselves on `workspace` 's live plane: what they said of
    /// themselves, where their pointer went.
    /// A session takes these from its planes on every [`follow`](Self::follow). One with no plane
    /// of its own — a test, an embedder that carries the frames itself — hands them here instead.
    pub fn heard_presence(&self, workspace: &str, from: EndpointId, frame: &Presence) {
        self.heard_word(workspace, from, |board, now| {
            board.hear(*from.as_bytes(), frame, now)
        });
    }

    /// One member's presence frame as its track carried it, opened by the workspace's board: what a
    /// host that carries the frames itself (a page's endpoint) hands its session.
    pub fn heard_body(&self, workspace: &str, from: EndpointId, body: &[u8]) {
        self.heard_word(workspace, from, |board, now| {
            board.heard(*from.as_bytes(), body, now)
        });
    }

    /// Take one member's word about themselves into `workspace`'s board. Every surface that lists
    /// people draws what it changed: a pointer landing draws one frame, and one that says the same
    /// place again draws none.
    fn heard_word(
        &self,
        workspace: &str,
        from: EndpointId,
        take: impl FnOnce(&mut Board, f64) -> bool,
    ) {
        let now = now_ms();
        let changed = {
            let mut plane = self.plane.borrow_mut();
            if from == plane.publisher {
                return;
            }
            plane
                .workspaces
                .iter_mut()
                .find(|held| held.id == workspace)
                .is_some_and(|held| take(&mut held.board, now))
        };
        if !changed {
            return;
        }
        #[cfg(target_os = "linux")]
        self.note_reachability();
        self.show_here();
        self.show_waves();
    }

    /// Take one thing the plane heard, and hand the desktop who is here now: who said they are
    /// here, what they said of themselves, where their pointer went. A session's loop calls this
    /// for everything the peer actor hands it.
    pub fn hear(&self, event: Event) {
        // Every message from another member is the workspace's to admit before anything here takes
        // it: what a member the workspace's policy narrowed may send is what that policy grants
        // them, and nothing else. A refused message is dropped where it arrives, with the reason
        // said here.
        if let Some(reason) = self.refusal(&event) {
            if let Event::Msg { ws, peer, msg } = &event {
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
        #[cfg(target_os = "linux")]
        {
            // What a workspace says of its channels' governance is the session's channels' to take
            // in, wherever the message came from.
            if let (Some(link), Event::Msg { msg, .. }) = (&self.link, &event) {
                link.heard(msg);
            }
            if let Some(fabric) = &self.fabric {
                match &event {
                    Event::Sibling {
                        peer,
                        certs,
                        revocations,
                    } => fabric.sibling(*peer, certs, revocations),
                    Event::SiblingLeft { peer } => fabric.sibling_left(*peer),
                    Event::PersonalFabric {
                        peer,
                        payload,
                        received,
                    } => {
                        fabric.arrived(*peer, payload, *received);
                    }
                    _ => {}
                }
            }
            // What a device coming or going means for what Files may offer, and how far a stream
            // this device is sending got: the Nearby Send engine keeps both.
            match &event {
                Event::Sibling { .. } | Event::SiblingLeft { .. } => {
                    if let Some(nearby) = self.nearby() {
                        nearby.devices_changed();
                    }
                }
                Event::NearbyTransferProgress {
                    transfer_id,
                    sent_bytes,
                    total_bytes,
                    status,
                } => {
                    if let Some(nearby) = self.nearby() {
                        nearby.progress(*transfer_id, *sent_bytes, *total_bytes, *status);
                    }
                }
                _ => {}
            }
            // What a workspace's members say about a record's content is the records' own plane's
            // to take, and the devices it can be handed over from follow them.
            self.follow_content_sources(&event);
        }
        // A window's stream is not presence: it goes to the shares, which read it on the pass
        // ([`Self::take_surfaces`]).
        #[cfg(target_os = "linux")]
        if self.keep_surface(&event) {
            return;
        }
        #[cfg(target_os = "linux")]
        if self.keep_content(&event) {
            return;
        }
        self.plane.borrow_mut().apply(event);
        #[cfg(target_os = "linux")]
        self.note_reachability();
        self.show_here();
        self.show_waves();
    }

    /// Hand the records' content plane its place on this client: the workspaces' topics are the
    /// records' routes, and what it asks for and answers there travels with this device's own
    /// messages. A session takes part in the plane of the client it was given
    /// ([`Self::attach_desk`]).
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_content(&self, content: Arc<content::Plane>) {
        content.bind(self.outgoing.clone(), self.publisher());
        self.content.replace(Some(content));
    }

    /// Take the person's own store, opened where this session keeps its state, to their other
    /// devices: what the broker keeps their own records in, and what a sibling's writes reach this
    /// session's apps through.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_personal(&self, personal: crate::personal::Personal) {
        self.personal_store.replace(Some(personal));
    }

    /// The person's own store this session keeps, once it has one.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn personal(&self) -> Option<std::cell::Ref<'_, crate::personal::Personal>> {
        std::cell::Ref::filter_map(self.personal_store.borrow(), Option::as_ref).ok()
    }

    /// Keep what reached here of the devices a workspace's content can be handed over from: those
    /// that said anything on its topic, the hosts that serve it and the members they report, and
    /// those that left. A record's content follows them ([`content::Plane::sources_changed`]).
    #[cfg(target_os = "linux")]
    fn follow_content_sources(&self, event: &Event) {
        let Some(content) = self.content.borrow().clone() else {
            return;
        };
        let (ws, changed) = match event {
            Event::Msg { ws, peer, .. } => (
                *ws,
                self.content_devices(*ws, |devices| devices.heard(*peer)),
            ),
            Event::HostReady { ws, peer } => (
                *ws,
                self.content_devices(*ws, |devices| devices.host(*peer)),
            ),
            Event::HostMember {
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
            Event::PeerLeft { ws, peer } => (
                *ws,
                self.content_devices(*ws, |devices| devices.left(*peer)),
            ),
            _ => return,
        };
        if !changed {
            return;
        }
        let (sources, connected) = {
            let workspace = self
                .plane
                .borrow()
                .workspaces
                .iter()
                .find(|workspace| workspace.ws == ws)
                .map(|workspace| workspace.id.clone());
            let topologies = self.topologies.borrow();
            match (workspace, topologies.get(&ws)) {
                (Some(id), Some(topology)) => (
                    topology.sources(self.publisher()),
                    Some((id, topology.devices(self.publisher()))),
                ),
                _ => (ContentSources::default(), None),
            }
        };
        // The devices of the workspace this session reaches are the ones an offer round asks: one
        // that is away is not asked, and not waited for.
        if let Some(actions) = self.device_actions.get()
            && let Some((workspace, devices)) = connected
        {
            actions.here(
                &workspace,
                devices.iter().map(|device| *device.as_bytes()).collect(),
            );
        }
        content.sources_changed(ws, sources);
    }

    /// Apply `change` to the devices this session reaches in the workspace on `ws` ; whether that
    /// was news.
    #[cfg(target_os = "linux")]
    fn content_devices(
        &self,
        ws: WorkspaceId,
        change: impl FnOnce(&mut content::Topology) -> bool,
    ) -> bool {
        let mut topologies = self.topologies.borrow_mut();
        let Some(topology) = topologies.get_mut(&ws) else {
            return false;
        };
        change(topology)
    }

    /// Hand `event` to the content plane when it is one of a record's content messages; whether it
    /// was, in which case nothing else here takes it. Every such message reached here through the
    /// workspace's policy, which admitted the device that sent it.
    #[cfg(target_os = "linux")]
    fn keep_content(&self, event: &Event) -> bool {
        let Some(content) = self.content.borrow().clone() else {
            return false;
        };
        let Event::Msg { ws, peer, msg } = event else {
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

    /// Keep `event` 's message for the shares if it is one of a window's stream; whether it was.
    #[cfg(target_os = "linux")]
    fn keep_surface(&self, event: &Event) -> bool {
        let Event::Msg { ws, peer, msg } = event else {
            return false;
        };
        if !matches!(
            &**msg,
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
            msg: (**msg).clone(),
        });
        true
    }

    /// Take the window-stream messages that reached this device since the last call.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_surfaces(&self) -> Vec<ReceivedSurface> {
        std::mem::take(&mut *self.surfaces.borrow_mut())
    }

    /// Take the call audio the workspaces' live planes heard since the last call, each with the
    /// workspace it was heard in and already opened. A call plays the frames of the devices its
    /// channel says are in it.
    #[must_use]
    pub(crate) fn take_call_audio(&self) -> Vec<(String, Heard)> {
        std::mem::take(&mut *self.call_audio.borrow_mut())
    }

    /// Take the call pictures the workspaces' live planes heard since the last call, each with the
    /// workspace it was heard in and already opened. A call draws the pictures of the devices its
    /// channel says are in it.
    #[must_use]
    pub(crate) fn take_call_pictures(&self) -> Vec<(String, Heard)> {
        std::mem::take(&mut *self.call_pictures.borrow_mut())
    }

    /// Send one message of `surface` 's stream on the topic of the workspace it is shared in, to
    /// `viewers` alone: over the authenticated route to each of them, never fanned out to the
    /// workspace.
    pub(crate) fn publish_surface(
        &self,
        ws: Topic,
        msg: NeighbourMsg,
        viewers: &[neighbour_wire::PeerId],
    ) {
        let _ = self.outgoing.send(Cmd::PublishSurfaceTo {
            ws,
            msg: Box::new(msg),
            viewers: viewers.to_vec(),
        });
    }

    /// Say `msg` to `target` alone, as a workspace-control message on the topic of the workspace
    /// `ws` .
    pub(crate) fn publish_control(&self, ws: Topic, msg: NeighbourMsg, target: EndpointId) {
        let _ = self.outgoing.send(Cmd::SendControl {
            ws,
            msg: Box::new(msg),
            target,
        });
    }

    /// The topic of the workspace `workspace` , while this session takes part in it.
    #[must_use]
    pub(crate) fn workspace_topic(&self, workspace: &str) -> Option<Topic> {
        self.plane
            .borrow()
            .workspaces
            .iter()
            .find(|held| held.id == workspace)
            .map(|held| held.ws)
    }

    /// The devices heard in `workspace` beside this one, by publisher and the name each device goes
    /// by: what shows which device a window of the workspace runs on.
    #[must_use]
    pub(crate) fn workspace_devices(&self, workspace: &str) -> Vec<(EndpointId, String)> {
        self.plane
            .borrow()
            .workspaces
            .iter()
            .find(|held| held.id == workspace)
            .map(|held| {
                held.board
                    .members()
                    .iter()
                    .filter_map(|member| {
                        let publisher = EndpointId::from_bytes(&member.publisher).ok()?;
                        let name = if member.device.trim().is_empty() {
                            member.label().to_owned()
                        } else {
                            member.device.clone()
                        };
                        Some((publisher, name))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Keep what the person's other devices said of their workspaces: a device's certificates when
    /// its session comes up, and the lists it publishes with the certificate that signed them.
    fn keep_personal(&self, event: &Event) {
        match event {
            Event::Sibling { peer, certs, .. } => {
                self.siblings.borrow_mut().insert(*peer, certs.clone());
                self.sibling_arrived.set(true);
            }
            Event::SiblingLeft { peer } => {
                self.siblings.borrow_mut().remove(peer);
            }
            Event::SiblingMsg { peer, msg } => match &**msg {
                NeighbourMsg::PersonalWorkspaces { update, .. } => {
                    // The update names its author; the certificate the device was heard by is the
                    // only one that says who it is, and the list checks it against the person's
                    // root before it merges.
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
                NeighbourMsg::PersonalHosts {
                    user_public_key,
                    hosts,
                } => {
                    // Only this person's workspaces are entered, and only through a host the
                    // workspace's own governance signed for.
                    if Some(*user_public_key) != self.user {
                        return;
                    }
                    for host in hosts {
                        match postcard::from_bytes::<tilo_peer_state::join::TransportHint>(
                            &host.hint,
                        ) {
                            Ok(hint) => {
                                self.host_hints
                                    .borrow_mut()
                                    .insert(host.workspace.clone(), hint);
                            }
                            Err(error) => tracing::warn!(
                                workspace = %host.workspace,
                                %error,
                                "a device's host hint did not decode"
                            ),
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Say this person's workspaces to their other devices: every one of them whose session is up
    /// hears the list, and the ones this device has and its sender does not are opened there, each
    /// with where its host is reached, so the device that opens it enters it at its host.
    pub fn tell_personal(&self, update: &tilo_user_identity::DirectoryUpdate) {
        let Some(user) = self.user else {
            return;
        };
        let _ = self.outgoing.send(Cmd::TellSiblings(Box::new(
            NeighbourMsg::PersonalWorkspaces {
                user_public_key: user,
                update: Box::new(update.clone()),
            },
        )));
        #[cfg(target_os = "linux")]
        let hosts = self
            .engine
            .as_ref()
            .map(|engine| engine.host_hints())
            .unwrap_or_default();
        #[cfg(not(target_os = "linux"))]
        let hosts: Vec<(String, Vec<u8>)> = Vec::new();
        if hosts.is_empty() {
            return;
        }
        let _ = self
            .outgoing
            .send(Cmd::TellSiblings(Box::new(NeighbourMsg::PersonalHosts {
                user_public_key: user,
                hosts: hosts
                    .into_iter()
                    .map(|(workspace, hint)| neighbour_wire::PersonalHost { workspace, hint })
                    .collect(),
            })));
    }

    /// Take the workspace lists the person's other devices sent since the last call, each with the
    /// certificate its device was heard by.
    pub fn take_personal(
        &self,
    ) -> Vec<(
        tilo_user_identity::DirectoryUpdate,
        tilo_user_identity::DeviceCert,
    )> {
        std::mem::take(&mut *self.personal.borrow_mut())
    }

    /// Whether one of the person's other devices came up since the last call: it has heard nothing
    /// of this device's workspaces yet, whatever changed here since the list was last said.
    pub(crate) fn sibling_arrived(&self) -> bool {
        self.sibling_arrived.replace(false)
    }

    /// Tell the person's other devices what this device says of itself, when that moved since they
    /// last heard: the session's pass calls this once it offered the clipboard, so a copy leaves on
    /// the pass that made it.
    #[cfg(target_os = "linux")]
    pub fn announce_fabric(&self) {
        if let Some(fabric) = &self.fabric {
            fabric.announce_if_changed();
        }
    }

    /// This device's fabric, when the person certified it: the link the session's services act on.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn fabric(&self) -> Option<tilo_device_fabric::link::SharedLink> {
        self.fabric
            .as_ref()
            .map(|fabric| Arc::clone(fabric) as tilo_device_fabric::link::SharedLink)
    }

    /// This session's Nearby Send, when the session made one.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn nearby(&self) -> Option<Arc<crate::nearby_transfer::NearbyTransfer>> {
        self.nearby.borrow().clone()
    }

    /// Give this session's Nearby Send the fabric it runs on: what the person's devices offer and
    /// stream here, and what they say of a transfer's progress, reaches it.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_nearby(&self, nearby: Arc<crate::nearby_transfer::NearbyTransfer>) {
        if let Some(fabric) = &self.fabric {
            fabric.attach_nearby(Arc::clone(&nearby));
        }
        self.nearby.replace(Some(nearby));
    }

    /// Where this device's actions are served, and how an app here runs its actions on the person's
    /// other devices: `None` on a session that takes part as nobody, which has no key of its own to
    /// ask with.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn device_actions(&self) -> Option<DeviceActions> {
        self.device_actions.get().cloned()
    }

    /// The actions one of the person's devices sent this one since the last call, for the session's
    /// pass to carry out. A Nearby Send is carried out here instead, by this client's own transfer
    /// engine: the offer, the checkpoint, the withdrawal and the ending of a transfer are the
    /// engine's to take, whether or not a session runs on this client.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn fabric_actions(&self) -> Vec<tilo_device_fabric::PersonalFabricActionEnvelope> {
        let actions = self
            .fabric
            .as_ref()
            .map(|fabric| fabric.take_actions())
            .unwrap_or_default();
        let Some(nearby) = self.nearby() else {
            return actions;
        };
        let mut rest = Vec::with_capacity(actions.len());
        for envelope in actions {
            if crate::nearby_transfer::carried_by_engine(&envelope.action) {
                nearby.action(&envelope.source, envelope.request_id, &envelope.action);
            } else {
                rest.push(envelope);
            }
        }
        rest
    }

    /// Where other devices reach this one: the endpoint it bound, with the addresses it bound on.
    #[must_use]
    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.endpoint.clone()
    }

    /// This device's collaboration endpoint, once it has bound: where what a call carries runs,
    /// beside the workspaces' own traffic. `None` on a client taking part in no transport.
    #[must_use]
    pub fn endpoint(&self) -> Option<iroh::Endpoint> {
        self.endpoint_handle.get().cloned()
    }

    /// Where that endpoint lands, for what starts before it has bound: a call joins the endpoint
    /// the moment the plane has it.
    #[must_use]
    pub(crate) fn endpoint_handle(&self) -> Arc<std::sync::OnceLock<iroh::Endpoint>> {
        Arc::clone(&self.endpoint_handle)
    }

    /// Dial one of the person's own devices, whose address this session was told: the session
    /// between the two carries their device fabric.
    #[cfg(target_os = "linux")]
    pub fn connect(&self, addr: EndpointAddr) {
        // An action for this device is sent to where it was told to be,
        // without waiting for the two sessions to meet first.
        if let Some(actions) = self.device_actions.get() {
            actions.learn(addr.clone());
        }
        let _ = self.outgoing.send(Cmd::ConnectDevice(addr));
    }

    /// Where the person's own devices are dialled as this device's siblings from a thread of their
    /// own (the store's), once the endpoint has bound.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn sibling_dials(&self) -> Option<SiblingDials> {
        Some(SiblingDials::new(
            *self.endpoint()?.id().as_bytes(),
            self.outgoing.clone(),
        ))
    }

    /// Why what this session takes part in needs this device reachable, if it does: a workspace it
    /// shows has someone else here. A host is dialled at start and on every reconnect, and the peer
    /// actor holds the relays up for every remote party it is connected to or keeps dialling
    /// ([`tilo_endpoint_reachability::Need::Transport`]).
    #[cfg(target_os = "linux")]
    fn reachability_need(&self) -> Option<&'static str> {
        let plane = self.plane.borrow();
        let own = plane.publisher;
        plane
            .workspaces
            .iter()
            .any(|held| {
                held.board
                    .here()
                    .any(|member| member.publisher != *own.as_bytes())
            })
            .then_some("a workspace is shown with someone else")
    }

    /// Tell the relays what this session has recorded: they stay up while a workspace it shows has
    /// someone else here, and may rest once none does ([`tilo_endpoint_reachability::REST_AFTER`]).
    #[cfg(target_os = "linux")]
    fn note_reachability(&self) {
        self.reachability
            .need(Need::Session, self.reachability_need());
    }

    /// Say what this device is, now, in the workspace in front: only what changed since its last
    /// words leaves, under the board's one schedule.
    fn say_own(&self) {
        let word = self.own_word();
        let now = now_ms();
        let Some((desk, frames)) = self.plane.borrow_mut().say_in_front(&word, now) else {
            return;
        };
        self.say_frames(desk, frames);
    }

    /// Say this device's whole word in `workspace` , on joining it: a device that came into a
    /// workspace says who it is at once, so the people already there read it from the first frame.
    fn say_into(&self, workspace: &str) {
        let word = self.own_word();
        let now = now_ms();
        let (desk, frames) = {
            let mut plane = self.plane.borrow_mut();
            let Some(held) = plane
                .workspaces
                .iter_mut()
                .find(|held| held.id == workspace)
            else {
                return;
            };
            held.board.say(&word);
            (DeskWorkspaceId::parse(&held.id), held.board.flush(now))
        };
        let Some(desk) = desk else {
            return;
        };
        self.say_frames(desk, frames);
    }

    /// Take what the workspace in front's board owes now: what changed since its last burst, the
    /// loss repairs, and the slow restatement that reaches someone who came in after the first
    /// word. Whether anything left.
    fn pump_presence(&self) -> bool {
        let now = now_ms();
        let Some((desk, frames)) = self.plane.borrow_mut().due_in_front(now) else {
            return false;
        };
        self.say_frames(desk, frames);
        true
    }

    /// What this device says of itself, as its board carries it.
    fn own_word(&self) -> presence::Own {
        let wave = self.sent_wave.borrow().as_ref().map(|wave| presence::Wave {
            seq: wave.seq,
            target: wave.target.clone(),
        });
        let mut word = self.own.borrow().word(&self.device, wave);
        let says = self.says.borrow();
        word.pointer = says.pointer;
        word.shell.clone_from(&says.shell);
        word
    }

    /// Say `frames` on `ws` 's presence track, each as the object the board numbered: one object
    /// per frame, sealed with the workspace scope's keys. Nothing while this session says presence
    /// nowhere: presence is ephemeral, and the next change says where its sender is.
    fn say_frames(&self, ws: DeskWorkspaceId, frames: Vec<(u32, Presence)>) {
        #[cfg(test)]
        self.said
            .borrow_mut()
            .extend(frames.iter().map(|(_, frame)| frame.clone()));
        let Some(saying) = &self.saying else {
            return;
        };
        for (sequence, frame) in frames {
            if let Err(error) = saying.say(ws, sequence, &frame) {
                tracing::warn!(%error, "presence was not said on the live plane");
            }
        }
    }

    /// What this device said since the last call, as a session with no plane of its own would have
    /// published it.
    #[cfg(test)]
    pub(crate) fn take_said(&self) -> Vec<Presence> {
        std::mem::take(&mut *self.said.borrow_mut())
    }

    /// This device's presence handle, as a publish stamps it.
    #[cfg(target_os = "linux")]
    fn peer_hash(&self) -> u64 {
        tilo_network_actor::behaviour::peer_liveness_handle(&self.plane.borrow().publisher)
    }

    /// This device's handle in a workspace, as a window's stream names it.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn peer(&self) -> neighbour_wire::PeerId {
        self.peer_hash()
    }

    /// Whether `publisher` is this person: this device, or any device of the person this session
    /// runs for.
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

    /// Wave at `target` , a member here by their live publisher: said in the workspace in front,
    /// where the person it is for hears it once.
    pub fn wave(&self, target: &str) {
        if target.is_empty() || self.is_own_person(target) {
            return;
        }
        let seq = self.wave_seq.get().wrapping_add(1);
        self.wave_seq.set(seq);
        *self.sent_wave.borrow_mut() = Some(Wave {
            seq,
            target: target.to_owned(),
        });
        self.say_own();
    }

    /// How many times what the workspace in front knows of its people changed: a surface that draws
    /// them draws again when this moved, and asks for no frame while it holds still.
    #[must_use]
    pub fn presence_revision(&self) -> u64 {
        self.plane.borrow().revision()
    }

    /// How many times what any of the person's workspaces knows of its people changed, in one
    /// number: who is here, which window they are in and the call they are in are read again when
    /// this moved.
    #[must_use]
    pub fn people_revision(&self) -> u64 {
        self.plane
            .borrow()
            .workspaces
            .iter()
            .fold(0_u64, |seen, held| {
                seen.wrapping_mul(31).wrapping_add(held.board.revision())
            })
    }

    /// Everyone here in `workspace`, as a surface that draws them reads them: where each of them
    /// points, on the tiles the surface shows, and what they say beside their pointer.
    #[must_use]
    pub fn members(&self, workspace: &str) -> Vec<Shown> {
        self.plane.borrow().shown(workspace, now_ms())
    }

    /// How many times what `workspace` knows of its people changed: a surface that draws one
    /// workspace's people draws again when this moved, and asks for no frame while it holds still.
    #[must_use]
    pub fn here_revision(&self, workspace: &str) -> u64 {
        self.plane.borrow().revision_of(workspace)
    }

    /// This device's own word, as the roster draws it beside everyone else's here.
    #[must_use]
    pub fn own_here(&self) -> Shown {
        let publisher = *self.plane.borrow().publisher.as_bytes();
        presence::own_shown(&self.own_word(), publisher)
    }

    /// The calls in progress in every workspace this session takes part in, as those in them say:
    /// who to join, where.
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
        let publisher = *self.plane.borrow().publisher.as_bytes();
        LiveMember::of(&presence::own_shown(&self.own_word(), publisher))
    }

    /// Who is here, as the desktop lists them: `tree` places them where their focused record is
    /// shown — `records` (what each window shows, by tab) names the window each person works in —
    /// and `app_name` names an app.
    #[must_use]
    pub fn people(
        &self,
        tree: &DesktopTree,
        records: &std::collections::BTreeMap<TabId, String>,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<tilo_desktop::here::Person> {
        self.here.borrow().people(
            tree,
            &super::presence::bindings(tree, records),
            app_name,
            ui::time::unix_now_ms(),
        )
    }

    /// Who is here, written into `tree` 's active workspace as its collaborators: the roster every
    /// surface that lists people reads.
    pub fn show(
        &self,
        tree: &mut DesktopTree,
        records: &std::collections::BTreeMap<TabId, String>,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<tilo_desktop::here::Person> {
        let people = self.people(tree, records, app_name);
        tilo_desktop::here::overlay_collaborators(tree, &people);
        people
    }

    /// A client taking part in no transport: it publishes nowhere and hears only what it is handed
    /// ([`Self::hear`]). A session given one (`Options::sync`) takes part in the plane this client
    /// does, so what a test hands it is what the desktop shows.
    #[must_use]
    pub fn untethered(publisher: EndpointId, own: OwnPresence) -> Self {
        Self::planeless(publisher, own, Vec::new()).0
    }

    /// A session taking part in a plane with no transport, for tests that hand it what it would
    /// have heard and read what it would have said.
    #[cfg(test)]
    pub(crate) fn offline(
        publisher: EndpointId,
        own: OwnPresence,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        Self::planeless(publisher, own, Vec::new())
    }

    /// A client of a plane with no transport: what both the real client and [`Self::untethered`]
    /// are, at their middle. A test that reads what one plane would have sent to another takes the
    /// receiver.
    pub(crate) fn planeless(
        publisher: EndpointId,
        own: OwnPresence,
        hosts: Vec<EndpointAddr>,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        let (outgoing, said) = tokio::sync::mpsc::unbounded_channel();
        let (_unheard, arrived) = std::sync::mpsc::channel();
        let (_unlanded, enrolled) = std::sync::mpsc::channel();
        let client = Self::over(
            Transport {
                outgoing,
                arrived,
                enrolled,
                saying: None,
            },
            Device {
                publisher,
                endpoint: EndpointAddr::new(publisher),
                person: None,
                name: String::new(),
            },
            own,
        );
        for host in hosts {
            let _ = client.outgoing.send(Cmd::Connect(host));
        }
        (client, said)
    }

    /// The same plane, taken part in as `person` (their user root, hex): what a device's
    /// certificate says of it on a plane that has one, and what the workspaces' policies hold its
    /// members' messages to.
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

/// This device's collaboration identity: the key and certificate this session's endpoint runs on,
/// when this host holds one.
/// A session on a host without one takes part in no workspace as anybody: it binds a fresh key
/// and stays anonymous until the person sets a device up.
#[cfg(target_os = "linux")]
#[must_use]
pub fn device_identity() -> Option<HostIdentity> {
    HostIdentity::open(crate::platform::identity::custody()?)
}

#[cfg(target_os = "linux")]
/// The workspace hosts this session was started to dial: an always-on peer that keeps what the
/// person's devices publish while the others are away, named by `TILO_COLLAB_HOST` (one address or
/// endpoint id) or `TILO_COLLAB_HOSTS` (a JSON array of addresses).
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

#[cfg(target_os = "linux")]
/// A configured host, as its JSON address or as a bare endpoint id.
fn host_addr(value: &str) -> Option<EndpointAddr> {
    serde_json::from_str(value)
        .ok() // ignored-failure: a host value that is not JSON names no endpoint address here
        .or_else(|| value.parse::<EndpointId>().ok().map(EndpointAddr::new))
}

#[cfg(target_os = "linux")]
/// Who this device's certificate says its person is, and what the device calls itself: the id that
/// stays the person's, and the name its other devices are told apart by.
fn certified_as(identity: Option<&HostIdentity>) -> (Option<[u8; 32]>, String) {
    identity.map_or_else(
        || (None, String::new()),
        |identity| {
            (
                Some(identity.device_cert.user_public_key),
                identity.device_cert.device_label.trim().to_owned(),
            )
        },
    )
}

#[cfg(target_os = "linux")]
/// This device's fabric before the endpoint runs, so a device that proves itself at the actor's
/// first tick is recorded with it: the record it keeps is this device's, named by its certificate.
fn restored_fabric(
    identity: Option<&HostIdentity>,
    outgoing: tokio::sync::mpsc::UnboundedSender<Cmd>,
    continuity: Continuity,
) -> Option<Arc<SessionFabric>> {
    let identity = identity?;
    let device = (
        DeviceId::new(hex::encode(identity.device_cert.device_public_key)).ok()?,
        identity.device_cert.device_label.clone(),
    );
    let runtime = FabricRuntime::restored(Some(device), None).ok()?;
    Some(SessionFabric::new(
        runtime,
        outgoing,
        identity.device_cert.user_public_key,
        identity.device_signing_key.clone(),
        continuity,
    ))
}

#[cfg(target_os = "linux")]
/// The endpoint the peer actor binds, and the relays held back from it until something needs this
/// device dialable: number 0's for a device with an identity, none for a session that takes part as
/// nobody and stays on this machine's loopback.
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
    // What `presets::N0` builds, with its publisher behind number 0's directory gate and its public
    // relays held back ([`tilo_endpoint_reachability`]): binding with an empty relay map keeps net
    // report, address discovery and port mapping off the network until a workspace this session
    // shows has someone else in it. A peer's key also resolves on the local network
    // ([`tilo_endpoint_reachability::lan`]), so two of the person's devices that share one find
    // each other while their relays rest.
    let builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(key)
        .address_lookup(tilo_endpoint_reachability::collaborating())
        .address_lookup(tilo_endpoint_reachability::lan())
        .address_lookup(iroh::address_lookup::DnsAddressLookup::n0_dns())
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::empty()));
    Ok((builder, iroh::endpoint::default_relay_mode().relay_map()))
}

/// Where an admitted peer's app document goes so this device's apps see it as they see this
/// device's own writes: the broker's channels, which record it and answer an app's subscription,
/// and its fan-out, which shows it to the apps that hold its route open.
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct PeerDocuments {
    channels: Arc<std::sync::Mutex<tilo_action_broker::collab_core::BrokerChannels>>,
    fanout: Arc<tilo_action_broker::collab_broker::FanoutState>,
    /// What search reads again of a record a peer wrote.
    changes: Option<Arc<dyn tilo_action_broker::services::RecordIndexChanges>>,
}

#[cfg(target_os = "linux")]
impl PeerDocuments {
    /// Hand peer app documents to the broker's `channels` and show them to the apps through
    /// `fanout` .
    #[must_use]
    pub fn new(
        channels: Arc<std::sync::Mutex<tilo_action_broker::collab_core::BrokerChannels>>,
        fanout: Arc<tilo_action_broker::collab_broker::FanoutState>,
        changes: Option<Arc<dyn tilo_action_broker::services::RecordIndexChanges>>,
    ) -> Self {
        Self {
            channels,
            fanout,
            changes,
        }
    }

    /// Hand one admitted input's app document to this device's apps on its route.
    fn deliver(&self, input: &native_ingress::Input, report: &tilo_peer_state::AdmissionReport) {
        let Some(write) = input.app_write(report) else {
            return;
        };
        let author = input.author();
        let workspace = input.workspace().to_string();
        let (app, record) = (write.app_id.clone(), write.record_id.clone());
        tilo_action_broker::collab_inbound::deliver(
            &self.channels,
            &self.fanout,
            tilo_action_broker::collab_inbound::RemoteDocument {
                app_id: write.app_id,
                cluster_id: write.cluster_id,
                record_id: write.record_id,
                author: write.author,
                replayed: write.replayed,
                envelope: write.envelope,
            },
            |_| format!("peer@{}", hex::encode(author)),
        );
        // What search holds of the record is read again from what the peer wrote into it.
        if let Some(changes) = &self.changes {
            changes.changed(&workspace, &app, &record);
        }
    }
}

#[cfg(target_os = "linux")]
/// Hear the peer actor: what it says reaches the session, and what a workspace's host says after
/// this device joined it is taken in where this device's workspaces are kept, on a thread of its
/// own and in the host's order. What admission takes in from another device goes to this device's
/// apps through `documents`.
fn heard(
    holds_custody: bool,
    arrivals: std::sync::mpsc::Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
    link: Option<Arc<ChannelLink>>,
    documents: Option<PeerDocuments>,
) -> (
    tokio::sync::mpsc::Sender<NetEvent>,
    tokio::task::JoinHandle<()>,
    Option<tokio::task::JoinHandle<()>>,
) {
    let (admissions, attending) = if holds_custody {
        let (say, hear) = tokio::sync::mpsc::unbounded_channel();
        let attending = tokio::spawn(async move { take_in(hear, link, documents).await });
        (Some(say), Some(attending))
    } else {
        (None, None)
    };
    let (events, mut heard) = tokio::sync::mpsc::channel::<NetEvent>(256);
    let carrying = tokio::spawn(async move {
        while let Some(event) = heard.recv().await {
            if let (NetEvent::NativeInput(input), Some(say)) = (&event, admissions.as_ref()) {
                let _ = say.send(input.as_ref().clone());
            }
            if let Some(event) = event_of(event)
                && arrivals.send(event).is_err()
            {
                break;
            }
            (wake)();
        }
    });
    (events, carrying, attending)
}

#[cfg(target_os = "linux")]
/// Take what hosts replay into the workspaces this device holds, as it arrives: each run of one
/// session admitted together and in the order the host said it, on a blocking thread so no replay
/// holds up the collaboration plane.
async fn take_in(
    mut replayed: tokio::sync::mpsc::UnboundedReceiver<native_ingress::Input>,
    link: Option<Arc<ChannelLink>>,
    documents: Option<PeerDocuments>,
) {
    while let Some(input) = replayed.recv().await {
        let mut waiting = vec![input];
        while let Ok(next) = replayed.try_recv() {
            waiting.push(next);
        }
        let mut run: Vec<native_ingress::Input> = Vec::new();
        for input in waiting {
            // The channels' governance is between the workspace's devices: its governor never
            // admits it, and the channel service settles the groups with it instead.
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
                admit(run, link.as_deref(), documents.as_ref()).await;
                run = Vec::new();
            }
            run.push(input);
        }
        admit(run, link.as_deref(), documents.as_ref()).await;
    }
}

#[cfg(target_os = "linux")]
/// Admit one session's inputs, as one commit of the workspace they belong to.
async fn admit(
    inputs: Vec<native_ingress::Input>,
    link: Option<&ChannelLink>,
    documents: Option<&PeerDocuments>,
) {
    let Some(workspace) = inputs.first().map(native_ingress::Input::workspace) else {
        return;
    };
    let admitted =
        match tokio::task::spawn_blocking(move || native_ingress::Input::admit_batch(inputs)).await
        {
            Ok(admitted) => admitted,
            Err(error) => {
                tracing::warn!(%error, %workspace, "a workspace's state was not taken in");
                return;
            }
        };
    let mut replay_wanted = false;
    for (input, report) in admitted.inputs {
        match report {
            Ok(report) => {
                replay_wanted |= report.replay_wanted;
                if let Some(documents) = documents {
                    documents.deliver(&input, &report);
                }
            }
            Err(error) => {
                tracing::warn!(%error, %workspace, "a workspace's state was not taken in");
            }
        }
    }
    if let Some(link) = link {
        if replay_wanted {
            // A message waited on something the workspace's host sends of its own accord as well,
            // such as the checkpoint a sealed record is founded on: the host replays what it
            // retains, and the message is decided again.
            link.ask_replay(workspace);
        }
        if admitted.changed {
            // The workspace's governance moved with it: its channels and the devices it admits are
            // read again, and the plane told what it admits now.
            link.workspace_changed(workspace);
        }
    }
}

#[cfg(target_os = "linux")]
/// Give one command from the session to the plane: whether it was taken, or `None` when it is the
/// command that ends it.
async fn carry_out(
    plane: &tokio::sync::mpsc::Sender<NetCmd>,
    command: Cmd,
    events: &tokio::sync::mpsc::Sender<NetEvent>,
) -> Option<bool> {
    Some(match command {
        Cmd::Join(ws) => plane
            .send(NetCmd::JoinWorkspace {
                ws,
                on_event: events.clone(),
            })
            .await
            .is_ok(),
        Cmd::TakePart(connect) => plane.send(NetCmd::NativeConnect(connect)).await.is_ok(),
        Cmd::Leave(ws) => plane.send(NetCmd::LeaveWorkspace { ws }).await.is_ok(),
        Cmd::Publish { ws, msg } => plane.send(NetCmd::Publish { ws, msg: *msg }).await.is_ok(),
        Cmd::Content(delivery) => plane
            .send(NetCmd::PublishRecordContent(delivery))
            .await
            .is_ok(),
        Cmd::PublishSurfaceTo { ws, msg, viewers } => plane
            .send(NetCmd::PublishSurfaceTo {
                ws,
                msg: *msg,
                viewers,
            })
            .await
            .is_ok(),
        Cmd::SendControl { ws, msg, target } => plane
            .send(NetCmd::PublishWorkspaceControl {
                ws,
                msg: *msg,
                route: WorkspaceControlRoute::Direct { target },
            })
            .await
            .is_ok(),
        Cmd::Connect(addr) => plane
            .send(NetCmd::ConnectHost {
                addr,
                expectation: None,
            })
            .await
            .is_ok(),
        Cmd::WatchSiblings => plane
            .send(NetCmd::WatchSiblings {
                on_event: events.clone(),
            })
            .await
            .is_ok(),
        Cmd::TellSiblings(msg) => plane
            .send(NetCmd::PublishToSiblings { msg: *msg })
            .await
            .is_ok(),
        Cmd::ConnectDevice(addr) => plane.send(NetCmd::Connect { addr }).await.is_ok(),
        // A fabric frame goes to the one device it is for, or to each of the person's devices whose
        // session is up.
        Cmd::Fabric {
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
        // A staged object's bytes go to the one device that asked for them,
        // from the checkpoint it confirmed it holds.
        Cmd::NearbySpool {
            user_public_key,
            target,
            checkpoint,
            file,
        } => plane
            .send(NetCmd::PublishNearbyTransfer {
                user_public_key,
                target,
                transfer: checkpoint,
                file,
            })
            .await
            .is_ok(),
        Cmd::Replay(ws) => plane.send(NetCmd::NativeReplay { ws }).await.is_ok(),
        Cmd::PublishCustody(ws) => plane.send(NetCmd::NativePublish { ws }).await.is_ok(),
        Cmd::OpenRecord { ws, channel } => {
            plane.send(NetCmd::OpenRecord { ws, channel }).await.is_ok()
        }
        Cmd::Roster { ws, devices } => plane
            .send(NetCmd::SetWorkspaceRoster { ws, devices })
            .await
            .is_ok(),
        // Redeemed beside this loop, which never waits on a host.
        Cmd::Enroll(_) | Cmd::Enter(_) => true,
        Cmd::Stop => return None,
    })
}

/// The certificate among `certs` that says who authored `update`, if the device was heard with
/// one: a list is worth no more than the device that signed it.
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
