//! The thread the collaboration plane runs on: this device's endpoint, bound on the device's own
//! key, the peer actor served on it, and the commands the session hands it until it stops.
//! The endpoint is what names this device on the workspaces' planes, and what carries every one of
//! them.

use std::sync::Arc;

use iroh::{EndpointAddr, EndpointId};
use tilo_endpoint_reachability::Reachability;
use tilo_network_actor::{HostIdentity, NetCmd, NetEvent};

use crate::platform::plane::Event;

use super::adopt::{enter, redeem};
use super::{ChannelLink, Cmd, DeviceActions, Engine, Enrolled, carry_out, endpoint_for, heard};

/// What the peer actor heard, as the session's own vocabulary says it: the transport keeps the
/// differences an endpoint's, and the session hears one kind of word from every host.
pub(super) fn event_of(event: NetEvent) -> Option<Event> {
    Some(match event {
        NetEvent::Msg { ws, peer, msg, .. } => Event::Msg {
            ws,
            peer,
            msg: Box::new(msg),
        },
        NetEvent::PeerLeft { ws, peer, .. } => Event::PeerLeft { ws, peer },
        NetEvent::HostReady { ws, peer } => Event::HostReady { ws, peer },
        NetEvent::HostMember {
            ws,
            host,
            device,
            active,
        } => Event::HostMember {
            ws,
            host,
            device,
            active,
        },
        NetEvent::Sibling {
            peer,
            certs,
            revocations,
            ..
        } => Event::Sibling {
            peer,
            certs,
            revocations,
        },
        NetEvent::SiblingLeft { peer, .. } => Event::SiblingLeft { peer },
        NetEvent::SiblingMsg { peer, msg, .. } => Event::SiblingMsg {
            peer,
            msg: Box::new(msg),
        },
        NetEvent::PersonalFabric {
            peer,
            payload,
            received,
            ..
        } => Event::PersonalFabric {
            peer,
            payload,
            received,
        },
        NetEvent::NearbyTransferProgress {
            transfer_id,
            sent_bytes,
            total_bytes,
            status,
            ..
        } => Event::NearbyTransferProgress {
            transfer_id,
            sent_bytes,
            total_bytes,
            status,
        },
        // Taken off this path before it gets here: what a host replays into the workspaces this
        // device holds is admitted on its own thread, and the rest is the plane's own bookkeeping.
        NetEvent::NativeInput(_) | NetEvent::RecordOpened { .. } | NetEvent::OwnReplay { .. } => {
            return None;
        }
    })
}

/// The runtime the collaboration plane's thread runs on: two workers of its own, so what the plane
/// settles never waits behind what a session shows.
fn plane_runtime(
    ready: &std::sync::mpsc::Sender<Result<(EndpointId, EndpointAddr, Reachability), String>>,
) -> Option<tokio::runtime::Runtime> {
    match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            let _ = ready.send(Err(format!(
                "no runtime for the collaboration plane: {error}"
            )));
            None
        }
    }
}

/// What the session's endpoint carries beside its workspaces: the channels it runs, and this
/// device's action plane, which answers the person's other devices on it.
pub(crate) struct Serving {
    /// This device's endpoint, once it has bound.
    pub(crate) bound: Bound,
    /// What answers on that endpoint beside the workspaces' own traffic.
    pub(crate) beside: Beside,
}

impl Serving {
    /// What the session serves on the endpoint it is about to bind: the endpoint it fills in once
    /// bound, and what answers beside the workspaces' traffic.
    pub(crate) fn new(
        endpoint: Arc<std::sync::OnceLock<iroh::Endpoint>>,
        link: Option<Arc<ChannelLink>>,
        actions: Arc<std::sync::OnceLock<DeviceActions>>,
        protocols: tilo_network_actor::local_host::Protocols,
        documents: Option<super::PeerDocuments>,
    ) -> Self {
        Self {
            bound: Bound { endpoint },
            beside: Beside {
                link,
                actions,
                protocols,
                documents,
            },
        }
    }
}

/// What the session serves on this device's endpoint beside the workspaces' traffic: the channels
/// they use, this device's action plane, and whatever else the session was handed — this device as
/// an exit for the person's other devices.
pub(crate) struct Beside {
    pub(crate) link: Option<Arc<ChannelLink>>,
    pub(crate) actions: Arc<std::sync::OnceLock<DeviceActions>>,
    /// The protocols the session answers besides its own, handed over before the endpoint binds: an
    /// endpoint's protocols are fixed once it is bound, so whatever answers on it is here from the
    /// first pass.
    pub(crate) protocols: tilo_network_actor::local_host::Protocols,
    /// Where what admission takes in from another device is handed to this device's apps: `None` on
    /// a session that serves no apps.
    pub(crate) documents: Option<super::PeerDocuments>,
}

/// This device's endpoint as the session binds it: the bound endpoint itself,
/// once it is.
#[derive(Clone, Default)]
pub(crate) struct Bound {
    pub(crate) endpoint: Arc<std::sync::OnceLock<iroh::Endpoint>>,
}

/// What this device answers on from the moment its endpoint is bound: the peer actor the session's
/// commands go to, the events it hears, and what runs beside it until the session stops.
struct Listening {
    endpoint: iroh::Endpoint,
    actor: tokio::sync::mpsc::Sender<NetCmd>,
    events: tokio::sync::mpsc::Sender<NetEvent>,
    actor_task: tokio::task::JoinHandle<()>,
    carrying: tokio::task::JoinHandle<()>,
    attending: Option<tokio::task::JoinHandle<()>>,
}

/// The thread: bind the endpoint, run the peer actor, and carry what it hears to the session's loop
/// until it is told to stop.
pub(super) fn run(
    identity: Option<HostIdentity>,
    engine: Option<Arc<Engine>>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    arrivals: std::sync::mpsc::Sender<Event>,
    enrollments: std::sync::mpsc::Sender<Enrolled>,
    ready: std::sync::mpsc::Sender<Result<(EndpointId, EndpointAddr, Reachability), String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    Serving { bound, beside }: Serving,
) {
    let Some(runtime) = plane_runtime(&ready) else {
        return;
    };
    // The channels' side of the plane, which the loop below carries what the session says to.
    let link = beside.link.clone();
    runtime.block_on(async move {
        let Some(Listening {
            endpoint,
            actor,
            events,
            actor_task,
            carrying,
            attending,
        }) = listening(
            identity.as_ref(),
            engine.as_ref(),
            arrivals,
            wake,
            &ready,
            beside,
            &bound,
        )
        .await
        else {
            return;
        };
        // What asks a workspace's host runs beside this loop, so what the session says meanwhile is
        // still carried.
        let asking = Asking {
            landed: enrollments,
            endpoint,
            plane: actor.clone(),
            events: events.clone(),
            link,
            engine,
            identity,
        };
        while let Some(command) = commands.recv().await {
            let command = match command {
                Cmd::Enroll(request) => {
                    asking.ask(Ask::Invitation(request));
                    continue;
                }
                Cmd::Enter(request) => {
                    asking.ask(Ask::Entry(Box::new(request)));
                    continue;
                }
                command => command,
            };
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
        if let Some(attending) = attending {
            attending.abort();
        }
        actor_task.abort();
    });
}

/// What asking a workspace's host needs, held for the plane's loop: the loop never waits on a host,
/// so an ask runs on its own task and hands what became of it back as an [`Enrolled`].
struct Asking {
    landed: std::sync::mpsc::Sender<Enrolled>,
    endpoint: iroh::Endpoint,
    plane: tokio::sync::mpsc::Sender<NetCmd>,
    events: tokio::sync::mpsc::Sender<NetEvent>,
    link: Option<Arc<ChannelLink>>,
    engine: Option<Arc<Engine>>,
    identity: Option<HostIdentity>,
}

/// One thing the plane asks a workspace's host for: someone's invitation redeemed into their
/// workspace, or one of the person's own workspaces entered at the device hosting it.
enum Ask {
    Invitation(super::Enrolling),
    Entry(Box<super::Entering>),
}

impl Asking {
    /// Ask `ask` beside the loop, and hand what became of it back. A session built without a
    /// workspace engine answers at once that it takes nothing in.
    fn ask(&self, ask: Ask) {
        let (slot, workspace_id, member, name, missing, held) = match &ask {
            Ask::Invitation(request) => (
                request.slot,
                request.workspace_id,
                false,
                None,
                "this session joins no workspaces",
                false,
            ),
            Ask::Entry(request) => (
                request.slot,
                request.workspace_id,
                true,
                Some(request.name.clone()),
                "this session keeps no workspaces",
                request.held,
            ),
        };
        let landed = self.landed.clone();
        let (Some(engine), Some(identity)) = (self.engine.clone(), self.identity.clone()) else {
            let _ = landed.send(Enrolled {
                slot,
                workspace_id,
                member,
                name,
                result: Err(missing.to_owned()),
                held,
            });
            return;
        };
        let endpoint = self.endpoint.clone();
        let plane = self.plane.clone();
        let events = self.events.clone();
        let channels = self.link.clone();
        tokio::spawn(async move {
            let result = match ask {
                Ask::Invitation(request) => {
                    redeem(
                        endpoint,
                        identity,
                        engine,
                        &plane,
                        &events,
                        request,
                        channels.as_deref(),
                    )
                    .await
                }
                Ask::Entry(request) => {
                    enter(
                        endpoint,
                        identity,
                        engine,
                        &plane,
                        &events,
                        *request,
                        channels.as_deref(),
                    )
                    .await
                }
            };
            let _ = landed.send(Enrolled {
                slot,
                workspace_id,
                member,
                name,
                result,
                held,
            });
        });
    }
}

/// Bind this device's endpoint, serve the peer actor and its action plane on it, and say where the
/// session's apps reach this device — or say why it could not, and answer nothing.
async fn listening(
    identity: Option<&HostIdentity>,
    engine: Option<&Arc<Engine>>,
    arrivals: std::sync::mpsc::Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
    ready: &std::sync::mpsc::Sender<Result<(EndpointId, EndpointAddr, Reachability), String>>,
    Beside {
        link,
        actions,
        protocols,
        documents,
    }: Beside,
    bound: &Bound,
) -> Option<Listening> {
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let (builder, deferred_relays) = match endpoint_for(identity) {
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
    // Nothing wakes the held-back relays from here: the peer actor does when it dials a host or a
    // peer, and the session while a workspace it shows has someone else in it
    // (`Client::note_reachability`).
    let reachability = tilo_endpoint_reachability::Reachability::defer(
        &endpoint,
        &deferred_relays,
        tokio::runtime::Handle::current(),
        Some(tilo_endpoint_reachability::REST_AFTER),
    );
    // This device's action plane is served on the same endpoint: another of the person's devices
    // reaches what this one publishes here, beside the workspaces' own traffic.
    let mut served: tilo_network_actor::local_host::Protocols = Vec::new();
    // What the session serves beside its own planes: this device's exit for the person's other
    // devices, when it offers one.
    served.extend(protocols);
    let _ = bound.endpoint.set(endpoint.clone());
    if let Some(identity) = identity {
        let device = DeviceActions::start(identity, endpoint.clone(), engine.cloned());
        served.push((
            tilo_peer_state::actions::ACTIONS_ALPN.to_vec(),
            Box::<dyn iroh::protocol::DynProtocolHandler>::from(device.plane()),
        ));
        // The session takes it from here: what it installs answers, and an app's own-device actions
        // leave through it.
        let _ = actions.set(device);
    }
    if let (Some(identity), Some(engine)) = (identity, engine) {
        // A link this device hands out names this endpoint, so the joins those links are redeemed
        // by are answered here: this device's own owner admits whoever a link it signed is for.
        served.push((
            tilo_delivery_protocol::workspace_join::WORKSPACE_JOIN_ALPN.to_vec(),
            Box::<dyn iroh::protocol::DynProtocolHandler>::from(
                tilo_peer_state::join_transport::WorkspaceJoinProtocol::new(
                    tilo_peer_state::join_answer::JoinAnswer(
                        tilo_peer_state::owner::OwnerJoinWorkspaces::new(
                            engine.owner().clone(),
                            identity.device_signing_key.clone(),
                        ),
                    ),
                ),
            ),
        ));
        // The workspaces this device holds, for their own devices to sync with and catch up from:
        // the same link every workspace host answers on.
        served.extend(engine.direct_protocols());
    }
    let (actor, actor_task) = tilo_network_actor::Actor::spawn_serving(
        endpoint.clone(),
        lookup,
        identity.cloned(),
        Arc::new(std::sync::Mutex::new(Vec::new())),
        reachability.clone(),
        served,
    );
    let (events, carrying, attending) = heard(identity.is_some(), arrivals, wake, link, documents);
    if ready.send(Ok((publisher, addr, reachability))).is_err() {
        return None;
    }
    Some(Listening {
        endpoint,
        actor,
        events,
        actor_task,
        carrying,
        attending,
    })
}
