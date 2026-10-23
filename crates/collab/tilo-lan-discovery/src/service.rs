//! The running discovery: a socket on the discovery port that answers the
//! person's devices' probes, a socket of its own that sends this device's
//! probes and receives the replies, and what it learns served to the
//! endpoint as an address lookup. Probes reach every session listening on
//! the port, several accounts' on one machine included; a reply goes back
//! to the asking session's own socket.

use std::collections::{HashMap, VecDeque};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use iroh::address_lookup::{AddressLookup, EndpointData, EndpointInfo, Error, Item};
use iroh::{EndpointAddr, EndpointId, TransportAddr};
use n0_future::boxed::BoxStream;
use tilo_user_identity::DeviceCert;
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc};

use crate::freshness::{Freshness, epoch_at};
use crate::wire::{self, Reply, SLOTS, SentProbe};

/// The UDP port probes are broadcast to and answered from.
pub const PORT: u16 = 23_832;
/// What an address this lookup found names as its source.
pub const PROVENANCE: &str = "tilo-lan";
/// A probe is sent at most this often, unless it asks for a device the last
/// one did not.
const PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// A device that does not answer is asked for again after this, doubling
/// up to [`MAX_RETRY`] until it answers.
const FIRST_RETRY: Duration = Duration::from_secs(2);
const MAX_RETRY: Duration = Duration::from_secs(120);
/// How long a resolve waits for an answer, and a sent probe's replies are
/// accepted.
const ANSWER_WINDOW: Duration = Duration::from_secs(4);
/// Most probes opened per second; each costs up to [`SLOTS`] key agreements.
const PROBES_PER_SECOND: u32 = 16;

/// The person's devices as this device knows them.
pub trait OwnDevices: Send + Sync + 'static {
    /// This device's certificate, presented in its probes; `None` while it
    /// has none.
    fn certificate(&self) -> Option<DeviceCert>;
    /// The person's other devices to look for.
    fn devices(&self) -> Vec<[u8; 32]>;
    /// Whether `cert`, whose device signed a probe, names a current,
    /// unrevoked device of the person other than this one.
    fn admits(&self, cert: &DeviceCert) -> bool;
}

/// What discovery found, for the caller to act on.
#[derive(Clone, Debug)]
pub enum Found {
    /// A device of the person answered at these addresses: dial it.
    Answered(EndpointAddr),
    /// A device of the person asked for this one, proving this certificate.
    Asked(DeviceCert),
}

/// Where discovery runs.
#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    /// Where probes go: the local network's broadcast address and the port
    /// the person's devices listen on.
    pub broadcast: SocketAddrV4,
    /// Where askers' newest epochs are kept across restarts.
    pub state: Option<PathBuf>,
}

impl Config {
    /// The local network, keeping freshness at `state`.
    #[must_use]
    pub const fn lan(state: Option<PathBuf>) -> Self {
        Self {
            port: PORT,
            broadcast: SocketAddrV4::new(Ipv4Addr::BROADCAST, PORT),
            state,
        }
    }
}

struct State {
    freshness: Freshness,
    sent: VecDeque<(Instant, SentProbe)>,
    last_probe: Option<Instant>,
    /// When each device not heard from may be asked for again, and the
    /// wait after that.
    retry: HashMap<[u8; 32], (Instant, Duration)>,
    budget: (Instant, u32),
}

struct Inner {
    runtime: tokio::runtime::Handle,
    device: SigningKey,
    trust: Arc<dyn OwnDevices>,
    /// Bound to the discovery port: receives probes, sends replies.
    listener: UdpSocket,
    /// This session's own: sends probes, receives replies.
    asker: UdpSocket,
    broadcast: SocketAddrV4,
    /// The endpoint's own direct addresses, named in replies.
    addrs: Mutex<Vec<SocketAddr>>,
    state: Mutex<State>,
    found: mpsc::Sender<Found>,
    answers: broadcast::Sender<Reply>,
}

/// Which of the two sockets.
#[derive(Clone, Copy)]
enum Role {
    Listener,
    Asker,
}

/// LAN discovery of the person's own devices. Cheap to clone.
#[derive(Clone)]
pub struct LanDiscovery {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for LanDiscovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanDiscovery")
            .field("broadcast", &self.inner.broadcast)
            .finish_non_exhaustive()
    }
}

/// A UDP socket on `port` (0: any), shared with other sessions' when not 0.
fn bind(port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_broadcast(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())?;
    UdpSocket::from_std(socket.into())
}

/// The epoch of the wall clock, `None` while it is not synchronised to a
/// time source.
fn clock_epoch() -> Option<u32> {
    clock_synchronised().then(|| epoch_at(tilo_clock::unix_now()))
}

#[cfg(target_os = "linux")]
fn clock_synchronised() -> bool {
    // SAFETY: `timex` is plain old data; with `modes` zero `ntp_adjtime`
    // only reads the kernel's clock state into it.
    let state = unsafe {
        let mut timex: libc::timex = std::mem::zeroed();
        libc::ntp_adjtime(&raw mut timex)
    };
    state >= 0 && state != libc::TIME_ERROR
}

#[cfg(not(target_os = "linux"))]
fn clock_synchronised() -> bool {
    true
}

/// Pick at most [`SLOTS`] of `devices`, `wanted` first, the rest at random.
fn pick(mut devices: Vec<[u8; 32]>, wanted: Option<[u8; 32]>) -> Vec<[u8; 32]> {
    devices.sort_unstable();
    devices.dedup();
    if devices.len() <= SLOTS {
        return devices;
    }
    let mut picked: Vec<[u8; 32]> = wanted.filter(|w| devices.contains(w)).into_iter().collect();
    devices.retain(|device| !picked.contains(device));
    while picked.len() < SLOTS && !devices.is_empty() {
        let mut draw = [0_u8; 8];
        let index = if tilo_seal::fill_random(&mut draw).is_ok() {
            let len = u64::try_from(devices.len()).unwrap_or(u64::MAX);
            usize::try_from(u64::from_le_bytes(draw).checked_rem(len).unwrap_or(0)).unwrap_or(0)
        } else {
            0
        };
        picked.push(devices.swap_remove(index));
    }
    picked
}

impl LanDiscovery {
    /// Bind the discovery socket and start answering on `runtime`.
    /// Returns what discovery finds as it finds it.
    ///
    /// # Errors
    ///
    /// When the socket cannot be bound.
    pub fn spawn(
        runtime: &tokio::runtime::Handle,
        device: SigningKey,
        trust: Arc<dyn OwnDevices>,
        config: Config,
    ) -> std::io::Result<(Self, mpsc::Receiver<Found>)> {
        let (listener, asker) = {
            let _entered = runtime.enter();
            (bind(config.port)?, bind(0)?)
        };
        let (found, found_rx) = mpsc::channel(64);
        let (answers, _) = broadcast::channel(32);
        let now = Instant::now();
        let discovery = Self {
            inner: Arc::new(Inner {
                runtime: runtime.clone(),
                device,
                trust,
                listener,
                asker,
                broadcast: config.broadcast,
                addrs: Mutex::new(Vec::new()),
                state: Mutex::new(State {
                    freshness: Freshness::open(config.state),
                    sent: VecDeque::new(),
                    last_probe: None,
                    retry: HashMap::new(),
                    budget: (now, 0),
                }),
                found,
                answers,
            }),
        };
        let probes = discovery.clone();
        runtime.spawn(async move { probes.receive(Role::Listener).await });
        let replies = discovery.clone();
        runtime.spawn(async move { replies.receive(Role::Asker).await });
        Ok((discovery, found_rx))
    }

    /// Ask the network for the person's devices now; `wanted` is the one a
    /// dial waits for, if any. Rate limited, and a wanted device that does
    /// not answer is asked for less and less often.
    pub fn probe(&self, wanted: Option<[u8; 32]>) {
        let Some(cert) = self.inner.trust.certificate() else {
            return;
        };
        let own = self.inner.device.verifying_key().to_bytes();
        let mut devices = self.inner.trust.devices();
        devices.retain(|device| *device != own);
        if devices.is_empty() {
            return;
        }
        let now = Instant::now();
        let Ok(mut state) = self.inner.state.lock() else {
            return;
        };
        let recent = state
            .last_probe
            .is_some_and(|at| now.saturating_duration_since(at) < PROBE_INTERVAL);
        let wanted_covered = wanted.is_none_or(|wanted| {
            state
                .sent
                .back()
                .is_some_and(|(_, sent)| sent.asked.contains(&wanted))
        });
        if recent && wanted_covered {
            return;
        }
        if let Some(wanted) = wanted {
            let (next, wait) = state
                .retry
                .get(&wanted)
                .copied()
                .unwrap_or((now, FIRST_RETRY));
            if now < next {
                return;
            }
            state.retry.insert(
                wanted,
                (
                    now.checked_add(wait).unwrap_or(now),
                    wait.saturating_mul(2).min(MAX_RETRY),
                ),
            );
        }
        let asked = pick(devices, wanted);
        let epoch = state.freshness.send_epoch(clock_epoch(), now);
        let (datagram, sent) = match wire::probe(&self.inner.device, &cert, &asked, epoch) {
            Ok(probe) => probe,
            Err(error) => {
                tracing::warn!(error, "could not build a LAN discovery probe");
                return;
            }
        };
        state.last_probe = Some(now);
        state
            .sent
            .retain(|(at, _)| now.saturating_duration_since(*at) < ANSWER_WINDOW);
        state.sent.push_back((now, sent));
        drop(state);
        self.send(Role::Asker, datagram, self.inner.broadcast.into());
    }

    fn socket(&self, role: Role) -> &UdpSocket {
        match role {
            Role::Listener => &self.inner.listener,
            Role::Asker => &self.inner.asker,
        }
    }

    fn send(&self, role: Role, datagram: Vec<u8>, to: SocketAddr) {
        let discovery = self.clone();
        self.inner.runtime.spawn(async move {
            if let Err(error) = discovery.socket(role).send_to(&datagram, to).await {
                tracing::debug!(%error, %to, "could not send a LAN discovery datagram");
            }
        });
    }

    async fn receive(self, role: Role) {
        // One byte more than a datagram, so a longer one shows as longer.
        let mut buffer = vec![0_u8; wire::DATAGRAM_LEN.saturating_add(1)];
        loop {
            let (len, from) = match self.socket(role).recv_from(&mut buffer).await {
                Ok(received) => received,
                Err(error) => {
                    tracing::warn!(%error, "LAN discovery stopped receiving");
                    return;
                }
            };
            let Some(datagram) = buffer.get(..len) else {
                continue;
            };
            match role {
                Role::Asker if wire::reply_salt(datagram).is_some() => self.on_reply(datagram),
                Role::Listener if wire::probe_header(datagram).is_some() => {
                    self.on_probe(datagram, from);
                }
                _ => {}
            }
        }
    }

    fn within_budget(&self, now: Instant) -> bool {
        let Ok(mut state) = self.inner.state.lock() else {
            return false;
        };
        let (since, used) = &mut state.budget;
        if now.saturating_duration_since(*since) >= Duration::from_secs(1) {
            *since = now;
            *used = 0;
        }
        *used = used.saturating_add(1);
        *used <= PROBES_PER_SECOND
    }

    fn on_probe(&self, datagram: &[u8], from: SocketAddr) {
        let now = Instant::now();
        if !self.within_budget(now) {
            return;
        }
        let Some(probe) = wire::open_probe(&self.inner.device, datagram) else {
            return;
        };
        let asker = probe.asker.device_public_key;
        if asker == self.inner.device.verifying_key().to_bytes()
            || !self.inner.trust.admits(&probe.asker)
        {
            return;
        }
        let fresh = self.inner.state.lock().is_ok_and(|mut state| {
            let fresh = state
                .freshness
                .accept(asker, probe.epoch, probe.salt, clock_epoch(), now);
            if fresh {
                // It is here: a dial for it may ask at once.
                state.retry.remove(&asker);
            }
            fresh
        });
        if !fresh {
            tracing::debug!("ignoring a stale or repeated LAN discovery probe");
            return;
        }
        let addrs = self
            .inner
            .addrs
            .lock()
            .map(|addrs| addrs.clone())
            .unwrap_or_default();
        if !addrs.is_empty() {
            match wire::reply(&self.inner.device, &probe, &addrs) {
                Ok(answer) => self.send(Role::Listener, answer, from),
                Err(error) => tracing::warn!(error, "could not answer a LAN discovery probe"),
            }
        }
        let _ = self.inner.found.try_send(Found::Asked(probe.asker));
    }

    fn on_reply(&self, datagram: &[u8]) {
        let Some(reply) = self.inner.state.lock().ok().and_then(|mut state| {
            let reply = state.sent.iter().rev().find_map(|(at, sent)| {
                (at.elapsed() < ANSWER_WINDOW)
                    .then(|| wire::open_reply(&self.inner.device, sent, datagram))
                    .flatten()
            })?;
            state.retry.remove(&reply.device);
            Some(reply)
        }) else {
            return;
        };
        let Ok(id) = EndpointId::from_bytes(&reply.device) else {
            return;
        };
        let addr = EndpointAddr::from_parts(id, reply.addrs.iter().copied().map(TransportAddr::Ip));
        let _ = self.inner.found.try_send(Found::Answered(addr));
        let _ = self.inner.answers.send(reply);
    }
}

impl AddressLookup for LanDiscovery {
    fn publish(&self, data: &EndpointData) {
        let addrs: Vec<SocketAddr> = data
            .ip_addrs()
            .filter(|addr| !addr.ip().is_loopback())
            .copied()
            .collect();
        if let Ok(mut current) = self.inner.addrs.lock() {
            *current = addrs;
        }
    }

    fn resolve(&self, endpoint_id: EndpointId) -> Option<BoxStream<Result<Item, Error>>> {
        let device = *endpoint_id.as_bytes();
        if !self.inner.trust.devices().contains(&device) {
            return None;
        }
        let answers = self.inner.answers.subscribe();
        self.probe(Some(device));
        let now = tokio::time::Instant::now();
        let deadline = now.checked_add(ANSWER_WINDOW).unwrap_or(now);
        let stream = n0_future::stream::unfold(answers, move |mut answers| async move {
            loop {
                match tokio::time::timeout_at(deadline, answers.recv()).await {
                    Ok(Ok(reply)) if reply.device == device => {
                        let data = EndpointData::new(
                            reply.addrs.into_iter().map(TransportAddr::Ip).collect(),
                        );
                        let item = Item::new(
                            EndpointInfo::from_parts(endpoint_id, data),
                            PROVENANCE,
                            None,
                        );
                        return Some((Ok(item), answers));
                    }
                    Ok(Ok(_) | Err(broadcast::error::RecvError::Lagged(_))) => {}
                    Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => return None,
                }
            }
        });
        Some(Box::pin(stream))
    }
}
