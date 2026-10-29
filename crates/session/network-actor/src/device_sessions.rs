//! The sessions a personal topic travels on.
//!
//! A person's topics go through every host that stores them, on the one
//! session protocol a room host serves: the person's workspace hosts
//! ([`NetCmd::ConnectHost`](super::NetCmd::ConnectHost)), this device's own
//! host, joined in process, and the host each of the person's other devices
//! runs ([`NetCmd::Connect`](super::NetCmd::Connect)). Every device serves
//! its person's topics to the person's other devices, so two of them reach
//! each other with no relay up, and each device's host keeps what was
//! published for the ones that are away. What a device publishes goes on
//! all of its sessions; what arrives twice, through two hosts, is taken
//! once.
//!
//! The other device is a member of the topic while this device's session
//! with its host is up: what is for
//! that device alone (its fabric, input, window streams) goes directly to
//! it then.
//!
//! The person's own devices are dialled as the device's person, whatever
//! identity a topic is presented with: their hosts admit their own person
//! to any of its workspaces.
//!
//! Two of the person's devices keep one connection between them
//! ([`SiblingLink`]), however many topics they share and whichever way a
//! session goes: each session is a stream of its own on it. The device
//! with the lower endpoint id dials it, the other only when it is not
//! dialled within a few seconds (the first may not know of it), and the
//! dialler's host serves the sessions the other opens on it; the other
//! device's host hands the connection on to this actor when it is dialled
//! ([`RoomHost::siblings`]). An idle
//! connection keeps both devices waking for its keep-alives, so a second one
//! would cost as much again.
//!
//! [`RoomHost::siblings`]: crate::local_host::room_host::RoomHost::siblings
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_relay_session::{RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCert, RevocationList};

use super::{
    ActorState, DEFAULT_WS_TOPIC, HostIdentity, HostRouteScheduler, InternalEvent, WorkspaceId,
};
use crate::local_host::LocalHost;

/// How long dialling one of the person's devices may take.
const DEVICE_DIAL: Duration = Duration::from_secs(10);

impl ActorState {
    /// Open whatever session of personal topic `ws` is missing and not
    /// being opened: with each workspace host, with this device's own host,
    /// and with each of the person's other devices.
    pub(super) fn schedule_topic_sessions(&mut self, ws: WorkspaceId) {
        if !self.topics.contains_key(&ws) || self.native_routes.contains(ws) {
            return;
        }
        let Some(identity) = self.host_identity.clone() else {
            return;
        };
        let mut routes = HostRouteScheduler {
            endpoint: &self.endpoint,
            identity: &identity,
            internal_tx: &self.internal_tx,
            sessions: &self.host_sessions,
            connecting: &mut self.connecting_hosts,
            content_discovery_tasks: &mut self.content_discovery_tasks,
        };
        for (addr, expectation) in self.hosts.values().cloned() {
            routes.schedule(ws, addr, expectation);
        }
        let Some((cert, revocations)) = native_material(&identity) else {
            tracing::warn!(ws, "this device's identity has an invalid revocation entry");
            return;
        };
        self.join_own_host(ws, &identity, &cert, &revocations);
        let devices: Vec<EndpointAddr> = self.own_devices.values().cloned().collect();
        for addr in devices {
            self.dial_own_device(ws, addr, &identity, &cert, &revocations);
        }
    }

    /// Join `ws` at this device's own host, in process, once it is open.
    fn join_own_host(
        &mut self,
        ws: WorkspaceId,
        identity: &HostIdentity,
        cert: &DeviceCert,
        revocations: &RevocationList,
    ) {
        let own = self.endpoint.id();
        if self.host_sessions.contains_key(&(ws, own)) || self.connecting_hosts.contains(&(ws, own))
        {
            return;
        }
        let Some(host) = self.local_host.as_ref().and_then(|local| local.get()) else {
            return;
        };
        self.connecting_hosts.insert((ws, own));
        let (cert, revocations) = (cert.clone(), revocations.clone());
        let key = identity.device_signing_key.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let device = key.verifying_key().to_bytes();
            let result = host
                .connect_local(cert, key, revocations)
                .await
                .map_err(|error| error.to_string())
                .and_then(|client| {
                    RelayCoeditSession::join(client, ws, device).map_err(|error| error.to_string())
                });
            let _ = tx
                .send(InternalEvent::HostConnected {
                    ws,
                    peer: own,
                    addr: EndpointAddr::new(own),
                    result: Box::new(result),
                })
                .await;
        });
    }

    /// Open a session of `ws` with the host of the person's device at
    /// `addr`, pinned to that device and the person, on the one connection
    /// between the two devices: dialled now when this device dials it and
    /// none is up, waited for when the other device dials it.
    fn dial_own_device(
        &mut self,
        ws: WorkspaceId,
        addr: EndpointAddr,
        identity: &HostIdentity,
        cert: &DeviceCert,
        revocations: &RevocationList,
    ) {
        let device = addr.id;
        // A device the person also configured as a host is dialled as one.
        if self.hosts.contains_key(&device)
            || self.host_sessions.contains_key(&(ws, device))
            || self.connecting_hosts.contains(&(ws, device))
        {
            return;
        }
        let link = self.sibling_links.entry(device).or_default().clone();
        let dial = if link.is_up() {
            None
        } else if dials(self.endpoint.id(), device) || link.waited_out() {
            Some(Dial {
                endpoint: self.endpoint.clone(),
                addr: addr.clone(),
                host: self.local_host.clone(),
            })
        } else {
            return;
        };
        self.connecting_hosts.insert((ws, device));
        let (cert, revocations) = (cert.clone(), revocations.clone());
        let key = identity.device_signing_key.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let own = key.verifying_key().to_bytes();
            let result = tokio::time::timeout(DEVICE_DIAL, async {
                let connection = link.connection(dial).await?;
                let (send, recv) = connection
                    .open_bi()
                    .await
                    .map_err(|error| error.to_string())?;
                RelayClient::connect_v2_over_stream(
                    recv,
                    send,
                    *device.as_bytes(),
                    cert,
                    key,
                    revocations,
                )
                .await
                .map_err(|error| super::relay_error(&device, &error))
            })
            .await
            .map_err(|_| "the device did not answer in time".to_string())
            .and_then(|result| result)
            .and_then(|client| {
                RelayCoeditSession::join(client, ws, own).map_err(|error| error.to_string())
            });
            let _ = tx
                .send(InternalEvent::HostConnected {
                    ws,
                    peer: device,
                    addr,
                    result: Box::new(result),
                })
                .await;
        });
    }

    /// One of the person's other devices dialled this one: its sessions with
    /// this device's host run on `connection`, and so do this device's with
    /// its host from now on.
    pub(super) fn sibling_connected(&mut self, connection: Connection) {
        self.sibling_links
            .entry(connection.remote_id())
            .or_default()
            .offer(connection, self.endpoint.id());
        for ws in self.topics.keys().copied().collect::<Vec<_>>() {
            self.schedule_topic_sessions(ws);
        }
    }

    /// Hear the connections the person's other devices dial this device's
    /// host on, from the host open now.
    pub(super) fn watch_siblings(&mut self) {
        let Some(host) = self.local_host.as_ref().and_then(|local| local.get()) else {
            return;
        };
        let mut siblings = host.siblings();
        let tx = self.internal_tx.clone();
        self.sibling_watch = Some(SiblingWatch(tokio::spawn(async move {
            loop {
                match siblings.recv().await {
                    Ok(connection) => {
                        if tx
                            .send(InternalEvent::SiblingConnected(connection))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        })));
    }
}

/// Whether this device, `own`, dials the connection to the person's device
/// `other` at once: the one with the lower endpoint id does, so the two do
/// not bring up a connection each. The other dials only once it waited
/// [`UNDIALLED`] for it in vain: the device may not know of it.
fn dials(own: EndpointId, other: EndpointId) -> bool {
    own < other
}

/// How long the device with the higher endpoint id waits to be dialled
/// before it dials itself.
const UNDIALLED: Duration = Duration::from_secs(5);

/// Whether `connection` is the one the device with the lower endpoint id
/// dialled, of the two a pair may bring up at once.
/// `own` is this device.
fn dialled_by_the_lower(connection: &Connection, own: EndpointId) -> bool {
    let dialled_here = connection.side() == iroh::endpoint::Side::Client;
    dialled_here == dials(own, connection.remote_id())
}

/// What dialling the connection to one of the person's devices takes.
pub(super) struct Dial {
    endpoint: Endpoint,
    addr: EndpointAddr,
    /// This device's host, which serves the sessions the other device opens
    /// on the connection while it is open.
    host: Option<LocalHost>,
}

/// The one connection to one of the person's other devices, which every
/// session between the two runs on. It closes once no one holds the link.
#[derive(Clone, Default)]
pub(super) struct SiblingLink(Arc<Held>);

#[derive(Default)]
struct Held {
    connection: Mutex<Option<Connection>>,
    /// Since when the device waits to be dialled ([`UNDIALLED`]).
    waiting: Mutex<Option<tokio::time::Instant>>,
    /// Lets one dial at a time bring the connection up.
    dialling: tokio::sync::Mutex<()>,
}

impl SiblingLink {
    fn current(&self) -> Option<Connection> {
        self.0
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .filter(|connection| connection.close_reason().is_none())
    }

    fn is_up(&self) -> bool {
        self.current().is_some()
    }

    /// Whether this device waited [`UNDIALLED`] to be dialled, counting from
    /// the first time it asks.
    fn waited_out(&self) -> bool {
        let mut waiting = self
            .0
            .waiting
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        waiting
            .get_or_insert_with(tokio::time::Instant::now)
            .elapsed()
            >= UNDIALLED
    }

    /// Take `connection`, dialled by either device (`own` is this one), and
    /// answer the one the link keeps. A newer connection dialled the same
    /// way replaces the one held (its dialler started anew); of two dialled
    /// at once, one each way, both devices keep the one the lower endpoint
    /// id dialled.
    fn offer(&self, connection: Connection, own: EndpointId) -> Connection {
        let mut held = self
            .0
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let kept = match held
            .as_ref()
            .filter(|current| current.close_reason().is_none())
        {
            Some(current)
                if current.stable_id() != connection.stable_id()
                    && current.side() != connection.side()
                    && !dialled_by_the_lower(&connection, own) =>
            {
                Some(current.clone())
            }
            _ => None,
        };
        if let Some(current) = kept {
            connection.close(0_u32.into(), b"the lower endpoint id's connection is kept");
            return current;
        }
        *self
            .0
            .waiting
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        if let Some(previous) = held.replace(connection.clone())
            && previous.stable_id() != connection.stable_id()
        {
            previous.close(0_u32.into(), b"replaced");
        }
        connection
    }

    /// The connection up now, or the one `dial` brings up when there is
    /// none.
    async fn connection(&self, dial: Option<Dial>) -> Result<Connection, String> {
        if let Some(connection) = self.current() {
            return Ok(connection);
        }
        let Some(dial) = dial else {
            return Err("the device's connection closed".into());
        };
        let _dialling = self.0.dialling.lock().await;
        if let Some(connection) = self.current() {
            return Ok(connection);
        }
        let connection = dial
            .endpoint
            .connect(dial.addr, tilo_relay_protocol::transport::SIBLING_ALPN)
            .await
            .map_err(|error| error.to_string())?;
        tokio::spawn(answer_sessions(connection.clone(), dial.host));
        Ok(self.offer(connection, dial.endpoint.id()))
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(connection) = self
            .connection
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            connection.close(0_u32.into(), b"no longer linked");
        }
    }
}

/// Serve each session the other device opens on `connection`, which this
/// device dialled, at this device's host; refuse it while no host is open.
async fn answer_sessions(connection: Connection, host: Option<LocalHost>) {
    while let Ok(streams) = connection.accept_bi().await {
        match host.as_ref().and_then(LocalHost::get) {
            Some(host) => host.serve_stream(&connection, streams),
            // Dropped, the streams end and the device's handshake fails.
            None => drop(streams),
        }
    }
}

/// The task that hears the host's [`SiblingLink`] connections, stopped
/// with the actor.
pub(super) struct SiblingWatch(tokio::task::JoinHandle<()>);

impl Drop for SiblingWatch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The device's own certificate and revocations, as its person's: what the
/// person's devices know it by.
fn native_material(identity: &HostIdentity) -> Option<(DeviceCert, RevocationList)> {
    let (cert, entries) = identity.material_for(DEFAULT_WS_TOPIC);
    let mut revocations = RevocationList::new();
    for entry in entries {
        revocations.insert(entry).ok()?;
    }
    Some((cert, revocations))
}
