//! A participant as a client of the collaboration plane: the workspaces it takes part in, this
//! person's presence in them, and everyone else's.
//!
//! The client says what a participant says and never how a host carries it. A host brings a
//! [`Transport`] and, where it keeps more of its own, a [`Host`] that takes what the plane heard
//! beside the boards and opens what only it can open.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use collab_types::presence::Presence as Frame;
use iroh::{EndpointAddr, EndpointId};
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::DesktopTree;

use crate::Enrolled;
use crate::plane::{Cmd, Device, Event, Outbox, Topic, Transport, topic_of};
use crate::presence::OwnPresence;

pub use gate::Desk;
pub use presence::Presence;

mod gate;
mod presence;

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

/// The client of the collaboration plane: the workspaces this participant shows and this person's
/// presence in them — [`Presence`], which any host's own thread can hold — beside what this host
/// keeps of the plane and of the person's other devices.
pub struct Client {
    presence: Presence,
    /// Where this session says what it says to the plane.
    outgoing: std::sync::Arc<dyn Outbox>,
    /// What the plane heard, as this session reads it.
    arrived: std::sync::mpsc::Receiver<Event>,
    /// The person this device belongs to, by their root key, as its certificate names them.
    user: Option<[u8; 32]>,
    /// What became of the enrollments asked for since the last call.
    enrolled: std::sync::mpsc::Receiver<Enrolled>,
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
        std::fmt::Debug::fmt(&self.presence, f)
    }
}

impl std::ops::Deref for Client {
    type Target = Presence;

    fn deref(&self) -> &Self::Target {
        &self.presence
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
        let Transport {
            outgoing,
            arrived,
            enrolled,
            saying,
        } = plane;
        Self {
            presence: Presence::over(saying, &device, own),
            outgoing,
            arrived,
            user: device.person,
            enrolled,
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

    /// Whether this session answers `alpn` on this device's endpoint: what it was handed beside its
    /// own planes, from the pass the endpoint bound.
    #[must_use]
    pub fn serves(&self, alpn: &[u8]) -> bool {
        self.beside.iter().any(|served| served == alpn)
    }

    /// The person this device belongs to, by their root key, as its certificate names them.
    #[must_use]
    pub fn person(&self) -> Option<[u8; 32]> {
        self.user
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
        let outgoing = &self.outgoing;
        let changed = self.presence.hold(
            live,
            active,
            |workspace, topic| {
                let _ = outgoing.send(Cmd::Join(topic));
                // A workspace joined is one this device takes part in through whatever its host
                // opens there: the desktop's custody session, which carries the workspace's app
                // records.
                if let Some(host) = self.host.borrow().clone() {
                    host.joined(workspace);
                }
            },
            |topic| {
                let _ = outgoing.send(Cmd::Leave(topic));
            },
        );
        if changed {
            self.touched();
        }
        changed
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
        self.presence.pump_presence() || heard
    }

    /// One member's word about themselves on `workspace`'s live plane: what they said of
    /// themselves, where their pointer went.
    /// A session takes these from its planes on every [`follow`](Self::follow). One with no plane
    /// of its own — a test, an embedder that carries the frames itself — hands them here instead.
    pub fn heard_presence(&self, workspace: &str, from: EndpointId, frame: &Frame) {
        if self.presence.heard_presence(workspace, from, frame) {
            self.touched();
        }
    }

    /// One member's presence frame as its track carried it, opened by the workspace's board: what a
    /// host that carries the frames itself (a page's endpoint) hands its session.
    pub fn heard_body(&self, workspace: &str, from: EndpointId, body: &[u8]) {
        if self.presence.heard_body(workspace, from, body) {
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
        self.presence.heard(event);
        self.touched();
    }

    /// Say `command` to the plane.
    pub fn say(&self, command: Cmd) -> bool {
        self.outgoing.send(command)
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
        let _ = self.say(Cmd::TellSiblings(Box::new(
            NeighbourMsg::PersonalWorkspaces {
                user_public_key: user,
                update: Box::new(update.clone()),
            },
        )));
        if hosts.is_empty() {
            return;
        }
        let _ = self.say(Cmd::TellSiblings(Box::new(NeighbourMsg::PersonalHosts {
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

    /// Who is here, or where they are, changed: what the host draws of that is refreshed.
    fn touched(&self) {
        if let Some(host) = self.host.borrow().clone() {
            host.touched(self);
        }
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
                outgoing: std::sync::Arc::new(outgoing),
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
            let _ = client.say(Cmd::Connect(host));
        }
        (client, said)
    }

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
