//! The session's side of "linked device, where are you?" on the nearby
//! rendezvous ([`tilo_relay_session::nearby::linked`] documents the exchange and its
//! privacy properties): ask the person's linked devices on the local network
//! for their endpoint addresses, answer them when they ask, and dial each
//! device found through the same path as the enrolled direct-peer list.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tilo_relay_session::nearby::{self, linked};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

use super::actor::NetCmd;

/// Asking again after a probe, doubling up to [`PROBE_MAX_INTERVAL`] while
/// nothing changes, so a device that comes up later is found within seconds
/// of asking and an idle network sees little traffic.
const PROBE_FIRST_INTERVAL: Duration = Duration::from_secs(1);
const PROBE_MAX_INTERVAL: Duration = Duration::from_secs(30);
/// How often the runner looks at its interfaces and addresses.
const POLL: Duration = Duration::from_secs(2);
/// Probes whose replies are still accepted.
const PENDING_PROBES: usize = 16;
const PROBE_LIFETIME: Duration = Duration::from_secs(60);
/// Replies opened per probe at most: one per linked device, with room for
/// a device answering twice. Anything beyond is a flood.
const OPENS_PER_PROBE_SLACK: usize = 2;
/// Probes already answered, so each is answered once.
const ANSWERED_SALTS: usize = 1024;
/// Answers sent to one source address at most per second.
const ANSWERS_PER_SOURCE_PER_SEC: f64 = 4.0;
const ANSWER_SOURCES: usize = 256;
/// A device found again at the same addresses is dialled at most this often.
const REDIAL_INTERVAL: Duration = Duration::from_secs(20);

/// What the session wants from discovery right now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LanDiscoveryConfig {
    /// The continuity switch allows it.
    pub enabled: bool,
    /// Linked devices of the person this device still trusts.
    pub peers: BTreeSet<[u8; 32]>,
}

/// Whether `ip` may be named in a reply: addresses a device on the same
/// local network can reach. Link-local IPv6 needs a scope a reply cannot
/// carry.
fn local_network(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

/// Whether a probe's source may be answered: a unicast address and port,
/// never one of this machine's broadcast addresses.
fn answerable(from: SocketAddr, broadcasts: &[std::net::SocketAddrV4]) -> bool {
    from.port() != 0
        && match from.ip() {
            IpAddr::V4(v4) => {
                !v4.is_broadcast()
                    && !v4.is_multicast()
                    && !v4.is_unspecified()
                    && !broadcasts.iter().any(|broadcast| *broadcast.ip() == v4)
            }
            IpAddr::V6(v6) => !v6.is_multicast() && !v6.is_unspecified(),
        }
}

fn endpoint_addrs(endpoint: &iroh::Endpoint) -> Vec<SocketAddr> {
    endpoint
        .addr()
        .ip_addrs()
        .copied()
        .filter(|addr| local_network(addr.ip()))
        .collect()
}

/// This device's epoch when its clock is synchronised to a time source.
fn synced_epoch() -> Option<u32> {
    linked::clock_is_synchronized().then(|| linked::epoch_at(tilo_clock::unix_now()))
}

fn bind_ephemeral(broadcast: bool) -> Option<UdpSocket> {
    let socket = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.set_broadcast(broadcast).ok()?;
    tokio_socket(Ok(socket))
}

fn tokio_socket(socket: std::io::Result<std::net::UdpSocket>) -> Option<UdpSocket> {
    let socket = socket.ok()?;
    socket.set_nonblocking(true).ok()?;
    UdpSocket::from_std(socket).ok()
}

fn short(device: &[u8; 32]) -> String {
    hex::encode(device.get(..5).unwrap_or_default())
}

struct PendingProbe {
    salt: linked::Salt,
    sent: Instant,
    opened: usize,
}

struct Runner {
    endpoint: iroh::Endpoint,
    keys: linked::LinkKeys,
    cmd_tx: mpsc::Sender<NetCmd>,
    config: LanDiscoveryConfig,
    freshness: linked::Freshness,
    freshness_path: Option<PathBuf>,
    pending: VecDeque<PendingProbe>,
    answered: VecDeque<linked::Salt>,
    answered_set: HashSet<linked::Salt>,
    sources: HashMap<IpAddr, (f64, Instant)>,
    dialled: HashMap<[u8; 32], (Vec<SocketAddr>, Instant)>,
    started: Instant,
}

impl Runner {
    fn new(
        endpoint: iroh::Endpoint,
        keys: linked::LinkKeys,
        cmd_tx: mpsc::Sender<NetCmd>,
        freshness_path: Option<PathBuf>,
    ) -> Self {
        let freshness = freshness_path
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .map(|bytes| linked::Freshness::decode(&bytes))
            .unwrap_or_default();
        Self {
            endpoint,
            keys,
            cmd_tx,
            config: LanDiscoveryConfig::default(),
            freshness,
            freshness_path,
            pending: VecDeque::new(),
            answered: VecDeque::new(),
            answered_set: HashSet::new(),
            sources: HashMap::new(),
            dialled: HashMap::new(),
            started: Instant::now(),
        }
    }

    fn active(&self) -> bool {
        self.config.enabled && !self.config.peers.is_empty()
    }

    fn persist_freshness(&self) {
        if let Some(path) = &self.freshness_path
            && let Err(error) =
                tilo_files::native::atomic::write_with_mode(path, &self.freshness.encode(), 0o600)
        {
            tracing::debug!(%error, "could not remember linked-device epochs");
        }
    }

    async fn probe(&mut self, socket: &UdpSocket) {
        let epoch = self.freshness.outgoing(synced_epoch());
        let Ok(probe) = linked::Probe::build(&mut self.keys, &self.config.peers, epoch) else {
            return;
        };
        let bytes = probe.encode();
        let mut sent = false;
        for destination in nearby::broadcast_destinations(nearby::NEARBY_PORT) {
            sent |= socket.send_to(&bytes, destination).await.is_ok();
        }
        if sent {
            self.pending.push_back(PendingProbe {
                salt: *probe.salt(),
                sent: Instant::now(),
                opened: 0,
            });
            while self.pending.len() > PENDING_PROBES {
                self.pending.pop_front();
            }
        }
    }

    /// Whether another answer to `ip` fits its budget, taking it if so.
    fn answer_budget(&mut self, ip: IpAddr) -> bool {
        let now = Instant::now();
        if self.sources.len() >= ANSWER_SOURCES && !self.sources.contains_key(&ip) {
            // Keep active sources' budgets: rotating forged source addresses
            // must not reset a source that already exhausted its answers.
            self.sources
                .retain(|_, (_, at)| now.duration_since(*at) < Duration::from_secs(1));
            if self.sources.len() >= ANSWER_SOURCES {
                return false;
            }
        }
        let (tokens, at) = self
            .sources
            .entry(ip)
            .or_insert((ANSWERS_PER_SOURCE_PER_SEC, now));
        *tokens = (*tokens + now.duration_since(*at).as_secs_f64() * ANSWERS_PER_SOURCE_PER_SEC)
            .min(ANSWERS_PER_SOURCE_PER_SEC);
        *at = now;
        if *tokens < 1.0 {
            return false;
        }
        *tokens -= 1.0;
        true
    }

    /// Answer, from `socket`, a linked device's probe that arrived from
    /// `from`.
    async fn answer(&mut self, socket: &UdpSocket, bytes: &[u8], from: SocketAddr) {
        if !self.active() || !answerable(from, &nearby::broadcast_destinations(nearby::NEARBY_PORT))
        {
            return;
        }
        let Some(probe) = linked::Probe::decode(bytes) else {
            return;
        };
        if self.answered_set.contains(probe.salt()) {
            return;
        }
        let Some(asker) = probe.sender(&mut self.keys, &self.config.peers) else {
            return;
        };
        if !self
            .freshness
            .accepts(&asker, probe.epoch(), synced_epoch())
        {
            tracing::debug!(peer = %short(&asker), "ignored a stale probe");
            return;
        }
        self.answered.push_back(*probe.salt());
        self.answered_set.insert(*probe.salt());
        while self.answered.len() > ANSWERED_SALTS {
            if let Some(old) = self.answered.pop_front() {
                self.answered_set.remove(&old);
            }
        }
        if self.freshness.record(&asker, probe.epoch()) {
            self.persist_freshness();
        }
        if !self.answer_budget(from.ip()) {
            return;
        }
        let addrs = endpoint_addrs(&self.endpoint);
        if addrs.is_empty() {
            return;
        }
        let Ok(reply) = linked::seal_reply(&mut self.keys, asker, probe.salt(), &addrs) else {
            return;
        };
        if socket.send_to(&reply, from).await.is_ok() {
            tracing::debug!(
                peer = %short(&asker),
                "answered a linked device asking on the local network"
            );
        }
    }

    /// A reply to one of this device's probes: dial the device it names.
    /// Returns `false` once the network has shut down.
    async fn found(&mut self, bytes: &[u8]) -> bool {
        let Some(salt) = linked::reply_salt(bytes) else {
            return true;
        };
        let now = Instant::now();
        let most_opens = self
            .config
            .peers
            .len()
            .min(linked::SLOTS)
            .saturating_add(OPENS_PER_PROBE_SLACK);
        let Some(pending) = self.pending.iter_mut().find(|pending| {
            pending.salt == salt && now.duration_since(pending.sent) < PROBE_LIFETIME
        }) else {
            return true;
        };
        if !self.config.enabled || pending.opened >= most_opens {
            return true;
        }
        pending.opened = pending.opened.saturating_add(1);
        let Some((device, addrs)) =
            linked::open_reply(&mut self.keys, bytes, &salt, &self.config.peers)
        else {
            return true;
        };
        let addrs: Vec<SocketAddr> = addrs
            .into_iter()
            .filter(|addr| local_network(addr.ip()))
            .collect();
        let Ok(id) = iroh::EndpointId::from_bytes(&device) else {
            return true;
        };
        if addrs.is_empty()
            || self.dialled.get(&device).is_some_and(|(known, at)| {
                *known == addrs && now.duration_since(*at) < REDIAL_INTERVAL
            })
        {
            return true;
        }
        tracing::info!(
            peer = %id.fmt_short(),
            addrs = ?addrs,
            since_start_ms = now.duration_since(self.started).as_millis(),
            "found a linked device on the local network"
        );
        self.dialled.insert(device, (addrs.clone(), now));
        let addr =
            iroh::EndpointAddr::from_parts(id, addrs.into_iter().map(iroh::TransportAddr::Ip));
        self.cmd_tx.send(NetCmd::Connect { addr }).await.is_ok()
    }

    fn apply(&mut self, config: LanDiscoveryConfig) {
        self.keys.retain(&config.peers);
        self.dialled
            .retain(|device, _| config.peers.contains(device));
        if self.freshness.retain(&config.peers) {
            self.persist_freshness();
        }
        if config.enabled && !self.config.enabled {
            tracing::info!("linked-device discovery on the local network started");
        } else if !config.enabled && self.config.enabled {
            tracing::info!("linked-device discovery on the local network stopped");
        }
        self.config = config;
    }
}

/// Run linked-device discovery on the current tokio runtime until the
/// returned sender is dropped: while the config is enabled, ask and answer
/// the devices it names, and dial each one found through `cmd_tx`. The
/// newest epoch accepted from each device is remembered at
/// `freshness_path`.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the runner lives for the whole discovery loop; it is used on every iteration"
)]
pub fn spawn(
    endpoint: iroh::Endpoint,
    device: &ed25519_dalek::SigningKey,
    freshness_path: Option<PathBuf>,
    cmd_tx: mpsc::Sender<NetCmd>,
) -> watch::Sender<LanDiscoveryConfig> {
    let (config_tx, mut config_rx) = watch::channel(LanDiscoveryConfig::default());
    let keys = linked::LinkKeys::new(device);
    tokio::spawn(async move {
        // Probes leave from, and replies return to, a socket of this
        // session's own, opened afresh on every new network so its port does
        // not follow the device around. Answers leave from another socket
        // that cannot broadcast. Probes from other devices arrive on the
        // shared rendezvous port.
        let (Some(mut asking), Some(answering)) = (bind_ephemeral(true), bind_ephemeral(false))
        else {
            tracing::warn!("linked-device discovery could not open its sockets");
            return;
        };
        let listening = tokio_socket(nearby::bind_listener(nearby::NEARBY_PORT));
        if listening.is_none() {
            tracing::warn!(
                port = nearby::NEARBY_PORT,
                "linked-device discovery cannot listen; it can still find devices that listen"
            );
        }
        let mut runner = Runner::new(endpoint, keys, cmd_tx, freshness_path);
        let mut interval = PROBE_FIRST_INTERVAL;
        let mut next_probe = Instant::now();
        let mut network = (Vec::new(), Vec::new());
        let mut asking_buf = vec![0_u8; nearby::MAX_PACKET];
        let mut listening_buf = vec![0_u8; nearby::MAX_PACKET];
        loop {
            let config = config_rx.borrow_and_update().clone();
            if config != runner.config {
                runner.apply(config);
                interval = PROBE_FIRST_INTERVAL;
                next_probe = Instant::now();
            }
            // A new interface or address is a new network to ask on.
            let current = (
                nearby::broadcast_destinations(nearby::NEARBY_PORT),
                endpoint_addrs(&runner.endpoint),
            );
            if current != network {
                if !network.1.is_empty()
                    && let Some(fresh) = bind_ephemeral(true)
                {
                    asking = fresh;
                    runner.pending.clear();
                }
                network = current;
                interval = PROBE_FIRST_INTERVAL;
                next_probe = Instant::now();
            }
            let now = Instant::now();
            if runner.active() && now >= next_probe && !network.1.is_empty() {
                runner.probe(&asking).await;
                next_probe = now.checked_add(interval).unwrap_or(now);
                interval = interval.saturating_mul(2).min(PROBE_MAX_INTERVAL);
            }
            let wake = next_probe
                .saturating_duration_since(Instant::now())
                .clamp(Duration::from_millis(50), POLL);
            tokio::select! {
                changed = config_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                () = tokio::time::sleep(wake) => {}
                received = asking.recv_from(&mut asking_buf) => {
                    if let Ok((len, _)) = received
                        && let Some(bytes) = asking_buf.get(..len)
                        && !runner.found(bytes).await
                    {
                        return;
                    }
                }
                received = async {
                    match &listening {
                        Some(socket) => socket.recv_from(&mut listening_buf).await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Ok((len, from)) = received
                        && let Some(bytes) = listening_buf.get(..len)
                    {
                        runner.answer(&answering, bytes, from).await;
                    }
                }
            }
        }
    });
    config_tx
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn discovery_for_test() -> (Runner, mpsc::Receiver<NetCmd>, ed25519_dalek::SigningKey) {
        let local = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let peer = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&local.to_bytes()))
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let (sender, receiver) = mpsc::channel(8);
        let mut runner = Runner::new(endpoint, linked::LinkKeys::new(&local), sender, None);
        runner.apply(LanDiscoveryConfig {
            enabled: true,
            peers: BTreeSet::from([peer.verifying_key().to_bytes()]),
        });
        (runner, receiver, peer)
    }

    fn reply_for_test(
        runner: &mut Runner,
        peer: &ed25519_dalek::SigningKey,
        salt: u8,
        address: &str,
    ) -> Vec<u8> {
        runner.pending.push_back(PendingProbe {
            salt: [salt; 16],
            sent: Instant::now(),
            opened: 0,
        });
        linked::seal_reply(
            &mut linked::LinkKeys::new(peer),
            *runner.keys.local(),
            &[salt; 16],
            &[address.parse().unwrap()],
        )
        .unwrap()
    }

    #[tokio::test]
    async fn replies_enqueue_only_fresh_authenticated_local_connections() {
        let (mut runner, mut commands, peer) = discovery_for_test().await;
        let reply = reply_for_test(&mut runner, &peer, 1, "10.0.0.2:4433");
        assert!(runner.found(&reply).await);
        let NetCmd::Connect { addr } = commands.try_recv().unwrap() else {
            panic!("discovery must enqueue the production connection command");
        };
        assert_eq!(*addr.id.as_bytes(), peer.verifying_key().to_bytes());
        assert_eq!(
            addr.ip_addrs().copied().collect::<Vec<_>>(),
            vec!["10.0.0.2:4433".parse().unwrap()]
        );
        assert!(runner.found(&reply).await);
        assert!(commands.try_recv().is_err(), "duplicates must not redial");

        let public = reply_for_test(&mut runner, &peer, 2, "8.8.8.8:4433");
        let stale = reply_for_test(&mut runner, &peer, 3, "10.0.0.3:4433");
        runner.pending.back_mut().unwrap().sent =
            Instant::now().checked_sub(PROBE_LIFETIME).unwrap();
        let unknown = reply_for_test(&mut runner, &peer, 4, "10.0.0.4:4433");
        runner.pending.pop_back();
        let mut forged = reply_for_test(&mut runner, &peer, 5, "10.0.0.5:4433");
        *forged.last_mut().unwrap() ^= 1;
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let untrusted = reply_for_test(&mut runner, &stranger, 6, "10.0.0.6:4433");
        for refused in [public, stale, unknown, forged, untrusted] {
            assert!(runner.found(&refused).await);
            assert!(commands.try_recv().is_err());
        }
        runner.endpoint.close().await;
    }

    #[tokio::test]
    async fn config_changes_and_open_budget_refuse_pending_replies() {
        let (mut runner, mut commands, peer) = discovery_for_test().await;
        let reply = reply_for_test(&mut runner, &peer, 1, "10.0.0.2:4433");
        runner.apply(LanDiscoveryConfig::default());
        assert!(runner.found(&reply).await);
        assert!(
            commands.try_recv().is_err(),
            "disabling stops pending replies"
        );
        runner.apply(LanDiscoveryConfig {
            enabled: true,
            peers: BTreeSet::new(),
        });
        assert!(runner.found(&reply).await);
        assert!(commands.try_recv().is_err(), "a removed peer stays refused");
        runner.apply(LanDiscoveryConfig {
            enabled: true,
            peers: BTreeSet::from([peer.verifying_key().to_bytes()]),
        });
        runner.pending.front_mut().unwrap().opened = 1 + OPENS_PER_PROBE_SLACK;
        assert!(runner.found(&reply).await);
        assert!(
            commands.try_recv().is_err(),
            "exhausted probes stop opening replies"
        );
        runner.endpoint.close().await;
    }

    #[tokio::test]
    async fn discovery_reports_closed_network_and_bounds_answer_work() {
        let (mut runner, commands, peer) = discovery_for_test().await;
        let source: IpAddr = "10.0.0.1".parse().unwrap();
        for _ in 0..4 {
            assert!(runner.answer_budget(source));
        }
        assert!(!runner.answer_budget(source));
        assert!(runner.answer_budget("10.0.0.2".parse().unwrap()));
        for suffix in 3..=256_u16 {
            let [high, low] = suffix.to_be_bytes();
            let ip = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, high, low));
            assert!(runner.answer_budget(ip));
        }
        assert!(!runner.answer_budget("10.0.2.1".parse().unwrap()));
        assert!(
            !runner.answer_budget(source),
            "new source addresses must not replenish exhausted budgets"
        );
        assert_eq!(runner.sources.len(), ANSWER_SOURCES);
        let reply = reply_for_test(&mut runner, &peer, 1, "10.0.0.2:4433");
        drop(commands);
        assert!(
            !runner.found(&reply).await,
            "network shutdown must end the runner"
        );
        runner.endpoint.close().await;
    }

    #[test]
    fn only_local_network_addresses_are_named() {
        for (ip, allowed) in [
            ("192.168.1.4", true),
            ("10.0.0.2", true),
            ("172.20.1.1", true),
            ("169.254.3.3", true),
            ("fd12::1", true),
            ("8.8.8.8", false),
            ("2001:db8::1", false),
            ("fe80::1", false),
            ("127.0.0.1", false),
        ] {
            assert_eq!(local_network(ip.parse().unwrap()), allowed, "{ip}");
        }
    }

    /// Answers go only to a unicast source, never to a broadcast address a
    /// forged probe names as its source.
    #[test]
    fn only_unicast_sources_are_answered() {
        let broadcasts = ["10.0.0.255:23831".parse().unwrap()];
        for (from, allowed) in [
            ("10.0.0.7:40000", true),
            ("10.0.0.255:40000", false),
            ("255.255.255.255:40000", false),
            ("224.0.0.251:5353", false),
            ("0.0.0.0:40000", false),
            ("10.0.0.7:0", false),
            ("[fd00::1]:40000", true),
            ("[ff02::1]:40000", false),
        ] {
            assert_eq!(
                answerable(from.parse().unwrap(), &broadcasts),
                allowed,
                "{from}"
            );
        }
    }
}
