//! The exit side: who may route through this device, the addresses its
//! clients get, and the packets between their connections and the exit TUN.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use iroh::EndpointId;
use iroh::endpoint::{Connection, SendDatagramError, SendStream, VarInt};

use super::device::PacketDevice;
use super::dns::{DnsRelay, Query};
use super::wire::{self, ClientHello, Offer, Purpose, close};
use crate::net::{GATEWAY, Ipv4Net, MAX_MTU, is_client_address};
use crate::packet::{self, Ipv4Packet};

/// How long a client may take to say why it came.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a running session's right to route is checked again.
const RECHECK: Duration = Duration::from_secs(15);
/// The exit TUN's MTU: what a path that has found its MTU carries.
pub const EXIT_MTU: u16 = 1400;

/// Whether a device may route through this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Not one of this person's devices.
    NotYours,
    /// The person has not turned this device's exit on.
    ExitOff,
}

/// What the exit asks of the session it runs in.
pub trait Authority: Send + Sync + 'static {
    /// Whether `peer` may route through this device now.
    fn decide(&self, peer: &EndpointId) -> Decision;
    /// The networks this device is attached to, offered for "only my home
    /// network".
    fn home_networks(&self) -> Vec<Ipv4Net>;
}

/// Opens and closes this device's exit TUN: in the shell, by asking the
/// helper.
pub trait ExitDevices: Send + Sync + 'static {
    type Device: PacketDevice;
    fn open(&self, mtu: u16) -> impl Future<Output = io::Result<Self::Device>> + Send;
    fn close(&self) -> impl Future<Output = ()> + Send;
}

/// A device routing through this one, as the exit's status shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionView {
    pub peer: EndpointId,
    pub address: Ipv4Addr,
    pub since: SystemTime,
}

struct Session {
    id: u64,
    peer: EndpointId,
    connection: Connection,
    since: SystemTime,
}

struct Open<D> {
    device: Arc<D>,
    reader: tokio::task::AbortHandle,
}

struct State<D> {
    open: Option<Open<D>>,
    /// By client address.
    sessions: HashMap<Ipv4Addr, Session>,
    /// Each device keeps its address for as long as the exit runs.
    assigned: HashMap<EndpointId, Ipv4Addr>,
    next_id: u64,
}

struct Inner<A: Authority, X: ExitDevices> {
    authority: A,
    devices: X,
    dns: DnsRelay,
    state: Mutex<State<X::Device>>,
    /// Held while the TUN opens or closes, and while a session registers,
    /// so the last session leaving never closes a TUN a new one just took.
    lifecycle: tokio::sync::Mutex<()>,
    on_change: Box<dyn Fn() + Send + Sync>,
}

/// This device as an exit. Cloning shares it.
pub struct Exit<A: Authority, X: ExitDevices> {
    inner: Arc<Inner<A, X>>,
}

impl<A: Authority, X: ExitDevices> Clone for Exit<A, X> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<A: Authority, X: ExitDevices> std::fmt::Debug for Exit<A, X> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Exit")
    }
}

fn code(value: u32) -> VarInt {
    VarInt::from_u32(value)
}

impl<A: Authority, X: ExitDevices> Exit<A, X> {
    /// An exit deciding by `authority`, opening its TUN through `devices`,
    /// answering DNS through `dns`, and calling `on_change` whenever the
    /// devices routing through it change.
    pub fn new(
        authority: A,
        devices: X,
        dns: DnsRelay,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                authority,
                devices,
                dns,
                state: Mutex::new(State {
                    open: None,
                    sessions: HashMap::new(),
                    assigned: HashMap::new(),
                    next_id: 0,
                }),
                lifecycle: tokio::sync::Mutex::new(()),
                on_change: Box::new(on_change),
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State<X::Device>> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The devices routing through this one now.
    pub fn sessions(&self) -> Vec<SessionView> {
        let mut sessions: Vec<SessionView> = self
            .state()
            .sessions
            .iter()
            .map(|(address, session)| SessionView {
                peer: session.peer,
                address: *address,
                since: session.since,
            })
            .collect();
        sessions.sort_by_key(|session| session.address);
        sessions
    }

    /// Stop `peer` routing through this device: its connection closes with
    /// [`close::STOPPED`], which tells it not to dial straight back.
    pub fn stop(&self, peer: &EndpointId) {
        for (session_peer, connection) in self.connections() {
            if &session_peer == peer {
                connection.close(code(close::STOPPED), b"stopped");
            }
        }
    }

    fn connections(&self) -> Vec<(EndpointId, Connection)> {
        let state = self.state();
        state
            .sessions
            .values()
            .map(|session| (session.peer, session.connection.clone()))
            .collect()
    }

    /// End every session whose device may no longer route, with the code
    /// that says why: after the exit switch went off or a device was removed.
    pub fn recheck(&self) {
        for (peer, connection) in self.connections() {
            match self.inner.authority.decide(&peer) {
                Decision::Allow => {}
                Decision::NotYours => connection.close(code(close::NOT_YOURS), b"not yours"),
                Decision::ExitOff => connection.close(code(close::EXIT_OFF), b"exit off"),
            }
        }
    }

    /// Serve one connection on the tunnel ALPN until it ends.
    pub async fn serve(&self, connection: Connection) {
        let peer = connection.remote_id();
        // Nothing is read from a device that may not route.
        match self.inner.authority.decide(&peer) {
            Decision::Allow => {}
            Decision::NotYours => {
                connection.close(code(close::NOT_YOURS), b"not yours");
                return;
            }
            Decision::ExitOff => {
                connection.close(code(close::EXIT_OFF), b"exit off");
                return;
            }
        }
        let handshake = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            let (send, mut recv) = connection
                .accept_bi()
                .await
                .map_err(|error| error.to_string())?;
            let hello: ClientHello = wire::read(&mut recv).await?;
            Ok::<_, String>((send, hello))
        })
        .await;
        let (send, hello) = match handshake {
            Ok(Ok((send, hello))) if hello.version == wire::VERSION => (send, hello),
            _ => {
                connection.close(code(close::PROTOCOL), b"handshake");
                return;
            }
        };
        match hello.purpose {
            Purpose::Probe => {
                let offer = self.offer(None);
                if answer(send, &offer).await.is_ok() {
                    // The client closes once it has read the offer.
                    let _ = tokio::time::timeout(HANDSHAKE_TIMEOUT, connection.closed()).await;
                }
            }
            Purpose::Route => self.route(connection, send, peer).await,
        }
    }

    fn offer(&self, address: Option<Ipv4Addr>) -> Offer {
        Offer {
            gateway: GATEWAY,
            address,
            mtu: EXIT_MTU,
            home_networks: self
                .inner
                .authority
                .home_networks()
                .into_iter()
                .filter(|network| network.is_routable_home())
                .collect(),
        }
    }

    fn assign(state: &mut State<X::Device>, peer: EndpointId) -> Option<Ipv4Addr> {
        if let Some(address) = state.assigned.get(&peer) {
            return Some(*address);
        }
        let taken: Vec<Ipv4Addr> = state.assigned.values().copied().collect();
        let base = u32::from(crate::net::TUNNEL_NETWORK.address());
        let address = (2..=254_u32)
            .filter_map(|host| base.checked_add(host).map(Ipv4Addr::from))
            .find(|address| is_client_address(*address) && !taken.contains(address))?;
        state.assigned.insert(peer, address);
        Some(address)
    }

    /// The exit TUN, opened on first use. Call with `lifecycle` held.
    async fn device(&self) -> io::Result<Arc<X::Device>> {
        let existing = self
            .state()
            .open
            .as_ref()
            .map(|open| Arc::clone(&open.device));
        if let Some(device) = existing {
            return Ok(device);
        }
        let device = Arc::new(self.inner.devices.open(EXIT_MTU).await?);
        let reader =
            tokio::spawn(read_exit_device(self.clone(), Arc::clone(&device))).abort_handle();
        self.state().open = Some(Open {
            device: Arc::clone(&device),
            reader,
        });
        Ok(device)
    }

    async fn route(&self, connection: Connection, send: SendStream, peer: EndpointId) {
        let lifecycle = self.inner.lifecycle.lock().await;
        let device = match self.device().await {
            Ok(device) => device,
            Err(error) => {
                tracing::warn!(%error, "the exit could not open its tunnel");
                drop(lifecycle);
                connection.close(code(close::FULL), b"no tunnel");
                return;
            }
        };
        let registered = {
            let mut state = self.state();
            let registered = Self::assign(&mut state, peer).map(|address| {
                let id = state.next_id;
                state.next_id = state.next_id.wrapping_add(1);
                let replaced = state.sessions.insert(
                    address,
                    Session {
                        id,
                        peer,
                        connection: connection.clone(),
                        since: SystemTime::now(),
                    },
                );
                (id, address, replaced)
            });
            drop(state);
            registered
        };
        let Some((id, address, replaced)) = registered else {
            drop(lifecycle);
            connection.close(code(close::FULL), b"full");
            self.close_if_unused().await;
            return;
        };
        if let Some(replaced) = replaced {
            replaced
                .connection
                .close(code(close::REPLACED), b"replaced");
        }
        drop(lifecycle);
        (self.inner.on_change)();
        if answer(send, &self.offer(Some(address))).await.is_ok() {
            self.receive(&connection, &device, address, peer).await;
        }
        let ended = {
            let mut state = self.state();
            let ours = state
                .sessions
                .get(&address)
                .is_some_and(|session| session.id == id);
            if ours {
                state.sessions.remove(&address);
            }
            drop(state);
            ours
        };
        if ended {
            (self.inner.on_change)();
        }
        self.close_if_unused().await;
    }

    /// Hand a client's packets to the exit TUN until its connection ends,
    /// checking its right to route as it goes.
    async fn receive(
        &self,
        connection: &Connection,
        device: &Arc<X::Device>,
        address: Ipv4Addr,
        peer: EndpointId,
    ) {
        let mut recheck = tokio::time::interval(RECHECK);
        recheck.tick().await;
        loop {
            tokio::select! {
                datagram = connection.read_datagram() => {
                    let Ok(datagram) = datagram else { return };
                    let Some(packet) = Ipv4Packet::parse(&datagram) else { continue };
                    // A client sends only from the address it was given.
                    if packet.source() != address {
                        continue;
                    }
                    if let Some(query) = Query::of(&packet) {
                        let connection = connection.clone();
                        self.inner.dns.answer(query, move |answer| {
                            let _ = connection.send_datagram(Bytes::from(answer));
                        });
                        continue;
                    }
                    if let Err(error) = device.send(&datagram).await {
                        tracing::debug!(%error, "the exit tunnel refused a packet");
                    }
                }
                _ = recheck.tick() => {
                    let code_for = match self.inner.authority.decide(&peer) {
                        Decision::Allow => continue,
                        Decision::NotYours => close::NOT_YOURS,
                        Decision::ExitOff => close::EXIT_OFF,
                    };
                    connection.close(code(code_for), b"no longer allowed");
                    return;
                }
            }
        }
    }

    /// Close the exit TUN once nobody routes through it.
    async fn close_if_unused(&self) {
        let lifecycle = self.inner.lifecycle.lock().await;
        let open = {
            let mut state = self.state();
            if state.sessions.is_empty() {
                state.open.take()
            } else {
                None
            }
        };
        if let Some(open) = open {
            open.reader.abort();
            drop(open);
            self.inner.devices.close().await;
        }
        drop(lifecycle);
    }

    fn connection_for(&self, address: Ipv4Addr) -> Option<Connection> {
        self.state()
            .sessions
            .get(&address)
            .map(|session| session.connection.clone())
    }
}

async fn answer(mut send: SendStream, offer: &Offer) -> Result<(), String> {
    let framed = wire::frame(offer)?;
    send.write_all(&framed)
        .await
        .map_err(|error| error.to_string())?;
    send.finish().map_err(|error| error.to_string())
}

/// Send what the exit TUN hands back to the client it is for.
async fn read_exit_device<A: Authority, X: ExitDevices>(exit: Exit<A, X>, device: Arc<X::Device>) {
    let mut buf = vec![0_u8; usize::from(MAX_MTU)];
    loop {
        let length = match device.recv(&mut buf).await {
            Ok(length) => length,
            Err(error) => {
                tracing::debug!(%error, "the exit tunnel closed");
                return;
            }
        };
        let Some(bytes) = buf.get(..length) else {
            continue;
        };
        let Some(packet) = Ipv4Packet::parse(bytes) else {
            continue;
        };
        let Some(connection) = exit.connection_for(packet.destination()) else {
            continue;
        };
        send_or_shrink(&connection, &packet, &*device).await;
    }
}

/// Send `packet` on `connection`; one too large for the path is answered on
/// `device` with the size that fits.
pub(crate) async fn send_or_shrink<D: PacketDevice>(
    connection: &Connection,
    packet: &Ipv4Packet<'_>,
    device: &D,
) {
    match connection.send_datagram(Bytes::copy_from_slice(packet.bytes())) {
        Err(SendDatagramError::TooLarge) => {
            let fits = connection
                .max_datagram_size()
                .and_then(|size| u16::try_from(size).ok())
                .unwrap_or(crate::net::MIN_MTU);
            if let Some(icmp) = packet::fragmentation_needed(packet, GATEWAY, fits) {
                let _ = device.send(&icmp).await;
            }
        }
        Err(error) => tracing::trace!(%error, "a tunnel packet was not sent"),
        Ok(()) => {}
    }
}

impl<A: Authority, X: ExitDevices> iroh::protocol::ProtocolHandler for Exit<A, X> {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        self.serve(connection).await;
        Ok(())
    }
}
