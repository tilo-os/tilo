//! A participant as a client of the collaboration plane: the workspaces it takes part in, this
//! person's presence in them, and everyone else's.
//!
//! The client says what a participant says and never how a host carries it. A host brings a
//! [`Transport`] and, where it keeps more of its own, a [`Host`] that takes what the plane heard
//! beside the boards and opens what only it can open.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use tilo_clock::Instant;

use collab_types::presence::{Payload, Presence, ShellPresence, Shown};
use iroh::{EndpointAddr, EndpointId};
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::{DesktopTree, WorkspaceId as DeskWorkspaceId};
use tilo_live::presence;
use tilo_live::presence::Board;

use crate::Enrolled;
use crate::plane::{Cmd, Device, Event, Outbox, SayPresence, Topic, Transport, topic_of};
use crate::presence::{OwnPresence, Plane, Workspace, now_ms};
use crate::{InCall, LiveCall};

pub use gate::Desk;

mod gate;

/// What a host keeps of the plane beside the boards: its channels, its fabric, the windows it
/// streams, the workspace state it holds.
pub trait Host: 'static {
    /// The person the workspace `workspace`'s governance admitted `device` as, when the device
    /// announced no certificate of its own here.
    fn governed_person(&self, _workspace: &str, _device: EndpointId) -> Option<String> {
        None
    }

    /// What this host keeps of one thing the plane heard; whether it was the host's alone, in which
    /// case the boards never see it.
    fn heard(&self, _event: &Event, _client: &Client) -> bool {
        false
    }

    /// Who is here, or where they are, changed: the host refreshes what it draws of that and what
    /// it needs dialable.
    fn touched(&self, _client: &Client) {}

    /// This session joined `workspace` on the plane: what its host opens there.
    fn joined(&self, _workspace: &str) {}

    /// What this host owes on every pass of [`Client::follow`].
    fn pass(&self, _client: &Client) {}
}

/// Where this person is here: their pointer over a tile, and their shell.
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
            && self.shell.as_deref().map(presence::without_clock)
                == other.shell.as_deref().map(presence::without_clock)
    }
}

/// The client of the collaboration plane: the workspaces this participant shows, this person's
/// presence in them, and everyone else's.
pub struct Client {
    plane: RefCell<Plane>,
    own: RefCell<OwnPresence>,
    /// Where this person is on this desk, as the surfaces here read it: the pointer over the tile
    /// it is on, and their screen as a shell. Said with the rest of their word, and said again only
    /// when it moves.
    says: RefCell<Where>,
    /// The count this person's last wave carried here.
    wave_seq: Cell<u32>,
    /// This device's view was asked to follow someone by its own people panel, and the pass that
    /// reads the person's gesture has not heard it yet: the press that asks to follow is not the
    /// person at work taking the view back.
    follow_asked: Cell<bool>,
    /// What this device said since the last call, for what reads it off a session that keeps no
    /// plane of its own.
    said: RefCell<Vec<Presence>>,
    /// The wave this person sent last, for their own row to say sent.
    sent_wave: RefCell<Option<presence::Wave>>,
    outgoing: Rc<dyn Outbox>,
    arrived: std::sync::mpsc::Receiver<Event>,
    /// Where this device's word of itself goes on each workspace's presence track, as the transport
    /// it says on carries it: `None` on a host that says presence nowhere.
    saying: Option<Rc<dyn SayPresence>>,
    /// What became of the enrollments asked for since the last call.
    enrolled: std::sync::mpsc::Receiver<Enrolled>,
    /// The person this device belongs to, by their root key, as its certificate names them.
    user: Option<[u8; 32]>,
    /// What this device calls itself, from its certificate: the name its person's other devices are
    /// told apart by. Empty on a device that named none.
    device: String,
    /// The desk this session shows, which the workspace gate reads every message from another
    /// member against: `None` until the session hands it over.
    desk: RefCell<Option<Rc<dyn Desk>>>,
    /// What this host keeps of the plane beside the boards.
    host: RefCell<Option<Rc<dyn Host>>>,
    /// The certificates each of the person's other devices was heard with, by its publisher.
    siblings: RefCell<std::collections::BTreeMap<EndpointId, Vec<tilo_user_identity::DeviceCert>>>,
    /// Whether one of the person's other devices came up since the last call: a device that just
    /// said hello has not heard this device's list yet, and a list said only when something changed
    /// here would never reach it.
    sibling_arrived: Cell<bool>,
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
    /// Where this device's endpoint is reached, as other devices dial it.
    endpoint: EndpointAddr,
    /// This device's endpoint itself, once it has bound: what the session's traffic runs on, and
    /// what names this device.
    endpoint_handle: std::sync::Arc<std::sync::OnceLock<iroh::Endpoint>>,
    /// The protocols this session was handed to serve beside its own, by ALPN: what it answers on
    /// this device's endpoint, so the client can say whether it serves one.
    beside: Vec<Vec<u8>>,
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
        let _ = self.outgoing.send(Cmd::Stop);
    }
}

impl Client {
    /// A client on a transport the host brings: it says what the session says through `plane`, and
    /// reads on every [`Self::follow`] what the transport delivered there.
    #[must_use]
    pub fn over(plane: Transport, device: Device, own: OwnPresence) -> Self {
        Self {
            plane: RefCell::new(Plane::new(device.publisher)),
            own: RefCell::new(own),
            says: RefCell::default(),
            wave_seq: Cell::new(0),
            follow_asked: Cell::new(false),
            said: RefCell::new(Vec::new()),
            sent_wave: RefCell::new(None),
            outgoing: plane.outgoing,
            arrived: plane.arrived,
            saying: plane.saying,
            enrolled: plane.enrolled,
            user: device.person,
            device: device.name,
            desk: RefCell::new(None),
            host: RefCell::new(None),
            siblings: RefCell::new(std::collections::BTreeMap::new()),
            host_hints: RefCell::new(std::collections::BTreeMap::new()),
            sibling_arrived: Cell::new(false),
            personal: RefCell::new(Vec::new()),
            endpoint: device.endpoint,
            endpoint_handle: std::sync::Arc::new(std::sync::OnceLock::new()),
            beside: Vec::new(),
        }
    }

    /// Hand the workspace gate the desk this session shows: whose workspaces' policies hold every
    /// member's messages to what the workspace grants them.
    pub fn attach_desk(&self, desk: Rc<dyn Desk>) {
        self.desk.borrow_mut().replace(desk);
    }

    /// Hand this client its host: what the host keeps of the plane beside the boards.
    pub fn attach_host(&self, host: Rc<dyn Host>) {
        self.host.borrow_mut().replace(host);
    }

    /// The protocols this device's endpoint answers beside the plane's own, as the host bound it.
    #[must_use]
    pub fn serving(mut self, beside: Vec<Vec<u8>>) -> Self {
        self.beside = beside;
        self
    }

    /// The endpoint this device bound, for a host that serves more on it than the plane's own
    /// traffic.
    #[must_use]
    pub fn bound_to(
        mut self,
        endpoint: std::sync::Arc<std::sync::OnceLock<iroh::Endpoint>>,
    ) -> Self {
        self.endpoint_handle = endpoint;
        self
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

    /// What this session says of itself here now.
    #[must_use]
    pub fn own(&self) -> OwnPresence {
        self.own.borrow().clone()
    }

    /// The person this device belongs to, by their root key, as its certificate names them.
    #[must_use]
    pub fn person(&self) -> Option<[u8; 32]> {
        self.user
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

    /// Follow `target` (or nobody, `None`): this person's own word on whose view is theirs. Answers
    /// whether it changed. An ask made here is not the person working, so the gesture that carried
    /// it does not take back the view it began ([`Self::follow_asked`]).
    pub fn set_following(&self, target: Option<String>) -> bool {
        let mut own = self.own();
        if own.following == target {
            return false;
        }
        own.following = target;
        self.follow_asked.set(true);
        self.set_own(own)
    }

    /// Whether this device's view was asked to follow someone and the gesture carrying the ask is
    /// still in flight: what keeps it from ending the following it began.
    #[must_use]
    pub fn follow_asked(&self) -> bool {
        self.follow_asked.get()
    }

    /// The gesture that asked to follow has ended: the next ask is a new one.
    pub fn forget_follow_asked(&self) {
        self.follow_asked.set(false);
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

    /// This device joined `call`, or left it (`None`): said again in the workspace in front, so
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

    /// Whether this person is in the call in `channel`, on any of their devices: what a Join is
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

    /// Follow `tree`'s workspaces: take part in each, leave the ones gone, and remember which is in
    /// front. Whether that changed anything.
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
        let mut left: Vec<(Topic, String)> = Vec::new();
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
                    joined.push((*ws, id.clone()));
                }
            }
            for workspace in &plane.workspaces {
                if live.iter().all(|(ws, _)| *ws != workspace.ws) {
                    left.push((workspace.ws, workspace.id.clone()));
                }
            }
            plane
                .workspaces
                .retain(|held| left.iter().all(|(ws, _)| *ws != held.ws));
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
                // A workspace joined is one this device takes part in through whatever its host
                // opens there: the desktop's custody session, which carries the workspace's app
                // records.
                if let Some(host) = self.host.borrow().clone() {
                    host.joined(&id);
                }
            }
            changed = true;
        }
        for (ws, id) in left {
            // The members take this device down where it was.
            if let Some(workspace) = DeskWorkspaceId::parse(&id) {
                self.say_frames(workspace, vec![(0, Presence::desktop(&id, Payload::Left))]);
            }
            let _ = self.outgoing.send(Cmd::Leave(ws));
            changed = true;
        }
        if front_changed {
            // A workspace coming to the front has not heard this device yet: it says who it is
            // there at once.
            self.say_own();
        }
        if changed {
            self.touched();
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
        if let Some(host) = self.host.borrow().clone() {
            host.pass(self);
        }
        if self.pump_presence() {
            heard = true;
        }
        heard
    }

    /// One member's word about themselves on `workspace`'s live plane: what they said of
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
        if changed {
            self.touched();
        }
    }

    /// Take one thing the plane heard, and hand the host who is here now: who said they are here,
    /// what they said of themselves, where their pointer went. A session's loop calls this for
    /// everything its transport hands it.
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
        // What this host keeps of the plane — its channels, its fabric, the windows it streams — is
        // the host's before the boards are: what it takes alone, the boards never see.
        if let Some(host) = self.host.borrow().clone()
            && host.heard(&event, self)
        {
            return;
        }
        self.plane.borrow_mut().apply(event);
        self.touched();
    }

    /// Take what became of the enrollments and entries asked for since the last call.
    pub fn take_enrolled(&self) -> Vec<Enrolled> {
        self.enrolled.try_iter().collect()
    }

    /// Where the workspace `workspace` is hosted, as a device of the person's that holds it said:
    /// the host hint the person's other devices send with their list. `None` while none of them
    /// said it yet.
    #[must_use]
    pub fn host_hint(&self, workspace: &str) -> Option<tilo_peer_state::join::TransportHint> {
        self.host_hints.borrow().get(workspace).cloned()
    }

    /// Say `msg` on the topic of the workspace `ws`, addressed as `command` says.
    pub fn say(&self, command: Cmd) -> bool {
        self.outgoing.send(command)
    }

    /// The workspace on `topic`, while this session takes part in it.
    #[must_use]
    pub fn workspace_id(&self, topic: Topic) -> Option<String> {
        self.plane
            .borrow()
            .workspaces
            .iter()
            .find(|held| held.ws == topic)
            .map(|held| held.id.clone())
    }

    /// The workspaces this session takes part in, by their ids: what a host with a plane of its own
    /// reads them for.
    #[must_use]
    pub fn live_workspaces(&self) -> Vec<String> {
        self.plane
            .borrow()
            .workspaces
            .iter()
            .map(|held| held.id.clone())
            .collect()
    }

    /// Whether anyone but `own` is here in a workspace this session takes part in.
    #[must_use]
    pub fn anyone_else_here(&self, own: EndpointId) -> bool {
        self.plane.borrow().workspaces.iter().any(|held| {
            held.board
                .here()
                .any(|member| member.publisher != *own.as_bytes())
        })
    }

    /// Who is here in the workspace in front, as every surface that draws them reads them: the
    /// workspace and its members, each where they point and what they say.
    #[must_use]
    pub fn front_members(&self) -> Option<(String, Vec<Shown>)> {
        let now = now_ms();
        let plane = self.plane.borrow();
        let in_front = plane
            .active
            .and_then(|ws| plane.workspaces.iter().find(|held| held.ws == ws))?;
        Some((in_front.id.clone(), in_front.shown(now)))
    }

    /// The topic of the workspace `workspace`, while this session takes part in it.
    #[must_use]
    pub fn workspace_topic(&self, workspace: &str) -> Option<Topic> {
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
    pub fn workspace_devices(&self, workspace: &str) -> Vec<(EndpointId, String)> {
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
    /// with where its host is reached, so the device that opens it enters it at its host. `hosts`
    /// is where this device's workspaces are hosted, as its own state names them.
    pub fn tell_personal(
        &self,
        update: &tilo_user_identity::DirectoryUpdate,
        hosts: Vec<(String, Vec<u8>)>,
    ) {
        let Some(user) = self.user else {
            return;
        };
        let _ = self.outgoing.send(Cmd::TellSiblings(Box::new(
            NeighbourMsg::PersonalWorkspaces {
                user_public_key: user,
                update: Box::new(update.clone()),
            },
        )));
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
    pub fn sibling_arrived(&self) -> bool {
        self.sibling_arrived.replace(false)
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
    pub fn endpoint_handle(&self) -> std::sync::Arc<std::sync::OnceLock<iroh::Endpoint>> {
        std::sync::Arc::clone(&self.endpoint_handle)
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

    /// Say this device's whole word in `workspace`, on joining it: a device that came into a
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

    /// Say `frames` on `ws`'s presence track, each as the object the board numbered: one object per
    /// frame, sealed with the workspace scope's keys. Nothing while this session says presence
    /// nowhere: presence is ephemeral, and the next change says where its sender is.
    fn say_frames(&self, ws: DeskWorkspaceId, frames: Vec<(u32, Presence)>) {
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
    pub fn take_said(&self) -> Vec<Presence> {
        std::mem::take(&mut *self.said.borrow_mut())
    }

    /// Whether `publisher` is this person: this device, or any other device of the person this
    /// session runs for.
    fn is_own_person(&self, publisher: &str) -> bool {
        let Ok(bytes) = <[u8; 32]>::try_from(hex::decode(publisher).unwrap_or_default()) else {
            return false;
        };
        let plane = self.plane.borrow();
        if bytes == *plane.publisher.as_bytes() {
            return true;
        }
        let own_user = self
            .own
            .borrow()
            .user
            .clone()
            .filter(|user| !user.is_empty());
        let Some(own_user) = own_user else {
            return false;
        };
        plane.workspaces.iter().any(|held| {
            held.board
                .member(bytes)
                .and_then(|member| member.user.as_deref())
                .is_some_and(|user| user == own_user)
        })
    }

    /// Wave at `target`, a member here by their live publisher: said in the workspace in front,
    /// where the person it is for hears it once.
    pub fn wave(&self, target: &str) {
        if target.is_empty() || self.is_own_person(target) {
            return;
        }
        let seq = self.wave_seq.get().wrapping_add(1);
        self.wave_seq.set(seq);
        *self.sent_wave.borrow_mut() = Some(presence::Wave {
            seq,
            target: target.to_owned(),
        });
        self.say_own();
    }

    /// Who is here, or where they are, changed: what the host draws of that is refreshed.
    fn touched(&self) {
        if let Some(host) = self.host.borrow().clone() {
            host.touched(self);
        }
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

    /// This device's own word as the boards hold it, for a host that draws the roster.
    #[must_use]
    pub fn own_word_here(&self) -> presence::Own {
        self.own_word()
    }

    /// A client taking part in no transport: it publishes nowhere and hears only what it is handed
    /// ([`Self::hear`]). A session given one (`Options::sync`) takes part in the plane this client
    /// does, so what a test hands it is what the desktop shows.
    #[must_use]
    pub fn untethered(publisher: EndpointId, own: OwnPresence) -> Self {
        Self::planeless(publisher, own, Vec::new()).0
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
                outgoing: Rc::new(outgoing),
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
    /// The same plane, taken part in as `person` (their user root, hex): what a device's
    /// certificate says of it on a plane that has one, and what the workspaces' policies hold its
    /// members' messages to.
    #[must_use]
    pub fn taking_part_as(mut self, person: &str) -> Self {
        self.as_person(person);
        self
    }

    /// Take part as `person` (their user root, hex), as a device's certificate says.
    pub fn as_person(&mut self, person: &str) {
        let mut root = [0u8; 32];
        assert!(
            hex::decode_to_slice(person, &mut root).is_ok(),
            "a person is their root key"
        );
        self.user = Some(root);
    }
}

/// The certificate among `certs` that says who authored `update`, if the device was heard with one:
/// a list is worth no more than the device that signed it.
pub fn author_of(
    certs: &[tilo_user_identity::DeviceCert],
    update: &tilo_user_identity::DirectoryUpdate,
) -> Option<tilo_user_identity::DeviceCert> {
    certs
        .iter()
        .find(|cert| cert.device_public_key == update.author_device_public_key)
        .cloned()
}
