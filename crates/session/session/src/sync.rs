//! Who is here with this person: the session as a client of the
//! collaboration plane.
//!
//! The session takes part in the room each workspace is. It says this
//! person's presence on the room's topic — that they are here, what they say
//! they are doing, how available they are, whose view they follow, where
//! their pointer is — and it hears everyone else's from there, as one
//! [`PeerPresence`] under the room's workspace scope. The wire is the
//! collaboration plane's own message ([`NeighbourMsg::AppPresenceV2`]), so a
//! browser, an agent and this session are present to each other alike, and
//! the session needs nothing of its own to be one of them.
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

use collab_types::presence::{Availability, PointerSpace, PresenceKey};
use collab_types::{PeerPresence, PeerPresencePayload};
use iroh::{EndpointAddr, EndpointId};
use neighbour_wire::NeighbourMsg;
use tilo_desktop::chrome::WaveNotice;
use tilo_desktop::here::{Here, LiveMember, Wave};
use tilo_desktop_model::{DesktopTree, TabId};
use tilo_endpoint_reachability::{Need, Reachability};
use tilo_network_actor::{HostIdentity, NetCmd, NetEvent, WorkspaceId, room_topic_id};

/// The app id the desktop of a room says its own presence under, as the
/// browser, the agents and every app reading a room expect it.
const DESKTOP_PRESENCE_APP: &str = "tilo-shell";

/// How often this person says again where they are, so someone whose host
/// session came up after the first word still hears them, and so a room never
/// mistakes nobody speaking for nobody being here.
const PRESENCE_AGAIN: Duration = Duration::from_secs(20);

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
    /// Whose view they follow, by that member's publisher.
    pub following: Option<String>,
}

/// Someone else here, as their presence and their certificate tell it.
#[derive(Clone, Debug, PartialEq)]
struct Peer {
    /// Their device's publisher: the endpoint id every room member names it
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
    /// Where their pointer is: the tab of this room its tile shows, and the
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
        }
    }
}

/// One room this session takes part in, and who it heard is here.
#[derive(Debug)]
struct Room {
    /// The room's topic.
    ws: WorkspaceId,
    /// The room, as its id travels: the uuid string every publish names.
    id: String,
    /// Everyone heard here, in the order they arrived.
    peers: Vec<Peer>,
}

impl Room {
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
                    PeerPresencePayload::Availability { state, .. } => {
                        peer.availability =
                            Availability::from_id(state).unwrap_or(Availability::Available);
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

    /// Where their pointer moved on the tile of a tab of this room.
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
    /// person it belongs to.
    fn certified(&mut self, from: EndpointId, cert: &tilo_user_identity::DeviceCert) {
        self.with(from, |peer| {
            peer.user = Some(hex::encode(cert.user_public_key));
            let label = cert.device_label.trim();
            if !label.is_empty() {
                label.clone_into(&mut peer.label);
            }
        });
    }

    /// They left the room.
    fn depart(&mut self, publisher: EndpointId) {
        self.peers.retain(|peer| peer.publisher != publisher);
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

/// The rooms this session takes part in, and the one in front.
#[derive(Debug)]
struct Plane {
    /// This device's publisher.
    publisher: EndpointId,
    rooms: Vec<Room>,
    /// The room in front: whose roster the desktop shows.
    active: Option<WorkspaceId>,
}

impl Plane {
    fn room(&mut self, ws: WorkspaceId) -> Option<&mut Room> {
        self.rooms.iter_mut().find(|room| room.ws == ws)
    }

    /// Take one event from the plane.
    fn apply(&mut self, event: NetEvent) {
        match event {
            NetEvent::Msg { ws, peer, msg, .. } => self.message(ws, peer, msg),
            NetEvent::PeerLeft { ws, peer, .. } => {
                if let Some(room) = self.room(ws) {
                    room.depart(peer);
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
            .rooms
            .iter()
            .find(|room| room.ws == ws)
            .map(|room| room.id.clone())
        else {
            return;
        };
        let Some(room) = self.room(ws) else {
            return;
        };
        match msg {
            NeighbourMsg::AppPresenceV2 {
                app_id,
                cluster_id,
                presence,
                ..
            } if app_id == DESKTOP_PRESENCE_APP && cluster_id == id => {
                room.hear(from, &presence);
            }
            NeighbourMsg::PresenceMoved { key, point, .. } => {
                if let Some(key) = pointer_key(&key) {
                    room.point(from, &key, point);
                }
            }
            NeighbourMsg::PresenceCleared { key, .. } => {
                if let Some(key) = pointer_key(&key) {
                    room.clear_point(from, &key);
                }
            }
            NeighbourMsg::IdentityHello { cert, .. } => room.certified(from, &cert),
            _ => {}
        }
    }

    /// Everyone whose pointer is over `tab` of `room` now, by publisher, if
    /// this session takes part in that room.
    fn pointers(&self, room: &str, tab: u64) -> Vec<(String, f64, f64)> {
        self.rooms
            .iter()
            .find(|held| held.id == room)
            .map(|held| held.pointers(tab))
            .unwrap_or_default()
    }
}

/// Where a moved pointer was, from the key a pointer message names it by:
/// the wire's own JSON of a [`PresenceKey`].
fn pointer_key(key: &str) -> Option<PresenceKey> {
    serde_json::from_str(key).ok()
}

/// One room's address, as a publish names it.
fn room_key(room: &str) -> Vec<u8> {
    postcard::to_stdvec(&PresenceKey::Workspace {
        id: room.to_owned(),
    })
    .unwrap_or_default()
}

/// What the sync thread is asked to do.
#[derive(Debug)]
enum Outgoing {
    /// Take part in the topic of a room.
    Join(WorkspaceId),
    /// Stop taking part in the topic of a room.
    Leave(WorkspaceId),
    /// Publish `msg` on the topic of a room.
    Publish {
        ws: WorkspaceId,
        msg: Box<NeighbourMsg>,
    },
    /// Dial a workspace host: an always-on peer that keeps what this
    /// person's devices publish while the others are away.
    Connect(EndpointAddr),
    /// Hear the person's other devices from now on.
    WatchSiblings,
    /// Say `msg` to each of the person's devices whose session is up.
    TellSiblings(Box<NeighbourMsg>),
    /// The session is going: stop the peer actor.
    Stop,
}

/// The session's client of the collaboration plane: the rooms of the
/// workspaces it shows, this person's presence in them, and everyone else's.
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
    /// The person this device belongs to, by their root key, as its
    /// certificate names them.
    user: Option<[u8; 32]>,
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
    /// This device's relays, held back until a room this session takes part
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
                "rooms",
                &plane
                    .rooms
                    .iter()
                    .map(|room| room.id.as_str())
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
    /// start the peer actor, and from then on hear the rooms the session
    /// joins.
    ///
    /// `identity` is this device's collaboration identity, on whose key the
    /// endpoint runs: with it, every room is told which device and which
    /// person is here. Without one — a test, a stand-in session — the
    /// session is a device nobody certified and stays anonymous in rooms.
    ///
    /// # Errors
    /// The endpoint could not be bound.
    pub fn start(
        identity: Option<HostIdentity>,
        own: OwnPresence,
        hosts: Vec<EndpointAddr>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<Self> {
        let (outgoing, commands) = tokio::sync::mpsc::unbounded_channel();
        let (arrivals, arrived) = std::sync::mpsc::channel();
        let (ready, bound) = std::sync::mpsc::channel();
        let user = identity
            .as_ref()
            .map(|identity| identity.device_cert.user_public_key);
        let thread = std::thread::Builder::new()
            .name("tilo-sync".into())
            .spawn(move || run(identity, commands, arrivals, ready, wake))?;
        let (publisher, reachability) = bound
            .recv()
            .map_err(std::io::Error::other)?
            .map_err(std::io::Error::other)?;
        let sync = Self {
            plane: RefCell::new(Plane {
                publisher,
                rooms: Vec::new(),
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
            user,
            siblings: RefCell::new(std::collections::BTreeMap::new()),
            personal: RefCell::new(Vec::new()),
            reachability,
            thread: Some(thread),
        };
        // The person's other devices are heard from the start, so a list one
        // of them published is taken even when this session shows none of
        // their workspaces yet.
        let _ = sync.outgoing.send(Outgoing::WatchSiblings);
        // The hosts next: a room joined before them is replayed to this
        // device when its host session comes up, never lost.
        for host in hosts {
            let _ = sync.outgoing.send(Outgoing::Connect(host));
        }
        Ok(sync)
    }

    /// This device's publisher here: the endpoint id every room names it by.
    #[must_use]
    pub fn publisher(&self) -> EndpointId {
        self.plane.borrow().publisher
    }

    /// The desktop's presence, shared with whatever draws who is here.
    #[must_use]
    pub fn here(&self) -> &Here {
        &self.here
    }

    /// What this session says of itself: changed here, it is said again in
    /// the room in front. Whether it changed.
    pub fn set_own(&self, own: OwnPresence) -> bool {
        if *self.own.borrow() == own {
            return false;
        }
        *self.own.borrow_mut() = own;
        self.say_again();
        true
    }

    /// Follow `tree`'s workspaces: take part in the room of each, leave the
    /// ones gone, and remember which is in front. Whether that changed
    /// anything.
    pub fn follow_desktop(&self, tree: &DesktopTree) -> bool {
        let live: Vec<(WorkspaceId, String)> = tree
            .workspaces
            .iter()
            .filter(|workspace| !workspace.room().is_nil())
            .map(|workspace| {
                (
                    room_topic_id(workspace.room()),
                    workspace.room().to_string(),
                )
            })
            .collect();
        let active = tree
            .active_workspace()
            .filter(|workspace| !workspace.room().is_nil())
            .map(|workspace| room_topic_id(workspace.room()));
        let mut joined = Vec::new();
        let mut left = Vec::new();
        let mut changed = false;
        {
            let mut plane = self.plane.borrow_mut();
            for (ws, id) in &live {
                if plane.rooms.iter().all(|room| room.ws != *ws) {
                    plane.rooms.push(Room {
                        ws: *ws,
                        id: id.clone(),
                        peers: Vec::new(),
                    });
                    joined.push((*ws, id.clone()));
                }
            }
            for room in &plane.rooms {
                if live.iter().all(|(ws, _)| *ws != room.ws) {
                    left.push(room.ws);
                }
            }
            plane.rooms.retain(|room| !left.contains(&room.ws));
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
        if self.said_at.get().elapsed() >= PRESENCE_AGAIN {
            self.say_again();
        }
        heard
    }

    /// Take one thing the plane heard, and hand the desktop who is here now.
    fn hear(&self, event: NetEvent) {
        self.keep_personal(&event);
        self.plane.borrow_mut().apply(event);
        self.note_reachability();
        self.show_here();
        self.show_waves();
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
            .rooms
            .iter()
            .any(|room| {
                room.peers
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
    /// room in front.
    fn show_here(&self) {
        let (own, members) = {
            let plane = self.plane.borrow();
            let members = plane
                .active
                .and_then(|ws| plane.rooms.iter().find(|room| room.ws == ws))
                .map(Room::members)
                .unwrap_or_default();
            (self.own_member(), members)
        };
        self.here.borrow_mut().set_members(Some(own), Some(members));
    }

    /// Say this person's presence again in the room in front.
    fn say_again(&self) {
        let (ws, id) = {
            let plane = self.plane.borrow();
            let Some(ws) = plane.active else {
                return;
            };
            let Some(room) = plane.rooms.iter().find(|room| room.ws == ws) else {
                return;
            };
            (ws, room.id.clone())
        };
        self.say(ws, &id);
        self.said_at.set(Instant::now());
    }

    /// Say all of this person's presence in the room `room`, on its topic
    /// `ws`.
    fn say(&self, ws: WorkspaceId, room: &str) {
        let own = self.own.borrow().clone();
        let mut payloads = vec![
            PeerPresencePayload::Arrived,
            PeerPresencePayload::Availability {
                state: own.availability.id().to_owned(),
                call: None,
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
                cluster_id: room.to_owned(),
                presence: PeerPresence {
                    key: room_key(room),
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

    /// Tell the room in front that this person's pointer is over `(x, y)` of
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
        let Some(room) = plane.rooms.iter().find(|room| room.ws == ws) else {
            return;
        };
        let key = postcard::to_stdvec(&PresenceKey::Pointer {
            space: PointerSpace::Tile {
                room: room.id.clone(),
                tab,
            },
        })
        .unwrap_or_default();
        let msg = NeighbourMsg::AppPresenceV2 {
            peer: tilo_network_actor::behaviour::peer_liveness_handle(&plane.publisher),
            app_id: DESKTOP_PRESENCE_APP.into(),
            cluster_id: room.id.clone(),
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
    /// room in front, where the person it is for hears it once.
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
        let Some(room) = plane.rooms.iter().find(|room| room.ws == ws) else {
            return;
        };
        let msg = NeighbourMsg::AppPresenceV2 {
            peer: tilo_network_actor::behaviour::peer_liveness_handle(&plane.publisher),
            app_id: DESKTOP_PRESENCE_APP.into(),
            cluster_id: room.id.clone(),
            presence: PeerPresence {
                key: room_key(&room.id),
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

    /// Everyone whose pointer is over `tab` of `room` now, by publisher. The
    /// desktop draws each over its own tile for that tab, above the windows.
    #[must_use]
    pub fn pointers(&self, room: &str, tab: u64) -> Vec<(String, f64, f64)> {
        self.plane.borrow().pointers(room, tab)
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
        let (outgoing, said) = tokio::sync::mpsc::unbounded_channel();
        let (_unheard, arrived) = std::sync::mpsc::channel();
        for host in hosts {
            let _ = outgoing.send(Outgoing::Connect(host));
        }
        (
            Self {
                plane: RefCell::new(Plane {
                    publisher,
                    rooms: Vec::new(),
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
                user: None,
                siblings: RefCell::new(std::collections::BTreeMap::new()),
                personal: RefCell::new(Vec::new()),
                reachability,
                thread: None,
            },
            said,
        )
    }
}

/// This device's collaboration identity: the key and certificate this
/// session's endpoint runs on, when this machine has been provisioned.
///
/// A session on a machine without one takes part in no room as anybody: it
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

/// The thread: bind the endpoint, run the peer actor, and carry what it
/// hears to the session's loop until it is told to stop.
fn run(
    identity: Option<HostIdentity>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Outgoing>,
    arrivals: std::sync::mpsc::Sender<NetEvent>,
    ready: std::sync::mpsc::Sender<Result<(EndpointId, Reachability), String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(format!(
                "no runtime for the collaboration plane: {error}"
            )));
            return;
        }
    };
    runtime.block_on(async move {
        let lookup = iroh::address_lookup::memory::MemoryLookup::new();
        let (builder, deferred_relays) = match endpoint_for(identity.as_ref()) {
            Ok(building) => building,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        let endpoint = match builder
            .address_lookup(lookup.clone())
            .transport_config(tilo_network_actor::transport_config())
            .bind()
            .await
        {
            Ok(endpoint) => endpoint,
            Err(error) => {
                let _ = ready.send(Err(format!("the collaboration endpoint: {error}")));
                return;
            }
        };
        let publisher = endpoint.id();
        // Nothing wakes the held-back relays from here: the peer actor does
        // when it dials a host or a peer, and the session while a room it
        // shows has someone else in it (`Client::note_reachability`).
        let reachability = tilo_endpoint_reachability::Reachability::defer(
            &endpoint,
            &deferred_relays,
            tokio::runtime::Handle::current(),
            Some(tilo_endpoint_reachability::REST_AFTER),
        );
        let (actor, actor_task) = tilo_network_actor::Actor::spawn_deferring_relays(
            endpoint,
            lookup,
            identity,
            Arc::new(std::sync::Mutex::new(Vec::new())),
            reachability.clone(),
        );
        let (events, mut heard) = tokio::sync::mpsc::channel::<NetEvent>(256);
        let carrying = tokio::spawn(async move {
            while let Some(event) = heard.recv().await {
                if arrivals.send(event).is_err() {
                    break;
                }
                (wake)();
            }
        });
        if ready.send(Ok((publisher, reachability))).is_err() {
            return;
        }
        while let Some(command) = commands.recv().await {
            let Some(taken) = carry_out(&actor, command, &events).await else {
                break;
            };
            if !taken {
                tracing::debug!("the collaboration plane stopped taking commands");
                break;
            }
        }
        drop(events);
        carrying.abort();
        actor_task.abort();
    });
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
        Outgoing::Stop => return None,
    })
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
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_desktop_model::{RoomId, Workspace};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert};

    /// The room the tests' only workspace is.
    const ROOM: &str = "6f1b4a7e-0b2f-4d3a-9a1e-2c3d4e5f6a7b";

    /// A room no test's desktop shows.
    const OTHER_ROOM: &str = "0a2c4e6f-8b1d-4f3a-9c5e-7d9b1f3a5c7e";

    /// A device's publisher, from a seed: an endpoint id nobody's identity
    /// certifies.
    fn device(seed: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn topic(room: &str) -> WorkspaceId {
        room_topic_id(RoomId::parse(room).expect("a room id"))
    }

    /// A desktop showing one workspace, in `room`.
    fn desk(room: &str) -> DesktopTree {
        let room = RoomId::parse(room).expect("a room id");
        let id = tilo_desktop_model::WorkspaceId::of_room(room);
        assert_eq!(topic(&room.as_uuid().to_string()), id.raw());
        let workspace = Workspace::in_room(id, room, "Desk");
        DesktopTree {
            workspaces: vec![workspace.clone()],
            active: workspace.id,
            ..DesktopTree::default()
        }
    }

    /// A device of the person's, certified for their root key, and the key
    /// it signs with.
    fn certified(user: u8, device_seed: u8, label: &str) -> (DeviceCert, SigningKey) {
        let person = SigningKey::from_bytes(&[user; 32]);
        let this = SigningKey::from_bytes(&[device_seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: this.verifying_key().to_bytes(),
            device_label: label.to_owned(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
        (cert, this)
    }

    /// A device's workspace list for `room`, signed by that device.
    fn list_of(
        device: &DeviceCert,
        signing: &SigningKey,
        room: &str,
    ) -> tilo_user_identity::DirectoryUpdate {
        let mut directory = tilo_user_identity::WorkspaceDirectory::new(
            device.user_public_key,
            device.device_public_key,
        );
        let room = RoomId::parse(room).expect("a room id");
        directory
            .add_room_workspace(room_topic_id(room), &room.to_string(), "Desk")
            .expect("a listed workspace");
        let update = directory
            .signed_snapshot(signing, tilo_clock::unix_now_ns())
            .expect("a signed list");
        assert_eq!(update.author_device_public_key, device.device_public_key);
        update
    }

    /// A session in `ROOM`'s room with no transport, and what it would have
    /// said.
    fn session(
        label: &str,
        user: Option<u8>,
    ) -> (Client, tokio::sync::mpsc::UnboundedReceiver<Outgoing>) {
        let own = OwnPresence {
            label: label.to_owned(),
            user: user.map(|user| hex::encode([user; 32])),
            ..OwnPresence::default()
        };
        let (sync, said) = Client::offline(device(0xf0), own);
        sync.follow_desktop(&desk(ROOM));
        (sync, said)
    }

    fn sent(said: &mut tokio::sync::mpsc::UnboundedReceiver<Outgoing>) -> Vec<Outgoing> {
        let mut commands = Vec::new();
        while let Ok(command) = said.try_recv() {
            commands.push(command);
        }
        commands
    }

    /// One presence a peer said, on `room`'s topic.
    fn presence(from: u8, room: &str, payload: PeerPresencePayload) -> NetEvent {
        let from = device(from);
        NetEvent::Msg {
            ws: topic(room),
            peer: from,
            membership_epoch: None,
            replayed: false,
            msg: NeighbourMsg::AppPresenceV2 {
                peer: neighbour_wire::peer_handle(from.as_bytes()),
                app_id: DESKTOP_PRESENCE_APP.to_owned(),
                cluster_id: room.to_owned(),
                presence: PeerPresence {
                    key: room_key(room),
                    payload,
                },
                colour: None,
            },
        }
    }

    fn here(from: u8) -> NetEvent {
        presence(from, ROOM, PeerPresencePayload::Arrived)
    }

    /// A device's identity announcement: which person it is a device of,
    /// and what to call it.
    fn hello(from: u8, user: u8, device_label: &str) -> NetEvent {
        let from = device(from);
        let cert = DeviceCert {
            user_public_key: [user; 32],
            device_public_key: *from.as_bytes(),
            device_label: device_label.to_owned(),
            capabilities: DeviceCapabilities {
                can_own_workspaces: false,
                can_admit_devices: false,
                can_act_as_host: false,
                can_read_all_workspaces: false,
                foreign: false,
                can_administer_devices: false,
            },
            issued_at_ns: 0,
            expires_at_ns: None,
            signature: [0; 64],
        };
        NetEvent::Msg {
            ws: topic(ROOM),
            peer: from,
            membership_epoch: None,
            replayed: false,
            msg: NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(cert),
                revocations: Vec::new(),
            },
        }
    }

    /// Where a peer's pointer moved, on the tile of `tab` of `room`.
    fn pointer_moved(from: u8, room: &str, tab: u64, point: (f64, f64)) -> NetEvent {
        let from = device(from);
        NetEvent::Msg {
            ws: topic(room),
            peer: from,
            membership_epoch: None,
            replayed: false,
            msg: NeighbourMsg::PresenceMoved {
                peer: neighbour_wire::peer_handle(from.as_bytes()),
                user: String::new(),
                key: serde_json::to_string(&PresenceKey::Pointer {
                    space: PointerSpace::Tile {
                        room: room.to_owned(),
                        tab,
                    },
                })
                .expect("a tile pointer key"),
                point,
                ts: 0,
                colour: None,
            },
        }
    }

    fn pointer_cleared(from: u8, room: &str, tab: u64) -> NetEvent {
        let from = device(from);
        NetEvent::Msg {
            ws: topic(room),
            peer: from,
            membership_epoch: None,
            replayed: false,
            msg: NeighbourMsg::PresenceCleared {
                peer: neighbour_wire::peer_handle(from.as_bytes()),
                key: serde_json::to_string(&PresenceKey::Pointer {
                    space: PointerSpace::Tile {
                        room: room.to_owned(),
                        tab,
                    },
                })
                .expect("a tile pointer key"),
            },
        }
    }

    #[test]
    fn a_list_is_kept_only_from_the_device_that_said_it() {
        let (laptop, laptop_key) = certified(0x04, 0x11, "Laptop");
        let (phone, _) = certified(0x04, 0x12, "Phone");
        let by_laptop = list_of(&laptop, &laptop_key, ROOM);

        let kept = author_of(&[phone.clone(), laptop.clone()], &by_laptop)
            .expect("the laptop's own list is the laptop's");
        assert_eq!(kept.device_public_key, laptop.device_public_key);

        // Someone else's device, and a list the laptop's session carried but
        // another device signed: neither says the laptop said it.
        let (stranger, _) = certified(0x55, 0x13, "Stranger");
        assert!(author_of(&[stranger], &by_laptop).is_none());
        assert!(author_of(&[phone], &by_laptop).is_none());
        assert!(author_of(&[], &by_laptop).is_none());
    }

    #[test]
    fn a_peer_here_is_a_collaborator_of_the_active_workspace() {
        let (sync, _said) = session("Quinten", Some(0x04));
        sync.hear(hello(1, 0xaa, "Framework 16"));
        sync.hear(here(1));

        let mut tree = desk(ROOM);
        let people = sync.show(&mut tree, &|_| None);
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].label, "Framework 16");
        assert_eq!(people[0].user, Some(hex::encode([0xaa; 32])));

        let collaborators = &tree.workspaces[0].collaborators;
        assert_eq!(collaborators.len(), 1);
        assert_eq!(collaborators[0].id, device(1).to_string());
        assert_eq!(collaborators[0].name, "Framework 16");
        assert!(collaborators[0].cursor_color.is_some());
        assert_eq!(collaborators[0].role_id, None);
    }

    #[test]
    fn what_a_peer_says_they_are_doing_is_the_roster_line() {
        let (sync, _said) = session("Quinten", Some(0x04));
        sync.hear(hello(1, 0xaa, "Framework 16"));
        sync.hear(here(1));
        sync.hear(presence(
            1,
            ROOM,
            PeerPresencePayload::Status {
                text: "  Shipping the roster  ".to_owned(),
                automatic: false,
            },
        ));

        let mut tree = desk(ROOM);
        let people = sync.show(&mut tree, &|_| None);
        assert_eq!(
            people[0].status.as_ref().map(|line| line.text.as_str()),
            Some("Shipping the roster")
        );
    }

    #[test]
    fn a_peer_who_leaves_leaves_the_roster() {
        let (sync, _said) = session("Quinten", Some(0x04));
        sync.hear(hello(1, 0xaa, "Framework 16"));
        sync.hear(here(1));
        assert_eq!(sync.show(&mut desk(ROOM), &|_| None).len(), 1);

        sync.hear(presence(1, ROOM, PeerPresencePayload::Left));
        assert!(sync.show(&mut desk(ROOM), &|_| None).is_empty());

        sync.hear(here(1));
        assert_eq!(sync.show(&mut desk(ROOM), &|_| None).len(), 1);
        sync.hear(NetEvent::PeerLeft {
            ws: topic(ROOM),
            peer: device(1),
            cause: tilo_network_actor::PeerDepartureCause::HostSessionClosed,
            membership_epoch: None,
        });
        assert!(sync.show(&mut desk(ROOM), &|_| None).is_empty());

        let mut tree = desk(ROOM);
        sync.show(&mut tree, &|_| None);
        assert!(tree.workspaces[0].collaborators.is_empty());
    }

    /// Two relays nobody listens on: this endpoint never has a home relay
    /// to be published with, so the directory gate cannot open, and nothing
    /// here waits on a network.
    fn unused_relays() -> iroh::RelayMap {
        ["http://127.0.0.1:9/", "http://127.0.0.1:10/"]
            .into_iter()
            .map(|url| url.parse::<iroh::RelayUrl>().expect("a relay url"))
            .collect()
    }

    #[test]
    fn the_relays_wait_until_a_workspace_with_someone_else_is_shown() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime for the collaboration endpoint");
        // The endpoint the real client binds, its directory gated and its
        // relays held back.
        let gate = tilo_endpoint_reachability::collaborating();
        let (endpoint, reachability) = runtime.block_on(async {
            let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .address_lookup(gate.clone())
                .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::empty()))
                .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
                .expect("a loopback address")
                .secret_key(iroh::SecretKey::generate())
                .bind()
                .await
                .expect("a collaboration endpoint binds");
            let reachability = Reachability::defer(
                &endpoint,
                &unused_relays(),
                tokio::runtime::Handle::current(),
                Some(tilo_endpoint_reachability::REST_AFTER),
            );
            (endpoint, reachability)
        });
        let own = OwnPresence {
            label: "Quinten".into(),
            ..OwnPresence::default()
        };
        let (sync, _said) =
            Client::offline_reaching(device(0xf0), own, Vec::new(), reachability.clone());
        sync.follow_desktop(&desk(ROOM));

        // Nobody else in the workspace this desktop shows: it names no relay,
        // and number 0's directory is told nothing of this device.
        assert!(reachability.is_dormant());
        assert_eq!(reachability.holders(), (None, None));
        assert!(!gate.is_open());
        assert_eq!(endpoint.addr().relay_urls().count(), 0);

        // Someone else is here: what could dial this device starts.
        sync.hear(here(1));
        assert!(!reachability.is_dormant());
        assert_eq!(
            reachability.holders(),
            (None, Some("a workspace is shown with someone else"))
        );

        // They leave: nothing here needs this device dialable again.
        sync.hear(presence(1, ROOM, PeerPresencePayload::Left));
        assert_eq!(reachability.holders(), (None, None));

        // Someone in a workspace this desktop does not show needs nothing
        // of this device.
        sync.hear(presence(2, OTHER_ROOM, PeerPresencePayload::Arrived));
        assert_eq!(reachability.holders().1, None);
    }

    #[test]
    fn only_the_room_in_front_lists_people() {
        let (sync, _said) = session("Quinten", Some(0x04));
        sync.hear(hello(1, 0xaa, "Framework 16"));
        sync.hear(here(1));

        let elsewhere = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
        sync.hear(presence(2, elsewhere, PeerPresencePayload::Arrived));
        assert_eq!(sync.show(&mut desk(ROOM), &|_| None).len(), 1);

        // Presence said on this room's topic under another room's name is
        // that other room's.
        let mut foreign = here(3);
        if let NetEvent::Msg { msg, .. } = &mut foreign
            && let NeighbourMsg::AppPresenceV2 { cluster_id, .. } = msg
        {
            *cluster_id = elsewhere.to_owned();
        }
        sync.hear(foreign);
        assert_eq!(sync.show(&mut desk(ROOM), &|_| None).len(), 1);
    }

    #[test]
    fn another_apps_presence_is_not_the_desktops() {
        let (sync, _said) = session("Quinten", Some(0x04));
        let mut event = here(1);
        if let NetEvent::Msg { msg, .. } = &mut event
            && let NeighbourMsg::AppPresenceV2 { app_id, .. } = msg
        {
            *app_id = "tilo-notes".to_owned();
        }
        sync.hear(event);
        assert!(sync.show(&mut desk(ROOM), &|_| None).is_empty());
    }

    #[test]
    fn this_device_is_not_someone_here() {
        let (sync, _said) = session("Quinten", Some(0x04));
        let mut own = here(0xf0);
        if let NetEvent::Msg { peer, .. } = &mut own {
            *peer = device(0xf0);
        }
        sync.hear(own);
        assert!(sync.show(&mut desk(ROOM), &|_| None).is_empty());
    }

    #[test]
    fn two_devices_of_one_peer_are_one_face() {
        let (sync, _said) = session("Quinten", Some(0x04));
        sync.hear(hello(1, 0xaa, "Framework 16"));
        sync.hear(here(1));
        sync.hear(hello(2, 0xaa, "Phone"));
        sync.hear(here(2));

        let people = sync.show(&mut desk(ROOM), &|_| None);
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].label, "Framework 16");
    }

    #[test]
    fn another_device_of_this_person_is_this_person() {
        let (sync, _said) = session("Quinten", Some(0x04));
        sync.hear(hello(1, 0x04, "Studio"));
        sync.hear(here(1));
        assert!(sync.show(&mut desk(ROOM), &|_| None).is_empty());

        sync.hear(hello(2, 0xbb, "Framework 16"));
        sync.hear(here(2));
        assert_eq!(sync.show(&mut desk(ROOM), &|_| None).len(), 1);
    }

    #[test]
    fn pointers_are_followed_on_the_room_tile() {
        let (sync, _said) = session("Quinten", Some(0x04));
        sync.hear(hello(1, 0xaa, "Framework 16"));
        sync.hear(here(1));
        assert!(sync.pointers(ROOM, 3).is_empty());

        sync.hear(pointer_moved(1, ROOM, 3, (12.5, 34.5)));
        assert_eq!(
            sync.pointers(ROOM, 3),
            vec![(device(1).to_string(), 12.5, 34.5)]
        );
        assert!(sync.pointers(ROOM, 4).is_empty());
        assert!(
            sync.pointers("0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c", 3)
                .is_empty()
        );

        sync.hear(pointer_cleared(1, ROOM, 3));
        assert!(sync.pointers(ROOM, 3).is_empty());
    }

    #[test]
    fn a_wave_reaches_the_person_it_is_for() {
        let (sync, _said) = session("Quinten", Some(0x04));
        sync.hear(hello(1, 0xaa, "Framework 16"));
        sync.hear(here(1));
        let me = device(0xf0).to_string();
        sync.hear(presence(
            1,
            ROOM,
            PeerPresencePayload::Wave { seq: 1, target: me },
        ));
        assert!(
            sync.here().borrow_mut().take_waves().is_empty(),
            "the client takes the wave and hands it to the bar"
        );
        let notice = sync.wave_notice().expect("the bar has a wave to say");
        assert_eq!(notice.from, "Framework 16");

        // A wave at someone else here is not this person's news: the bar
        // keeps saying the wave at this person.
        sync.hear(hello(2, 0xbb, "Desktop"));
        sync.hear(here(2));
        sync.hear(presence(
            2,
            ROOM,
            PeerPresencePayload::Wave {
                seq: 1,
                target: device(9).to_string(),
            },
        ));
        assert_eq!(
            sync.wave_notice().map(|notice| notice.from),
            Some("Framework 16".to_owned()),
            "a wave at someone else here is not this person's news"
        );
    }

    #[test]
    fn waving_says_a_new_wave_each_time() {
        let (sync, mut said) = session("Quinten", None);
        sent(&mut said);
        let target = device(1).to_string();
        sync.wave(&target);
        sync.wave(&target);
        let waves: Vec<(u32, String)> = payloads(&mut said)
            .into_iter()
            .filter_map(|payload| match payload {
                PeerPresencePayload::Wave { seq, target } => Some((seq, target)),
                _ => None,
            })
            .collect();
        assert_eq!(waves, vec![(1, target.clone()), (2, target)]);
        assert_eq!(
            sync.own_member().wave.map(|wave| wave.seq),
            Some(2),
            "this person's row reads their last wave as sent"
        );

        // Waving at this person says nothing: there is no one to greet.
        sync.wave(&device(0xf0).to_string());
        assert!(
            payloads(&mut said).is_empty(),
            "a wave at this person goes nowhere"
        );
    }

    #[test]
    fn a_configured_host_is_read_as_an_id_or_an_address() {
        let host = device(7);
        assert_eq!(host_addr(&host.to_string()).map(|addr| addr.id), Some(host));
        let json = serde_json::to_string(&EndpointAddr::new(host)).unwrap();
        assert_eq!(host_addr(&json).map(|addr| addr.id), Some(host));
        assert!(host_addr("").is_none());
        assert!(host_addr("not an address").is_none());
    }

    #[test]
    fn a_host_is_dialled_before_the_rooms_it_carries() {
        let host = device(7);
        let (sync, mut said) = Client::offline_at(
            device(0xf0),
            OwnPresence::default(),
            vec![EndpointAddr::new(host)],
        );
        sync.follow_desktop(&desk(ROOM));
        let commands = sent(&mut said);
        let connect = commands
            .iter()
            .position(|command| matches!(command, Outgoing::Connect(addr) if addr.id == host));
        let join = commands
            .iter()
            .position(|command| matches!(command, Outgoing::Join(_)));
        assert_eq!(connect, Some(0), "the host is dialled first");
        assert_eq!(join, Some(1), "the room is joined after its host");
    }

    #[test]
    fn taking_part_says_where_this_person_is() {
        let (sync, mut said) = session("Quinten", None);
        let commands = sent(&mut said);
        assert!(matches!(commands[0], Outgoing::Join(ws) if ws == topic(ROOM)));

        let published: Vec<(WorkspaceId, Box<NeighbourMsg>)> = commands
            .into_iter()
            .filter_map(|command| match command {
                Outgoing::Publish { ws, msg } => Some((ws, msg)),
                _ => None,
            })
            .collect();
        assert!(!published.is_empty());
        for (ws, msg) in published {
            assert_eq!(ws, topic(ROOM));
            let NeighbourMsg::AppPresenceV2 {
                peer,
                app_id,
                cluster_id,
                presence,
                ..
            } = *msg
            else {
                panic!("not a presence");
            };
            assert_eq!(
                peer,
                neighbour_wire::peer_handle(sync.publisher().as_bytes())
            );
            assert_eq!(app_id, DESKTOP_PRESENCE_APP);
            assert_eq!(cluster_id, ROOM);
            assert_eq!(
                postcard::from_bytes::<PresenceKey>(&presence.key).expect("a key"),
                PresenceKey::Workspace {
                    id: ROOM.to_owned()
                }
            );
        }
    }

    #[test]
    fn changing_what_this_person_says_is_said_again() {
        let (sync, mut said) = session("Quinten", None);
        sent(&mut said);

        let stated = OwnPresence {
            label: "Quinten".to_owned(),
            status: Some("Wiring the plane".to_owned()),
            ..OwnPresence::default()
        };
        assert!(sync.set_own(stated.clone()));
        let said_payloads = payloads(&mut said);
        assert!(said_payloads.contains(&PeerPresencePayload::Status {
            text: "Wiring the plane".to_owned(),
            automatic: false,
        }));

        assert!(!sync.set_own(stated));
        assert!(payloads(&mut said).is_empty());
    }

    #[test]
    fn leaving_a_workspace_leaves_its_room() {
        let (sync, mut said) = session("Quinten", None);
        sent(&mut said);
        sync.follow_desktop(&DesktopTree::default());
        assert!(matches!(sent(&mut said)[0], Outgoing::Leave(ws) if ws == topic(ROOM)));
    }

    fn payloads(
        commands: &mut tokio::sync::mpsc::UnboundedReceiver<Outgoing>,
    ) -> Vec<PeerPresencePayload> {
        sent(commands)
            .into_iter()
            .filter_map(|command| match command {
                Outgoing::Publish { msg, .. } => match *msg {
                    NeighbourMsg::AppPresenceV2 { presence, .. } => Some(presence.payload),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }
}
