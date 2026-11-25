//! A member as a client of the peer network: the workspaces it takes part in, this
//! person's presence in them, and everyone else's.
//!
//! The client says what a member says and never how a host carries it. A host brings a
//! [`Transport`] and, where it keeps more of its own, a [`Host`] that takes what the peer network heard
//! beside the boards and opens what only it can open.

use std::cell::RefCell;
use std::rc::Rc;

use collab_types::presence::{Presence as Frame, ShellPresence};
use iroh_base::{EndpointAddr, EndpointId};
use tilo_desktop_model::Layout;

use crate::network::{Cmd, Device, Event, Outbox, Topic, Transport, topic_of};
use crate::presence::OwnPresence;

pub use message_policy::Shell;
pub use presence::Presence;

mod message_policy;
mod presence;

/// What a host keeps of the peer network beside the boards: its channels, its fabric, the windows it
/// streams, the workspace state it holds.
pub trait Host: 'static {
    /// The person the workspace `workspace`'s governance admitted `device` as, when the device
    /// announced no certificate of its own here.
    fn admitted_person(&self, _workspace: &str, _device: EndpointId) -> Option<String> {
        None
    }

    /// What this host keeps of one thing the peer network heard; whether it was the host's alone, in which
    /// case the boards never see it.
    fn heard(&self, _event: &Event, _client: &Client) -> bool {
        false
    }

    /// Who is here, or where they are, changed: the host refreshes what it draws of that and what
    /// it needs dialable.
    fn touched(&self, _client: &Client) {}

    /// This session joined `workspace` on the peer network: what its host opens there.
    fn joined(&self, _workspace: &str) {}

    /// The device `workspace`'s governance pins as its host, as this device verified it: the only
    /// one whose word for the runs it executes this session hears ([`Client::set_workspace_host`]).
    /// `None` when this host cannot tell, which leaves the host the board has.
    fn workspace_host(&self, _workspace: &str) -> Option<[u8; 32]> {
        None
    }

    /// What this host owes on every pass of [`Client::follow`].
    fn pass(&self, _client: &Client) {}
}

/// The client of the peer network: the workspaces this member shows and this person's
/// presence in them — [`Presence`], which any host's own thread can hold — beside what this host
/// keeps of the peer network and of the person's other devices.
pub struct Client {
    presence: Presence,
    /// Where this session says what it says to the peer network.
    outgoing: std::sync::Arc<dyn Outbox>,
    /// What the peer network heard, as this session reads it.
    arrived: std::sync::mpsc::Receiver<Event>,
    /// The person this device belongs to, by their root key, as its certificate names them.
    user: Option<[u8; 32]>,
    /// The shell this session shows, which the workspace check reads every message from another
    /// member against: `None` until the session hands it over.
    shell: RefCell<Option<Rc<dyn Shell>>>,
    /// What this host keeps of the peer network beside the boards.
    host: RefCell<Option<Rc<dyn Host>>>,
    /// Where this device's endpoint is reached, as other devices dial it.
    endpoint: EndpointAddr,
    /// This device's endpoint itself, once it has bound: what the session's traffic runs on, and
    /// what names this device.
    #[cfg(not(target_arch = "wasm32"))]
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
    /// A client on a transport the host brings: it says what the session says through `network`, and
    /// reads on every [`Self::follow`] what the transport delivered there.
    #[must_use]
    pub fn over(network: Transport, device: Device, own: OwnPresence) -> Self {
        let Transport {
            outgoing,
            arrived,
            saying,
        } = network;
        Self {
            presence: Presence::over(saying, &device, own),
            outgoing,
            arrived,
            user: device.person,
            shell: RefCell::new(None),
            host: RefCell::new(None),
            endpoint: device.endpoint,
            #[cfg(not(target_arch = "wasm32"))]
            endpoint_handle: std::sync::Arc::new(std::sync::OnceLock::new()),
            beside: Vec::new(),
        }
    }

    /// Hand the workspace check the shell this session shows: whose workspaces' policies hold every
    /// member's messages to what the workspace grants them.
    pub fn attach_shell(&self, shell: Rc<dyn Shell>) {
        self.shell.borrow_mut().replace(shell);
    }

    /// Hand this client its host: what the host keeps of the peer network beside the boards.
    pub fn attach_host(&self, host: Rc<dyn Host>) {
        self.host.borrow_mut().replace(host);
    }

    /// The protocols this device's endpoint answers beside the peer network's own, as the host bound it.
    #[must_use]
    pub fn serving(mut self, beside: Vec<Vec<u8>>) -> Self {
        self.beside = beside;
        self
    }

    /// The endpoint this device bound, for a host that serves more on it than the peer network's own
    /// traffic.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn bound_to(
        mut self,
        endpoint: std::sync::Arc<std::sync::OnceLock<iroh::Endpoint>>,
    ) -> Self {
        self.endpoint_handle = endpoint;
        self
    }

    /// Whether this session answers `alpn` on this device's endpoint: what it was handed beside its
    /// own peer networks, from the pass the endpoint bound.
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
    pub fn follow_desktop(&self, tree: &Layout) -> bool {
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
    /// [`Self::follow_desktop`] follows a layout's: take part in each of `live`, leave the ones gone,
    /// and remember `active` as the one in front. Whether that changed anything.
    pub fn follow_workspaces(&self, live: &[(Topic, String)], active: Option<Topic>) -> bool {
        let outgoing = &self.outgoing;
        let changed = self.presence.hold(
            live,
            active,
            |workspace, topic| {
                let _ = outgoing.send(Cmd::Join(topic));
                // A workspace joined is one this device takes part in through whatever its host
                // opens there: the desktop's ownership session, which carries the workspace's app
                // records.
                if let Some(host) = self.host.borrow().clone() {
                    host.joined(workspace);
                }
            },
            |topic| {
                let _ = outgoing.send(Cmd::Leave(topic));
            },
        );
        // Each workspace's board hears the runs its host speaks for from that host alone. A host
        // that cannot tell (a page's session, which holds no workspace of its own and names the
        // host it was admitted by itself) leaves the board's host as it is: taking its "don't
        // know" for "nobody" dropped every run that host speaks for on each pass.
        let hosted = self.host.borrow().clone().is_some_and(|host| {
            live.iter().fold(false, |moved, (_, workspace)| {
                host.workspace_host(workspace)
                    .is_some_and(|device| self.presence.set_host(workspace, Some(device)))
                    || moved
            })
        });
        let changed = changed || hosted;
        if changed {
            self.touched();
        }
        changed
    }

    /// Take `host` as the device `workspace`'s governance pins as its host, for a session whose
    /// host names it itself (a page, with the host its session was admitted by): the workspace's
    /// board hears what that host says for the runs it executes, one member per run.
    pub fn set_workspace_host(&self, workspace: &str, host: Option<[u8; 32]>) {
        if self.presence.set_host(workspace, host) {
            self.touched();
        }
    }

    /// Speak for the agents this device executes ([`Presence::speak_for`]), and hand the host who
    /// is here when that changed.
    pub fn speak_for(&self, sessions: &[(String, String, String, ShellPresence)]) {
        if self.presence.speak_for(sessions) {
            self.touched();
        }
    }

    /// Hear everything that reached the peer network since the last call. Whether anything did.
    pub fn follow(&self) -> bool {
        let mut heard = false;
        while let Ok(event) = self.arrived.try_recv() {
            self.hear(event);
            heard = true;
        }
        if let Some(host) = self.host.borrow().clone() {
            host.pass(self);
        }
        let (said, forgot) = self.presence.pump_presence();
        if forgot {
            // A member gone quiet was taken down: who is here changed.
            self.touched();
        }
        said || forgot || heard
    }

    /// One member's word about themselves on `workspace`'s live session: what they said of
    /// themselves, where their pointer went.
    /// A session takes these from its peer networks on every [`follow`](Self::follow). One with no peer network
    /// of its own — a test, an embedder that carries the frames itself — hands them here instead.
    pub fn heard_presence(&self, workspace: &str, from: EndpointId, frame: &Frame) {
        let heard = self.presence.heard_presence(workspace, from, frame);
        if heard | self.know_admitted(workspace, from) {
            self.touched();
        }
    }

    /// One member's presence frame as its track carried it, opened by the workspace's board: what a
    /// host that carries the frames itself (a page's endpoint) hands its session.
    pub fn heard_body(&self, workspace: &str, from: EndpointId, body: &[u8]) {
        let heard = self.presence.heard_body(workspace, from, body);
        if heard | self.know_admitted(workspace, from) {
            self.touched();
        }
    }

    /// Know `from`, heard in `workspace` without a certificate of theirs, as the person the
    /// workspace's governance admitted their device as: every surface that draws them — the bar's
    /// face, the People list, their pointer, their caret in an app — then draws one person, by one
    /// name and in one colour. Whether that is new here.
    fn know_admitted(&self, workspace: &str, from: EndpointId) -> bool {
        self.presence.unknown_person(workspace, from)
            && self
                .admitted_person(workspace, from)
                .is_some_and(|person| self.presence.admitted_as(workspace, from, person))
    }

    /// The person the workspace `workspace`'s governance admitted `device` as: who a member is
    /// whose device announced no certificate here.
    #[must_use]
    pub fn admitted_person(&self, workspace: &str, device: EndpointId) -> Option<String> {
        self.host
            .borrow()
            .as_ref()
            .and_then(|host| host.admitted_person(workspace, device))
    }

    /// Take one thing the peer network heard, and hand the host who is here now: who said they are here,
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
                    ws = %ws,
                    peer = %peer,
                    kind = msg.kind(),
                    %reason,
                    "dropping a workspace message"
                );
            }
            return;
        }
        // What this host keeps of the peer network — its channels, its fabric, the windows it streams — is
        // the host's before the boards are: what it takes alone, the boards never see.
        if let Some(host) = self.host.borrow().clone()
            && host.heard(&event, self)
        {
            return;
        }
        self.presence.heard(event);
        self.touched();
    }

    /// Say `command` to the peer network.
    pub fn say(&self, command: Cmd) -> bool {
        self.outgoing.send(command).is_ok()
    }

    /// Where other devices reach this one, as it answers now: the endpoint it bound, with the
    /// addresses and the relay it is reached through at this moment. A device that has not bound
    /// one is reached where it was handed.
    #[must_use]
    pub fn endpoint_addr(&self) -> EndpointAddr {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(endpoint) = self.endpoint_handle.get() {
            return endpoint.addr();
        }
        self.endpoint.clone()
    }

    /// This device's collaboration endpoint, once it has bound: where what a call carries runs,
    /// beside the workspaces' own traffic. `None` on a client taking part in no transport.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn endpoint(&self) -> Option<iroh::Endpoint> {
        self.endpoint_handle.get().cloned()
    }

    /// Where that endpoint lands, for what starts before it has bound: a call joins the endpoint
    /// the moment the peer network has it.
    #[cfg(not(target_arch = "wasm32"))]
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
    /// ([`Self::hear`]). A session given one (`Options::sync`) takes part in the peer network this client
    /// does, so what a test hands it is what the desktop shows.
    #[must_use]
    pub fn untethered(publisher: EndpointId, own: OwnPresence) -> Self {
        Self::without_network(publisher, own, Vec::new()).0
    }

    /// A client of a peer network with no transport: what both the real client and [`Self::untethered`]
    /// are, at their middle. A test that reads what one peer network would have sent to another takes the
    /// receiver.
    pub(crate) fn without_network(
        publisher: EndpointId,
        own: OwnPresence,
        hosts: Vec<EndpointAddr>,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        let (outgoing, said) = tokio::sync::mpsc::unbounded_channel();
        let (_unheard, arrived) = std::sync::mpsc::channel();
        let client = Self::over(
            Transport {
                outgoing: std::sync::Arc::new(outgoing),
                arrived,
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

    /// The same peer network, taken part in as `person` (their user root, hex): what a device's
    /// certificate says of it on a peer network that has one, and what the workspaces' policies hold its
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
