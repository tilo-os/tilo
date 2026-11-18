//! The thread the peer network runs on: this device's endpoint, bound on the device's own
//! key, the peer actor served on it, and the commands the session hands it until it stops.
//! The endpoint is what names this device on the workspaces' peer networks, and what carries every one of
//! them.

use std::sync::Arc;

use iroh::{EndpointAddr, EndpointId};
use tilo_endpoint_reachability::Reachability;
use tilo_network_actor::own_service::OwnService;
use tilo_network_actor::{DeviceIdentity, NetCmd, NetEvent};
use tilo_session_client::Cmd;
use tilo_session_client::network::Event;

use super::adopt::{Carrier, enter, redeem};
use super::{
    DesktopEvent, DeviceActions, Engine, Enrolled, HostCmd, KeyLink, PeerDocuments, endpoint_for,
};
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
        NetEvent::HostRecordOpened {
            ws,
            channel,
            answer,
        } => Event::DocumentOpened {
            ws,
            channel,
            route: None,
            answer,
        },
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
        NetEvent::NearbyTransferProgress {
            transfer_id,
            sent_bytes,
            total_bytes,
            status,
            ..
        } => {
            return Some(Err(DesktopEvent::NearbyTransferProgress {
                transfer_id,
                sent_bytes,
                total_bytes,
                status,
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
    pub(crate) fn new(
        bound: Bound,
        link: Option<Arc<KeyLink>>,
        actions: Arc<std::sync::OnceLock<DeviceActions>>,
        protocols: tilo_network_actor::own_service::Protocols,
        documents: Option<super::PeerDocuments>,
    ) -> Self {
        Self {
            bound,
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
/// they use, this device's app wire, and whatever else the session was handed — this device as
/// an exit for the person's other devices.
pub(crate) struct Beside {
    pub(crate) link: Option<Arc<KeyLink>>,
    pub(crate) actions: Arc<std::sync::OnceLock<DeviceActions>>,
    /// The protocols the session answers besides its own, handed over before the endpoint binds: an
    /// endpoint's protocols are fixed once it is bound, so whatever answers on it is here from the
    /// first pass.
    pub(crate) protocols: tilo_network_actor::own_service::Protocols,
    /// Where what admission takes in from another device is handed to this device's apps: `None` on
    /// a session that serves no apps.
    pub(crate) documents: Option<super::PeerDocuments>,
}

/// This device's endpoint as the session binds it: the bound endpoint itself,
/// once it is, the relays it holds back until something needs it dialable, and
/// where it was given and reached hosts.
#[derive(Clone)]
pub(crate) struct Bound {
    pub(crate) endpoint: Arc<std::sync::OnceLock<iroh::Endpoint>>,
    pub(crate) relays: tilo_endpoint_reachability::EndpointRelays,
    /// What it hears on the local network.
    pub(crate) lan: tilo_endpoint_reachability::Nearby,
    pub(crate) reached: Reached,
}

impl Bound {
    /// The endpoint to bind, filled in once it is, holding back `relays`, hearing the local
    /// network into `lan`, and dialling each host where this device reached it ([`Reached`]).
    pub(crate) fn new(
        endpoint: Arc<std::sync::OnceLock<iroh::Endpoint>>,
        relays: tilo_endpoint_reachability::EndpointRelays,
        lan: tilo_endpoint_reachability::Nearby,
    ) -> Self {
        Self {
            endpoint,
            relays,
            lan,
            reached: Reached::default(),
        }
    }
}

/// Where this device was given a host's address and reached the host there, by the host's id.
///
/// The address a host advertises for itself is the one it sees from its own network: behind a NAT,
/// a port forward or a VM's network, that is its loopback or a private address, which from here is
/// nobody or this machine itself. The address this device was handed (a VM's host rewrites the
/// server's to the one the VM reaches it at) and reached the host at is the one that works, so it is
/// kept apart from everything heard about the host: an invitation, a join's answer or a host's
/// signed address adds paths beside it, and never replaces it. The endpoint dials every address it
/// knows of a host at once, so the one that answers carries the connection.
#[derive(Clone, Debug)]
pub(crate) struct Reached(iroh::address_lookup::memory::MemoryLookup);

impl Default for Reached {
    fn default() -> Self {
        Self(iroh::address_lookup::memory::MemoryLookup::with_provenance(
            "reached",
        ))
    }
}

impl Reached {
    /// This device was given `addr` for its host and reached the host there: from now on it dials
    /// the host there too, whatever the host says of itself. A later address it reached the same
    /// host at replaces this one.
    pub(crate) fn note(&self, addr: &EndpointAddr) {
        if addr.addrs.is_empty() {
            return;
        }
        self.0.set_endpoint_info(addr.clone());
    }

    /// The endpoint `builder` makes, resolving a host's id through `lookup` (what the session hears
    /// of hosts) and, beside it, where this device reached the host.
    pub(crate) fn resolving(
        &self,
        builder: iroh::endpoint::Builder,
        lookup: &iroh::address_lookup::memory::MemoryLookup,
    ) -> iroh::endpoint::Builder {
        builder
            .address_lookup(lookup.clone())
            .address_lookup(self.0.clone())
    }
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
    /// This device's own workspace service, which holds what it founds.
    own_service: Option<OwnService>,
}

/// The session's ends of the peer network's thread: what the client hears, what only this desktop keeps,
/// what became of an enrollment, and where the bound endpoint is said.
pub(super) struct Wire {
    pub(super) arrivals: std::sync::mpsc::Sender<Event>,
    pub(super) desktop_events: std::sync::mpsc::Sender<DesktopEvent>,
    pub(super) enrollments: std::sync::mpsc::Sender<Enrolled>,
    pub(super) ready:
        std::sync::mpsc::Sender<Result<(EndpointId, EndpointAddr, Reachability), String>>,
}

/// The thread: bind the endpoint, run the peer actor, and carry what it hears to the session's loop
/// until it is told to stop.
pub(super) fn run(
    identity: Option<DeviceIdentity>,
    engine: Option<Arc<Engine>>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<HostCmd>,
    wire: Wire,
    wake: Arc<dyn Fn() + Send + Sync>,
    Serving { bound, beside }: Serving,
) {
    let Wire {
        arrivals,
        desktop_events,
        enrollments,
        ready,
    } = wire;
    let Some(runtime) = network_runtime(&ready) else {
        return;
    };
    // The channels' side of the peer network, which the loop below carries what the session says to.
    let link = beside.link.clone();
    runtime.block_on(async move {
        let Some(Listening {
            endpoint,
            actor,
            events,
            actor_task,
            carrying,
            attending,
            own_service,
        }) = listening(
            identity.as_ref(),
            engine.as_ref(),
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
        // What asks a workspace's host runs beside this loop, so what the session says meanwhile is
        // still carried.
        let asking = Asking {
            landed: enrollments,
            endpoint,
            network: actor.clone(),
            events: events.clone(),
            link,
            engine,
            identity,
            own_service,
        };
        while let Some(command) = commands.recv().await {
            let Some(taken) = carry_host(&actor, &events, &asking, command).await else {
                break;
            };
            if !taken {
                tracing::debug!("the peer network stopped taking commands");
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

/// Give one command to the peer network — a client's own, or what only this desktop keeps beside it —
/// answering whether it was taken, or `None` when it is the command that ends the peer network.
async fn carry_host(
    actor: &tokio::sync::mpsc::Sender<NetCmd>,
    events: &tokio::sync::mpsc::Sender<NetEvent>,
    asking: &Asking,
    command: HostCmd,
) -> Option<bool> {
    Some(match command {
        HostCmd::Network(Cmd::Stop) => return None,
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
        HostCmd::Network(command) => carry_out(actor, command, events).await?,
    })
}

/// What asking a workspace's host needs, held for the peer network's loop: the loop never waits on a host,
/// so an ask runs on its own task and hands what became of it back as an [`Enrolled`].
struct Asking {
    landed: std::sync::mpsc::Sender<Enrolled>,
    endpoint: iroh::Endpoint,
    network: tokio::sync::mpsc::Sender<NetCmd>,
    events: tokio::sync::mpsc::Sender<NetEvent>,
    link: Option<Arc<KeyLink>>,
    engine: Option<Arc<Engine>>,
    identity: Option<DeviceIdentity>,
    /// This device's own workspace service: a workspace founded here is placed on it, and an
    /// invitation that names it is redeemed with it in process.
    own_service: Option<OwnService>,
}

/// One thing the peer network asks a workspace's host for: someone's invitation redeemed into their
/// workspace, or one of the person's own workspaces entered at the device serving it.
enum Ask {
    Invitation(Box<super::Enrolling>),
    Entry(Box<super::Entering>),
}

impl Asking {
    /// Ask `ask` beside the loop, and hand what became of it back. A session built without a
    /// workspace engine answers at once that it takes nothing in.
    fn ask(&self, ask: Ask) {
        let (workspace_id, member, name, missing, held) = match &ask {
            Ask::Invitation(request) => (
                request.workspace_id,
                false,
                None,
                "this session joins no workspaces",
                false,
            ),
            Ask::Entry(request) => (
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
                workspace_id,
                member,
                name,
                result: Err(missing.into()),
                held,
            });
            return;
        };
        let endpoint = self.endpoint.clone();
        let network = self.network.clone();
        let events = self.events.clone();
        let channels = self.link.clone();
        let own_service = self.own_service.clone();
        tokio::spawn(async move {
            let result = match ask {
                Ask::Invitation(request) => {
                    redeem(
                        endpoint,
                        identity,
                        engine,
                        Some(Carrier {
                            network: &network,
                            events: &events,
                        }),
                        *request,
                        channels.as_deref(),
                        own_service.as_ref(),
                    )
                    .await
                }
                Ask::Entry(request) => {
                    enter(
                        endpoint,
                        identity,
                        engine,
                        &network,
                        &events,
                        *request,
                        channels.as_deref(),
                    )
                    .await
                }
            };
            let _ = landed.send(Enrolled {
                workspace_id,
                member,
                name,
                result,
                held,
            });
        });
    }
}

/// Where the device's files daemon reaches the network: through this endpoint rather than one of
/// its own under the same key, since a device has one endpoint id (`fs::net`).
fn files_link(
    endpoint: &iroh::Endpoint,
    reachability: &Reachability,
) -> tilo_network_actor::own_service::Protocols {
    let woken = reachability.clone();
    fs::net::DaemonLink::serve(
        endpoint.clone(),
        launcher::services::blob_socket().with_file_name(fs::net::LINK_FILE),
        Arc::new(move || {
            woken.wake("the files daemon reaches a peer");
        }),
    )
}

/// Wait for `opening`, this device's own workspace service opening, and tell the peer actor once
/// it is open: whether it opened.
async fn own_service_opened(
    opening: impl std::future::Future<Output = Result<impl Sized, String>>,
    actor: &tokio::sync::mpsc::Sender<NetCmd>,
) -> bool {
    match opening.await {
        Ok(_) => {
            let _ = actor.send(NetCmd::OwnServiceReady).await;
            true
        }
        Err(error) => {
            tracing::warn!(%error, "this device's workspace service did not open, so it founds no workspace");
            false
        }
    }
}

/// Bind this device's endpoint, serve the peer actor and its app wire on it, and say where the
/// session's apps reach this device — or say why it could not, and answer nothing.
async fn listening(
    identity: Option<&DeviceIdentity>,
    engine: Option<&Arc<Engine>>,
    arrivals: std::sync::mpsc::Sender<Event>,
    desktop_events: std::sync::mpsc::Sender<DesktopEvent>,
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
    let (builder, deferred_relays) = match endpoint_for(identity, &bound.relays, &bound.lan) {
        Ok(building) => building,
        Err(error) => {
            let _ = ready.send(Err(error));
            return None;
        }
    };
    let endpoint = match bound
        .reached
        .resolving(builder, &lookup)
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
    // This device's app wire is served on the same endpoint: another of the person's devices
    // reaches what this one publishes here, beside the workspaces' own traffic.
    let mut served: tilo_network_actor::own_service::Protocols = Vec::new();
    // What the session serves beside its own peer networks: this device's exit for the person's other
    // devices, when it offers one.
    served.extend(protocols);
    if identity.is_some() {
        served.extend(files_link(&endpoint, &reachability));
    }
    let _ = bound.endpoint.set(endpoint.clone());
    if let Some(device) =
        identity.map(|identity| DeviceActions::start(identity, endpoint.clone(), engine.cloned()))
    {
        served.push((
            tilo_peer_state::actions::ACTIONS_ALPN.to_vec(),
            Box::<dyn iroh::protocol::DynProtocolHandler>::from(device.action_service()),
        ));
        // The session takes it from here: what it installs answers, and an app's own-device actions
        // leave through it.
        let _ = actions.set(device);
    }
    // This device's own workspace service answers here: the workspaces it founds are held there,
    // and their other devices and members join, sync and act with it on its own ALPNs. A device
    // action service of the session's own, pushed above, answers that ALPN in its place.
    let (own_service, serving_own) = match identity.zip(engine).map(|_| OwnService::deferred()) {
        Some((service, protocols)) => (
            Some(service.clone()),
            Some(tilo_network_actor::OwnServing { service, protocols }),
        ),
        None => (None, None),
    };
    let (actor, actor_task) = tilo_network_actor::Actor::spawn_serving(
        endpoint.clone(),
        lookup,
        identity.cloned(),
        Arc::new(std::sync::Mutex::new(Vec::new())),
        reachability.clone(),
        serving_own,
        served,
    );
    let mut opened = false;
    if let (Some(service), Some(identity), Some(engine)) = (&own_service, identity, engine) {
        let opening = super::own_service::open(service, identity, engine, endpoint.clone());
        opened = own_service_opened(Box::pin(opening), &actor).await;
    }
    let founding_link = link.clone();
    let (events, carrying, attending) = heard(
        identity.is_some(),
        arrivals,
        desktop_events,
        wake,
        link,
        documents,
    );
    // A workspace this device makes is founded on its own service from now on, and carried on
    // this network.
    if let (true, Some(service), Some(identity), Some(engine)) =
        (opened, &own_service, identity, engine)
    {
        engine.serve_founding(Arc::new(super::own_service::Founding {
            service: service.clone(),
            endpoint: endpoint.clone(),
            identity: identity.clone(),
            engine: Arc::downgrade(engine),
            network: Some((actor.clone(), events.clone())),
            link: founding_link,
        }));
    }
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
        own_service,
    })
}

/// Hear the peer actor: what it says reaches the client, and what a workspace's host says after
/// this device joined it is taken in where this device's workspaces are kept, on a thread of its
/// own and in the host's order. What admission takes in from another device goes to this device's
/// apps through `documents`.
fn heard(
    holds_ownership: bool,
    arrivals: std::sync::mpsc::Sender<Event>,
    desktop_events: std::sync::mpsc::Sender<DesktopEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    link: Option<Arc<KeyLink>>,
    documents: Option<PeerDocuments>,
) -> (
    tokio::sync::mpsc::Sender<NetEvent>,
    tokio::task::JoinHandle<()>,
    Option<tokio::task::JoinHandle<()>>,
) {
    let replayed = link.clone();
    let (admissions, attending) = if holds_ownership {
        let (say, hear) = tokio::sync::mpsc::unbounded_channel();
        let attending = tokio::spawn(async move { take_in(hear, link, documents).await });
        (Some(say), Some(attending))
    } else {
        (None, None)
    };
    let (events, mut heard) = tokio::sync::mpsc::channel::<NetEvent>(256);
    let carrying = tokio::spawn(async move {
        while let Some(event) = heard.recv().await {
            match (&event, admissions.as_ref()) {
                (NetEvent::NativeInput(input), Some(say)) => {
                    let _ = say.send(Taken::Input(input.clone()));
                }
                // Its replay ended, behind the replay's writes: told after they are taken in.
                (
                    NetEvent::HostReady {
                        ws,
                        documents_when_opened,
                        ..
                    },
                    Some(say),
                ) => {
                    let _ = say.send(Taken::Ready(*ws, *documents_when_opened));
                }
                (
                    NetEvent::HostReady {
                        ws,
                        documents_when_opened,
                        ..
                    },
                    None,
                ) => {
                    if let Some(link) = &replayed {
                        link.host_ready(*ws, *documents_when_opened);
                    }
                }
                _ => {}
            }
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
    (events, carrying, attending)
}

/// What a host says that is taken in on the replay's own thread, in the order it said it.
enum Taken {
    /// A write it replays or carries into a workspace this device holds.
    Input(Box<tilo_network_actor::native_ingress::Input>),
    /// It finished replaying what it retains of the workspace on this topic, and whether it sends
    /// records only once a device opens them.
    Ready(WorkspaceId, bool),
}

/// Take what hosts replay into the workspaces this device holds, as it arrives: each run of one
/// session admitted together and in the order the host said it, on a blocking thread so no replay
/// holds up the peer network.
async fn take_in(
    mut replayed: tokio::sync::mpsc::UnboundedReceiver<Taken>,
    link: Option<Arc<KeyLink>>,
    documents: Option<PeerDocuments>,
) {
    while let Some(taken) = replayed.recv().await {
        let mut waiting = vec![taken];
        while let Ok(next) = replayed.try_recv() {
            waiting.push(next);
        }
        let mut run: Vec<tilo_network_actor::native_ingress::Input> = Vec::new();
        for taken in waiting {
            let input = match taken {
                Taken::Input(input) => *input,
                Taken::Ready(ws, documents_when_opened) => {
                    admit(
                        std::mem::take(&mut run),
                        link.as_deref(),
                        documents.as_ref(),
                    )
                    .await;
                    if let Some(link) = &link {
                        link.host_ready(ws, documents_when_opened);
                    }
                    continue;
                }
            };
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
    link: Option<&KeyLink>,
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
            Err(error) if error.is_device_failure() => {
                tracing::warn!(
                    %error,
                    %workspace,
                    "this computer could not take a workspace's state in; nothing was refused"
                );
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
            // read again, and the peer network told what it admits now.
            link.workspace_changed(workspace);
        }
    }
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
        Cmd::SendControl { ws, msg, target } => network
            .send(NetCmd::PublishWorkspaceControl {
                ws,
                msg: *msg,
                target,
            })
            .await
            .is_ok(),
        Cmd::Connect(addr) => network
            .send(NetCmd::ConnectHost {
                addr,
                expectation: None,
            })
            .await
            .is_ok(),
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
        Cmd::OpenDocument { ws, channel, own } => network
            .send(NetCmd::OpenDocument { ws, channel, own })
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
