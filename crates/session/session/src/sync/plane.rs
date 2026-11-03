//! The thread the collaboration plane runs on: this device's endpoint, bound
//! on the device's own key, the peer actor served on it, and the commands the
//! session hands it until it stops.
//!
//! The endpoint is also where a call's lane is served
//! ([`tilo_call_lane::ALPN`]): from the first pass, so a peer reaching this
//! device for a call finds someone to answer, and the call installed on it
//! when there is one.

use std::sync::Arc;

use iroh::{EndpointAddr, EndpointId};
use tilo_endpoint_reachability::Reachability;
use tilo_network_actor::{HostIdentity, NetCmd, NetEvent};

use super::{
    ChannelLink, DeviceActions, Engine, Enrolled, Outgoing, carry_out, endpoint_for, heard, redeem,
};

/// The runtime the collaboration plane's thread runs on: two workers of its
/// own, so what the plane settles never waits behind what a session shows.
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

/// What the session's endpoint carries beside its workspaces: the channels it
/// runs, and this device's action plane, which answers the person's other
/// devices on it.
pub(crate) struct Serving {
    /// This device's endpoint, once it has bound, and what answers a call's
    /// lanes on it.
    pub(crate) bound: Bound,
    /// What answers on that endpoint beside the workspaces' own traffic.
    pub(crate) beside: Beside,
}

/// What the session serves on this device's endpoint beside the workspaces'
/// traffic: the channels they use, this device's action plane, and whatever
/// else the session was handed — this device as an exit for the person's
/// other devices.
pub(crate) struct Beside {
    pub(crate) link: Option<Arc<ChannelLink>>,
    pub(crate) actions: Arc<std::sync::OnceLock<DeviceActions>>,
    /// The protocols the session answers besides its own, handed over before
    /// the endpoint binds: an endpoint's protocols are fixed once it is
    /// bound, so whatever answers on it is here from the first pass.
    pub(crate) protocols: tilo_network_actor::local_host::Protocols,
}

/// This device's endpoint as the session binds it: the bound endpoint itself,
/// once it is, and the call lane served on it from the first pass.
#[derive(Clone, Default)]
pub(crate) struct Bound {
    pub(crate) endpoint: Arc<std::sync::OnceLock<iroh::Endpoint>>,
    pub(crate) lane: crate::calls::LaneSlot,
}

/// What this device answers on from the moment its endpoint is bound: the
/// peer actor the session's commands go to, the events it hears, and what
/// runs beside it until the session stops.
struct Listening {
    endpoint: iroh::Endpoint,
    actor: tokio::sync::mpsc::Sender<NetCmd>,
    events: tokio::sync::mpsc::Sender<NetEvent>,
    actor_task: tokio::task::JoinHandle<()>,
    carrying: tokio::task::JoinHandle<()>,
    attending: Option<tokio::task::JoinHandle<()>>,
}

/// The thread: bind the endpoint, run the peer actor, and carry what it
/// hears to the session's loop until it is told to stop.
pub(super) fn run(
    identity: Option<HostIdentity>,
    engine: Option<Arc<Engine>>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Outgoing>,
    arrivals: std::sync::mpsc::Sender<NetEvent>,
    enrollments: std::sync::mpsc::Sender<Enrolled>,
    ready: std::sync::mpsc::Sender<Result<(EndpointId, EndpointAddr, Reachability), String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    Serving { bound, beside }: Serving,
) {
    let Some(runtime) = plane_runtime(&ready) else {
        return;
    };
    // The channels' side of the plane, which the loop below carries what the
    // session says to.
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
        while let Some(command) = commands.recv().await {
            let command = match command {
                // A redemption asks a host, waits for it and takes the workspace
                // in: it runs beside this loop, which keeps carrying what the
                // session says meanwhile.
                Outgoing::Enroll(request) => {
                    let slot = request.slot;
                    let workspace_id = request.workspace_id;
                    let landed = enrollments.clone();
                    let (Some(engine), Some(identity)) = (engine.clone(), identity.clone()) else {
                        let _ = landed.send(Enrolled {
                            slot,
                            workspace_id,
                            result: Err("this session joins no workspaces".into()),
                        });
                        continue;
                    };
                    let endpoint = endpoint.clone();
                    let plane = actor.clone();
                    let events = events.clone();
                    let channels = link.clone();
                    tokio::spawn(async move {
                        let result = redeem(
                            endpoint,
                            identity,
                            engine,
                            &plane,
                            &events,
                            request,
                            channels.as_deref(),
                        )
                        .await;
                        let _ = landed.send(Enrolled {
                            slot,
                            workspace_id,
                            result,
                        });
                    });
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

/// Bind this device's endpoint, serve the peer actor and its action plane on
/// it, and say where the session's apps reach this device — or say why it
/// could not, and answer nothing.
async fn listening(
    identity: Option<&HostIdentity>,
    engine: Option<&Arc<Engine>>,
    arrivals: std::sync::mpsc::Sender<NetEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    ready: &std::sync::mpsc::Sender<Result<(EndpointId, EndpointAddr, Reachability), String>>,
    Beside {
        link,
        actions,
        protocols,
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
    // Nothing wakes the held-back relays from here: the peer actor does when
    // it dials a host or a peer, and the session while a workspace it shows has
    // someone else in it (`Client::note_reachability`).
    let reachability = tilo_endpoint_reachability::Reachability::defer(
        &endpoint,
        &deferred_relays,
        tokio::runtime::Handle::current(),
        Some(tilo_endpoint_reachability::REST_AFTER),
    );
    // This device's action plane is served on the same endpoint: another of
    // the person's devices reaches what this one publishes here, beside the
    // workspaces' own traffic.
    let mut served: tilo_network_actor::local_host::Protocols = Vec::new();
    // A call's lane is served here too, from the first pass: it refuses every
    // connection until a call installs its carrier, and answers those from
    // then until the call ends.
    served.push((
        tilo_call_lane::ALPN.to_vec(),
        Box::<dyn iroh::protocol::DynProtocolHandler>::from(bound.lane.handler()),
    ));
    // What the session serves beside its own planes: this device's exit for
    // the person's other devices, when it offers one.
    served.extend(protocols);
    let _ = bound.endpoint.set(endpoint.clone());
    if let Some(identity) = identity {
        let device = DeviceActions::start(identity, endpoint.clone(), engine.cloned());
        served.push((
            tilo_peer_state::actions::ACTIONS_ALPN.to_vec(),
            Box::<dyn iroh::protocol::DynProtocolHandler>::from(device.plane()),
        ));
        // The session takes it from here: what it installs answers, and an
        // app's own-device actions leave through it.
        let _ = actions.set(device);
    }
    let (actor, actor_task) = tilo_network_actor::Actor::spawn_serving(
        endpoint.clone(),
        lookup,
        identity.cloned(),
        Arc::new(std::sync::Mutex::new(Vec::new())),
        reachability.clone(),
        served,
    );
    let (events, carrying, attending) = heard(identity.cloned(), arrivals, wake, link);
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
