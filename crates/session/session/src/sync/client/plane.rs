//! The thread the collaboration plane runs on: this device's endpoint, bound on the device's own
//! key, the peer actor served on it, and the commands the session hands it until it stops.
//! The endpoint is what names this device on the workspaces' planes, and what carries every one of
//! them.

use std::sync::Arc;

use iroh::{EndpointAddr, EndpointId};
use tilo_endpoint_reachability::Reachability;
use tilo_network_actor::{HostIdentity, NetCmd, NetEvent, WorkspaceControlRoute};
use tilo_session_client::Cmd;
use tilo_session_client::plane::Event;

use super::adopt::{enter, redeem};
use super::{
    ChannelLink, DeviceActions, Engine, Enrolled, HostCmd, HostEvent, PeerDocuments, endpoint_for,
};

/// What the peer actor heard, as the client's own vocabulary says it: the transport keeps the
/// differences an endpoint's, and the client hears one kind of word from every host. What only this
/// desktop keeps — the person's fabric, how far a transfer got — is the host's own beside it.
pub(super) fn event_of(event: NetEvent) -> Option<Result<Event, HostEvent>> {
    Some(Ok(match event {
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
        } => {
            return Some(Err(HostEvent::PersonalFabric {
                peer,
                payload,
                received,
            }));
        }
        NetEvent::NearbyTransferProgress {
            transfer_id,
            sent_bytes,
            total_bytes,
            status,
            ..
        } => {
            return Some(Err(HostEvent::NearbyTransferProgress {
                transfer_id,
                sent_bytes,
                total_bytes,
                status,
            }));
        }
        // Taken off this path before it gets here: what a host replays into the workspaces this
        // device holds is admitted on its own thread, and the rest is the plane's own bookkeeping.
        NetEvent::NativeInput(_) | NetEvent::RecordOpened { .. } | NetEvent::OwnReplay { .. } => {
            return None;
        }
    }))
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

/// The session's ends of the plane's thread: what the client hears, what only this desktop keeps,
/// what became of an enrollment, and where the bound endpoint is said.
pub(super) struct Wire {
    pub(super) arrivals: std::sync::mpsc::Sender<Event>,
    pub(super) host_events: std::sync::mpsc::Sender<HostEvent>,
    pub(super) enrollments: std::sync::mpsc::Sender<Enrolled>,
    pub(super) ready:
        std::sync::mpsc::Sender<Result<(EndpointId, EndpointAddr, Reachability), String>>,
}

/// The thread: bind the endpoint, run the peer actor, and carry what it hears to the session's loop
/// until it is told to stop.
pub(super) fn run(
    identity: Option<HostIdentity>,
    engine: Option<Arc<Engine>>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<HostCmd>,
    wire: Wire,
    wake: Arc<dyn Fn() + Send + Sync>,
    Serving { bound, beside }: Serving,
) {
    let Wire {
        arrivals,
        host_events,
        enrollments,
        ready,
    } = wire;
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
            host_events,
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
            let Some(taken) = carry_host(&actor, &events, &asking, command).await else {
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

/// Give one command to the plane — a client's own, or what only this desktop keeps beside it —
/// answering whether it was taken, or `None` when it is the command that ends the plane.
async fn carry_host(
    actor: &tokio::sync::mpsc::Sender<NetCmd>,
    events: &tokio::sync::mpsc::Sender<NetEvent>,
    asking: &Asking,
    command: HostCmd,
) -> Option<bool> {
    Some(match command {
        HostCmd::Plane(Cmd::Stop) => return None,
        HostCmd::Enroll(request) => {
            asking.ask(Ask::Invitation(Box::new(request)));
            true
        }
        HostCmd::Enter(request) => {
            asking.ask(Ask::Entry(request));
            true
        }
        HostCmd::TakePart(connect) => actor.send(NetCmd::NativeConnect(connect)).await.is_ok(),
        HostCmd::Content(delivery) => actor
            .send(NetCmd::PublishRecordContent(delivery))
            .await
            .is_ok(),
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
        HostCmd::NearbySpool {
            user_public_key,
            target,
            checkpoint,
            file,
        } => actor
            .send(NetCmd::PublishNearbyTransfer {
                user_public_key,
                target,
                transfer: checkpoint,
                file,
            })
            .await
            .is_ok(),
        HostCmd::Plane(command) => carry_out(actor, command, events).await?,
    })
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
    Invitation(Box<super::Enrolling>),
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
                        *request,
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
    host_events: std::sync::mpsc::Sender<HostEvent>,
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
    // The devices that take part in this device's workspaces through it reach the rest of each
    // workspace, and its apps, through this plane.
    if let Some(members) = engine.and_then(|engine| engine.members()) {
        let _ = actor.send(NetCmd::ServeMembers(members)).await;
    }
    let (events, carrying, attending) = heard(
        identity.is_some(),
        arrivals,
        host_events,
        wake,
        link,
        documents,
    );
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

/// Hear the peer actor: what it says reaches the client, and what a workspace's host says after
/// this device joined it is taken in where this device's workspaces are kept, on a thread of its
/// own and in the host's order. What admission takes in from another device goes to this device's
/// apps through `documents`.
fn heard(
    holds_custody: bool,
    arrivals: std::sync::mpsc::Sender<Event>,
    host_events: std::sync::mpsc::Sender<HostEvent>,
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
            match event_of(event) {
                Some(Ok(event)) => {
                    if arrivals.send(event).is_err() {
                        break;
                    }
                }
                Some(Err(event)) => {
                    if host_events.send(event).is_err() {
                        break;
                    }
                }
                None => {}
            }
            (wake)();
        }
    });
    (events, carrying, attending)
}

/// Take what hosts replay into the workspaces this device holds, as it arrives: each run of one
/// session admitted together and in the order the host said it, on a blocking thread so no replay
/// holds up the collaboration plane.
async fn take_in(
    mut replayed: tokio::sync::mpsc::UnboundedReceiver<tilo_network_actor::native_ingress::Input>,
    link: Option<Arc<ChannelLink>>,
    documents: Option<PeerDocuments>,
) {
    while let Some(input) = replayed.recv().await {
        let mut waiting = vec![input];
        while let Ok(next) = replayed.try_recv() {
            waiting.push(next);
        }
        let mut run: Vec<tilo_network_actor::native_ingress::Input> = Vec::new();
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
                .is_some_and(|first: &tilo_network_actor::native_ingress::Input| {
                    !first.same_session(&input)
                })
            {
                admit(run, link.as_deref(), documents.as_ref()).await;
                run = Vec::new();
            }
            run.push(input);
        }
        admit(run, link.as_deref(), documents.as_ref()).await;
    }
}

/// Admit one session's inputs, as one commit of the workspace they belong to.
async fn admit(
    inputs: Vec<tilo_network_actor::native_ingress::Input>,
    link: Option<&ChannelLink>,
    documents: Option<&PeerDocuments>,
) {
    let Some(workspace) = inputs
        .first()
        .map(tilo_network_actor::native_ingress::Input::workspace)
    else {
        return;
    };
    let admitted = match tokio::task::spawn_blocking(move || {
        tilo_network_actor::native_ingress::Input::admit_batch(inputs)
    })
    .await
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

/// Give one command from the client to the plane: whether it was taken, or `None` when it is the
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
        Cmd::Leave(ws) => plane.send(NetCmd::LeaveWorkspace { ws }).await.is_ok(),
        Cmd::Publish { ws, msg } => plane.send(NetCmd::Publish { ws, msg: *msg }).await.is_ok(),
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
        Cmd::Replay(ws) => plane.send(NetCmd::NativeReplay { ws }).await.is_ok(),
        Cmd::PublishCustody(ws) => plane.send(NetCmd::NativePublish { ws }).await.is_ok(),
        Cmd::OpenRecord { ws, channel } => {
            plane.send(NetCmd::OpenRecord { ws, channel }).await.is_ok()
        }
        Cmd::Roster { ws, devices } => plane
            .send(NetCmd::SetWorkspaceRoster { ws, devices })
            .await
            .is_ok(),
        Cmd::Stop => return None,
    })
}
