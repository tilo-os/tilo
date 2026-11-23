//! Finding the workspace's devices on the local network, without a relay,
//! and without telling anyone else on it whose devices are there, or how
//! many.
//!
//! A Tilo endpoint dials a peer by its key. Number 0's directory answers
//! that over the internet, and only while this endpoint has a home relay to
//! be found at; a device whose relays rest, or a network without the
//! internet, leaves two devices holding each other's keys and no way to
//! turn one into an address. So the session endpoint also asks the local
//! network for the key it dials, in a probe only that device can open, and
//! answers the probes made for it by devices its [`Fleet`] admits.
//!
//! Finding a device is not trusting it. A found address is dialed like any
//! other: the transport authenticates the endpoint key, and the device's
//! certificate and its person's revocations are checked again before
//! anything syncs. Discovery only saves the round trip through a relay, and
//! lets two devices meet on a network with no internet at all.
//!
//! # Exchange
//!
//! 1. A device sends a probe to the multicast group [`GROUP`] on UDP
//!    [`PORT`], on every network it is on, asking for up to [`SLOTS`]
//!    devices: the one it dials, and others it looked for before.
//! 2. A device that finds a slot sealed to it opens the probe, learns which
//!    device asked and checks that its [`Fleet`] admits the asker. It
//!    answers once per probe, sealed to the asker, naming its endpoint's
//!    addresses. It also learns the asker: a device enrolled after this one
//!    is found by it this way, and asked for in turn.
//! 3. The asker opens the reply, checks that a device it asked for signed
//!    it, and dials that device at the addresses it names.
//!
//! # Probe
//!
//! `"TLOLINK2" || epoch (u32 LE) || salt (16) || slot[0] … slot[7] || body`,
//! [`DATAGRAM_LEN`] bytes. `salt` is fresh for every probe and `epoch`
//! counts ten-minute steps (see *Freshness*). For each device `P` asked
//! for, a slot is the probe's fresh body key `K` sealed to `X25519(P)` with
//! HPKE ([`tilo_seal::Label::LanDiscoveryProbe`], the header as associated
//! data); `X25519(·)` is the device's Ed25519 key in its X25519 form, as
//! the personal device fabric seals to a device. Unused slots are boxes of
//! random bytes to keys nobody holds, and the slots are sorted, so the
//! probe says nothing about how many devices are asked for or which slot
//! is whose. The body is a `tilo-seal` envelope under `K`, bound to the
//! header and slots, holding the asking device's certificate (padded to a
//! fixed length) and its device key's signature over the header and slots:
//! a collaborator who has seen the certificate cannot ask in its name.
//!
//! Nothing is shared across the workspace: a device that left it or was
//! revoked learns nothing from probes not sealed to it, and is not answered
//! once the fleet refuses its certificate. No secret has to be
//! redistributed. Someone listening on the network sees fixed-size boxes of
//! random bytes: no key, no address, no count.
//!
//! # Reply
//!
//! `"TLOLNKR2" || salt || HPKE box`, also [`DATAGRAM_LEN`] bytes, so a
//! reply is no larger than the probe that caused it. The box is sealed to
//! `X25519(asker)` ([`tilo_seal::Label::LanDiscoveryReply`], associated data
//! `"TLOLNKR2" || salt || asker`) and holds, padded, the answering device's
//! key, its endpoint's addresses and its signature over the salt, both keys
//! and the addresses, which proves to the asker that the device it asked
//! answered, not a stranger redirecting its dial.
//!
//! # Freshness
//!
//! A probe recorded and replayed later must not make a device give away
//! that it is there:
//!
//! - A device whose clock is synchronised to a time source sends its epoch
//!   and accepts probes within one epoch of it.
//! - A device without one (a board with no battery-backed clock on a
//!   network without a time source) does not send its clock, which may be
//!   the image's build date and would single it out. It sends the newest
//!   epoch it learned from an admitted device, advanced by the time since,
//!   or 0.
//! - Every device remembers, per asking device and across restarts
//!   ([`Nearby::remembering_in`]), the newest epoch it accepted from it, and
//!   refuses older ones.
//! - Each probe is answered once, and at most a fixed number of probes are
//!   opened per second.
//!
//! What remains: a probe recorded from a device never heard from at a newer
//! epoch, replayed to a device with no time source, still gets one answer
//! per recording. The answer is sealed to the device that made the probe.

mod freshness;
mod wire;

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use iroh::address_lookup::{
    AddrFilter, AddressLookup, AddressLookupBuilder, AddressLookupBuilderError, EndpointData,
    EndpointInfo, Error, Item,
};
use iroh::{Endpoint, EndpointId};
use n0_future::boxed::BoxStream;
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;

use freshness::{Freshness, epoch_at};
pub use wire::{DATAGRAM_LEN, SLOTS};
use wire::{Reply, SentProbe};

/// The UDP port probes are sent to and answered from.
pub const PORT: u16 = 23_832;
/// The multicast group probes are sent to: administratively scoped, so it
/// stays on the networks the device is on.
pub const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 93, 32);

/// The networks a session's local discovery speaks on, as IPv4 addresses separated by commas,
/// when it is told some: everywhere the endpoint is otherwise.
pub const NETWORKS_ENV: &str = "TILO_LAN_NETWORKS";

/// The file in a session's state that keeps each asking device's newest
/// epoch.
const REMEMBERED: &str = "lan-discovery.json";

/// What an address this lookup found names as its source.
const PROVENANCE: &str = "tilo-lan";

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
/// How often the devices looked for are asked for again, so the ones here
/// stay heard while nothing dials them.
const REFRESH: Duration = Duration::from_secs(60);
/// A device not heard from for this long is no longer here.
const HEARD_FOR: Duration = Duration::from_secs(3 * 60 + 4);
/// Most devices looked for; the one looked for longest ago goes first.
const LOOKING_FOR: usize = 64;

/// The devices this one finds and answers on the local network: the
/// workspace's, as the caller knows them.
pub trait Fleet: Send + Sync + 'static {
    /// This device's certificate, presented in its probes; `None` while it
    /// has none, and then it asks nobody.
    fn certificate(&self) -> Option<DeviceCert>;
    /// Whether `cert`, whose device signed a probe for this one, names a
    /// current, unrevoked device this one answers: one of its person's, or
    /// one a workspace it is in admitted.
    fn admits(&self, cert: &DeviceCert) -> bool;
}

/// The devices this device hears on its local network, kept by the lookup
/// a session's endpoint takes ([`Nearby::lookup`]): what says which of the
/// workspace's devices are reachable here without a relay. Cloning shares it.
#[derive(Clone)]
pub struct Nearby {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Nearby {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Nearby")
            .field("heard", &self.heard())
            .finish()
    }
}

/// One device heard here.
#[derive(Clone, Debug)]
struct Heard {
    /// Where it said its endpoint listens; `None` while it only asked for
    /// this device and has not answered one of its probes yet.
    addrs: Option<EndpointData>,
    at: Instant,
}

impl Heard {
    fn fresh(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.at) < HEARD_FOR
    }
}

struct Shared {
    heard: Mutex<HashMap<EndpointId, Heard>>,
    news: broadcast::Sender<(EndpointId, EndpointData)>,
    changed: Arc<dyn Fn() + Send + Sync>,
    /// The networks its lookup speaks on, when it was told rather than
    /// taking them from what the endpoint publishes.
    on: Option<Vec<Ipv4Addr>>,
    /// Where askers' newest epochs are kept across restarts.
    remembered: Mutex<Option<PathBuf>>,
}

impl Nearby {
    /// Nothing heard yet; `changed` is told each time a device is first
    /// heard or lapses, from the discovery's own tasks.
    #[must_use]
    pub fn new(changed: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::hearing(changed, None)
    }

    /// Nothing heard yet, with this device's lookup speaking only on
    /// `networks` (IPv4), whatever networks the endpoint is on: a machine on
    /// more than one network can be told which one its workspace's other
    /// devices are on, and a test can keep its discovery to this machine's
    /// loopback instead of the network it runs on.
    #[must_use]
    pub fn on(
        changed: Arc<dyn Fn() + Send + Sync>,
        networks: impl IntoIterator<Item = Ipv4Addr>,
    ) -> Self {
        Self::hearing(changed, Some(networks.into_iter().collect()))
    }

    /// [`Nearby`] speaking only on the networks [`NETWORKS_ENV`] names, or `None` when it names
    /// none: a machine on more than one network is told which one its workspace's other devices
    /// are on, and a test keeps a session's discovery on this machine's loopback instead of the
    /// network the runner gives it.
    #[must_use]
    pub fn from_env(changed: Arc<dyn Fn() + Send + Sync>) -> Option<Self> {
        let named = std::env::var(NETWORKS_ENV).ok()?;
        let mut networks = Vec::new();
        for part in named.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            match part.parse::<Ipv4Addr>() {
                Ok(network) => networks.push(network),
                Err(_) => {
                    tracing::warn!(
                        value = part,
                        "a network named for local discovery is no IPv4 address, and is ignored"
                    );
                }
            }
        }
        (!networks.is_empty()).then(|| Self::on(changed, networks))
    }

    /// The same discovery, keeping the newest epoch each asking device sent
    /// in `state` (a session's state directory; `None` keeps it in memory),
    /// so a probe recorded before a restart is not answered after it. Set
    /// before the lookup is bound.
    #[must_use]
    pub fn remembering_in(self, state: Option<&std::path::Path>) -> Self {
        *self.shared.remembered.lock_recover() = state.map(|state| state.join(REMEMBERED));
        self
    }

    fn hearing(changed: Arc<dyn Fn() + Send + Sync>, on: Option<Vec<Ipv4Addr>>) -> Self {
        let (news, _) = broadcast::channel(64);
        Self {
            shared: Arc::new(Shared {
                heard: Mutex::default(),
                news,
                changed,
                on,
                remembered: Mutex::default(),
            }),
        }
    }

    /// The lookup to bind the endpoint with: it asks the local network for
    /// a key the endpoint dials, answers the devices `fleet` admits, and
    /// keeps what it heard here.
    #[must_use]
    pub fn lookup(&self, fleet: Arc<dyn Fleet>) -> Lan {
        Lan {
            shared: Arc::clone(&self.shared),
            fleet,
        }
    }

    /// The devices heard on the local network now, this one aside: those
    /// that answered this device's probes and those that asked for it, each
    /// proven by its device key and admitted.
    #[must_use]
    pub fn heard(&self) -> BTreeSet<EndpointId> {
        let now = Instant::now();
        self.shared
            .heard
            .lock_recover()
            .iter()
            .filter(|(_, heard)| heard.fresh(now))
            .map(|(id, _)| *id)
            .collect()
    }
}

/// The local-network lookup of a [`Nearby`].
#[derive(Clone)]
pub struct Lan {
    shared: Arc<Shared>,
    fleet: Arc<dyn Fleet>,
}

impl std::fmt::Debug for Lan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lan").finish_non_exhaustive()
    }
}

impl AddressLookupBuilder for Lan {
    fn into_address_lookup(
        self,
        endpoint: &Endpoint,
    ) -> Result<impl AddressLookup, AddressLookupBuilderError> {
        let device = SigningKey::from_bytes(&endpoint.secret_key().to_bytes());
        LanLookup::spawn(device, self.shared, self.fleet)
            .map_err(|error| AddressLookupBuilderError::from_err("tilo-lan", error))
    }
}

/// The running discovery of one endpoint. Its tasks stop when it is dropped.
pub struct LanLookup {
    inner: Arc<Inner>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for LanLookup {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl std::fmt::Debug for LanLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanLookup")
            .field("networks", &*self.inner.networks.lock_recover())
            .finish_non_exhaustive()
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
    /// The devices asked for, and when each was last wanted.
    looking_for: HashMap<[u8; 32], Instant>,
}

impl State {
    fn look_for(&mut self, device: [u8; 32], now: Instant) {
        if self.looking_for.len() >= LOOKING_FOR
            && !self.looking_for.contains_key(&device)
            && let Some(stalest) = self
                .looking_for
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(device, _)| *device)
        {
            self.looking_for.remove(&stalest);
        }
        self.looking_for.insert(device, now);
    }
}

struct Inner {
    runtime: tokio::runtime::Handle,
    shared: Arc<Shared>,
    fleet: Arc<dyn Fleet>,
    device: SigningKey,
    own: [u8; 32],
    /// Bound to [`PORT`]: receives probes, sends replies. `None` when the
    /// port could not be bound: this device still finds others, and is not
    /// found.
    listener: Option<UdpSocket>,
    /// This lookup's own: sends probes, receives the replies.
    asker: UdpSocket,
    /// One probe goes out on every network before the next one does.
    sending: tokio::sync::Mutex<()>,
    /// The networks the listener joined the group on.
    networks: Mutex<BTreeSet<Ipv4Addr>>,
    /// The endpoint's own direct addresses, named in replies.
    addrs: Mutex<Vec<SocketAddr>>,
    state: Mutex<State>,
}

/// Which of the two sockets.
#[derive(Clone, Copy)]
enum Role {
    Listener,
    Asker,
}

/// A UDP socket on `port` (0: any), shared with other sessions' when not 0.
fn bind(port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.set_multicast_loop_v4(true)?;
    socket.set_multicast_ttl_v4(1)?;
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

/// The device's own IPv4 addresses among `addrs`: one for each network it
/// is on. Loopback is no network another device is on.
fn networks(addrs: &[SocketAddr]) -> BTreeSet<Ipv4Addr> {
    addrs
        .iter()
        .filter_map(|addr| match addr.ip() {
            IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(v4),
            _ => None,
        })
        .collect()
}

impl LanLookup {
    fn spawn(
        device: SigningKey,
        shared: Arc<Shared>,
        fleet: Arc<dyn Fleet>,
    ) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Handle::current();
        let listener = match bind(PORT) {
            Ok(listener) => Some(listener),
            Err(error) => {
                tracing::warn!(%error, port = PORT, "lan: this device cannot be found on the local network");
                None
            }
        };
        let asker = bind(0)?;
        let now = Instant::now();
        let remembered = shared.remembered.lock_recover().clone();
        let inner = Arc::new(Inner {
            runtime: runtime.clone(),
            own: device.verifying_key().to_bytes(),
            device,
            fleet,
            listener,
            asker,
            sending: tokio::sync::Mutex::new(()),
            networks: Mutex::default(),
            addrs: Mutex::default(),
            state: Mutex::new(State {
                freshness: Freshness::open(remembered),
                sent: VecDeque::new(),
                last_probe: None,
                retry: HashMap::new(),
                budget: (now, 0),
                looking_for: HashMap::new(),
            }),
            shared,
        });
        if let Some(on) = &inner.shared.on {
            inner.join(on.iter().copied().collect());
        }
        let mut tasks = vec![runtime.spawn(Arc::clone(&inner).receive(Role::Asker))];
        if inner.listener.is_some() {
            tasks.push(runtime.spawn(Arc::clone(&inner).receive(Role::Listener)));
        }
        tasks.push(runtime.spawn(Arc::clone(&inner).refresh()));
        Ok(Self { inner, tasks })
    }
}

impl Inner {
    /// Hear probes on exactly `now`'s networks.
    fn join(&self, now: BTreeSet<Ipv4Addr>) {
        let Some(listener) = &self.listener else {
            *self.networks.lock_recover() = now;
            return;
        };
        let mut networks = self.networks.lock_recover();
        for gone in networks.difference(&now) {
            if let Err(error) = listener.leave_multicast_v4(GROUP, *gone) {
                tracing::debug!(%error, network = %gone, "lan: could not leave a network");
            }
        }
        for new in now.difference(&networks) {
            if let Err(error) = listener.join_multicast_v4(GROUP, *new) {
                tracing::debug!(%error, network = %new, "lan: could not hear a network");
            }
        }
        *networks = now;
    }

    /// Ask the network for the devices looked for now; `wanted` is the one a
    /// dial waits for, if any, and is looked for from now on. Rate limited,
    /// and a wanted device that does not answer is asked for less and less
    /// often.
    fn probe(self: &Arc<Self>, wanted: Option<[u8; 32]>) {
        let Some(cert) = self.fleet.certificate() else {
            return;
        };
        let now = Instant::now();
        let mut state = self.state.lock_recover();
        if let Some(wanted) = wanted {
            state.look_for(wanted, now);
        }
        let mut devices: Vec<[u8; 32]> = state.looking_for.keys().copied().collect();
        devices.retain(|device| *device != self.own);
        if devices.is_empty() {
            return;
        }
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
        let (datagram, sent) = match wire::probe(&self.device, &cert, &asked, epoch) {
            Ok(probe) => probe,
            Err(error) => {
                tracing::warn!(error, "lan: could not build a probe");
                return;
            }
        };
        state.last_probe = Some(now);
        state
            .sent
            .retain(|(at, _)| now.saturating_duration_since(*at) < ANSWER_WINDOW);
        state.sent.push_back((now, sent));
        drop(state);
        let lookup = Arc::clone(self);
        self.runtime
            .spawn(async move { lookup.send_probe(datagram).await });
    }

    /// Send `datagram` to the group on every network this device is on.
    async fn send_probe(&self, datagram: Vec<u8>) {
        let _sending = self.sending.lock().await;
        let networks: Vec<Ipv4Addr> = match &self.shared.on {
            Some(on) => on.clone(),
            None => self.networks.lock_recover().iter().copied().collect(),
        };
        for network in networks {
            if let Err(error) = socket2::SockRef::from(&self.asker).set_multicast_if_v4(&network) {
                tracing::debug!(%error, %network, "lan: cannot probe on a network");
                continue;
            }
            if let Err(error) = self
                .asker
                .send_to(&datagram, SocketAddrV4::new(GROUP, PORT))
                .await
            {
                tracing::debug!(%error, %network, "lan: a probe was not sent");
            }
        }
    }

    fn reply_to(self: &Arc<Self>, datagram: Vec<u8>, to: SocketAddr) {
        let lookup = Arc::clone(self);
        self.runtime.spawn(async move {
            if let Some(listener) = &lookup.listener
                && let Err(error) = listener.send_to(&datagram, to).await
            {
                tracing::debug!(%error, %to, "lan: a reply was not sent");
            }
        });
    }

    async fn receive(self: Arc<Self>, role: Role) {
        let socket = match role {
            Role::Listener => self.listener.as_ref(),
            Role::Asker => Some(&self.asker),
        };
        let Some(socket) = socket else {
            return;
        };
        // One byte more than a datagram, so a longer one shows as longer.
        let mut buffer = vec![0_u8; DATAGRAM_LEN.saturating_add(1)];
        loop {
            let (len, from) = match socket.recv_from(&mut buffer).await {
                Ok(received) => received,
                Err(error) => {
                    tracing::warn!(%error, "lan: stopped receiving");
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

    /// Every [`REFRESH`]: forget the devices no longer heard, and ask for
    /// the ones looked for again.
    async fn refresh(self: Arc<Self>) {
        loop {
            tokio::time::sleep(REFRESH).await;
            let now = Instant::now();
            let lapsed = {
                let mut heard = self.shared.heard.lock_recover();
                let before = heard.len();
                heard.retain(|_, heard| heard.fresh(now));
                heard.len() != before
            };
            if lapsed {
                (self.shared.changed)();
            }
            self.probe(None);
        }
    }

    fn within_budget(&self, now: Instant) -> bool {
        let mut state = self.state.lock_recover();
        let budget = &mut state.budget;
        if now.saturating_duration_since(budget.0) >= Duration::from_secs(1) {
            *budget = (now, 0);
        }
        budget.1 = budget.1.saturating_add(1);
        let within = budget.1 <= PROBES_PER_SECOND;
        drop(state);
        within
    }

    fn on_probe(self: &Arc<Self>, datagram: &[u8], from: SocketAddr) {
        let now = Instant::now();
        if !self.within_budget(now) {
            return;
        }
        let Some(probe) = wire::open_probe(&self.device, datagram) else {
            return;
        };
        let asker = probe.asker.device_public_key;
        if asker == self.own || !self.fleet.admits(&probe.asker) {
            return;
        }
        let fresh = {
            let mut state = self.state.lock_recover();
            let fresh = state
                .freshness
                .accept(asker, probe.epoch, probe.salt, clock_epoch(), now);
            if fresh {
                // It is here: a dial for it may ask at once.
                state.retry.remove(&asker);
            }
            fresh
        };
        if !fresh {
            tracing::debug!("lan: ignoring a stale or repeated probe");
            return;
        }
        let addrs = self.addrs.lock_recover().clone();
        if !addrs.is_empty() {
            match wire::reply(&self.device, &probe, &addrs) {
                Ok(answer) => self.reply_to(answer, from),
                Err(error) => tracing::warn!(error, "lan: could not answer a probe"),
            }
        }
        let Ok(id) = EndpointId::from_bytes(&asker) else {
            return;
        };
        // The asker is here: one this device never heard of (a device
        // enrolled after it) is known from now on, and asked for its
        // addresses unless they were heard lately.
        let (new, answered_lately) = {
            let mut heard = self.shared.heard.lock_recover();
            let before = heard.get(&id).filter(|heard| heard.fresh(now)).cloned();
            let answered = before.as_ref().filter(|heard| heard.addrs.is_some());
            let answered_lately =
                answered.is_some_and(|heard| now.saturating_duration_since(heard.at) < REFRESH);
            if answered.is_none() {
                heard.insert(
                    id,
                    Heard {
                        addrs: None,
                        at: now,
                    },
                );
            }
            drop(heard);
            (before.is_none(), answered_lately)
        };
        if new {
            tracing::debug!(asker = %id, "lan: a device asked for this one");
            (self.shared.changed)();
        }
        if !answered_lately {
            self.probe(Some(asker));
        }
    }

    fn on_reply(&self, datagram: &[u8]) {
        let Some(reply) = self
            .state
            .lock_recover()
            .sent
            .iter()
            .rev()
            .find_map(|(at, sent)| {
                (at.elapsed() < ANSWER_WINDOW)
                    .then(|| wire::open_reply(&self.device, sent, datagram))
                    .flatten()
            })
        else {
            return;
        };
        let Reply { device, addrs } = reply;
        self.state.lock_recover().retry.remove(&device);
        let Ok(id) = EndpointId::from_bytes(&device) else {
            return;
        };
        let data = EndpointData::from(addrs.into_iter().collect::<BTreeSet<_>>());
        let before = self.shared.heard.lock_recover().insert(
            id,
            Heard {
                addrs: Some(data.clone()),
                at: Instant::now(),
            },
        );
        tracing::debug!(from = %id, ?data, "lan: a device answered");
        let _ = self.shared.news.send((id, data.clone()));
        if before.and_then(|heard| heard.addrs) != Some(data) {
            (self.shared.changed)();
        }
    }
}

impl AddressLookup for LanLookup {
    fn publish(&self, data: &EndpointData) {
        let data = data.apply_filter(&AddrFilter::ip_only()).into_owned();
        // Addresses another device can reach first: a loopback one only
        // serves a device on this same machine.
        let mut addrs: Vec<SocketAddr> = data.ip_addrs().copied().collect();
        addrs.sort_by_key(|addr| addr.ip().is_loopback());
        addrs.truncate(wire::MAX_REPLY_ADDRS);
        if self.inner.shared.on.is_none() {
            // The networks this device's addresses say it is on, kept as the
            // endpoint's addresses come and go.
            self.inner.join(networks(&addrs));
        }
        *self.inner.addrs.lock_recover() = addrs;
    }

    fn resolve(&self, endpoint_id: EndpointId) -> Option<BoxStream<Result<Item, Error>>> {
        let device = *endpoint_id.as_bytes();
        if device == self.inner.own {
            return None;
        }
        let item = move |data: EndpointData| {
            Item::new(
                EndpointInfo::from_parts(endpoint_id, data),
                PROVENANCE,
                None,
            )
        };
        let now = Instant::now();
        let known = self
            .inner
            .shared
            .heard
            .lock_recover()
            .get(&endpoint_id)
            .filter(|heard| heard.fresh(now))
            .and_then(|heard| heard.addrs.clone())
            .map(item);
        let news = self.inner.shared.news.subscribe();
        self.inner.probe(Some(device));
        let now = tokio::time::Instant::now();
        let until = now.checked_add(ANSWER_WINDOW).unwrap_or(now);
        let stream =
            n0_future::stream::unfold((known, news), move |(known, mut news)| async move {
                if let Some(known) = known {
                    return Some((Ok(known), (None, news)));
                }
                loop {
                    match tokio::time::timeout_at(until, news.recv()).await {
                        Ok(Ok((from, data))) if from == endpoint_id => {
                            return Some((Ok(item(data)), (None, news)));
                        }
                        Ok(Ok(_) | Err(broadcast::error::RecvError::Lagged(_))) => {}
                        Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => return None,
                    }
                }
            });
        Some(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::{NETWORKS_ENV, Nearby};

    /// The networks the environment names are the ones discovery speaks on, and none named leaves
    /// it on the networks the endpoint is on.
    #[test]
    fn the_environment_names_the_networks_discovery_speaks_on() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(NETWORKS_ENV, "127.0.0.1, 10.0.0.7");
        let named = Nearby::from_env(std::sync::Arc::new(|| {})).expect("the networks it names");
        assert_eq!(
            named.shared.on,
            Some(vec![Ipv4Addr::LOCALHOST, Ipv4Addr::new(10, 0, 0, 7)])
        );
        env.set(NETWORKS_ENV, "the local one");
        assert!(
            Nearby::from_env(std::sync::Arc::new(|| {})).is_none(),
            "a name that is no address leaves discovery where the endpoint is"
        );
        env.remove(NETWORKS_ENV);
        assert!(Nearby::from_env(std::sync::Arc::new(|| {})).is_none());
    }
}
