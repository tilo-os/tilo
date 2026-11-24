//! This desktop's half of the peer network: the peer actor on this device's own iroh
//! endpoint, the live session, the workspaces this session holds and their channels, the person's
//! device fabric, the transfers between their devices, and the roster the bar draws.
//!
//! The member itself is `tilo_session_client::Client`, which every host links. [`Client`] here
//! is that client plus this desktop's host. The endpoint keeps number 0's directory behind a check
//! and its relays held back ([`tilo_endpoint_reachability`]): a desktop with nobody else in its
//! workspaces publishes nothing and keeps no relay.

use std::cell::RefCell;
use std::rc::Rc;
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use iroh::{EndpointAddr, EndpointId};
#[cfg(target_os = "linux")]
use tilo_desktop_model::Layout;
#[cfg(target_os = "linux")]
use tilo_endpoint_reachability::Nearby;
#[cfg(target_os = "linux")]
use tilo_live::live::Track;
#[cfg(target_os = "linux")]
use tilo_live::live_session::Heard;
#[cfg(target_os = "linux")]
use tilo_server_protocol::live::{LiveFrame, OpsFrame};
use tilo_session_client::client::Host;
#[cfg(target_os = "linux")]
use tilo_session_client::client::Shell as PolicyShell;
#[cfg(target_os = "linux")]
use tilo_session_client::message_policy::WorkspaceAction;
use tilo_session_client::{Client as SessionClient, Device, OwnPresence, Transport};
#[cfg(target_os = "linux")]
use tilo_session_client::{Cmd, Event, Outbox, Topic};
#[cfg(target_os = "linux")]
use tilo_session_core::desktop::here::{Here, LiveMember, Person};
#[cfg(target_os = "linux")]
use tilo_session_core::desktop::state::DesktopHandle;

#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
use crate::platform::fabric::DeviceId;
#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
use crate::platform::fabric::{Continuity, Reach, SessionFabric};
#[cfg(target_os = "linux")]
use tilo_endpoint_reachability::{Need, Reachability};
#[cfg(target_os = "linux")]
use tilo_identity::identity_store::DeviceIdentity;
#[cfg(target_os = "linux")]
use tilo_network_actor::WorkspaceId;
#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
mod network;

#[cfg(target_os = "linux")]
use network::{Serving, Wire, run};

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

/// What this desktop's endpoint carries beside the client's own commands: the ownership sessions and
/// records its workspaces are held through, its device fabric, and the transfers between the
/// person's devices.
#[derive(Debug)]
#[cfg(target_os = "linux")]
pub(crate) enum HostCmd {
    /// One of the client's own commands.
    Network(Cmd),
    /// Send this device's fabric to the person's other devices, or to the one at `target`.
    Fabric {
        user_public_key: [u8; 32],
        target: Option<EndpointId>,
        payload: tilo_network_actor::PersonalFabricPayload,
    },
}

/// One host's end of its transport, as the client says its own commands through it.
#[derive(Clone, Debug)]
#[cfg(target_os = "linux")]
pub(crate) struct HostOutbox {
    commands: tokio::sync::mpsc::UnboundedSender<HostCmd>,
}

#[cfg(target_os = "linux")]
impl Outbox for HostOutbox {
    fn send(&self, command: Cmd) -> Result<(), tilo_session_client::Closed> {
        self.network(command)
            .then_some(())
            .ok_or(tilo_session_client::Closed)
    }
}

#[cfg(target_os = "linux")]
impl HostOutbox {
    /// An outbox over `commands`, for a service that speaks the peer network's vocabulary and nothing
    /// else.
    #[cfg(test)]
    pub(crate) fn of(commands: tokio::sync::mpsc::UnboundedSender<HostCmd>) -> Self {
        Self { commands }
    }

    /// Say one of the client's own commands to the peer network; `false` when the transport is gone.
    pub(crate) fn network(&self, command: Cmd) -> bool {
        self.say(HostCmd::Network(command))
    }

    /// Say one command of this desktop's own to the peer network.
    pub(crate) fn say(&self, command: HostCmd) -> bool {
        self.commands.send(command).is_ok()
    }

    /// Send this device's fabric to the person's other devices, or to the one at `target`.
    pub(crate) fn fabric(
        &self,
        user_public_key: [u8; 32],
        target: Option<EndpointId>,
        payload: tilo_network_actor::PersonalFabricPayload,
    ) {
        let _ = self.say(HostCmd::Fabric {
            user_public_key,
            target,
            payload,
        });
    }
}

/// What the peer actor and the fabric said that only this desktop keeps: which of the person's
/// devices proved itself on a session with this one, and the session's end, the fabric traffic they
/// send.
#[derive(Debug)]
#[cfg(target_os = "linux")]
enum DesktopEvent {
    /// One of the person's devices proved itself on its session with this one, or proved itself
    /// anew there: it comes before anything that device sent.
    Sibling {
        peer: EndpointId,
        certs: Vec<tilo_user_identity::DeviceCert>,
        revocations: Vec<tilo_user_identity::RevocationEntry>,
    },
    /// The session with one of the person's devices ended.
    SiblingLeft { peer: EndpointId },
    PersonalFabric {
        peer: EndpointId,
        payload: tilo_network_actor::PersonalFabricPayload,
        /// When the frame reached this device, read where it was taken off the wire.
        received: std::time::Instant,
    },
    /// The host of the workspace on topic `ws` said its owner deleted it.
    WorkspaceDeleted { ws: Topic },
}

/// The person's other devices, dialled as this device's siblings: the session between two of their
/// sessions is where each tells the other the workspaces it holds, and what it hosts, so one can adopt
/// what the other keeps.
#[derive(Clone)]
#[cfg(target_os = "linux")]
pub(crate) struct SiblingDials {
    /// This device's endpoint id: its own session is never dialled.
    ours: [u8; 32],
    outgoing: HostOutbox,
    /// The relay this device's settings name, when they name one: the
    /// person's devices are configured with the same relay, so a sibling is
    /// reached through it while its key resolves nowhere else.
    relay: Option<iroh::RelayUrl>,
}

#[cfg(target_os = "linux")]
impl SiblingDials {
    /// Dial from `ours`, the endpoint this device's sessions run on, through
    /// `relay` when the person's settings name one.
    pub(crate) fn new(ours: [u8; 32], outgoing: HostOutbox, relay: Option<iroh::RelayUrl>) -> Self {
        Self {
            ours,
            outgoing,
            relay,
        }
    }

    /// Dial `device` as a sibling, unless it is this device. The peer network dials again whatever has no
    /// session, for as long as that lasts.
    pub(crate) fn dial(&self, device: [u8; 32]) {
        if device == self.ours {
            return;
        }
        let Ok(id) = iroh::EndpointId::from_bytes(&device) else {
            tracing::warn!(device = %hex::encode(device), "a device of the person has no endpoint id");
            return;
        };
        let mut addr = EndpointAddr::new(id);
        if let Some(relay) = &self.relay {
            addr = addr.with_relay_url(relay.clone());
        }
        let _ = self.outgoing.send(Cmd::ConnectDevice(addr));
    }
}

/// Everything this device keeps of the peer network beside the client's boards: who is here as the bar
/// lists them, the workspaces' state and channels, the live session, the fabric and the transfers.
#[cfg(target_os = "linux")]
pub(crate) struct Local {
    /// The session this session shows, whose workspaces' policies every message from another member is
    /// held to.
    session: RefCell<Option<DesktopHandle>>,
    /// Where this device's own commands go: the client's vocabulary is its own, this is the
    /// desktop's own beside it.
    commands: tokio::sync::mpsc::UnboundedSender<HostCmd>,
    /// What the peer actor and the fabric said that only this desktop keeps.
    desktop_events: std::sync::mpsc::Receiver<DesktopEvent>,
    /// The call audio the workspaces' live sessions heard since the last call, each with the
    /// workspace it was heard in: what the calls playing it take ([`Self::take_call_audio`]).
    call_audio: RefCell<Vec<(String, Heard)>>,
    /// The call pictures the workspaces' live sessions heard since the last call, each with the
    /// workspace it was heard in: what the calls playing there draw
    /// ([`Self::take_call_pictures`]).
    call_pictures: RefCell<Vec<(String, Heard)>>,
    /// The shared windows' pictures and pages the workspaces' live sessions heard since the last
    /// call, each with the workspace it was heard in: what the sessions showing those windows draw
    /// ([`Self::take_window_objects`]).
    window_objects: RefCell<Vec<(String, Heard)>>,
    /// What shared windows' viewers and hosts said to this device on the workspaces' live sessions
    /// since the last call, each with the workspace it was heard in: what the shares take
    /// ([`Self::take_share_objects`]).
    share_objects: RefCell<Vec<(String, Heard)>>,
    /// The workspaces whose host said their owner deleted them since the last call: what the
    /// session takes off the shell and out of this device's record ([`Self::take_deleted`]).
    deleted: RefCell<Vec<tilo_desktop_model::WorkspaceId>>,
    /// This device's workspaces as its daemon says them: `None` until the session reaches the
    /// daemon ([`Client::live_through`]), and on a session that keeps none.
    view: RefCell<Option<Arc<crate::workspace_view::WorkspaceView>>>,
    /// The member's devices the peer network heard at another peer protocol version since the last
    /// call: what the person is answered.
    versions: std::sync::mpsc::Receiver<crate::network::VersionMismatch>,
    /// The devices each workspace's topic carries to this session: those heard on it, and its
    /// hosts, which the members they reach are behind.
    topologies: RefCell<std::collections::BTreeMap<WorkspaceId, Topology>>,
    /// This device's fabric, when it is a certified device of the person's: `None` on a session
    /// that takes part as nobody.
    fabric: Option<Arc<SessionFabric>>,
    /// The workspaces' live resources, told which devices this session reaches in each: a row
    /// live on a device that went away goes with it.
    live_rows: tilo_workspace_service::live::LiveRows,
    /// This device's relays, held back until a workspace this session takes part in has someone
    /// else in it.
    reachability: RefCell<Reachability>,
    /// The workspaces' live session: their relays and this device's sessions in them.
    live: RefCell<Option<crate::live::Live>>,
    /// Live sessions a test joined this session to, by workspace, in place of the live session's, and the
    /// runtime they run on.
    #[cfg(test)]
    test_live_sessions: RefCell<std::collections::BTreeMap<String, crate::live::Sessions>>,
    #[cfg(test)]
    test_runtime: RefCell<Option<tokio::runtime::Handle>>,
    /// Everything this device said on the share tracks, in order: what a test counts.
    #[cfg(test)]
    test_said: RefCell<Vec<tilo_live::share::Said>>,
}

/// The devices a workspace's topic carries to this session, as what the workspace says here
/// reports them: those that published there, and the workspace's hosts, which the members they
/// reach are behind. A record's content is not routed here: the device's daemon keeps it and
/// asks the workspace's devices itself; these are the devices live rows follow.
#[derive(Debug, Default)]
struct Topology {
    peers: std::collections::BTreeSet<EndpointId>,
    hosts: std::collections::BTreeSet<EndpointId>,
}

impl Topology {
    /// `device` said something on the workspace's topic.
    fn heard(&mut self, device: EndpointId) -> bool {
        self.peers.insert(device)
    }

    /// `host` serves the workspace: what its topic publishes, and the members it reports reaching.
    fn host(&mut self, host: EndpointId) -> bool {
        self.hosts.insert(host)
    }

    /// `device` left the workspace's topic, or the host that reached it did.
    fn left(&mut self, device: EndpointId) -> bool {
        let peer = self.peers.remove(&device);
        let host = self.hosts.remove(&device);
        peer || host
    }

    /// The devices of the workspace this session reaches now: those heard on its topic and the
    /// hosts that serve it, never `own` , this device.
    fn devices(&self, own: EndpointId) -> std::collections::BTreeSet<EndpointId> {
        self.peers
            .iter()
            .chain(self.hosts.iter())
            .filter(|device| **device != own)
            .copied()
            .collect()
    }
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for Local {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Local").finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
impl Local {
    /// What this desktop's peer network does with one message of the client's own.
    fn heard(&self, event: &Event, client: &SessionClient) -> bool {
        // The devices a workspace's topic carries are the ones live rows follow; a record's
        // content is the device's daemon's, which asks them itself.
        self.follow_present_devices(event, client);
        false
    }

    /// Keep what reached here of the devices a workspace's topic carries: those that said
    /// anything on it, the hosts that serve it and the members they report, and those that left.
    /// What live rows follow of the workspace follows them; a record's content is the device's
    /// daemon's to route, and it hears the workspace's devices itself.
    fn follow_present_devices(&self, event: &Event, client: &SessionClient) {
        let (ws, changed) = match event {
            Event::Msg { ws, peer, .. } => (
                *ws,
                self.present_devices(*ws, |devices| devices.heard(*peer)),
            ),
            Event::HostReady { ws, peer, .. } => (
                *ws,
                self.present_devices(*ws, |devices| devices.host(*peer)),
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
                    self.present_devices(*ws, |devices| {
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
                self.present_devices(*ws, |devices| devices.left(*peer)),
            ),
            _ => return,
        };
        if !changed {
            return;
        }
        let connected = {
            let workspace = client.workspace_id(ws);
            let topologies = self.topologies.borrow();
            match (workspace, topologies.get(&ws)) {
                (Some(id), Some(topology)) => Some((id, topology.devices(client.publisher()))),
                _ => None,
            }
        };
        // The devices the workspace reaches are the ones live rows follow: a row live on a device
        // that went away goes with it.
        if let Some((workspace, devices)) = connected {
            let devices: std::collections::BTreeSet<[u8; 32]> =
                devices.iter().map(|device| *device.as_bytes()).collect();
            self.live_rows.reach(&workspace, devices.iter().copied());
        }
    }

    /// Apply `change` to the devices this session reaches in the workspace on `ws`; whether that
    /// was news.
    fn present_devices(&self, ws: WorkspaceId, change: impl FnOnce(&mut Topology) -> bool) -> bool {
        let mut topologies = self.topologies.borrow_mut();
        let Some(topology) = topologies.get_mut(&ws) else {
            return false;
        };
        change(topology)
    }

    /// Take the workspaces whose host said their owner deleted them since the last call.
    #[must_use]
    pub(crate) fn take_deleted(&self) -> Vec<tilo_desktop_model::WorkspaceId> {
        std::mem::take(&mut *self.deleted.borrow_mut())
    }

    /// Take the call audio the workspaces' live sessions heard since the last call, each with the
    /// workspace it was heard in and already opened. A call plays the frames of the devices its
    /// channel says are in it.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_call_audio(&self) -> Vec<(String, Heard)> {
        std::mem::take(&mut *self.call_audio.borrow_mut())
    }

    /// Take the call pictures the workspaces' live sessions heard since the last call, each with the
    /// workspace it was heard in and already opened. A call draws the pictures of the devices its
    /// channel says are in it.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_call_pictures(&self) -> Vec<(String, Heard)> {
        std::mem::take(&mut *self.call_pictures.borrow_mut())
    }

    /// Take the shared windows' pictures and pages the workspaces' live sessions heard since the last
    /// call, each with the workspace it was heard in and already opened: what the session showing that
    /// window draws.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_window_objects(&self) -> Vec<(String, Heard)> {
        #[cfg(test)]
        self.hear_test_live_sessions();
        std::mem::take(&mut *self.window_objects.borrow_mut())
    }

    /// Take what shared windows' viewers and hosts said to this device on the workspaces' live
    /// sessions since the last call, each with the workspace it was heard in and already opened
    /// ([`tilo_live::share`]).
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_share_objects(&self) -> Vec<(String, Heard)> {
        #[cfg(test)]
        self.hear_test_live_sessions();
        std::mem::take(&mut *self.share_objects.borrow_mut())
    }

    /// Sort what a test's live sessions heard into what the shares take.
    #[cfg(all(test, target_os = "linux"))]
    fn hear_test_live_sessions(&self) {
        for (workspace, live_session) in self.test_live_sessions.borrow().iter() {
            while let Some(heard) = live_session.try_recv() {
                let kept = match heard.track {
                    Track::Page(_) | Track::Window(..) => &self.window_objects,
                    Track::Share => &self.share_objects,
                    _ => continue,
                };
                kept.borrow_mut().push((workspace.clone(), heard));
            }
        }
    }

    /// The workspaces' live sessions this session is in now, by workspace.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_sessions(
        &self,
    ) -> std::collections::BTreeMap<String, crate::live::Sessions> {
        #[cfg(test)]
        if !self.test_live_sessions.borrow().is_empty() {
            return self.test_live_sessions.borrow().clone();
        }
        self.live()
            .map(|live| {
                live.live_sessions()
                    .into_iter()
                    .map(|(workspace, live_session)| (workspace.to_string(), live_session))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Moves whenever a workspace's live session stands otherwise or opened again
    /// ([`crate::live::Live::states_revision`]): what [`Self::live_states`] and
    /// [`Self::live_sessions`] are read again on. It never moves for a session that runs no live session.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_revision(&self) -> u64 {
        #[cfg(test)]
        if !self.test_live_sessions.borrow().is_empty() {
            // A test's live sessions are joined before its session runs, and stay.
            return 1;
        }
        self.live().map_or(0, |live| live.states_revision())
    }

    /// The endpoints the device's relay turned away from each workspace's live session and has
    /// not let in since, by workspace ([`crate::live::Live::turned_away`]).
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_turned_away(&self) -> crate::live::TurnedAwayLists {
        self.live()
            .map(|live| live.turned_away())
            .unwrap_or_default()
    }

    /// How far each workspace's live session came on this device, by workspace
    /// ([`crate::live::LiveState`]): nothing for a session that runs no live session.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_states(&self) -> std::collections::BTreeMap<String, crate::live::LiveState> {
        #[cfg(test)]
        if !self.test_live_sessions.borrow().is_empty() {
            // A test's live sessions are joined on a relay of the test's own: open, as this device's.
            return self
                .test_live_sessions
                .borrow()
                .iter()
                .map(|(workspace, live_session)| {
                    let relay = crate::live::Relayer {
                        device: live_session.device(),
                        name: String::new(),
                        here: true,
                    };
                    let open = crate::live::LiveState::Open {
                        relay,
                        epoch: 0,
                        lent: false,
                    };
                    (workspace.clone(), open)
                })
                .collect();
        }
        self.live()
            .map(|live| {
                live.states()
                    .into_iter()
                    .map(|(workspace, state)| (workspace.to_string(), state))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Show `tracks` of `sender` on `live_session`, and no other of its tracks: the live session's readers start
    /// on the runtime the live sessions run on, not on the session's own thread.
    #[cfg(target_os = "linux")]
    pub(crate) fn show_tracks(
        &self,
        live_session: &dyn tilo_live::capture::Session,
        sender: [u8; 32],
        tracks: &[Track],
    ) {
        #[cfg(test)]
        let runtime = self.test_runtime.borrow().clone();
        #[cfg(not(test))]
        let runtime = None;
        let runtime = runtime.or_else(|| self.live().and_then(|live| live.runtime()));
        let _entered = runtime.as_ref().map(tokio::runtime::Handle::enter);
        live_session.show(sender, tracks);
    }

    /// Send one message of `surface`'s stream on the topic of the workspace it is shared in, to
    /// `viewers` alone: over the authenticated route to each of them, never fanned out to the
    /// workspace.
    #[cfg(target_os = "linux")]
    /// Take what the peer actor and the fabric said that only this desktop keeps.
    fn keep_desktop_events(&self, client: &SessionClient) {
        while let Ok(event) = self.desktop_events.try_recv() {
            match event {
                DesktopEvent::WorkspaceDeleted { ws } => {
                    if let Some(workspace) = client
                        .workspace_id(ws)
                        .and_then(|id| tilo_desktop_model::WorkspaceId::parse(&id))
                    {
                        self.deleted.borrow_mut().push(workspace);
                    }
                }
                DesktopEvent::Sibling {
                    peer,
                    certs,
                    revocations,
                } => {
                    if let Some(fabric) = &self.fabric {
                        fabric.sibling(peer, &certs, &revocations);
                    }
                }
                DesktopEvent::SiblingLeft { peer } => {
                    if let Some(fabric) = &self.fabric {
                        fabric.sibling_left(peer);
                    }
                }
                DesktopEvent::PersonalFabric {
                    peer,
                    payload,
                    received,
                } => {
                    if let Some(fabric) = &self.fabric {
                        fabric.arrived(peer, &payload, received);
                    }
                }
            }
        }
    }

    /// Tell the person's other devices what this device says of itself and the copies it offers,
    /// when either moved since they last heard: the session's pass calls this once it offered the
    /// clipboard, so a copy leaves on the pass that made it.
    pub(crate) fn announce_fabric(&self) {
        if let Some(fabric) = &self.fabric {
            fabric.announce();
        }
    }

    /// This device's fabric, when the person certified it: the link the session's services act on.
    #[must_use]
    pub(crate) fn fabric(&self) -> Option<crate::platform::fabric::SharedLink> {
        self.fabric
            .as_ref()
            .map(|fabric| Arc::clone(fabric) as crate::platform::fabric::SharedLink)
    }

    /// The person's own workspace's resources, read and written through this device's service when
    /// it holds the workspace.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn person_resources(&self) -> Option<crate::PersonResources> {
        self.fabric.as_ref()?.person_resources()
    }

    /// The actions one of the person's devices sent this one since the last call, for the session's
    /// pass to carry out.
    #[must_use]
    pub(crate) fn fabric_actions(
        &self,
    ) -> Vec<crate::platform::fabric::PersonalFabricActionEnvelope> {
        self.fabric
            .as_ref()
            .map(|fabric| fabric.take_actions())
            .unwrap_or_default()
    }

    /// The workspaces' live session: what this session says into and hears from each of them, and the
    /// relay each one is carried by; `None` when this session holds no workspaces.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live(&self) -> Option<std::cell::Ref<'_, crate::live::Live>> {
        std::cell::Ref::filter_map(self.live.borrow(), Option::as_ref).ok()
    }

    /// This device's workspaces as its daemon says them, once the session reached it.
    #[must_use]
    pub(crate) fn view(&self) -> Option<Arc<crate::workspace_view::WorkspaceView>> {
        self.view.borrow().clone()
    }

    /// The member's devices the peer network heard at another peer protocol version since the last
    /// call, each once per device and version.
    #[cfg(target_os = "linux")]
    pub(crate) fn take_version_mismatches(&self) -> Vec<crate::network::VersionMismatch> {
        self.versions.try_iter().collect()
    }

    /// Why what this session takes part in needs this device reachable, if it does: a workspace it
    /// shows has someone else here, or a link is out of a workspace it holds as its holder. A host
    /// is dialled at start and on every reconnect, and the peer actor holds the relays up for every
    /// remote party it is connected to or keeps dialling
    /// ([`tilo_endpoint_reachability::Need::Transport`]).
    fn reachability_need(&self, client: &SessionClient) -> Option<&'static str> {
        let own = client.publisher();
        if client.anyone_else_here(own) {
            return Some("a workspace is shown with someone else");
        }
        self.view()
            .is_some_and(|view| view.invitations_open(tilo_clock::wall()))
            .then_some("a link into a workspace is out")
    }

    /// Tell the relays what this session has recorded: they stay up while a workspace it shows has
    /// someone else here or a link it handed out waits to be redeemed, and may rest once none does
    /// ([`tilo_endpoint_reachability::REST_AFTER`]).
    fn note_reachability(&self, client: &SessionClient) {
        self.reachability
            .borrow()
            .need(Need::Session, self.reachability_need(client));
    }

    /// Take every object the workspaces' live sessions heard since the last call: a member's presence
    /// frame, or a call's voice or pictures. Whether any did.
    fn hear_live(&self, client: &SessionClient) -> bool {
        let held = self.live.borrow();
        let Some(live) = held.as_ref() else {
            return false;
        };
        let own = client.publisher();
        let topics: Vec<(tilo_desktop_model::WorkspaceId, String)> = client
            .live_workspaces()
            .into_iter()
            .filter_map(|workspace| {
                let id = tilo_desktop_model::WorkspaceId::parse(&workspace)?;
                Some((id, workspace))
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
                if matches!(object.track, Track::Page(_) | Track::Window(..)) {
                    // A shared window's pictures or page, already opened: the session showing that
                    // window draws them.
                    self.window_objects
                        .borrow_mut()
                        .push((workspace.clone(), object));
                    continue;
                }
                if object.track == Track::Share {
                    // What a shared window's viewer or host said to this device, already opened:
                    // the shares take it.
                    self.share_objects
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
                client.heard_body(&workspace, from, &object.body);
                heard = true;
            }
        }
        heard
    }
}

/// The person's presence as this desktop's surfaces read it: who is here, their pointer, and the
/// bar's word of a wave or an arrival.
pub(crate) struct Roster {
    /// The roster the bar lists, and the pointer over the tile it is on.
    here: Here,
    /// The last wave at this person, for the bar to say.
    wave_notice: RefCell<Option<tilo_session_core::desktop::host::chrome::WaveNotice>>,
    /// The workspace whose arrival the bar is saying, so a person who comes to a workspace where
    /// people are is shown arriving once, not on every roster change.
    arrived_in: RefCell<Option<String>>,
    /// The arrival the bar shows.
    arrival: RefCell<Option<tilo_session_core::desktop::host::chrome::ArrivalNotice>>,
    /// The semantic work the workspace's agents narrated since its surface last took it, by
    /// workspace, admitted and bound to the member that published it
    /// ([`tilo_session_client::narration::Held`]).
    #[cfg(target_os = "linux")]
    narrated: RefCell<tilo_session_client::narration::Held>,
    /// The same work, by workspace, for the apps here: each window showing the record an agent
    /// acts in draws its cursor there ([`Client::take_narrated`]).
    #[cfg(target_os = "linux")]
    for_apps: RefCell<Vec<(String, tilo_actions::SemanticInteraction)>>,
}

/// What this session keeps of the peer network beside the client's boards: the roster every surface reads,
/// and, on a desktop, the peer actor's own half.
pub(crate) struct Hosting {
    roster: Rc<Roster>,
    #[cfg(target_os = "linux")]
    local: Rc<Local>,
}

impl Roster {
    /// A roster nobody is on yet.
    fn new() -> Self {
        Self {
            here: Here::default(),
            wave_notice: RefCell::new(None),
            arrived_in: RefCell::new(None),
            arrival: RefCell::new(None),
            #[cfg(target_os = "linux")]
            narrated: RefCell::new(tilo_session_client::narration::Held::default()),
            #[cfg(target_os = "linux")]
            for_apps: RefCell::new(Vec::new()),
        }
    }

    #[cfg(target_os = "linux")]
    /// Who is here, as the desktop lists them: `tree` places them where their focused record is
    /// shown — `records` (what each window shows, by tab) names the window each person works in —
    /// and `app_name` names an app.
    #[must_use]
    pub(crate) fn people(
        &self,
        tree: &Layout,
        notes: &crate::continuations::Continuations,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Person> {
        self.here.borrow().people(
            tree,
            &crate::sync::layout::bindings_of(tree, &notes.windows()),
            app_name,
        )
    }

    #[cfg(target_os = "linux")]
    /// Who is here, written into `tree`'s active workspace as its members and their presence: the roster every
    /// surface that lists people reads.
    pub(crate) fn show(
        &self,
        tree: &mut Layout,
        notes: &crate::continuations::Continuations,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Person> {
        let people = self.people(tree, notes, app_name);
        // Whose view this person follows, so the bar keeps that face when it has room for only one.
        let following = self.here.borrow().following().map(str::to_owned);
        tilo_session_core::desktop::here::place_here(tree, &people, following.as_deref());
        // Who follows this person: their faces in the bar say so.
        let followers = self.here.borrow().followers(&people);
        tilo_session_core::desktop::here::mark_followers(tree, &followers);
        people
    }

    /// The last wave at this person, for the bar to say.
    #[must_use]
    pub(crate) fn wave_notice(
        &self,
    ) -> Option<tilo_session_core::desktop::host::chrome::WaveNotice> {
        self.wave_notice.borrow().clone()
    }

    /// The arrival the bar shows, while its moment lasts.
    #[must_use]
    pub(crate) fn arrival_notice(
        &self,
    ) -> Option<tilo_session_core::desktop::host::chrome::ArrivalNotice> {
        self.arrival.borrow().clone()
    }

    /// Hand the desktop this person and whoever is here with them in the workspace in front.
    fn show_here(&self, client: &SessionClient) {
        let own = Self::own_member(client);
        let (workspace, members) = client
            .front_members()
            .map(|(workspace, members)| {
                (
                    workspace,
                    members
                        .iter()
                        .map(LiveMember::of)
                        .collect::<Vec<LiveMember>>(),
                )
            })
            .unwrap_or_default();
        self.here
            .borrow_mut()
            .set_members(Some(own), Some(members.clone()));
        self.show_arrival(&workspace, &members);
    }

    /// The person came to a workspace where people already are: the bar's faces pulse and a short
    /// name slides out of them, once for the workspace they came to. A workspace nobody else is in
    /// says nothing.
    fn show_arrival(&self, workspace: &str, members: &[LiveMember]) {
        let people: Vec<&LiveMember> = members
            .iter()
            .filter(|member| member.kind != "agent" && !member.label.trim().is_empty())
            .collect();
        if people.is_empty() || self.arrived_in.borrow().as_deref() == Some(workspace) {
            return;
        }
        *self.arrived_in.borrow_mut() = Some(workspace.to_owned());
        *self.arrival.borrow_mut() =
            Some(tilo_session_core::desktop::host::chrome::ArrivalNotice {
                people: people
                    .iter()
                    .map(|member| member.publisher.clone())
                    .collect(),
                names: people
                    .iter()
                    .map(|member| member.label.trim().to_owned())
                    .collect(),
                seen: None,
                at: ui::time::now(),
            });
    }

    /// This session's own person, as everyone here reads them.
    fn own_member(client: &SessionClient) -> LiveMember {
        LiveMember::of(&client.own_here())
    }

    #[cfg(target_os = "linux")]
    /// Take one semantic frame the peer network heard: an agent's narrated action in a workspace's record.
    /// The work is admitted only from a member the workspace's board knows as an agent
    /// ([`tilo_session_client::narration::Held`]) and bound to that member. Each app here keeps its
    /// own presence, so the work is kept for the apps too ([`Client::take_narrated`]), which draw
    /// the agent's cursor in the window showing its record. Whether `event` was one.
    fn hear_narrated(&self, event: &Event, client: &SessionClient) -> bool {
        let Event::Live { ws, from, frame } = event else {
            return false;
        };
        let Ok(LiveFrame::Ops(OpsFrame::Semantic { event, .. })) = LiveFrame::decode(frame) else {
            return false;
        };
        let Some(workspace) = client.workspace_id(*ws) else {
            return false;
        };
        let here = client.members(&workspace);
        let mut narrated = self.narrated.borrow_mut();
        let admitted = narrated.hear(&workspace, &here, *from, &event);
        let work = narrated.take(&workspace);
        drop(narrated);
        if admitted {
            self.for_apps
                .borrow_mut()
                .extend(work.into_iter().map(|work| (workspace.clone(), work)));
        }
        true
    }

    /// Take the waves at this person heard since the last call: the bar says who waved, for a
    /// moment. A shell that draws its own faces reads them by this too.
    pub(crate) fn show_waves(&self) {
        let heard = self.here.borrow_mut().take_waves();
        if let Some(last) = heard.last() {
            *self.wave_notice.borrow_mut() =
                Some(tilo_session_core::desktop::host::chrome::WaveNotice {
                    from: last.name.clone(),
                    at: ui::time::now(),
                });
        }
    }
}

#[cfg(target_os = "linux")]
impl Host for Hosting {
    fn governed_person(&self, workspace: &str, device: EndpointId) -> Option<String> {
        // The call gate names whose each of the workspace's member devices is.
        let (_, persons) = self.local.view()?.call_gate(workspace);
        persons.get(device.as_bytes()).cloned()
    }

    fn heard(&self, event: &Event, client: &SessionClient) -> bool {
        // An agent's narrated action is the roster's to take: it moves nothing inside a window here,
        // and says where the person's own agent works.
        self.roster.hear_narrated(event, client) || self.local.heard(event, client)
    }

    fn touched(&self, client: &SessionClient) {
        self.local.note_reachability(client);
        self.roster.show_here(client);
        self.roster.show_waves();
    }

    fn workspace_host(&self, workspace: &str) -> Option<[u8; 32]> {
        self.local
            .view()?
            .holder_of(tilo_desktop_model::WorkspaceId::parse(workspace)?)
    }

    fn pass(&self, client: &SessionClient) {
        let local = &self.local;
        local.keep_desktop_events(client);
        let moved = local.hear_live(client);
        // What arrived moved this device's fabric — a copy from another device, a device that is
        // gone: the person's other devices hear so on the same pass.
        local.announce_fabric();
        // The clock of every peer something holds in sync: its probes leave here, so the audio
        // timed against them can play in time.
        if let Some(fabric) = &local.fabric {
            fabric.pump_clock();
        }
        if moved {
            local.note_reachability(client);
        }
    }
}

/// The session a session shows, as the workspace check reads it.
/// A page's host: the roster, over the transport the page brings.
#[cfg(not(target_os = "linux"))]
impl Host for Hosting {
    fn touched(&self, client: &SessionClient) {
        self.roster.show_here(client);
        self.roster.show_waves();
    }
}

#[cfg(target_os = "linux")]
struct MessagePolicy {
    session: DesktopHandle,
}

#[cfg(target_os = "linux")]
impl PolicyShell for MessagePolicy {
    fn allows(&self, workspace: &str, person: &str, required: &WorkspaceAction, now: u64) -> bool {
        self.session.read(|tree| {
            tilo_session_client::message_policy::allows(tree, workspace, person, required, now)
        })
    }
}

/// The session's client of the peer network: the member's own client, and the desktop
/// host that carries it — the peer actor, the live session, this device's workspaces and channels,
/// its fabric and its transfers.
pub struct Client {
    inner: SessionClient,
    hosting: Rc<Hosting>,
    /// Where this client says its own commands, beside the client's.
    #[cfg(target_os = "linux")]
    outgoing: Arc<dyn Outbox>,
    /// The thread running the peer actor, stopped when this drops.
    #[cfg(target_os = "linux")]
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.inner, f)
    }
}

impl std::ops::Deref for Client {
    type Target = SessionClient;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            // The live sessions end before the peer network's thread joins.
            self.hosting.local.live.borrow_mut().take();
            let _ = self.outgoing.send(Cmd::Stop);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

/// What this device brings as it enters the peer network: the identity its endpoint runs on,
/// where it dials, and how it hears back.
#[cfg(target_os = "linux")]
pub struct Entry {
    /// This device's collaboration identity, on whose key the endpoint runs: with it, every
    /// workspace is told which device and which person is here. Without one — a test, a stand-in
    /// session — the session is a device nobody certified and stays anonymous in workspaces.
    pub identity: Option<DeviceIdentity>,
    /// What the person's settings say this device says of them here.
    pub own: OwnPresence,
    /// The hosts the person points this device at.
    pub hosts: Vec<EndpointAddr>,
    /// What the person's settings say may continue on their other devices,
    /// kept current by the session's settings watch.
    pub continuity: Continuity,
    /// What tells the peer actor the network speaks.
    pub wake: Arc<dyn Fn() + Send + Sync>,
    /// The relays the person's setting names ([`tilo_endpoint_reachability::relays`]), which this
    /// device's endpoint holds back until something needs it dialable.
    pub relays: tilo_endpoint_reachability::EndpointRelays,
    /// How this device hears the person's other devices on a local network
    /// ([`tilo_endpoint_reachability::Nearby`]): `None` discovers on the networks the endpoint is
    /// on, and a caller that knows which one its person's devices share says so.
    pub lan: Option<tilo_endpoint_reachability::Nearby>,
}

impl Client {
    /// Take part in the peer network: bind this device's endpoint, start the peer actor,
    /// and from then on hear the workspaces the session joins. `beside` is what else this session
    /// serves on that endpoint (its protocols are fixed once it binds).
    ///
    /// # Errors
    /// The endpoint could not be bound.
    #[cfg(target_os = "linux")]
    pub fn start(
        entry: Entry,
        questions: Option<tilo_service::AskSender>,
        beside: tilo_network_actor::own_service::Protocols,
    ) -> std::io::Result<Self> {
        let (identity, wake) = (entry.identity, entry.wake);
        let live_rows = tilo_workspace_service::live::LiveRows::default();
        let (commands, host_commands) = tokio::sync::mpsc::unbounded_channel();
        let (arrivals, arrived) = std::sync::mpsc::channel();
        let (desktop_events, desktop_arrived) = std::sync::mpsc::channel();
        let (ready, bound) = std::sync::mpsc::channel();
        let (user, device) = certified_as(identity.as_ref());
        // What this device hears on its local network wakes the session, which shows which of the
        // person's devices are nearby.
        let lan = entry.lan.unwrap_or_else(|| Nearby::new(Arc::clone(&wake)));
        let host_outbox = HostOutbox {
            commands: commands.clone(),
        };
        let outgoing: Arc<dyn Outbox> = Arc::new(host_outbox.clone());
        // What is the person's alone, read from their own workspace once the session reaches the
        // device's daemon that keeps it ([`crate::person_store::DaemonPersonStore`]).
        let person = crate::PersonResources::default();
        let fabric = fabric_of(
            identity.as_ref(),
            person,
            host_outbox,
            entry.continuity,
            lan.clone(),
        );
        // What the actor serves is installed before it starts: every actor built after answers it.
        let versions = crate::network::install(fabric.as_ref(), questions);
        let endpoint_handle = Arc::new(std::sync::OnceLock::new());
        let binding = network::Bound::new(Arc::clone(&endpoint_handle), entry.relays, lan);
        // The workspaces' live sessions run through this device's daemon, which holds the relay
        // and the device's endpoint ([`Self::live_through`]).
        let live = identity.as_ref().map(|identity| {
            crate::live::Live::new(identity.device_cert.clone(), Arc::clone(&wake))
        });
        let served = beside.iter().map(|(alpn, _)| alpn.clone()).collect();
        let thread = std::thread::Builder::new()
            .name("tilo-sync".into())
            .spawn(move || {
                run(
                    identity,
                    host_commands,
                    Wire {
                        arrivals,
                        desktop_events,
                        ready,
                    },
                    wake,
                    Serving::new(binding, beside),
                );
            })?;
        let (publisher, endpoint, reachability) = wait_for_bound(&bound)?;
        // This device's presence is its daemon's to say: the session tells it the person's word.
        let saying = live.as_ref().map(|live| {
            tilo_session_client::Saying::Word(
                Arc::new(live.telling()) as Arc<dyn tilo_session_client::TellWord>
            )
        });
        let inner = SessionClient::over(
            Transport {
                outgoing: Arc::clone(&outgoing),
                arrived,
                saying,
            },
            Device {
                publisher,
                endpoint,
                person: user,
                name: device,
            },
            entry.own,
        )
        .serving(served)
        .bound_to(endpoint_handle);
        let hosting = Local::hosting(Hosted {
            commands,
            desktop_events: desktop_arrived,
            versions,
            fabric,
            live_rows,
            reachability,
            live,
        });
        Ok(Self::started(inner, hosting, outgoing, thread, entry.hosts))
    }

    /// The client, hearing the person's other devices and then the hosts the entry names.
    #[cfg(target_os = "linux")]
    fn started(
        inner: SessionClient,
        hosting: Rc<Hosting>,
        outgoing: Arc<dyn Outbox>,
        thread: std::thread::JoinHandle<()>,
        hosts: Vec<EndpointAddr>,
    ) -> Self {
        inner.attach_host(Rc::clone(&hosting) as Rc<dyn Host>);
        let client = Self {
            inner,
            hosting,
            outgoing,
            thread: Some(thread),
        };
        // The person's other devices are heard from the start, so a list one of them published is
        // taken even when this session shows none of their workspaces yet.
        let _ = client.outgoing.send(Cmd::WatchSiblings);
        // The hosts next: a workspace joined before them is replayed to this device when its host
        // session comes up, never lost.
        for host in hosts {
            let _ = client.outgoing.send(Cmd::Connect(host));
        }
        client
    }

    /// A client on a transport another host brings, with this desktop's host beside it: what a test
    /// hands it is what the session shows. The transport's own commands stay the client's, so a
    /// host that reads them off its end reads what this session said.
    #[must_use]
    pub fn over(transport: Transport, device: Device, own: OwnPresence) -> Self {
        #[cfg(target_os = "linux")]
        let host = {
            let (commands, _host_commands) = tokio::sync::mpsc::unbounded_channel();
            let (_desktop_events, desktop_arrived) = std::sync::mpsc::channel();
            Rc::new(Local::bare(commands, desktop_arrived))
        };
        #[cfg(target_os = "linux")]
        let outgoing = std::sync::Arc::clone(&transport.outgoing);
        let inner = SessionClient::over(transport, device, own);
        let hosting = Rc::new(Hosting {
            roster: Rc::new(Roster::new()),
            #[cfg(target_os = "linux")]
            local: host,
        });
        inner.attach_host(Rc::clone(&hosting) as Rc<dyn Host>);
        Self {
            inner,
            hosting,
            #[cfg(target_os = "linux")]
            outgoing,
            #[cfg(target_os = "linux")]
            thread: None,
        }
    }

    /// A session taking part in a peer network with no transport, for tests that hand it what it would
    /// have heard and read what it would have said.
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    pub(crate) fn offline(
        publisher: EndpointId,
        own: OwnPresence,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        Self::without_network(publisher, own, Vec::new())
    }

    /// A client taking part in no transport: it publishes nowhere and hears only what it is handed.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn untethered(publisher: EndpointId, own: OwnPresence) -> Self {
        Self::without_network(publisher, own, Vec::new()).0
    }

    /// A client of a peer network with no transport: what both the real client and [`Self::untethered`]
    /// are, at their middle. A test that reads what one peer network would have sent to another takes the
    /// receiver.
    #[cfg(target_os = "linux")]
    pub(crate) fn without_network(
        publisher: EndpointId,
        own: OwnPresence,
        hosts: Vec<EndpointAddr>,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        let (outgoing, said) = tokio::sync::mpsc::unbounded_channel();
        let (_unheard, arrived) = std::sync::mpsc::channel();
        let (commands, _host_commands) = tokio::sync::mpsc::unbounded_channel();
        let (_desktop_events, desktop_arrived) = std::sync::mpsc::channel();
        let outbox: Arc<dyn Outbox> = Arc::new(outgoing);
        let inner = SessionClient::over(
            Transport {
                outgoing: Arc::clone(&outbox),
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
        let hosting = Rc::new(Hosting {
            roster: Rc::new(Roster::new()),
            local: Rc::new(Local::bare(commands, desktop_arrived)),
        });
        inner.attach_host(Rc::clone(&hosting) as Rc<dyn Host>);
        let client = Self {
            inner,
            hosting,
            outgoing: outbox,
            thread: None,
        };
        for host in hosts {
            let _ = client.outgoing.send(Cmd::Connect(host));
        }
        (client, said)
    }

    /// The same peer network, taken part in as `person` (their user root, hex): what a device's
    /// certificate says of it on a peer network that has one, and what the workspaces' policies hold its
    /// members' messages to.
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    pub(crate) fn taking_part_as(mut self, person: &str) -> Self {
        self.inner.as_person(person);
        self
    }

    /// Take the work the workspaces' agents narrated since the last call, by workspace, admitted
    /// and bound to the agent that did it: what this device's apps draw as that agent's cursor.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_narrated(&self) -> Vec<(String, tilo_actions::SemanticInteraction)> {
        std::mem::take(&mut *self.hosting.roster.for_apps.borrow_mut())
    }

    /// Take the workspaces whose host said their owner deleted them since the last call.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_deleted(&self) -> Vec<tilo_desktop_model::WorkspaceId> {
        self.hosting.local.take_deleted()
    }

    /// Take the call audio the workspaces' live sessions heard since the last call, already opened.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_call_audio(&self) -> Vec<(String, Heard)> {
        self.hosting.local.take_call_audio()
    }

    /// Take the call pictures the workspaces' live sessions heard since the last call, already
    /// opened.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_call_pictures(&self) -> Vec<(String, Heard)> {
        self.hosting.local.take_call_pictures()
    }

    /// Take the shared windows' pictures and pages the workspaces' live sessions heard since the last
    /// call, already opened.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_window_objects(&self) -> Vec<(String, Heard)> {
        self.hosting.local.take_window_objects()
    }

    /// Take what shared windows' viewers and hosts said to this device since the last call,
    /// already opened.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_share_objects(&self) -> Vec<(String, Heard)> {
        self.hosting.local.take_share_objects()
    }

    /// The workspaces' live sessions this session is in now, by workspace: what a call's voice and
    /// pictures and a shared window's pictures and pages travel.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_sessions(
        &self,
    ) -> std::collections::BTreeMap<String, crate::live::Sessions> {
        self.hosting.local.live_sessions()
    }

    /// Moves whenever a workspace's live session stands otherwise or opened again: what
    /// [`Self::live_states`] and [`Self::live_sessions`] are read again on.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_revision(&self) -> u64 {
        self.hosting.local.live_revision()
    }

    /// How far each workspace's live session came on this device, by workspace: open, or what it
    /// waits for ([`crate::live::LiveState`]).
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_states(&self) -> std::collections::BTreeMap<String, crate::live::LiveState> {
        self.hosting.local.live_states()
    }

    /// The endpoints the device's relay turned away from each workspace's live session and has
    /// not let in since, by workspace ([`crate::live::Live::turned_away`]).
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_turned_away(&self) -> crate::live::TurnedAwayLists {
        self.hosting.local.live_turned_away()
    }

    /// Join this session to `live_sessions`, by workspace, running on `runtime`, in place of a live session
    /// of its own: what a test's sessions meet on.
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    pub(crate) fn on_live_sessions(
        self,
        live_sessions: std::collections::BTreeMap<String, crate::live::Sessions>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        *self.hosting.local.test_live_sessions.borrow_mut() = live_sessions;
        *self.hosting.local.test_runtime.borrow_mut() = Some(runtime);
        self
    }

    /// Show `tracks` of `sender` on `live_session`, and no other of its tracks ([`Local::show_tracks`]).
    #[cfg(target_os = "linux")]
    pub(crate) fn show_tracks(
        &self,
        live_session: &dyn tilo_live::capture::Session,
        sender: [u8; 32],
        tracks: &[Track],
    ) {
        self.hosting.local.show_tracks(live_session, sender, tracks);
    }

    /// Say `said` on `workspace`'s live session's share track: what a shared window's viewer says to
    /// its host and a host to its viewers, each object to the device it names alone
    /// ([`tilo_live::share`]). Nothing is said while the workspace's live session is not open: a
    /// viewer says its view again, and input lands within its frame or not at all.
    #[cfg(target_os = "linux")]
    pub(crate) fn say(&self, workspace: &str, said: &[tilo_live::share::Said]) {
        if said.is_empty() {
            return;
        }
        #[cfg(test)]
        self.hosting
            .local
            .test_said
            .borrow_mut()
            .extend_from_slice(said);
        let Some(live_session) = self.live_sessions().remove(workspace) else {
            tracing::debug!(%workspace, "a shared window's word waits for no live session");
            return;
        };
        if let Err(error) = live_session.say(said) {
            // The share it belongs to ended with the session: nothing of it is left to say it to.
            tilo_failure::ignored!(
                "the share the word belongs to has ended",
                tracing::debug!(%workspace, %error, "a shared window's word was not said")
            );
        }
    }

    /// Everything this device said on the share tracks so far, in order.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn said_on_shares(&self) -> Vec<tilo_live::share::Said> {
        self.hosting.local.test_said.borrow().clone()
    }

    /// The workspaces' live resources, as far as the devices this session reaches go: a row live
    /// on a device that went away goes with it.
    #[cfg(target_os = "linux")]
    pub(crate) fn live_rows(&self) -> tilo_workspace_service::live::LiveRows {
        self.hosting.local.live_rows.clone()
    }

    /// This device came to hold another workspace: its live session opens that workspace's live session
    /// now ([`crate::live::Live::workspace_arrived`]). Nothing to tell on a session with no live session.
    #[cfg(target_os = "linux")]
    pub(crate) fn workspace_arrived(&self) {
        if let Some(live) = self.hosting.local.live() {
            live.workspace_arrived();
        }
    }

    /// Run the workspaces' live sessions through this device's daemon from now on: each link opens
    /// on `daemon`, the shell link the daemon handed this session, as a stream of the daemon's
    /// socket the daemon carries into its relay or on to another device's. Nothing to run on a
    /// session that keeps no workspaces.
    #[cfg(target_os = "linux")]
    pub(crate) fn live_through(
        &self,
        daemon: runtime_collab::ShellClient,
        view: Option<Arc<crate::workspace_view::WorkspaceView>>,
        turned: crate::followed_resource::FollowedReading<crate::live::TurnedAwayLists>,
        sessions: crate::followed_resource::FollowedReading<crate::live::SessionRows>,
    ) {
        self.hosting.local.view.replace(view.clone());
        let mut held = self.hosting.local.live.borrow_mut();
        let Some(live) = held.as_mut() else {
            return;
        };
        if let Some(view) = view {
            live.follow_workspaces(view);
        }
        live.follow_turned_away(turned);
        live.follow_sessions(sessions);
        live.tell_through(daemon.clone());
        live.open_links(Arc::new(move || {
            let daemon = daemon.clone();
            Box::pin(async move {
                let stream = daemon
                    .live()
                    .await
                    .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
                let (reader, writer) = stream.into_split();
                Ok((
                    Box::new(reader) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                    Box::new(writer) as Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
                ))
            })
        }));
    }

    /// Dial the person's other devices as this session's siblings, and wake the session when
    /// their own workspace moves: the session between two of theirs is where they tell each other
    /// what they hold, and carries their device fabric.
    #[cfg(target_os = "linux")]
    pub(crate) fn follow_person(&self, runtime: &tokio::runtime::Handle, wake: tilo_service::Wake) {
        let (Some(dials), Some(store)) = (self.sibling_dials(), self.person_resources()) else {
            return;
        };
        runtime.spawn(async move {
            let mut changes = store.changes();
            let mut again = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                for device in store.kept_devices() {
                    dials.dial(device.device_public_key);
                }
                wake();
                tokio::select! {
                    moved = changes.changed() => {
                        if moved.is_err() {
                            return;
                        }
                    }
                    _ = again.tick() => {}
                }
            }
        });
    }

    /// The person's own workspace's resources, which this session reads and writes what is theirs
    /// alone through once its service holds the workspace.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn person_resources(&self) -> Option<crate::PersonResources> {
        self.hosting.local.person_resources()
    }

    /// The way to the person's other devices over the live link, and from them to this one;
    /// `None` when this session holds no workspaces.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn own_devices(&self) -> Option<crate::live::OwnDevices> {
        self.hosting.local.live().map(|live| live.own_devices())
    }

    /// The person's presence as the surfaces here read it.
    pub fn here(&self) -> &Here {
        &self.hosting.roster.here
    }

    /// Tell the person's other devices what this device says of itself, when that moved.
    #[cfg(target_os = "linux")]
    pub fn announce_fabric(&self) {
        self.hosting.local.announce_fabric();
    }

    /// This device's fabric, when the person certified it.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn fabric(&self) -> Option<crate::platform::fabric::SharedLink> {
        self.hosting.local.fabric()
    }

    /// The actions one of the person's devices sent this one since the last call.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn fabric_actions(&self) -> Vec<crate::platform::fabric::PersonalFabricActionEnvelope> {
        self.hosting.local.keep_desktop_events(&self.inner);
        self.hosting.local.fabric_actions()
    }

    /// The member's devices the peer network heard at another peer protocol version.
    #[cfg(target_os = "linux")]
    pub(crate) fn take_version_mismatches(&self) -> Vec<crate::network::VersionMismatch> {
        self.hosting.local.take_version_mismatches()
    }

    /// Hold this device's relays back behind `reachability`: what a test asks of what this desktop
    /// keeps dialable.
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    pub(crate) fn hold_relays_with(&self, reachability: Reachability) {
        *self.hosting.local.reachability.borrow_mut() = reachability;
    }

    /// This device's collaboration endpoint, once it has bound.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn endpoint(&self) -> Option<iroh::Endpoint> {
        self.inner.endpoint()
    }

    /// Hand the workspace check the session this session shows: whose workspaces' policies hold every
    /// member's messages to what the workspace grants them.
    #[cfg(target_os = "linux")]
    pub fn attach_shell(&self, session: DesktopHandle) {
        self.inner.attach_shell(Rc::new(MessagePolicy {
            session: session.clone(),
        }));
        *self.hosting.local.session.borrow_mut() = Some(session);
    }

    /// This device's workspaces as its daemon says them, once the session reached it.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn view(&self) -> Option<Arc<crate::workspace_view::WorkspaceView>> {
        self.hosting.local.view()
    }

    /// Dial the holders of workspace `ws` again now: the person pressed "Try again" in a window
    /// whose workspace could not arrive.
    #[cfg(target_os = "linux")]
    pub(crate) fn redial(&self, ws: tilo_desktop_model::WorkspaceId) {
        let _ = self.outgoing.send(Cmd::Redial(ws));
    }

    /// Dial one of the person's own devices, whose address this session was told: the session
    /// between the two carries their device fabric.
    #[cfg(target_os = "linux")]
    pub fn connect(&self, addr: EndpointAddr) {
        let _ = self.outgoing.send(Cmd::ConnectDevice(addr));
    }

    /// Where the person's own devices are dialled as this device's siblings from a thread of their
    /// own (the store's), once the endpoint has bound.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn sibling_dials(&self) -> Option<SiblingDials> {
        // One relay is the person's own, and their devices name the same one: a sibling is
        // reached through it. Number 0's relays are many, and the directory names which one a
        // device is at, so nothing is guessed there.
        let urls = self
            .hosting
            .local
            .reachability
            .try_borrow()
            .map(|reachability| reachability.relay_urls())
            .unwrap_or_default();
        let relay = match urls.as_slice() {
            [relay] => Some(relay.clone()),
            _ => None,
        };
        Some(SiblingDials::new(
            *self.inner.endpoint()?.id().as_bytes(),
            HostOutbox {
                commands: self.hosting.local.commands.clone(),
            },
            relay,
        ))
    }

    /// This device's handle in a workspace, as a window's stream names it.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn peer(&self) -> neighbour_wire::PeerId {
        tilo_network_actor::behaviour::peer_liveness_handle(&self.publisher())
    }

    #[cfg(target_os = "linux")]
    /// Who is here, as the desktop lists them.
    #[must_use]
    pub(crate) fn people(
        &self,
        tree: &Layout,
        notes: &crate::continuations::Continuations,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Person> {
        self.hosting.roster.people(tree, notes, app_name)
    }

    #[cfg(target_os = "linux")]
    /// Who is here, written into `tree`'s active workspace as its members.
    pub(crate) fn show(
        &self,
        tree: &mut Layout,
        notes: &crate::continuations::Continuations,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Person> {
        self.hosting.roster.show(tree, notes, app_name)
    }

    /// Take the waves heard since the last look, so a shell that draws its own faces says them.
    pub(crate) fn show_waves(&self) {
        self.hosting.roster.show_waves();
    }

    /// The waves at this person since the last call, as the bar says them.
    #[must_use]
    pub fn wave_notice(&self) -> Option<tilo_session_core::desktop::host::chrome::WaveNotice> {
        self.hosting.roster.wave_notice()
    }

    /// The arrival the bar shows, while its moment lasts.
    #[must_use]
    pub fn arrival_notice(
        &self,
    ) -> Option<tilo_session_core::desktop::host::chrome::ArrivalNotice> {
        self.hosting.roster.arrival_notice()
    }
}

#[cfg(target_os = "linux")]
/// What a desktop's host runs beside: the thread's channels, its workspaces, its channels'
/// service, its fabric, its app wire, its relays and its live sessions.
#[cfg(target_os = "linux")]
struct Hosted {
    commands: tokio::sync::mpsc::UnboundedSender<HostCmd>,
    desktop_events: std::sync::mpsc::Receiver<DesktopEvent>,
    versions: std::sync::mpsc::Receiver<crate::network::VersionMismatch>,
    fabric: Option<Arc<SessionFabric>>,
    live_rows: tilo_workspace_service::live::LiveRows,
    reachability: Reachability,
    live: Option<crate::live::Live>,
}

#[cfg(target_os = "linux")]
impl Local {
    /// The host this client runs with, and the roster beside it.
    fn hosting(
        Hosted {
            commands,
            desktop_events,
            versions,
            fabric,
            live_rows,
            reachability,
            live,
        }: Hosted,
    ) -> Rc<Hosting> {
        Rc::new(Hosting {
            roster: Rc::new(Roster::new()),
            local: Rc::new(Self {
                session: RefCell::new(None),
                commands,
                desktop_events,
                call_audio: RefCell::new(Vec::new()),
                call_pictures: RefCell::new(Vec::new()),
                window_objects: RefCell::new(Vec::new()),
                share_objects: RefCell::new(Vec::new()),
                deleted: RefCell::new(Vec::new()),
                view: RefCell::new(None),
                versions,
                topologies: RefCell::new(std::collections::BTreeMap::new()),
                fabric,
                live_rows,
                reachability: RefCell::new(reachability),
                live: RefCell::new(live),
                #[cfg(test)]
                test_live_sessions: RefCell::default(),
                #[cfg(test)]
                test_said: RefCell::default(),
                #[cfg(test)]
                test_runtime: RefCell::default(),
            }),
        })
    }

    /// A local with no peer network of its own: what a client takes part in over a transport another host
    /// brings.
    fn bare(
        commands: tokio::sync::mpsc::UnboundedSender<HostCmd>,
        desktop_events: std::sync::mpsc::Receiver<DesktopEvent>,
    ) -> Self {
        let (_said, versions) = std::sync::mpsc::channel();
        Self {
            session: RefCell::new(None),
            commands,
            desktop_events,
            call_audio: RefCell::new(Vec::new()),
            call_pictures: RefCell::new(Vec::new()),
            window_objects: RefCell::new(Vec::new()),
            share_objects: RefCell::new(Vec::new()),
            deleted: RefCell::new(Vec::new()),
            view: RefCell::new(None),
            versions,
            topologies: RefCell::new(std::collections::BTreeMap::new()),
            fabric: None,
            live_rows: tilo_workspace_service::live::LiveRows::default(),
            reachability: RefCell::new(Reachability::default()),
            live: RefCell::new(None),
            #[cfg(test)]
            test_live_sessions: RefCell::default(),
            #[cfg(test)]
            test_said: RefCell::default(),
            #[cfg(test)]
            test_runtime: RefCell::default(),
        }
    }
}

/// This device's collaboration identity: the key and certificate this session's endpoint runs on,
/// when this host holds one.
/// A session on a host without one takes part in no workspace as anybody: it binds a fresh key
/// and stays anonymous until the person sets a device up.
#[must_use]
#[cfg(target_os = "linux")]
pub fn device_identity() -> Option<DeviceIdentity> {
    tilo_session_client::identity::device_identity()
}

/// The workspace hosts this session was started to dial: an always-on peer that keeps what the
/// person's devices publish while the others are away, named by `TILO_COLLAB_HOST` (one address or
/// endpoint id) or `TILO_COLLAB_HOSTS` (a JSON array of addresses).
#[must_use]
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
fn host_addr(value: &str) -> Option<EndpointAddr> {
    tilo_failure::ignored!(
        "a host value that is not JSON names no endpoint address here",
        serde_json::from_str(value)
            .ok()
            .or_else(|| value.parse::<EndpointId>().ok().map(EndpointAddr::new))
    )
}

/// Who this device's certificate says its person is, and what the device calls itself: the id that
/// stays the person's, and the name its other devices are told apart by. A device the person never
/// named says no name: the stand-in its certificate carries is no word of theirs, and every reader
/// says which device it is in its own language.
#[cfg(target_os = "linux")]
fn certified_as(identity: Option<&DeviceIdentity>) -> (Option<[u8; 32]>, String) {
    identity.map_or_else(
        || (None, String::new()),
        |identity| {
            (
                Some(identity.device_cert.user_public_key),
                tilo_user_identity::device_name(&identity.device_cert.device_label)
                    .unwrap_or_default()
                    .to_owned(),
            )
        },
    )
}

/// This device's link onto the person's other devices before the endpoint runs, so a device that
/// proves itself at the actor's first tick is heard: named by this device's certificate, with what
/// lasts of the person's devices in their own workspace's `resources`.
#[cfg(target_os = "linux")]
fn fabric_of(
    identity: Option<&DeviceIdentity>,
    resources: crate::PersonResources,
    outgoing: HostOutbox,
    continuity: Continuity,
    lan: tilo_endpoint_reachability::Nearby,
) -> Option<Arc<SessionFabric>> {
    let identity = identity?;
    let device = DeviceId::new(hex::encode(identity.device_cert.device_public_key)).ok()?;
    let reach = Reach {
        outgoing,
        user_public_key: identity.device_cert.user_public_key,
        continuity,
        lan,
    };
    Some(SessionFabric::new(
        device,
        identity.device_cert.device_label.clone(),
        Some((resources, identity.device_cert.clone())),
        reach,
    ))
}

/// An endpoint only this machine reaches: on loopback, with nothing that finds it from elsewhere.
#[cfg(target_os = "linux")]
pub(super) fn loopback_endpoint() -> Result<iroh::endpoint::Builder, String> {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .portmapper_config(iroh::endpoint::PortmapperConfig::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .map_err(|error| format!("no loopback address: {error}"))
}

/// The endpoint the peer actor binds, and the relays held back from it until something needs this
/// device dialable: the person's `relays` for a device with an identity, none for a session that
/// takes part as nobody and stays on this machine's loopback.
#[cfg(target_os = "linux")]
fn endpoint_for(
    identity: Option<&DeviceIdentity>,
    relays: &tilo_endpoint_reachability::EndpointRelays,
    lan: &tilo_endpoint_reachability::Nearby,
    workspace_devices: &network::WorkspaceDevices,
) -> Result<(iroh::endpoint::Builder, iroh::RelayMap), String> {
    let Some(identity) = identity else {
        return Ok((loopback_endpoint()?, iroh::RelayMap::empty()));
    };
    let key = iroh::SecretKey::from_bytes(
        &identity
            .signer
            .endpoint_secret()
            .ok_or("this device's key dials only through its ownership")?,
    );
    // The relays are held back ([`tilo_endpoint_reachability`]): binding with an empty relay map
    // keeps net report, address discovery and port mapping off the network until a workspace this
    // session shows has someone else in it. A peer's key also resolves on the local network
    // ([`tilo_endpoint_reachability::Nearby`]), so two of the workspace's devices that share one
    // find each other while their relays rest, and with their own relay (or none) that is what
    // finds them. The lookup asks only for a key this device dials, in a probe only that device
    // opens, and answers only the devices [`network::LanFleet`] admits: someone else on the
    // network learns no key and no address.
    let fleet = network::LanFleet {
        identity: identity.clone(),
        workspace_devices: workspace_devices.clone(),
    };
    let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(key)
        .address_lookup(lan.lookup(Arc::new(fleet)))
        .portmapper_config(relays.portmapper())
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::empty()));
    if relays.n0_dns {
        builder = builder
            .address_lookup(tilo_endpoint_reachability::collaborating())
            .address_lookup(iroh::address_lookup::DnsAddressLookup::n0_dns());
    }
    Ok((builder, relays.relays.clone()))
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod membership_check_tests;

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;
