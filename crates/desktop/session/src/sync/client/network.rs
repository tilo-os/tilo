//! The thread the peer network runs on: this device's endpoint, bound on the device's own
//! key, the peer actor served on it, and the commands the session hands it until it stops.
//! The endpoint is what names this device on the workspaces' peer networks, and what carries every one of
//! them.

use std::sync::Arc;

use tilo_lock::MutexRecover as _;

use iroh::{EndpointAddr, EndpointId};
use tilo_endpoint_reachability::Reachability;
use tilo_network_actor::{DeviceIdentity, NetCmd, NetEvent};
use tilo_session_client::Cmd;
use tilo_session_client::network::Event;

use super::{DesktopEvent, HostCmd, endpoint_for};
use tilo_network_actor::WorkspaceId;

/// What the peer actor heard, as the client's own vocabulary says it: the transport keeps the
/// differences an endpoint's, and the client hears one kind of word from every host. What only this
/// desktop keeps — which of the person's devices proved itself and what it sends, how far a transfer
/// got — is this desktop's own beside it, on the one path and in the order the actor said them.
pub(super) fn event_of(event: NetEvent) -> Option<Result<Event, DesktopEvent>> {
    Some(Ok(match event {
        NetEvent::Msg { ws, peer, msg, .. } => Event::Msg {
            ws,
            peer,
            msg: Box::new(msg),
        },
        NetEvent::PeerLeft { ws, peer, .. } => Event::PeerLeft { ws, peer },
        NetEvent::HostReady {
            ws,
            peer,
            documents_when_opened,
        } => Event::HostReady {
            ws,
            peer,
            documents_when_opened,
        },
        // A device that proved itself and what it sends travel the one queue, so the session hears
        // of the device before anything it said: a frame is bound to the certificate it proved
        // there, never to what the frame claims.
        NetEvent::Sibling {
            peer,
            certs,
            revocations,
            ..
        } => {
            return Some(Err(DesktopEvent::Sibling {
                peer,
                certs,
                revocations,
            }));
        }
        NetEvent::WorkspaceDeleted { ws } => {
            return Some(Err(DesktopEvent::WorkspaceDeleted { ws }));
        }
        NetEvent::SiblingLeft { peer, .. } => {
            return Some(Err(DesktopEvent::SiblingLeft { peer }));
        }
        NetEvent::SiblingMsg { peer, msg, .. } => Event::SiblingMsg {
            peer,
            msg: Box::new(msg),
        },
        NetEvent::PersonalFabric {
            peer,
            payload,
            received,
            ..
        } => {
            return Some(Err(DesktopEvent::PersonalFabric {
                peer,
                payload,
                received,
            }));
        }
        // Taken off this path before it gets here: what a host replays into the workspaces this
        // device holds is admitted on its own thread, and the rest is the peer network's own bookkeeping.
        NetEvent::NativeInput(_) | NetEvent::OwnReplay { .. } => {
            return None;
        }
    }))
}

/// The runtime the peer network's thread runs on: two workers of its own, so what the peer network
/// settles never waits behind what a session shows.
fn network_runtime(
    ready: &std::sync::mpsc::Sender<Result<(EndpointId, EndpointAddr, Reachability), String>>,
) -> Option<tokio::runtime::Runtime> {
    match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            let _ = ready.send(Err(format!("no runtime for the peer network: {error}")));
            None
        }
    }
}

/// What the session's endpoint carries beside its workspaces: the channels it runs, and this
/// device's app wire, which answers the person's other devices on it.
pub(crate) struct Serving {
    /// This device's endpoint, once it has bound.
    pub(crate) bound: Bound,
    /// What answers on that endpoint beside the workspaces' own traffic.
    pub(crate) beside: Beside,
}

impl Serving {
    /// What the session serves on the endpoint it is about to bind: the endpoint it fills in once
    /// bound, and what answers beside the workspaces' traffic.
    pub(crate) fn new(bound: Bound, protocols: tilo_network_actor::own_service::Protocols) -> Self {
        Self {
            bound,
            beside: Beside { protocols },
        }
    }
}

/// What the session serves on this device's endpoint beside the workspaces' traffic: this
/// device's app wire, and whatever else the session was handed — this device as an exit for the
/// person's other devices.
pub(crate) struct Beside {
    /// The protocols the session answers besides its own, handed over before the endpoint binds: an
    /// endpoint's protocols are fixed once it is bound, so whatever answers on it is here from the
    /// first pass.
    pub(crate) protocols: tilo_network_actor::own_service::Protocols,
}

/// This device's endpoint as the session binds it: the bound endpoint itself,
/// once it is, and the relays it holds back until something needs it dialable.
#[derive(Clone)]
pub(crate) struct Bound {
    pub(crate) endpoint: Arc<std::sync::OnceLock<iroh::Endpoint>>,
    pub(crate) relays: tilo_endpoint_reachability::EndpointRelays,
    /// What it hears on the local network.
    pub(crate) lan: tilo_endpoint_reachability::Nearby,
    /// The devices the workspaces it is in admit, whom that discovery answers too.
    pub(crate) workspace_devices: WorkspaceDevices,
}

/// The devices each workspace this device is in admitted, as the session last handed the peer
/// network the devices each admits: beside the person's own devices, the ones this device's local discovery
/// answers ([`LanFleet`]).
#[derive(Clone, Default)]
pub(crate) struct WorkspaceDevices(
    Arc<
        std::sync::Mutex<
            std::collections::HashMap<WorkspaceId, Vec<tilo_network_actor::RosterDevice>>,
        >,
    >,
);

impl WorkspaceDevices {
    /// Take what `command` says of a workspace's devices: the devices it admits now, or none once
    /// this device left it.
    fn note(&self, command: &Cmd) {
        match command {
            Cmd::Roster { ws, devices } => {
                self.0.lock_recover().insert(*ws, devices.clone());
            }
            Cmd::Leave(ws) => {
                self.0.lock_recover().remove(ws);
            }
            _ => {}
        }
    }

    /// Whether a workspace this device is in admits `device` at `now_ns`.
    fn admit(&self, device: &[u8; 32], now_ns: u64) -> bool {
        self.0
            .lock_recover()
            .values()
            .flatten()
            .any(|(id, expires)| {
                id.as_bytes() == device && expires.is_none_or(|expires| expires > now_ns)
            })
    }
}

/// The devices this device's local discovery answers ([`tilo_endpoint_reachability::lan`]): the
/// person's own, current and unrevoked, as the sibling session admits them, and the devices a
/// workspace this device is in admitted. Its probes present the person's certificate.
pub(crate) struct LanFleet {
    pub(crate) identity: DeviceIdentity,
    pub(crate) workspace_devices: WorkspaceDevices,
}

impl tilo_endpoint_reachability::Fleet for LanFleet {
    fn certificate(&self) -> Option<tilo_user_identity::DeviceCert> {
        Some(
            self.identity
                .material_for(tilo_network_actor::DEFAULT_WS_TOPIC)
                .0,
        )
    }

    fn admits(&self, cert: &tilo_user_identity::DeviceCert) -> bool {
        let now = tilo_clock::unix_now_ns();
        let (own, revocations) = self
            .identity
            .material_for(tilo_network_actor::DEFAULT_WS_TOPIC);
        if cert.device_public_key == own.device_public_key
            || tilo_user_identity::verify_device_cert_at(cert, now).is_err()
        {
            return false;
        }
        let persons = cert.user_public_key == own.user_public_key
            && !revocations.iter().any(|entry| {
                (entry.user_public_key, entry.revoked_device_public_key)
                    == (cert.user_public_key, cert.device_public_key)
                    && entry.revoked_at_ns <= now
            });
        persons || self.workspace_devices.admit(&cert.device_public_key, now)
    }
}

impl Bound {
    /// The endpoint to bind, filled in once it is, holding back `relays` and hearing the local
    /// network into `lan`.
    pub(crate) fn new(
        endpoint: Arc<std::sync::OnceLock<iroh::Endpoint>>,
        relays: tilo_endpoint_reachability::EndpointRelays,
        lan: tilo_endpoint_reachability::Nearby,
    ) -> Self {
        Self {
            endpoint,
            relays,
            lan,
            workspace_devices: WorkspaceDevices::default(),
        }
    }
}

/// What this device answers on from the moment its endpoint is bound: the peer actor the session's
/// commands go to, the events it hears, and what runs beside it until the session stops.
struct Listening {
    actor: tokio::sync::mpsc::Sender<NetCmd>,
    events: tokio::sync::mpsc::Sender<NetEvent>,
    actor_task: tokio::task::JoinHandle<()>,
    carrying: tokio::task::JoinHandle<()>,
}

/// The session's ends of the peer network's thread: what the client hears, what only this desktop keeps,
/// what became of an enrollment, and where the bound endpoint is said.
pub(super) struct Wire {
    pub(super) arrivals: std::sync::mpsc::Sender<Event>,
    pub(super) desktop_events: std::sync::mpsc::Sender<DesktopEvent>,
    pub(super) ready:
        std::sync::mpsc::Sender<Result<(EndpointId, EndpointAddr, Reachability), String>>,
}

/// The thread: bind the endpoint, run the peer actor, and carry what it hears to the session's loop
/// until it is told to stop.
pub(super) fn run(
    identity: Option<DeviceIdentity>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<HostCmd>,
    wire: Wire,
    wake: Arc<dyn Fn() + Send + Sync>,
    Serving { bound, beside }: Serving,
) {
    let Wire {
        arrivals,
        desktop_events,
        ready,
    } = wire;
    let Some(runtime) = network_runtime(&ready) else {
        return;
    };
    runtime.block_on(async move {
        let Some(Listening {
            actor,
            events,
            actor_task,
            carrying,
        }) = listening(
            identity.as_ref(),
            arrivals,
            desktop_events,
            wake,
            &ready,
            beside,
            &bound,
        )
        .await
        else {
            return;
        };
        while let Some(command) = commands.recv().await {
            if let HostCmd::Network(command) = &command {
                bound.workspace_devices.note(command);
            }
            let Some(taken) = carry_host(&actor, &events, command).await else {
                break;
            };
            if !taken {
                tracing::debug!("the peer network stopped taking commands");
                break;
            }
        }
        drop(events);
        carrying.abort();
        actor_task.abort();
    });
}

/// Give one command to the peer network — a client's own, or what only this desktop keeps beside it —
/// answering whether it was taken, or `None` when it is the command that ends the peer network.
async fn carry_host(
    actor: &tokio::sync::mpsc::Sender<NetCmd>,
    events: &tokio::sync::mpsc::Sender<NetEvent>,
    command: HostCmd,
) -> Option<bool> {
    Some(match command {
        HostCmd::Network(Cmd::Stop) => return None,
        HostCmd::Fabric {
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
            actor.send(command).await.is_ok()
        }
        HostCmd::Network(command) => carry_out(actor, command, events).await?,
    })
}

/// Bind this device's endpoint, serve the peer actor and its app wire on it, and say where the
/// session's apps reach this device — or say why it could not, and answer nothing.
async fn listening(
    identity: Option<&DeviceIdentity>,
    arrivals: std::sync::mpsc::Sender<Event>,
    desktop_events: std::sync::mpsc::Sender<DesktopEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    ready: &std::sync::mpsc::Sender<Result<(EndpointId, EndpointAddr, Reachability), String>>,
    Beside { protocols }: Beside,
    bound: &Bound,
) -> Option<Listening> {
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let (builder, deferred_relays) = match endpoint_for(
        identity,
        &bound.relays,
        &bound.lan,
        &bound.workspace_devices,
    ) {
        Ok(building) => building,
        Err(error) => {
            let _ = ready.send(Err(error));
            return None;
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
            return None;
        }
    };
    let publisher = endpoint.id();
    let addr = endpoint.addr();
    // The peer actor wakes the held-back relays when it dials a holder or a peer, the session
    // while a workspace it shows has someone else in it (`Client::note_reachability`), and the
    // workspace service before it signs an invitation's address.
    let reachability = tilo_endpoint_reachability::Reachability::defer(
        &endpoint,
        &deferred_relays,
        tokio::runtime::Handle::current(),
        Some(tilo_endpoint_reachability::REST_AFTER),
    );
    // This device's app wire is served on the same endpoint, where other devices reach it.
    let mut served: tilo_network_actor::own_service::Protocols = Vec::new();
    // What the session serves beside its own peer networks: this device's exit for the person's other
    // devices, when it offers one.
    served.extend(protocols);
    let _ = bound.endpoint.set(endpoint.clone());
    // The workspaces this device keeps are its daemon's, which serves them on its own endpoint:
    // this one serves none.
    let (actor, actor_task) = tilo_network_actor::Actor::spawn_serving(
        endpoint,
        lookup,
        identity.cloned(),
        Arc::new(std::sync::Mutex::new(Vec::new())),
        reachability.clone(),
        None,
        served,
    );
    let (events, carrying) = heard(arrivals, desktop_events, wake);
    if ready.send(Ok((publisher, addr, reachability))).is_err() {
        return None;
    }
    Some(Listening {
        actor,
        events,
        actor_task,
        carrying,
    })
}

/// Hear the peer actor: what it says reaches the client, and what only this desktop keeps
/// reaches the desktop.
fn heard(
    arrivals: std::sync::mpsc::Sender<Event>,
    desktop_events: std::sync::mpsc::Sender<DesktopEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> (
    tokio::sync::mpsc::Sender<NetEvent>,
    tokio::task::JoinHandle<()>,
) {
    let (events, mut heard) = tokio::sync::mpsc::channel::<NetEvent>(256);
    let carrying = tokio::spawn(async move {
        while let Some(event) = heard.recv().await {
            match event_of(event) {
                Some(Ok(event)) => {
                    if arrivals.send(event).is_err() {
                        break;
                    }
                }
                Some(Err(event)) => {
                    if desktop_events.send(event).is_err() {
                        break;
                    }
                }
                None => {}
            }
            (wake)();
        }
    });
    (events, carrying)
}

/// Give one command from the client to the peer network: whether it was taken, or `None` when it is the
/// command that ends it.
async fn carry_out(
    network: &tokio::sync::mpsc::Sender<NetCmd>,
    command: Cmd,
    events: &tokio::sync::mpsc::Sender<NetEvent>,
) -> Option<bool> {
    Some(match command {
        Cmd::Join(ws) => network
            .send(NetCmd::JoinWorkspace {
                ws,
                on_event: events.clone(),
            })
            .await
            .is_ok(),
        Cmd::Leave(ws) => network.send(NetCmd::LeaveWorkspace { ws }).await.is_ok(),
        Cmd::Publish { ws, msg } => network
            .send(NetCmd::Publish { ws, msg: *msg })
            .await
            .is_ok(),
        // A shared window's word is said on the workspace's live session, which the desktop holds
        // itself ([`super::Client::say`]): nothing of it rides the peer actor.
        Cmd::Share(..) => true,
        Cmd::Connect(addr) => network.send(NetCmd::ConnectHost { addr }).await.is_ok(),
        Cmd::WatchSiblings => network
            .send(NetCmd::WatchSiblings {
                on_event: events.clone(),
            })
            .await
            .is_ok(),
        Cmd::TellSiblings(msg) => network
            .send(NetCmd::PublishToSiblings { msg: *msg })
            .await
            .is_ok(),
        Cmd::ConnectDevice(addr) => network.send(NetCmd::Connect { addr }).await.is_ok(),
        Cmd::Replay(ws) => network.send(NetCmd::NativeReplay { ws }).await.is_ok(),
        Cmd::Redial(ws) => network.send(NetCmd::RedialWorkspace { ws }).await.is_ok(),
        Cmd::OpenDocument { ws, channel } => network
            .send(NetCmd::OpenDocument { ws, channel })
            .await
            .is_ok(),
        Cmd::PublishOwnership(ws) => network.send(NetCmd::NativePublish { ws }).await.is_ok(),
        Cmd::Roster { ws, devices } => network
            .send(NetCmd::SetWorkspaceRoster { ws, devices })
            .await
            .is_ok(),
        Cmd::Stop => return None,
        // A server session's own commands: this desktop's peer network carries none of them, and an answer
        // one asks for is dropped unanswered.
        _ => false,
    })
}
