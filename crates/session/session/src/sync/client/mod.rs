//! This desktop's half of the collaboration plane: the peer actor on this device's own iroh
//! endpoint, the live plane, the workspaces this session holds and their channels, the person's
//! device fabric, the transfers between their devices, and the roster the bar draws.
//!
//! The participant itself is `tilo_session_client::Client`, which every host links. [`Client`] here
//! is that client plus this desktop's host. The endpoint keeps number 0's directory behind a gate
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
use tilo_desktop::here::{Here, LiveMember, Person};
use tilo_desktop_model::DesktopTree;
#[cfg(target_os = "linux")]
use tilo_desktop_surface::DesktopHandle;
#[cfg(target_os = "linux")]
use tilo_live::live::Track;
#[cfg(target_os = "linux")]
use tilo_live::plane::Heard;
use tilo_live::presence;
#[cfg(target_os = "linux")]
use tilo_session_client::client::Desk as GateDesk;
use tilo_session_client::client::Host;
#[cfg(target_os = "linux")]
use tilo_session_client::gate::WorkspaceAction;
use tilo_session_client::{Client as SessionClient, Device, OwnPresence, Transport};
#[cfg(target_os = "linux")]
use tilo_session_client::{Cmd, Enrolled, Event, Outbox, SayPresence, Topic};

#[cfg(target_os = "linux")]
use super::{Enrolling, Entering};
#[cfg(target_os = "linux")]
use crate::channels::ChannelLink;
#[cfg(target_os = "linux")]
use crate::content;
#[cfg(target_os = "linux")]
use crate::device_actions::DeviceActions;
#[cfg(target_os = "linux")]
use crate::engine::Engine;
#[cfg(target_os = "linux")]
use crate::platform::fabric::DeviceId;
#[cfg(target_os = "linux")]
use crate::platform::fabric::FabricRuntime;
#[cfg(target_os = "linux")]
use crate::platform::fabric::{Continuity, SessionFabric};
#[cfg(target_os = "linux")]
use tilo_action_broker::record_content::ContentSources;
#[cfg(target_os = "linux")]
use tilo_endpoint_reachability::{Need, Reachability};
#[cfg(target_os = "linux")]
use tilo_identity_host::custody::HostIdentity;
#[cfg(target_os = "linux")]
use tilo_network_actor::{WorkspaceId, native_ingress};

#[cfg(target_os = "linux")]
mod adopt;
#[cfg(target_os = "linux")]
mod plane;

#[cfg(target_os = "linux")]
use plane::{Serving, Wire, run};

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
#[derive(Clone, Debug)]
#[cfg(target_os = "linux")]
pub(crate) struct ReceivedSurface {
    /// The workspace's topic it came in on.
    pub(crate) ws: Topic,
    /// The device that published it.
    pub(crate) from: EndpointId,
    pub(crate) msg: NeighbourMsg,
}

/// What this desktop's endpoint carries beside the client's own commands: the custody sessions and
/// records its workspaces are held through, its device fabric, and the transfers between the
/// person's devices.
#[derive(Debug)]
#[cfg(target_os = "linux")]
pub(crate) enum HostCmd {
    /// One of the client's own commands.
    Plane(Cmd),
    /// Take part in a workspace through this device's own custody session with its host.
    TakePart(native_ingress::Connect),
    /// Send one piece of a record's content to the device of the workspace that asked for it.
    Content(tilo_network_actor::RecordContentDelivery),
    /// Redeem an invitation into the workspace the desk made for it.
    Enroll(Enrolling),
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
    fn send(&self, command: Cmd) -> bool {
        self.plane(command)
    }
}

#[cfg(target_os = "linux")]
impl HostOutbox {
    /// An outbox over `commands`, for a service that speaks the plane's vocabulary and nothing
    /// else.
    #[cfg(test)]
    pub(crate) fn of(commands: tokio::sync::mpsc::UnboundedSender<HostCmd>) -> Self {
        Self { commands }
    }

    /// Say one of the client's own commands to the plane; `false` when the transport is gone.
    pub(crate) fn plane(&self, command: Cmd) -> bool {
        self.say(HostCmd::Plane(command))
    }

    /// Say one command of this desktop's own to the plane.
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

/// What the peer actor and the fabric said that only this desktop keeps: the person's other devices'
/// fabric traffic, and how far a transfer this device is sending got.
#[derive(Debug)]
#[cfg(target_os = "linux")]
enum HostEvent {
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
/// desks is where each tells the other the workspaces it holds, and what it hosts, so one can adopt
/// what the other keeps.
#[derive(Clone)]
#[cfg(target_os = "linux")]
pub(crate) struct SiblingDials {
    /// This device's endpoint id: its own session is never dialled.
    ours: [u8; 32],
    outgoing: HostOutbox,
}

#[cfg(target_os = "linux")]
impl SiblingDials {
    /// Dial from `ours`, the endpoint this device's sessions run on.
    pub(crate) fn new(ours: [u8; 32], outgoing: HostOutbox) -> Self {
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

/// Everything this desktop keeps of the plane beside the client's boards: who is here as the bar
/// lists them, the workspaces' state and channels, the live plane, the fabric and the transfers.
#[cfg(target_os = "linux")]
pub(crate) struct Seat {
    /// The desk this session shows, whose workspaces' policies every message from another member is
    /// held to.
    desk: RefCell<Option<DesktopHandle>>,
    /// Where this device's own commands go: the client's vocabulary is its own, this is the
    /// desktop's own beside it.
    commands: tokio::sync::mpsc::UnboundedSender<HostCmd>,
    /// What the peer actor and the fabric said that only this desktop keeps.
    host_events: std::sync::mpsc::Receiver<HostEvent>,
    /// The window streams that reached this device: their frames, starts, ends and control, which
    /// the shares take ([`Self::take_surfaces`]).
    surfaces: RefCell<Vec<ReceivedSurface>>,
    /// The call audio the workspaces' live planes heard since the last call, each with the
    /// workspace it was heard in: what the calls playing it take ([`Self::take_call_audio`]).
    call_audio: RefCell<Vec<(String, Heard)>>,
    /// The call pictures the workspaces' live planes heard since the last call, each with the
    /// workspace it was heard in: what the calls playing there draw
    /// ([`Self::take_call_pictures`]).
    call_pictures: RefCell<Vec<(String, Heard)>>,
    /// The window pages the workspaces' live planes heard since the last call, each with the
    /// workspace it was heard in: what the desks showing those windows draw
    /// ([`Self::take_page_objects`]).
    page_objects: RefCell<Vec<(String, Heard)>>,
    /// The workspaces whose host said their owner deleted them since the last call: what the
    /// session takes off the desk and out of this device's record ([`Self::take_deleted`]).
    deleted: RefCell<Vec<tilo_desktop_model::WorkspaceId>>,
    /// This device's workspaces: `None` when the session keeps no state to hold them in.
    engine: Option<Arc<Engine>>,
    /// What the session's channels hear and say on this plane. What the engine holds, so a session
    /// that runs no channel service has none.
    link: Option<Arc<ChannelLink>>,
    /// The collaborator's devices the plane heard at another peer protocol version since the last
    /// call: what the person is answered.
    versions: std::sync::mpsc::Receiver<crate::network::VersionMismatch>,
    /// The records' content this session keeps and asks the workspaces for: `None` until the
    /// session hands its plane over ([`Self::attach_content`]).
    content: RefCell<Option<Arc<content::Plane>>>,
    /// The devices each workspace's record content reaches this one from: those heard on its topic,
    /// and its hosts.
    topologies: RefCell<std::collections::BTreeMap<WorkspaceId, content::Topology>>,
    /// The person's own store this session keeps for their other devices, served on this device's
    /// endpoint from the pass it binds. `None` on a session that has no state to keep it in.
    personal_store: RefCell<Option<crate::personal::Personal>>,
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
    /// The workspaces' live plane: their relays and this device's sessions in them.
    live: RefCell<Option<crate::live::Live>>,
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for Seat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seat").finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
impl Seat {
    /// What this desktop's plane does with one message of the client's own.
    fn heard(&self, event: &Event, client: &SessionClient) -> bool {
        // What a workspace says of its channels' governance is the session's channels' to take in,
        // wherever the message came from; a record a host answered for decides what a reading of it
        // waits on.
        if let Some(link) = &self.link {
            match event {
                Event::Msg { msg, .. } => link.heard(msg),
                Event::RecordOpened {
                    ws,
                    channel,
                    frames,
                    ..
                } => {
                    link.record_opened(*ws, *channel, *frames);
                }
                _ => {}
            }
        }
        if let Some(fabric) = &self.fabric {
            match event {
                Event::Sibling {
                    peer,
                    certs,
                    revocations,
                } => fabric.sibling(*peer, certs, revocations),
                Event::SiblingLeft { peer } => fabric.sibling_left(*peer),
                _ => {}
            }
        }
        // What a device coming or going means for what Files may offer, and how far a stream this
        // device is sending got: the Nearby Send engine keeps both.
        if matches!(event, Event::Sibling { .. } | Event::SiblingLeft { .. })
            && let Some(nearby) = self.nearby()
        {
            nearby.devices_changed();
        }
        // A window's stream is not presence: it goes to the shares, which read it on the pass
        // ([`Self::take_surfaces`]).
        if self.keep_surface(event) {
            return true;
        }
        // What a workspace's members say about a record's content is the records' own plane's to
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

    /// Hand `event` to the content plane when it is one of a record's content messages; whether it
    /// was, in which case nothing else here takes it. Every such message reached here through the
    /// workspace's policy, which admitted the device that sent it.
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
        content.heard(*ws, &workspace, *peer, msg)
    }

    /// Keep what reached here of the devices a workspace's content can be handed over from: those
    /// that said anything on its topic, the hosts that serve it and the members they report, and
    /// those that left. A record's content follows them ([`content::Plane::sources_changed`]).
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

    /// Take the call audio the workspaces' live planes heard since the last call, each with the
    /// workspace it was heard in and already opened. A call plays the frames of the devices its
    /// channel says are in it.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_call_audio(&self) -> Vec<(String, Heard)> {
        std::mem::take(&mut *self.call_audio.borrow_mut())
    }

    /// Take the call pictures the workspaces' live planes heard since the last call, each with the
    /// workspace it was heard in and already opened. A call draws the pictures of the devices its
    /// channel says are in it.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_call_pictures(&self) -> Vec<(String, Heard)> {
        std::mem::take(&mut *self.call_pictures.borrow_mut())
    }

    /// Take the window pages the workspaces' live planes heard since the last call, each with the
    /// workspace it was heard in and already opened: what the desk showing that window draws.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_page_objects(&self) -> Vec<(String, Heard)> {
        std::mem::take(&mut *self.page_objects.borrow_mut())
    }

    /// Send one message of `surface`'s stream on the topic of the workspace it is shared in, to
    /// `viewers` alone: over the authenticated route to each of them, never fanned out to the
    /// workspace.
    #[cfg(target_os = "linux")]
    /// Take what the peer actor and the fabric said that only this desktop keeps.
    fn keep_host_events(&self, client: &SessionClient) {
        while let Ok(event) = self.host_events.try_recv() {
            match event {
                HostEvent::WorkspaceDeleted { ws } => {
                    if let Some(workspace) = client
                        .workspace_id(ws)
                        .and_then(|id| tilo_desktop_model::WorkspaceId::parse(&id))
                    {
                        self.deleted.borrow_mut().push(workspace);
                    }
                }
                HostEvent::PersonalFabric {
                    peer,
                    payload,
                    received,
                } => {
                    if let Some(fabric) = &self.fabric {
                        fabric.arrived(peer, &payload, received);
                    }
                }
                HostEvent::NearbyTransferProgress {
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

    /// Tell the person's other devices what this device says of itself, when that moved since they
    /// last heard: the session's pass calls this once it offered the clipboard, so a copy leaves on
    /// the pass that made it.
    pub(crate) fn announce_fabric(&self) {
        if let Some(fabric) = &self.fabric {
            fabric.announce(true);
        }
    }

    /// This device's fabric, when the person certified it: the link the session's services act on.
    #[must_use]
    pub(crate) fn fabric(&self) -> Option<crate::platform::fabric::SharedLink> {
        self.fabric
            .as_ref()
            .map(|fabric| Arc::clone(fabric) as crate::platform::fabric::SharedLink)
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

    /// Hand the records' content plane its place on this client: the workspaces' topics are the
    /// records' routes, and what it asks for and answers there travels with this device's own
    /// messages.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_content(&self, content: Arc<content::Plane>, publisher: EndpointId) {
        content.bind(
            HostOutbox {
                commands: self.commands.clone(),
            },
            publisher,
        );
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

    /// The workspaces' live plane: what this session says into and hears from each of them, and the
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

    /// The collaborator's devices the plane heard at another peer protocol version since the last
    /// call, each once per device and version.
    #[cfg(target_os = "linux")]
    pub(crate) fn take_version_mismatches(&self) -> Vec<crate::network::VersionMismatch> {
        self.versions.try_iter().collect()
    }

    /// Why what this session takes part in needs this device reachable, if it does: a workspace it
    /// shows has someone else here. A host is dialled at start and on every reconnect, and the peer
    /// actor holds the relays up for every remote party it is connected to or keeps dialling
    /// ([`tilo_endpoint_reachability::Need::Transport`]).
    fn reachability_need(client: &SessionClient) -> Option<&'static str> {
        let own = client.publisher();
        client
            .anyone_else_here(own)
            .then_some("a workspace is shown with someone else")
    }

    /// Tell the relays what this session has recorded: they stay up while a workspace it shows has
    /// someone else here, and may rest once none does ([`tilo_endpoint_reachability::REST_AFTER`]).
    fn note_reachability(&self, client: &SessionClient) {
        self.reachability
            .borrow()
            .need(Need::Session, Self::reachability_need(client));
    }

    /// Take every object the workspaces' live planes heard since the last call: a member's presence
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
                if matches!(object.track, Track::Page(_)) {
                    // A window's page, already opened: the desk showing that window draws it.
                    self.page_objects
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
    wave_notice: RefCell<Option<tilo_desktop::chrome::WaveNotice>>,
    /// The workspace whose arrival the bar is saying, so a person who comes to a workspace where
    /// people are is shown arriving once, not on every roster change.
    arrived_in: RefCell<Option<String>>,
    /// The arrival the bar shows.
    arrival: RefCell<Option<tilo_desktop::chrome::ArrivalNotice>>,
}

/// What this session keeps of the plane beside the client's boards: the roster every surface reads,
/// and, on a desktop, the peer actor's own half.
pub(crate) struct Hosting {
    roster: Rc<Roster>,
    #[cfg(target_os = "linux")]
    seat: Rc<Seat>,
}

impl Roster {
    /// A roster nobody is on yet.
    fn new() -> Self {
        Self {
            here: Here::default(),
            wave_notice: RefCell::new(None),
            arrived_in: RefCell::new(None),
            arrival: RefCell::new(None),
        }
    }

    /// Who is here, as the desktop lists them: `tree` places them where their focused record is
    /// shown — `records` (what each window shows, by tab) names the window each person works in —
    /// and `app_name` names an app.
    #[must_use]
    pub(crate) fn people(
        &self,
        tree: &DesktopTree,
        records: &std::collections::BTreeMap<tilo_desktop_model::TabId, String>,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Person> {
        self.here.borrow().people(
            tree,
            &crate::sync::layout::bindings(tree, records),
            app_name,
        )
    }

    /// Who is here, written into `tree`'s active workspace as its collaborators: the roster every
    /// surface that lists people reads.
    pub(crate) fn show(
        &self,
        tree: &mut DesktopTree,
        records: &std::collections::BTreeMap<tilo_desktop_model::TabId, String>,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Person> {
        let people = self.people(tree, records, app_name);
        // Whose view this person follows, so the bar keeps that face when it has room for only one.
        let following = self.here.borrow().following().map(str::to_owned);
        tilo_desktop::here::overlay_collaborators(tree, &people, following.as_deref());
        people
    }

    /// The last wave at this person, for the bar to say.
    #[must_use]
    pub(crate) fn wave_notice(&self) -> Option<tilo_desktop::chrome::WaveNotice> {
        self.wave_notice.borrow().clone()
    }

    /// The arrival the bar shows, while its moment lasts.
    #[must_use]
    pub(crate) fn arrival_notice(&self) -> Option<tilo_desktop::chrome::ArrivalNotice> {
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
        *self.arrival.borrow_mut() = Some(tilo_desktop::chrome::ArrivalNotice {
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
        LiveMember::of(&presence::own_shown(
            &client.own_word_here(),
            *client.publisher().as_bytes(),
        ))
    }

    /// Take the waves at this person heard since the last call: the bar says who waved, for a
    /// moment.
    fn show_waves(&self) {
        let heard = self.here.borrow_mut().take_waves();
        if let Some(last) = heard.last() {
            *self.wave_notice.borrow_mut() = Some(tilo_desktop::chrome::WaveNotice {
                from: last.name.clone(),
                at: ui::time::now(),
            });
        }
    }
}

#[cfg(target_os = "linux")]
impl Host for Hosting {
    fn governed_person(&self, workspace: &str, device: EndpointId) -> Option<String> {
        let seat = &self.seat;
        let engine = seat.engine.as_ref()?;
        let identity = tilo_desktop_model::WorkspaceId::parse(workspace)?;
        let now = std::time::SystemTime::now();
        engine
            .owner()
            .with_current(identity, |view| {
                view.peer()
                    .permissions(*device.as_bytes(), now)
                    .map(|member| member.person)
                    .map_err(|error| error.to_string())
            })
            .ok()
    }

    fn heard(&self, event: &Event, client: &SessionClient) -> bool {
        self.seat.heard(event, client)
    }

    fn touched(&self, client: &SessionClient) {
        self.seat.note_reachability(client);
        self.roster.show_here(client);
        self.roster.show_waves();
    }

    fn workspace_host(&self, workspace: &str) -> Option<[u8; 32]> {
        let engine = self.seat.engine.as_ref()?;
        engine.host_of(tilo_desktop_model::WorkspaceId::parse(workspace)?)
    }

    fn joined(&self, workspace: &str) {
        let seat = &self.seat;
        let Some(engine) = seat.engine.as_ref() else {
            return;
        };
        if let Some(connect) = tilo_desktop_model::WorkspaceId::parse(workspace)
            .and_then(|desk| native_ingress::Connect::new(engine.owner().clone(), desk).ok())
        {
            let _ = seat.commands.send(HostCmd::TakePart(connect));
        }
    }

    fn pass(&self, client: &SessionClient) {
        let seat = &self.seat;
        seat.keep_host_events(client);
        let moved = seat.hear_live(client);
        // What arrived moved this device's fabric — a copy from another device, a device that is
        // gone: the person's other devices hear so on the same pass.
        seat.announce_fabric();
        // The clock of every peer something holds in sync: its probes leave here, so the audio
        // timed against them can play in time.
        if let Some(fabric) = &seat.fabric {
            fabric.pump_clock();
        }
        // What no device acknowledged of a send this device made is offered again, and what the
        // person's devices offered here waits for their answer.
        if let Some(nearby) = seat.nearby() {
            nearby.pump();
        }
        if moved {
            seat.note_reachability(client);
        }
    }
}

/// The desk a session shows, as the workspace gate reads it.
/// A page's host: the roster, over the transport the page brings.
#[cfg(not(target_os = "linux"))]
impl Host for Hosting {
    fn touched(&self, client: &SessionClient) {
        self.roster.show_here(client);
        self.roster.show_waves();
    }
}

#[cfg(target_os = "linux")]
struct DeskGate {
    desk: DesktopHandle,
}

#[cfg(target_os = "linux")]
impl GateDesk for DeskGate {
    fn allows(&self, workspace: &str, person: &str, required: &WorkspaceAction, now: u64) -> bool {
        self.desk
            .read(|tree| tilo_session_client::gate::allows(tree, workspace, person, required, now))
    }
}

/// The session's client of the collaboration plane: the participant's own client, and the desktop
/// host that carries it — the peer actor, the live plane, this device's workspaces and channels,
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
            // The live planes dial through this endpoint: they end before it does,
            // and before the thread that holds it joins.
            self.hosting.seat.live.borrow_mut().take();
            let _ = self.outgoing.send(Cmd::Stop);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
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
    /// Where this device keeps its part of the person's device fabric between runs, one
    /// checkpoint per device key; `None` keeps nothing (a test, a stand-in session).
    pub fabric: Option<std::path::PathBuf>,
}

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
        let (identity, wake) = (entry.identity, entry.wake);
        let (commands, host_commands) = tokio::sync::mpsc::unbounded_channel();
        let (arrivals, arrived) = std::sync::mpsc::channel();
        let (host_events, host_arrived) = std::sync::mpsc::channel();
        let (enrollments, enrolled) = std::sync::mpsc::channel();
        let (ready, bound) = std::sync::mpsc::channel();
        let (user, device) = certified_as(identity.as_ref());
        let host_outbox = HostOutbox {
            commands: commands.clone(),
        };
        let outgoing: Arc<dyn Outbox> = Arc::new(host_outbox.clone());
        let link_outbox = host_outbox.clone();
        let fabric = restored_fabric(
            identity.as_ref(),
            host_outbox,
            entry.continuity,
            entry.fabric,
        );
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
                    host_commands,
                    Wire {
                        arrivals,
                        host_events,
                        enrollments,
                        ready,
                    },
                    wake,
                    Serving::new(bound_endpoint, plane_link, serving, beside, documents),
                );
            })?;
        let (publisher, endpoint, reachability) = wait_for_bound(&bound)?;
        if let (Some(live), Some(bound)) = (&mut live, endpoint_handle.get()) {
            live.attach(bound.clone());
        }
        if let Some(link) = &link {
            link.attach(std::sync::Arc::new(link_outbox));
        }
        let saying = live
            .as_ref()
            .map(|live| Arc::new(live.saying()) as Arc<dyn SayPresence>);
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
        let hosting = Seat::hosting(Hosted {
            commands,
            host_events: host_arrived,
            engine,
            link,
            versions,
            fabric,
            device_actions,
            reachability,
            live,
        });
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
        for host in entry.hosts {
            let _ = client.outgoing.send(Cmd::Connect(host));
        }
        Ok(client)
    }

    /// A client on a transport another host brings, with this desktop's host beside it: what a test
    /// hands it is what the session shows. The transport's own commands stay the client's, so a
    /// host that reads them off its end reads what this session said.
    #[must_use]
    pub fn over(plane: Transport, device: Device, own: OwnPresence) -> Self {
        #[cfg(target_os = "linux")]
        let host = {
            let (commands, _host_commands) = tokio::sync::mpsc::unbounded_channel();
            let (_host_events, host_arrived) = std::sync::mpsc::channel();
            Rc::new(Seat::bare(commands, host_arrived))
        };
        #[cfg(target_os = "linux")]
        let outgoing = std::sync::Arc::clone(&plane.outgoing);
        let inner = SessionClient::over(plane, device, own);
        let hosting = Rc::new(Hosting {
            roster: Rc::new(Roster::new()),
            #[cfg(target_os = "linux")]
            seat: host,
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

    /// A session taking part in a plane with no transport, for tests that hand it what it would
    /// have heard and read what it would have said.
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    pub(crate) fn offline(
        publisher: EndpointId,
        own: OwnPresence,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        Self::planeless(publisher, own, Vec::new())
    }

    /// A client taking part in no transport: it publishes nowhere and hears only what it is handed.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn untethered(publisher: EndpointId, own: OwnPresence) -> Self {
        Self::planeless(publisher, own, Vec::new()).0
    }

    /// A client of a plane with no transport: what both the real client and [`Self::untethered`]
    /// are, at their middle. A test that reads what one plane would have sent to another takes the
    /// receiver.
    #[cfg(target_os = "linux")]
    pub(crate) fn planeless(
        publisher: EndpointId,
        own: OwnPresence,
        hosts: Vec<EndpointAddr>,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        let (outgoing, said) = tokio::sync::mpsc::unbounded_channel();
        let (_unheard, arrived) = std::sync::mpsc::channel();
        let (_unlanded, enrolled) = std::sync::mpsc::channel();
        let (commands, _host_commands) = tokio::sync::mpsc::unbounded_channel();
        let (_host_events, host_arrived) = std::sync::mpsc::channel();
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
            seat: Rc::new(Seat::bare(commands, host_arrived)),
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

    /// The same plane, taken part in as `person` (their user root, hex): what a device's
    /// certificate says of it on a plane that has one, and what the workspaces' policies hold its
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
        self.hosting.seat.take_surfaces()
    }

    /// Take the workspaces whose host said their owner deleted them since the last call.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_deleted(&self) -> Vec<tilo_desktop_model::WorkspaceId> {
        self.hosting.seat.take_deleted()
    }

    /// Take the call audio the workspaces' live planes heard since the last call, already opened.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_call_audio(&self) -> Vec<(String, Heard)> {
        self.hosting.seat.take_call_audio()
    }

    /// Take the call pictures the workspaces' live planes heard since the last call, already
    /// opened.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_call_pictures(&self) -> Vec<(String, Heard)> {
        self.hosting.seat.take_call_pictures()
    }

    /// Take the window pages the workspaces' live planes heard since the last call, already opened.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_page_objects(&self) -> Vec<(String, Heard)> {
        self.hosting.seat.take_page_objects()
    }

    /// Send one message of `surface`'s stream on the topic of the workspace it is shared in.
    #[cfg(target_os = "linux")]
    pub(crate) fn publish_surface(
        &self,
        ws: Topic,
        msg: NeighbourMsg,
        viewers: &[neighbour_wire::PeerId],
    ) {
        self.inner.say(Cmd::PublishSurfaceTo {
            ws,
            msg: Box::new(msg),
            viewers: viewers.to_vec(),
        });
    }

    /// Say `msg` to `target` alone, as a workspace-control message on `ws`'s topic.
    #[cfg(target_os = "linux")]
    pub(crate) fn publish_control(&self, ws: Topic, msg: NeighbourMsg, target: EndpointId) {
        self.inner.say(Cmd::SendControl {
            ws,
            msg: Box::new(msg),
            target,
        });
    }

    /// The workspaces' live plane: what this session says into and hears from each of them.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live(&self) -> Option<std::cell::Ref<'_, crate::live::Live>> {
        self.hosting.seat.live()
    }

    /// Give this session's Nearby Send the fabric it runs on.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_nearby(&self, nearby: Arc<crate::nearby_transfer::NearbyTransfer>) {
        self.hosting.seat.attach_nearby(nearby);
    }

    /// The person's own store this session keeps, once it has one.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn personal(&self) -> Option<std::cell::Ref<'_, crate::personal::Personal>> {
        self.hosting.seat.personal()
    }

    /// The person's presence as the surfaces here read it.
    pub fn here(&self) -> &Here {
        &self.hosting.roster.here
    }

    /// Tell the person's other devices what this device says of itself, when that moved.
    #[cfg(target_os = "linux")]
    pub fn announce_fabric(&self) {
        self.hosting.seat.announce_fabric();
    }

    /// This device's fabric, when the person certified it.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn fabric(&self) -> Option<crate::platform::fabric::SharedLink> {
        self.hosting.seat.fabric()
    }

    /// The actions one of the person's devices sent this one since the last call.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn fabric_actions(&self) -> Vec<crate::platform::fabric::PersonalFabricActionEnvelope> {
        self.hosting.seat.fabric_actions()
    }

    /// Where this device's actions are served.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn device_actions(&self) -> Option<DeviceActions> {
        self.hosting.seat.device_actions()
    }

    /// The collaborator's devices the plane heard at another peer protocol version.
    #[cfg(target_os = "linux")]
    pub(crate) fn take_version_mismatches(&self) -> Vec<crate::network::VersionMismatch> {
        self.hosting.seat.take_version_mismatches()
    }

    /// Hold this device's relays back behind `reachability`: what a test asks of what this desktop
    /// keeps dialable.
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    pub(crate) fn hold_relays_with(&self, reachability: Reachability) {
        *self.hosting.seat.reachability.borrow_mut() = reachability;
    }

    /// This device's collaboration endpoint, once it has bound.
    #[must_use]
    pub fn endpoint(&self) -> Option<iroh::Endpoint> {
        self.inner.endpoint()
    }

    /// Hand the workspace gate the desk this session shows: whose workspaces' policies hold every
    /// member's messages to what the workspace grants them.
    #[cfg(target_os = "linux")]
    pub fn attach_desk(&self, desk: DesktopHandle) {
        self.inner
            .attach_desk(Rc::new(DeskGate { desk: desk.clone() }));
        *self.hosting.seat.desk.borrow_mut() = Some(desk);
    }

    /// Hand the records' content plane its place on this client.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_content(&self, content: Arc<content::Plane>) {
        self.hosting.seat.attach_content(content, self.publisher());
    }

    /// Hand the person's own store, opened where this session keeps its state, to their other
    /// devices.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_personal(&self, personal: crate::personal::Personal) {
        self.hosting.seat.attach_personal(personal);
    }

    /// This device's workspaces, when this session holds any.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn engine(&self) -> Option<&Arc<Engine>> {
        self.hosting.seat.engine()
    }

    /// Redeem `request`'s invitation off the loop: this device joins the workspace and takes in
    /// what it held before. [`Self::take_enrolled`] says what became of it.
    ///
    /// # Errors
    /// The plane takes no enrollment: this session holds no workspaces, or it has stopped.
    #[cfg(target_os = "linux")]
    pub fn enroll(&self, request: Enrolling) -> Result<(), String> {
        if self.hosting.seat.engine.is_none() {
            return Err("this session keeps no workspaces to enroll into".into());
        }
        self.hosting
            .seat
            .commands
            .send(HostCmd::Enroll(request))
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
        if self.hosting.seat.engine.is_none() {
            return Err("this session keeps no workspaces to take in".into());
        }
        self.hosting
            .seat
            .commands
            .send(HostCmd::Enter(Box::new(request)))
            .map_err(|_| "the collaboration plane is gone".to_owned())
    }

    /// Say this person's workspaces to their other devices, each with where this device's are
    /// hosted, so a device that opens one enters it at its host.
    #[cfg(target_os = "linux")]
    pub fn tell_personal(&self, update: &tilo_user_identity::DirectoryUpdate) {
        let hosts = self
            .hosting
            .seat
            .engine
            .as_ref()
            .map(|engine| engine.host_hints())
            .unwrap_or_default();
        self.inner.tell_personal(update, hosts);
    }

    /// Dial one of the person's own devices, whose address this session was told: the session
    /// between the two carries their device fabric.
    #[cfg(target_os = "linux")]
    pub fn connect(&self, addr: EndpointAddr) {
        // An action for this device is sent to where it was told to be,
        // without waiting for the two sessions to meet first.
        if let Some(actions) = self.hosting.seat.device_actions.get() {
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
            *self.inner.endpoint()?.id().as_bytes(),
            HostOutbox {
                commands: self.hosting.seat.commands.clone(),
            },
        ))
    }

    /// This device's handle in a workspace, as a window's stream names it.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn peer(&self) -> neighbour_wire::PeerId {
        tilo_network_actor::behaviour::peer_liveness_handle(&self.publisher())
    }

    /// Who is here, as the desktop lists them.
    #[must_use]
    pub fn people(
        &self,
        tree: &DesktopTree,
        records: &std::collections::BTreeMap<tilo_desktop_model::TabId, String>,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Person> {
        self.hosting.roster.people(tree, records, app_name)
    }

    /// Who is here, written into `tree`'s active workspace as its collaborators.
    pub fn show(
        &self,
        tree: &mut DesktopTree,
        records: &std::collections::BTreeMap<tilo_desktop_model::TabId, String>,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Person> {
        self.hosting.roster.show(tree, records, app_name)
    }

    /// The waves at this person since the last call, as the bar says them.
    #[must_use]
    pub fn wave_notice(&self) -> Option<tilo_desktop::chrome::WaveNotice> {
        self.hosting.roster.wave_notice()
    }

    /// The arrival the bar shows, while its moment lasts.
    #[must_use]
    pub fn arrival_notice(&self) -> Option<tilo_desktop::chrome::ArrivalNotice> {
        self.hosting.roster.arrival_notice()
    }
}

#[cfg(target_os = "linux")]
/// What a desktop's host runs beside: the thread's channels, its workspaces, its channels'
/// service, its fabric, its action plane, its relays and its live planes.
#[cfg(target_os = "linux")]
struct Hosted {
    commands: tokio::sync::mpsc::UnboundedSender<HostCmd>,
    host_events: std::sync::mpsc::Receiver<HostEvent>,
    engine: Option<Arc<Engine>>,
    link: Option<Arc<ChannelLink>>,
    versions: std::sync::mpsc::Receiver<crate::network::VersionMismatch>,
    fabric: Option<Arc<SessionFabric>>,
    device_actions: Arc<std::sync::OnceLock<DeviceActions>>,
    reachability: Reachability,
    live: Option<crate::live::Live>,
}

#[cfg(target_os = "linux")]
impl Seat {
    /// The host this client runs with, and the roster beside it.
    fn hosting(
        Hosted {
            commands,
            host_events,
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
            seat: Rc::new(Self {
                desk: RefCell::new(None),
                commands,
                host_events,
                surfaces: RefCell::new(Vec::new()),
                call_audio: RefCell::new(Vec::new()),
                call_pictures: RefCell::new(Vec::new()),
                page_objects: RefCell::new(Vec::new()),
                deleted: RefCell::new(Vec::new()),
                engine,
                link,
                versions,
                content: RefCell::new(None),
                topologies: RefCell::new(std::collections::BTreeMap::new()),
                personal_store: RefCell::new(None),
                fabric,
                nearby: RefCell::new(None),
                device_actions,
                reachability: RefCell::new(reachability),
                live: RefCell::new(live),
            }),
        })
    }

    /// A seat with no plane of its own: what a client takes part in over a transport another host
    /// brings.
    fn bare(
        commands: tokio::sync::mpsc::UnboundedSender<HostCmd>,
        host_events: std::sync::mpsc::Receiver<HostEvent>,
    ) -> Self {
        let (_said, versions) = std::sync::mpsc::channel();
        Self {
            desk: RefCell::new(None),
            commands,
            host_events,
            surfaces: RefCell::new(Vec::new()),
            call_audio: RefCell::new(Vec::new()),
            call_pictures: RefCell::new(Vec::new()),
            page_objects: RefCell::new(Vec::new()),
            deleted: RefCell::new(Vec::new()),
            engine: None,
            link: None,
            versions,
            content: RefCell::new(None),
            topologies: RefCell::new(std::collections::BTreeMap::new()),
            personal_store: RefCell::new(None),
            fabric: None,
            nearby: RefCell::new(None),
            device_actions: Arc::new(std::sync::OnceLock::new()),
            reachability: RefCell::new(Reachability::default()),
            live: RefCell::new(None),
        }
    }
}

/// This device's collaboration identity: the key and certificate this session's endpoint runs on,
/// when this host holds one.
/// A session on a host without one takes part in no workspace as anybody: it binds a fresh key
/// and stays anonymous until the person sets a device up.
#[must_use]
#[cfg(target_os = "linux")]
pub fn device_identity() -> Option<HostIdentity> {
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
    serde_json::from_str(value)
        .ok() // ignored-failure: a host value that is not JSON names no endpoint address here
        .or_else(|| value.parse::<EndpointId>().ok().map(EndpointAddr::new))
}

/// Who this device's certificate says its person is, and what the device calls itself: the id that
/// stays the person's, and the name its other devices are told apart by. A device the person never
/// named says no name: the stand-in its certificate carries is no word of theirs, and every reader
/// says which device it is in its own language.
#[cfg(target_os = "linux")]
fn certified_as(identity: Option<&HostIdentity>) -> (Option<[u8; 32]>, String) {
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

/// This device's fabric before the endpoint runs, so a device that proves itself at the actor's
/// first tick is recorded with it: the record it keeps is this device's, named by its certificate.
/// What it kept in its last run under `state` is restored, so its record continues its own line of
/// revisions: a record said again from revision one is one the person's other devices already hold
/// something else at, and refuse.
#[cfg(target_os = "linux")]
fn restored_fabric(
    identity: Option<&HostIdentity>,
    outgoing: HostOutbox,
    continuity: Continuity,
    state: Option<std::path::PathBuf>,
) -> Option<Arc<SessionFabric>> {
    let identity = identity?;
    let device = (
        DeviceId::new(hex::encode(identity.device_cert.device_public_key)).ok()?,
        identity.device_cert.device_label.clone(),
    );
    let kept = state.map(|state| fabric_checkpoint(&state, &device.0));
    let checkpoint = kept.as_deref().and_then(|path| std::fs::read(path).ok());
    let runtime = match FabricRuntime::restored(Some(device.clone()), checkpoint.as_deref()) {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::warn!(%error, "this device's kept fabric does not read; it starts anew");
            FabricRuntime::restored(Some(device), None).ok()?
        }
    };
    Some(SessionFabric::new(
        runtime,
        outgoing,
        identity.device_cert.user_public_key,
        identity.signing_key().ok()?,
        continuity,
        kept,
    ))
}

/// The file under `state` the fabric of the device `device` names is kept in: one per device key,
/// so a machine copied from another, re-keyed at its first boot, starts a line of its own rather
/// than continuing the one it was copied with.
#[cfg(target_os = "linux")]
pub(crate) fn fabric_checkpoint(state: &std::path::Path, device: &DeviceId) -> std::path::PathBuf {
    state.join(format!("{}.fabric", device.as_str()))
}

/// The endpoint the peer actor binds, and the relays held back from it until something needs this
/// device dialable: the person's choice ([`tilo_endpoint_reachability::relays`]) for a device with
/// an identity, none for a session that takes part as nobody and stays on this machine's loopback.
#[cfg(target_os = "linux")]
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
    let key = iroh::SecretKey::from_bytes(&identity.signing_key()?.to_bytes());
    // The relays are the person's ([`tilo_endpoint_reachability::relays`]) and held back
    // ([`tilo_endpoint_reachability`]): binding with an empty relay map keeps net report, address
    // discovery and port mapping off the network until a workspace this session shows has someone
    // else in it. A peer's key also resolves on the local network
    // ([`tilo_endpoint_reachability::lan`]), so two of the person's devices that share one find
    // each other while their relays rest, and with their own relay (or none) that is what finds
    // them.
    let relays = tilo_endpoint_reachability::relays();
    let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(key)
        .address_lookup(tilo_endpoint_reachability::lan())
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::empty()));
    if relays.n0_dns {
        builder = builder
            .address_lookup(tilo_endpoint_reachability::collaborating())
            .address_lookup(iroh::address_lookup::DnsAddressLookup::n0_dns());
    }
    Ok((builder, relays.relays))
}

/// Where an admitted peer's app document goes so this device's apps see it as they see this
/// device's own writes: the broker's channels, which record it and answer an app's subscription,
/// and its fan-out, which shows it to the apps that hold its route open.
#[derive(Clone)]
#[cfg(target_os = "linux")]
pub struct PeerDocuments {
    channels: Arc<std::sync::Mutex<tilo_broker_core::collab_core::BrokerChannels>>,
    fanout: Arc<tilo_action_broker::collab_broker::FanoutState>,
    /// What search reads again of a record a peer wrote.
    changes: Option<Arc<dyn tilo_action_broker::services::RecordIndexChanges>>,
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
        fanout: Arc<tilo_action_broker::collab_broker::FanoutState>,
        changes: Option<Arc<dyn tilo_action_broker::services::RecordIndexChanges>>,
        arrivals: Option<Arc<tilo_session_client::Arrivals>>,
    ) -> Self {
        Self {
            channels,
            fanout,
            changes,
            arrivals,
        }
    }

    /// Hand one admitted input's app documents to this device's apps on their routes: its own,
    /// and every held write it brought what that write waited for, which reach the apps no other
    /// way.
    fn deliver(&self, input: &native_ingress::Input, report: &tilo_peer_state::AdmissionReport) {
        for write in input.app_writes(report) {
            self.deliver_write(&input.workspace().to_string(), write);
        }
    }

    /// Hand one app write to this device's apps on its route: another device's, or one the
    /// session wrote into the engine itself.
    pub(crate) fn deliver_write(&self, workspace: &str, write: tilo_peer_state::owner::AppWrite) {
        let author = write.envelope.author.as_bytes().to_owned();
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
            changes.changed(workspace, &app, &record);
        }
        // A read of the record that this device held none of until now waits for it, and one that
        // holds it open reads what landed: a write on its route answers both, and nothing else
        // does.
        if let Some(arrivals) = &self.arrivals {
            arrivals.landed();
        }
        if let Some(state) = self.fanout.app_broker() {
            state.wake_record_reads(workspace, &app, &record);
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod custody_tests;

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;
