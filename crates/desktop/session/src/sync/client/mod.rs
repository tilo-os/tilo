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
use neighbour_wire::NeighbourMsg;
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
use tilo_session_client::{Cmd, Enrolled, Event, Outbox, SayPresence, Topic};
#[cfg(target_os = "linux")]
use tilo_session_core::desktop::here::{Here, LiveMember, Person};
#[cfg(target_os = "linux")]
use tilo_session_core::desktop::state::DesktopHandle;

#[cfg(target_os = "linux")]
use super::Entering;
#[cfg(target_os = "linux")]
use crate::content;
#[cfg(target_os = "linux")]
use crate::device_actions::DeviceActions;
#[cfg(target_os = "linux")]
use crate::engine::Engine;
#[cfg(target_os = "linux")]
use crate::platform::fabric::DeviceId;
#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
use crate::platform::fabric::{Continuity, Reach, SessionFabric};
#[cfg(target_os = "linux")]
use crate::workspace_keys::KeyLink;
#[cfg(target_os = "linux")]
use tilo_endpoint_reachability::{Need, Reachability};
#[cfg(target_os = "linux")]
use tilo_identity::identity_store::DeviceIdentity;
#[cfg(target_os = "linux")]
use tilo_network_actor::{WorkspaceId, native_ingress};
#[cfg(target_os = "linux")]
use tilo_workspace_service::content::ContentSources;

#[cfg(target_os = "linux")]
mod adopt;
#[cfg(target_os = "linux")]
mod network;
#[cfg(target_os = "linux")]
mod own_service;

#[cfg(target_os = "linux")]
pub(crate) use network::Reached;
#[cfg(target_os = "linux")]
use network::{Serving, Wire, run};
#[cfg(target_os = "linux")]
pub(crate) use own_service::found_alone;

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

/// Hand what the endpoint that bound carries to what speaks on it: the live session takes it, the
/// channels' link says through it, and this device's presence is the live session's own.
#[cfg(target_os = "linux")]
fn attach_bound(
    live: &mut Option<crate::live::Live>,
    link: Option<&Arc<KeyLink>>,
    outbox: HostOutbox,
    endpoint: Option<&iroh::Endpoint>,
) -> Option<Arc<dyn SayPresence>> {
    let endpoint = endpoint?;
    if let Some(live) = live {
        live.attach(endpoint.clone());
    }
    link.inspect(|link| link.attach(Arc::new(outbox)));
    live.as_ref()
        .map(|live| Arc::new(live.saying()) as Arc<dyn SayPresence>)
}

/// One window stream's message as it reached this device: another member's shared window, or a
/// member's ask to view one of this device's.
#[derive(Clone, Debug)]
#[cfg(target_os = "linux")]
pub(crate) struct ReceivedSurface {
    /// The workspace's topic it came in on.
    pub(crate) ws: Topic,
    /// The device that published it.
    pub(crate) from: EndpointId,
    pub(crate) msg: NeighbourMsg,
}

/// What this desktop's endpoint carries beside the client's own commands: the ownership sessions and
/// records its workspaces are held through, its device fabric, and the transfers between the
/// person's devices.
#[derive(Debug)]
#[cfg(target_os = "linux")]
pub(crate) enum HostCmd {
    /// One of the client's own commands.
    Network(Cmd),
    /// Take part in a workspace through this device's own ownership session with its host.
    TakePart(native_ingress::Connect),
    /// Send one piece of a record's content to the device of the workspace that asked for it.
    Content(tilo_network_actor::RecordContentDelivery),
    /// Take one of the person's workspaces in as their further device, at the host the list names.
    Enter(Box<Entering>),
    /// Send this device's fabric to the person's other devices, or to the one at `target`.
    Fabric {
        user_public_key: [u8; 32],
        target: Option<EndpointId>,
        payload: tilo_network_actor::PersonalFabricPayload,
    },
    /// Stream one staged spool object to one of the person's devices from the checkpoint it
    /// confirmed.
    NearbySpool {
        user_public_key: [u8; 32],
        target: EndpointId,
        checkpoint: tilo_network_actor::NearbyTransferCheckpoint,
        file: std::fs::File,
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

    /// Send one piece of a record's content to the device of the workspace that asked for it.
    pub(crate) fn content(&self, delivery: tilo_network_actor::RecordContentDelivery) {
        let _ = self.say(HostCmd::Content(delivery));
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

    /// Stream one staged spool object to one of the person's devices from the checkpoint it
    /// confirmed.
    pub(crate) fn nearby_spool(
        &self,
        user_public_key: [u8; 32],
        target: EndpointId,
        checkpoint: tilo_network_actor::NearbyTransferCheckpoint,
        file: std::fs::File,
    ) -> bool {
        self.say(HostCmd::NearbySpool {
            user_public_key,
            target,
            checkpoint,
            file,
        })
    }
}

/// What the peer actor and the fabric said that only this desktop keeps: which of the person's
/// devices proved itself on a session with this one, and the session's end, the fabric traffic they
/// send, and how far a transfer this device is sending got.
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
    NearbyTransferProgress {
        transfer_id: [u8; 16],
        sent_bytes: u64,
        total_bytes: u64,
        status: tilo_network_actor::PersonalTransferStatus,
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
    /// The actions of the person's devices that arrived ahead of the pass routing them: what the
    /// session took at once ([`Client::fabric_actions_now`]) left these for it.
    fabric_waiting: RefCell<Vec<crate::platform::fabric::PersonalFabricActionEnvelope>>,
    /// The window streams that reached this device: their frames, starts, ends and control, which
    /// the shares take ([`Self::take_surfaces`]).
    surfaces: RefCell<Vec<ReceivedSurface>>,
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
    /// The workspaces whose host said their owner deleted them since the last call: what the
    /// session takes off the shell and out of this device's record ([`Self::take_deleted`]).
    deleted: RefCell<Vec<tilo_desktop_model::WorkspaceId>>,
    /// This device's workspaces: `None` when the session keeps no state to hold them in.
    engine: Option<Arc<Engine>>,
    /// What the session's channels hear and say on this peer network. What the engine holds, so a session
    /// that runs no channel service has none.
    link: Option<Arc<KeyLink>>,
    /// The member's devices the peer network heard at another peer protocol version since the last
    /// call: what the person is answered.
    versions: std::sync::mpsc::Receiver<crate::network::VersionMismatch>,
    /// The records' content this session keeps and asks the workspaces for: `None` until the
    /// session hands its content service over ([`Self::attach_content`]).
    content: RefCell<Option<Arc<content::ContentService>>>,
    /// The devices each workspace's record content reaches this one from: those heard on its topic,
    /// and its hosts.
    topologies: RefCell<std::collections::BTreeMap<WorkspaceId, content::Topology>>,
    /// This device's fabric, when it is a certified device of the person's: `None` on a session
    /// that takes part as nobody.
    fabric: Option<Arc<SessionFabric>>,
    /// This session's Nearby Send, once the session made it.
    nearby: RefCell<Option<Arc<crate::nearby_transfer::NearbyTransfer>>>,
    /// Where this device's actions are served, and how an app here reaches the person's other
    /// devices: `None` until the endpoint has bound.
    device_actions: Arc<std::sync::OnceLock<DeviceActions>>,
    /// This device's relays, held back until a workspace this session takes part in has someone
    /// else in it.
    reachability: RefCell<Reachability>,
    /// The workspaces' live session: their relays and this device's sessions in them.
    live: RefCell<Option<crate::live::Live>>,
    /// Live sessions a test joined this session to, by workspace, in place of the live session's, and the
    /// runtime they run on.
    #[cfg(test)]
    test_live_sessions:
        RefCell<std::collections::BTreeMap<String, Arc<tilo_live::live_session::LiveSession>>>,
    #[cfg(test)]
    test_runtime: RefCell<Option<tokio::runtime::Handle>>,
}

/// Whether the person of `device` may read `workspace`'s files, as the workspace's people and
/// grants decide it now; `false` for a device the workspace does not admit.
#[cfg(target_os = "linux")]
fn reads_files(engine: Option<&Engine>, workspace: &str, device: EndpointId) -> bool {
    let (Some(engine), Some(identity)) =
        (engine, tilo_desktop_model::WorkspaceId::parse(workspace))
    else {
        return false;
    };
    let now = tilo_clock::wall();
    engine
        .owner()
        .with_current(identity, |view| {
            let member = view
                .peer()
                .permissions(*device.as_bytes(), now)
                .map_err(|error| error.to_string())?;
            let Some(person) = member.person_id() else {
                return Ok(false);
            };
            let policy = view.peer().governance().governed_policy()?;
            let context = tilo_action_policy::Context::at_time(now);
            Ok(
                tilo_workspace_service::files::access::decide(&policy, workspace, person, &context)
                    .is_some(),
            )
        })
        .unwrap_or(false)
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
        // What a workspace says of its channels' governance is the session's channels' to take in,
        // wherever the message came from; a record a host answered for decides what a reading of it
        // waits on.
        if let Some(link) = &self.link {
            match event {
                Event::Msg { msg, .. } => link.heard(msg),
                // The person's other devices hand this one the group frames it lacks, and ask
                // for the ones they lack.
                Event::SiblingMsg { peer, msg } => link.heard_from_sibling(*peer.as_bytes(), msg),
                Event::DocumentOpened {
                    ws,
                    channel,
                    answer,
                    ..
                } => {
                    link.resource_opened(*ws, *channel, *answer);
                }
                _ => {}
            }
        }
        // A window's stream is not presence: it goes to the shares, which read it on the pass
        // ([`Self::take_surfaces`]).
        if self.keep_surface(event) {
            return true;
        }
        // What a workspace's members say about a record's content is the records' own content service's to
        // take, and the devices it can be handed over from follow them.
        self.follow_content_sources(event, client);
        if self.keep_content(event, client) {
            return true;
        }
        false
    }

    /// Keep `event`'s message for the shares if it is one of a window's stream; whether it was.
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

    /// Hand `event` to the content store when it is one of a record's content messages; whether it
    /// was, in which case nothing else here takes it. Every such message reached here through the
    /// workspace's policy, which admitted the device that sent it; the bytes of the workspace's
    /// files go only to a device whose person may read them.
    fn keep_content(&self, event: &Event, client: &SessionClient) -> bool {
        let Some(content) = self.content.borrow().clone() else {
            return false;
        };
        let Event::Msg { ws, peer, msg } = event else {
            return false;
        };
        let Some(workspace) = client.workspace_id(*ws) else {
            return false;
        };
        let engine = self.engine.as_deref();
        let may_read = |app: &str| {
            app != fs_protocol::tree::FILES_APP || reads_files(engine, &workspace, *peer)
        };
        content.heard(*ws, &workspace, *peer, msg, &may_read)
    }

    /// Keep what reached here of the devices a workspace's content can be handed over from: those
    /// that said anything on its topic, the hosts that serve it and the members they report, and
    /// those that left. A record's content follows them ([`content::ContentService::sources_changed`]).
    fn follow_content_sources(&self, event: &Event, client: &SessionClient) {
        let Some(content) = self.content.borrow().clone() else {
            return;
        };
        let (ws, changed) = match event {
            Event::Msg { ws, peer, .. } => (
                *ws,
                self.content_devices(*ws, |devices| devices.heard(*peer)),
            ),
            Event::HostReady { ws, peer, .. } => (
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
            let workspace = client.workspace_id(ws);
            let topologies = self.topologies.borrow();
            match (workspace, topologies.get(&ws)) {
                (Some(id), Some(topology)) => (
                    topology.sources(client.publisher()),
                    Some((id, topology.devices(client.publisher()))),
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

    /// Apply `change` to the devices this session reaches in the workspace on `ws`; whether that
    /// was news.
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

    /// Take the window-stream messages that reached this device since the last call.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_surfaces(&self) -> Vec<ReceivedSurface> {
        std::mem::take(&mut *self.surfaces.borrow_mut())
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
        for (workspace, live_session) in self.test_live_sessions.borrow().iter() {
            while let Some(heard) = live_session.try_recv() {
                if matches!(heard.track, Track::Page(_) | Track::Window(..)) {
                    self.window_objects
                        .borrow_mut()
                        .push((workspace.clone(), heard));
                }
            }
        }
        std::mem::take(&mut *self.window_objects.borrow_mut())
    }

    /// The workspaces' live sessions this session is in now, by workspace.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_sessions(
        &self,
    ) -> std::collections::BTreeMap<String, Arc<tilo_live::live_session::LiveSession>> {
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

    /// The endpoints this device's relay turned away from each workspace's live session and has not
    /// let in since, by workspace, and why ([`crate::live::TurnedAway`]): nothing for a session
    /// that runs no live session.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_turned_away(
        &self,
    ) -> std::collections::BTreeMap<String, Vec<crate::live::TurnedAway>> {
        self.live()
            .map(|live| {
                live.turned_away()
                    .into_iter()
                    .map(|(workspace, turned)| (workspace.to_string(), turned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Show `tracks` of `sender` on `live_session`, and no other of its tracks: the live session's readers start
    /// on the runtime the live sessions run on, not on the session's own thread.
    #[cfg(target_os = "linux")]
    pub(crate) fn show_tracks(
        &self,
        live_session: &tilo_live::live_session::LiveSession,
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
                    if let Some(link) = &self.link {
                        link.sibling_arrived();
                    }
                    if let Some(nearby) = self.nearby() {
                        nearby.devices_changed();
                    }
                }
                DesktopEvent::SiblingLeft { peer } => {
                    if let Some(fabric) = &self.fabric {
                        fabric.sibling_left(peer);
                    }
                    if let Some(nearby) = self.nearby() {
                        nearby.devices_changed();
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
                DesktopEvent::NearbyTransferProgress {
                    transfer_id,
                    sent_bytes,
                    total_bytes,
                    status,
                } => {
                    if let Some(nearby) = self.nearby() {
                        nearby.progress(transfer_id, sent_bytes, total_bytes, status);
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

    /// Hand the records' content store its place on this client: the workspaces' topics are the
    /// records' routes, and what it asks for and answers there travels with this device's own
    /// messages.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_content(
        &self,
        content: Arc<content::ContentService>,
        publisher: EndpointId,
    ) {
        content.bind(
            HostOutbox {
                commands: self.commands.clone(),
            },
            publisher,
        );
        self.content.replace(Some(content));
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
    pub(crate) fn fabric_actions(
        &self,
    ) -> Vec<crate::platform::fabric::PersonalFabricActionEnvelope> {
        let mut actions = std::mem::take(&mut *self.fabric_waiting.borrow_mut());
        if let Some(fabric) = &self.fabric {
            actions.extend(fabric.take_actions());
        }
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

    /// The workspaces' live session: what this session says into and hears from each of them, and the
    /// relay each one is carried by; `None` when this session holds no workspaces.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live(&self) -> Option<std::cell::Ref<'_, crate::live::Live>> {
        std::cell::Ref::filter_map(self.live.borrow(), Option::as_ref).ok()
    }

    /// This device's workspaces, when this session holds any.
    #[must_use]
    pub(crate) fn engine(&self) -> Option<&Arc<Engine>> {
        self.engine.as_ref()
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
        self.engine
            .as_ref()
            .is_some_and(|engine| engine.invitations_open(tilo_clock::wall()))
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

    #[cfg(target_os = "linux")]
    /// Bring where this person's own agent works into view, when it moved or acted since the last
    /// look, among the people this surface lists ([`tilo_session_core::desktop::here::reveal_own_agent_work`]).
    /// Whether it asked the strip to move.
    pub(crate) fn reveal_own_agent_work(&self, people: &[Person], handle: &DesktopHandle) -> bool {
        tilo_session_core::desktop::here::reveal_own_agent_work(
            &mut self.here.borrow_mut(),
            people,
            handle,
        )
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
    /// the agent's cursor in the window showing its record; and where the person's own agent works
    /// comes into view on the surface's next frame ([`Self::reveal_own_agent_work`]). Whether
    /// `event` was one.
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
            tilo_session_core::desktop::here::note_narrated(&mut self.here.borrow_mut(), &work);
            self.for_apps
                .borrow_mut()
                .extend(work.into_iter().map(|work| (workspace.clone(), work)));
        }
        true
    }

    /// Take the waves at this person heard since the last call: the bar says who waved, for a
    /// moment.
    fn show_waves(&self) {
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
        let local = &self.local;
        let engine = local.engine.as_ref()?;
        let identity = tilo_desktop_model::WorkspaceId::parse(workspace)?;
        let now = tilo_clock::wall();
        engine
            .owner()
            .with_current(identity, |view| {
                view.peer()
                    .permissions(*device.as_bytes(), now)
                    .map_err(|error| error.to_string())
                    .map(|member| member.person_id().map(str::to_owned))
            })
            .ok()
            .flatten()
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
        let engine = self.local.engine.as_ref()?;
        engine.holder_of(tilo_desktop_model::WorkspaceId::parse(workspace)?)
    }

    fn joined(&self, workspace: &str) {
        let local = &self.local;
        let Some(engine) = local.engine.as_ref() else {
            return;
        };
        if let Some(connect) = tilo_desktop_model::WorkspaceId::parse(workspace)
            .and_then(|session| native_ingress::Connect::new(engine.owner().clone(), session).ok())
        {
            let _ = local.commands.send(HostCmd::TakePart(connect));
        }
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
        // What no device acknowledged of a send this device made is offered again, and what the
        // person's devices offered here waits for their answer.
        if let Some(nearby) = local.nearby() {
            nearby.pump();
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
    /// Where this device was given and reached its hosts ([`live_session::Reached`]).
    #[cfg(target_os = "linux")]
    reached: network::Reached,
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
            // The live sessions dial through this endpoint: they end before it does,
            // and before the thread that holds it joins.
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
    /// The session's stream of record changes ([`crate::platform::records::Changes`]): what
    /// waits on a record of this device's workspace service, such as the cloud service's ask for
    /// accounts that changed, is told by it.
    pub records: crate::platform::records::Changes,
}

/// The live session's relay role, when this device may serve one: one more protocol on the
/// endpoint this session binds, among `beside`, and every protocol the endpoint serves then.
#[cfg(target_os = "linux")]
fn live_beside(
    engine: Option<&Arc<Engine>>,
    wake: &Arc<dyn Fn() + Send + Sync>,
    beside: &mut tilo_network_actor::own_service::Protocols,
) -> (Option<crate::live::Live>, Vec<Vec<u8>>) {
    let mut live =
        engine.map(|engine| crate::live::Live::new(Arc::clone(engine), Arc::clone(wake)));
    if let Some(live) = &mut live {
        beside.extend(live.protocols());
    }
    let served = beside.iter().map(|(alpn, _)| alpn.clone()).collect();
    (live, served)
}

impl Client {
    /// Take part in the peer network: bind this device's endpoint, start the peer actor,
    /// and from then on hear the workspaces the session joins. `beside` is what else this session
    /// serves on that endpoint (its protocols are fixed once it binds), `documents` is where
    /// what admission takes in from another device goes, and `service` is where this device's own
    /// workspace service is installed once it opens.
    ///
    /// # Errors
    /// The endpoint could not be bound.
    #[cfg(target_os = "linux")]
    pub fn start(
        entry: Entry,
        engine: Option<Arc<Engine>>,
        questions: Option<tilo_service::AskSender>,
        mut beside: tilo_network_actor::own_service::Protocols,
        documents: Option<PeerDocuments>,
        service: tilo_workspace_service::service::ServiceSlot,
    ) -> std::io::Result<Self> {
        let (identity, wake, records) = (entry.identity, entry.wake, entry.records);
        let (commands, host_commands) = tokio::sync::mpsc::unbounded_channel();
        let (arrivals, arrived) = std::sync::mpsc::channel();
        let (desktop_events, desktop_arrived) = std::sync::mpsc::channel();
        let (enrollments, enrolled) = std::sync::mpsc::channel();
        let (ready, bound) = std::sync::mpsc::channel();
        let (user, device) = certified_as(identity.as_ref());
        // What this device hears on its local network wakes the session, which shows which of the
        // person's devices are nearby.
        let lan = entry.lan.unwrap_or_else(|| Nearby::new(Arc::clone(&wake)));
        let host_outbox = HostOutbox {
            commands: commands.clone(),
        };
        let outgoing: Arc<dyn Outbox> = Arc::new(host_outbox.clone());
        let link_outbox = host_outbox.clone();
        // What is the person's alone, read from their own workspace once this device's service
        // holds it.
        let person = crate::PersonResources::default();
        let fabric = fabric_of(
            identity.as_ref(),
            person.clone(),
            host_outbox,
            entry.continuity,
            lan.clone(),
        );
        // What the actor serves is installed before it starts: every actor built after answers it.
        let versions = crate::network::install(fabric.as_ref(), questions);
        // The channels' side of the peer network is the engine's, opened before the peer network starts.
        let link = engine.as_ref().and_then(|engine| engine.link());
        let (network_link, enrolling) = (link.clone(), engine.clone());
        let device_actions = Arc::new(std::sync::OnceLock::new());
        let serving = Arc::clone(&device_actions);
        let endpoint_handle = Arc::new(std::sync::OnceLock::new());
        let binding = network::Bound::new(Arc::clone(&endpoint_handle), entry.relays, lan);
        let reached = binding.reached.clone();
        let (mut live, served) = live_beside(engine.as_ref(), &wake, &mut beside);
        let thread = std::thread::Builder::new()
            .name("tilo-sync".into())
            .spawn(move || {
                run(
                    identity,
                    enrolling,
                    host_commands,
                    Wire {
                        arrivals,
                        desktop_events,
                        enrollments,
                        ready,
                    },
                    wake,
                    Serving::new(
                        binding,
                        network_link,
                        serving,
                        beside,
                        documents,
                        (service, records, person),
                    ),
                );
            })?;
        let (publisher, endpoint, reachability) = wait_for_bound(&bound)?;
        let saying = attach_bound(&mut live, link.as_ref(), link_outbox, endpoint_handle.get());
        let inner = SessionClient::over(
            Transport {
                outgoing: Arc::clone(&outgoing),
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
        )
        .serving(served)
        .bound_to(endpoint_handle);
        let hosting = Local::hosting(Hosted {
            commands,
            desktop_events: desktop_arrived,
            engine,
            link,
            versions,
            fabric,
            device_actions,
            reachability,
            live,
        });
        Ok(Self::started(
            inner,
            hosting,
            outgoing,
            thread,
            reached,
            entry.hosts,
        ))
    }

    /// The client, hearing the person's other devices and then the hosts the entry names.
    #[cfg(target_os = "linux")]
    fn started(
        inner: SessionClient,
        hosting: Rc<Hosting>,
        outgoing: Arc<dyn Outbox>,
        thread: std::thread::JoinHandle<()>,
        reached: network::Reached,
        hosts: Vec<EndpointAddr>,
    ) -> Self {
        inner.attach_host(Rc::clone(&hosting) as Rc<dyn Host>);
        let client = Self {
            inner,
            hosting,
            outgoing,
            thread: Some(thread),
            reached,
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
            #[cfg(target_os = "linux")]
            #[cfg(target_os = "linux")]
            reached: network::Reached::default(),
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
        let (_unlanded, enrolled) = std::sync::mpsc::channel();
        let (commands, _host_commands) = tokio::sync::mpsc::unbounded_channel();
        let (_desktop_events, desktop_arrived) = std::sync::mpsc::channel();
        let outbox: Arc<dyn Outbox> = Arc::new(outgoing);
        let inner = SessionClient::over(
            Transport {
                outgoing: Arc::clone(&outbox),
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
            reached: network::Reached::default(),
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

    /// Take the window-stream messages that reached this device since the last call.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_surfaces(&self) -> Vec<ReceivedSurface> {
        self.hosting.local.take_surfaces()
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

    /// The workspaces' live sessions this session is in now, by workspace: what a call's voice and
    /// pictures and a shared window's pictures and pages travel.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_sessions(
        &self,
    ) -> std::collections::BTreeMap<String, Arc<tilo_live::live_session::LiveSession>> {
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

    /// The endpoints this device's relay turned away from each workspace's live session and has not
    /// let in since, by workspace, and why ([`crate::live::TurnedAway`]).
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_turned_away(
        &self,
    ) -> std::collections::BTreeMap<String, Vec<crate::live::TurnedAway>> {
        self.hosting.local.live_turned_away()
    }

    /// Join this session to `live_sessions`, by workspace, running on `runtime`, in place of a live session
    /// of its own: what a test's sessions meet on.
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    pub(crate) fn on_live_sessions(
        self,
        live_sessions: std::collections::BTreeMap<
            String,
            Arc<tilo_live::live_session::LiveSession>,
        >,
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
        live_session: &tilo_live::live_session::LiveSession,
        sender: [u8; 32],
        tracks: &[Track],
    ) {
        self.hosting.local.show_tracks(live_session, sender, tracks);
    }

    /// Say `msg` to `target` alone, as a workspace-control message on `ws`'s topic: a shared
    /// window's control, directly or through the workspace's host.
    #[cfg(target_os = "linux")]
    pub(crate) fn publish_control(&self, ws: Topic, msg: NeighbourMsg, target: EndpointId) {
        self.inner.say(Cmd::SendControl {
            ws,
            msg: Box::new(msg),
            target,
        });
    }

    /// This device came to hold another workspace: its live session opens that workspace's live session
    /// now ([`crate::live::Live::workspace_arrived`]). Nothing to tell on a session with no live session.
    #[cfg(target_os = "linux")]
    pub(crate) fn workspace_arrived(&self) {
        if let Some(live) = self.hosting.local.live() {
            live.workspace_arrived();
        }
    }

    /// Have this session's live session lent its own streams' media keys by `lender` in every
    /// workspace whose keys it may hold none of ([`crate::live::Live::lend_media_keys`]): false when
    /// it has no live session yet, and nothing was handed over.
    #[cfg(target_os = "linux")]
    pub(crate) fn lend_media_keys(&self, lender: Arc<dyn crate::live::MediaKeysLender>) -> bool {
        let Some(live) = self.hosting.local.live() else {
            return false;
        };
        live.lend_media_keys(lender);
        true
    }

    /// Give this session's Nearby Send the fabric it runs on.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_nearby(&self, nearby: Arc<crate::nearby_transfer::NearbyTransfer>) {
        self.hosting.local.attach_nearby(nearby);
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

    /// Hand `take` each action one of the person's devices sent this one since the last look, the
    /// moment it is here; what it does not take waits for [`Self::fabric_actions`] .
    #[cfg(target_os = "linux")]
    pub(crate) fn fabric_actions_now(
        &self,
        take: impl Fn(&crate::platform::fabric::PersonalFabricActionEnvelope) -> bool,
    ) {
        let local = &self.hosting.local;
        local.keep_desktop_events(&self.inner);
        let left: Vec<_> = local
            .fabric_actions()
            .into_iter()
            .filter(|envelope| !take(envelope))
            .collect();
        local.fabric_waiting.borrow_mut().extend(left);
    }

    /// Where this device's actions are served.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn device_actions(&self) -> Option<DeviceActions> {
        self.hosting.local.device_actions()
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

    /// Where this device notes the hosts it was given an address for and reached there: its
    /// endpoint dials them there from then on, beside whatever they say of themselves
    /// ([`Reached`]).
    #[cfg(target_os = "linux")]
    pub(crate) fn reached(&self) -> Reached {
        self.reached.clone()
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

    /// Hand the records' content store its place on this client.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_content(&self, content: Arc<content::ContentService>) {
        self.hosting.local.attach_content(content, self.publisher());
    }

    /// This device's workspaces, when this session holds any.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn engine(&self) -> Option<&Arc<Engine>> {
        self.hosting.local.engine()
    }

    /// Take `request`'s workspace in as this person's further device, off the loop: what the
    /// workspace holds before arrives with the host's answer. [`Self::take_enrolled`] says what
    /// became of it.
    ///
    /// # Errors
    /// The peer network takes no entry: this session holds no workspaces, or it has stopped.
    #[cfg(target_os = "linux")]
    pub fn enter(&self, request: Entering) -> Result<(), String> {
        if self.hosting.local.engine.is_none() {
            return Err("this session keeps no workspaces to take in".into());
        }
        self.hosting
            .local
            .commands
            .send(HostCmd::Enter(Box::new(request)))
            .map_err(|_| "the peer network is gone".to_owned())
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
        // An action for this device is sent to where it was told to be,
        // without waiting for the two sessions to meet first.
        if let Some(actions) = self.hosting.local.device_actions.get() {
            actions.learn(addr.clone());
        }
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

    #[cfg(target_os = "linux")]
    /// Bring where this person's own agent works into view, when one of them moved or acted since
    /// the surface last looked, among the `people` it lists
    /// ([`tilo_session_core::desktop::here::reveal_own_agent_work`]): the strip scrolls to the window without
    /// taking the keyboard. Whether it asked the strip to move.
    pub fn reveal_own_agent_work(&self, people: &[Person], handle: &DesktopHandle) -> bool {
        self.hosting.roster.reveal_own_agent_work(people, handle)
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
    engine: Option<Arc<Engine>>,
    link: Option<Arc<KeyLink>>,
    versions: std::sync::mpsc::Receiver<crate::network::VersionMismatch>,
    fabric: Option<Arc<SessionFabric>>,
    device_actions: Arc<std::sync::OnceLock<DeviceActions>>,
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
            engine,
            link,
            versions,
            fabric,
            device_actions,
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
                fabric_waiting: RefCell::default(),
                surfaces: RefCell::new(Vec::new()),
                call_audio: RefCell::new(Vec::new()),
                call_pictures: RefCell::new(Vec::new()),
                window_objects: RefCell::new(Vec::new()),
                deleted: RefCell::new(Vec::new()),
                engine,
                link,
                versions,
                content: RefCell::new(None),
                topologies: RefCell::new(std::collections::BTreeMap::new()),
                fabric,
                nearby: RefCell::new(None),
                device_actions,
                reachability: RefCell::new(reachability),
                live: RefCell::new(live),
                #[cfg(test)]
                test_live_sessions: RefCell::default(),
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
            fabric_waiting: RefCell::default(),
            surfaces: RefCell::new(Vec::new()),
            call_audio: RefCell::new(Vec::new()),
            call_pictures: RefCell::new(Vec::new()),
            window_objects: RefCell::new(Vec::new()),
            deleted: RefCell::new(Vec::new()),
            engine: None,
            link: None,
            versions,
            content: RefCell::new(None),
            topologies: RefCell::new(std::collections::BTreeMap::new()),
            fabric: None,
            nearby: RefCell::new(None),
            device_actions: Arc::new(std::sync::OnceLock::new()),
            reachability: RefCell::new(Reachability::default()),
            live: RefCell::new(None),
            #[cfg(test)]
            test_live_sessions: RefCell::default(),
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
        // Secrets the person's devices seal to this one open with its endpoint's key.
        device_signing_key: ed25519_dalek::SigningKey::from_bytes(
            &identity.signer.endpoint_secret()?,
        ),
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
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::empty()));
    if relays.n0_dns {
        builder = builder
            .address_lookup(tilo_endpoint_reachability::collaborating())
            .address_lookup(iroh::address_lookup::DnsAddressLookup::n0_dns());
    }
    Ok((builder, relays.relays.clone()))
}

/// Where an admitted peer's app document goes so this device's apps see it as they see this
/// device's own writes: the broker's channels, which record it and answer an app's subscription,
/// and its fan-out, which shows it to the apps that hold its route open.
#[derive(Clone)]
#[cfg(target_os = "linux")]
pub struct PeerDocuments {
    channels: Arc<std::sync::Mutex<tilo_broker_core::collab_core::BrokerChannels>>,
    fanout: Arc<tilo_workspace_service::apps::collab_broker::FanoutState>,
    /// What search reads again of a record a peer wrote.
    changes: Option<Arc<dyn tilo_workspace_service::apps::services::DocumentIndexChanges>>,
    /// What wakes a read waiting for a record this device held none of until it lands.
    arrivals: Option<Arc<tilo_session_client::Arrivals>>,
}

#[cfg(target_os = "linux")]
impl PeerDocuments {
    /// Hand peer app documents to the broker's `channels` and show them to the apps through
    /// `fanout`.
    #[must_use]
    pub fn new(
        channels: Arc<std::sync::Mutex<tilo_broker_core::collab_core::BrokerChannels>>,
        fanout: Arc<tilo_workspace_service::apps::collab_broker::FanoutState>,
        changes: Option<Arc<dyn tilo_workspace_service::apps::services::DocumentIndexChanges>>,
        arrivals: Option<Arc<tilo_session_client::Arrivals>>,
    ) -> Self {
        Self {
            channels,
            fanout,
            changes,
            arrivals,
        }
    }

    /// Show this device's apps every write of another device that `workspaces`, this device's
    /// workspace service, takes in from now on: a member's write to a workspace it governs, and
    /// what the device serving a workspace this device joined sends. Also tell them each record
    /// of theirs whose reach moved, as this device's own writes reach the workspace's other
    /// devices. Followed until the service stops.
    pub(crate) fn follow(&self, workspaces: &tilo_workspace_service::workspaces::Workspaces) {
        let documents = self.clone();
        let arrivals = self.arrivals.clone();
        let fanout = Arc::clone(&self.fanout);
        tokio::spawn(
            tilo_workspace_service::apps::collab_inbound::follow_workspace_writes(
                workspaces,
                fanout,
                move |document| documents.deliver(document),
                // Writes went by unheard: every read waiting for one reads again.
                move || {
                    if let Some(arrivals) = &arrivals {
                        arrivals.landed();
                    }
                },
            ),
        );
    }

    /// Hand one app write the session wrote through this device's workspace service to this
    /// device's apps on its route.
    pub(crate) fn deliver_write(&self, write: tilo_peer_state::owner::AppWrite) {
        self.deliver(
            tilo_workspace_service::apps::collab_inbound::RemoteDocument {
                app_id: write.app_id,
                cluster_id: write.cluster_id,
                document_id: write.document_id,
                author: write.author,
                replayed: write.replayed,
                envelope: write.envelope,
            },
        );
    }

    /// Hand one app write to this device's apps on its route: another device's, or one the
    /// session wrote itself.
    fn deliver(&self, document: tilo_workspace_service::apps::collab_inbound::RemoteDocument) {
        let (workspace, app, record) = (
            document.cluster_id.clone(),
            document.app_id.clone(),
            document.document_id.clone(),
        );
        tilo_workspace_service::apps::collab_inbound::deliver(
            &self.channels,
            &self.fanout,
            document,
        );
        // What search holds of the record is read again from what was written into it.
        if let Some(changes) = &self.changes {
            changes.changed(&workspace, &app, &record);
        }
        // A read of the record that this device held none of until now waits for it, and one that
        // holds it open reads what landed: a write on its route answers both, and nothing else
        // does.
        if let Some(arrivals) = &self.arrivals {
            arrivals.landed();
        }
        if let Some(state) = self.fanout.app_broker() {
            state.wake_document_reads(&workspace, &app, &record);
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod membership_check_tests;

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;
