//! The session's side of "linked device, where are you?" on the nearby
//! rendezvous ([`tilo_relay::nearby::linked`] documents the exchange and its
//! privacy properties): ask the person's linked devices on the local network
//! for their endpoint addresses, answer them when they ask, and dial each
//! device found through the same path as the enrolled direct-peer list.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use tilo_relay::nearby::{self, linked};
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
/// A device is answered at most this often.
const ANSWER_INTERVAL: Duration = Duration::from_millis(500);
/// A device found again at the same addresses is dialled at most this often.
const REDIAL_INTERVAL: Duration = Duration::from_secs(20);
/// How long after a dial the path in use is reported.
const PATH_REPORT_DELAY: Duration = Duration::from_secs(3);

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

fn endpoint_addrs(endpoint: &iroh::Endpoint) -> Vec<SocketAddr> {
    endpoint
        .addr()
        .ip_addrs()
        .copied()
        .filter(|addr| local_network(addr.ip()))
        .collect()
}

fn now_epoch() -> u32 {
    linked::epoch_at(tilo_clock::unix_now())
}

fn tokio_socket(socket: std::io::Result<std::net::UdpSocket>) -> Option<UdpSocket> {
    let socket = socket.ok()?;
    socket.set_nonblocking(true).ok()?;
    UdpSocket::from_std(socket).ok()
}

fn short(device: &[u8; 32]) -> String {
    hex::encode(device.get(..5).unwrap_or_default())
}

struct Runner {
    endpoint: iroh::Endpoint,
    keys: linked::LinkKeys,
    cmd_tx: mpsc::Sender<NetCmd>,
    config: LanDiscoveryConfig,
    pending: VecDeque<(linked::Salt, Instant)>,
    answered: HashMap<[u8; 32], Instant>,
    dialled: HashMap<[u8; 32], (Vec<SocketAddr>, Instant)>,
    started: Instant,
}

impl Runner {
    fn active(&self) -> bool {
        self.config.enabled && !self.config.peers.is_empty()
    }

    async fn probe(&mut self, socket: &UdpSocket) {
        let Ok(probe) = linked::Probe::build(&mut self.keys, &self.config.peers, now_epoch())
        else {
            return;
        };
        let bytes = probe.encode();
        let mut sent = false;
        for destination in nearby::broadcast_destinations(nearby::NEARBY_PORT) {
            sent |= socket.send_to(&bytes, destination).await.is_ok();
        }
        if sent {
            self.pending.push_back((*probe.salt(), Instant::now()));
            while self.pending.len() > PENDING_PROBES {
                self.pending.pop_front();
            }
        }
    }

    /// Answer a linked device's probe that arrived from `from`.
    async fn answer(&mut self, socket: &UdpSocket, bytes: &[u8], from: SocketAddr) {
        if !self.active() {
            return;
        }
        let Some(probe) = linked::Probe::decode(bytes) else {
            return;
        };
        let now_epoch = linked::clock_is_synchronized().then(now_epoch);
        let Some(asker) = probe.sender(&mut self.keys, &self.config.peers, now_epoch) else {
            return;
        };
        let now = Instant::now();
        if self
            .answered
            .get(&asker)
            .is_some_and(|at| now.duration_since(*at) < ANSWER_INTERVAL)
        {
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
            self.answered.insert(asker, now);
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
        let fresh = self
            .pending
            .iter()
            .any(|(sent, at)| *sent == salt && now.duration_since(*at) < PROBE_LIFETIME);
        if !fresh || !self.active() {
            return true;
        }
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
        if self.cmd_tx.send(NetCmd::Connect { addr }).await.is_err() {
            return false;
        }
        self.report_path(id);
        true
    }

    /// Say, once the connection has had a moment to settle, which of the
    /// device's addresses the endpoint is using: a local-network address or
    /// a relay.
    fn report_path(&self, id: iroh::EndpointId) {
        let endpoint = self.endpoint.clone();
        tokio::spawn(async move {
            tokio::time::sleep(PATH_REPORT_DELAY).await;
            let Some(info) = endpoint.remote_info(id).await else {
                return;
            };
            let active: Vec<String> = info
                .addrs()
                .filter(|addr| matches!(addr.usage(), iroh::endpoint::TransportAddrUsage::Active))
                .map(|addr| match addr.addr() {
                    iroh::TransportAddr::Ip(ip) => format!("ip {ip}"),
                    iroh::TransportAddr::Relay(url) => format!("relay {url}"),
                    other => format!("{other:?}"),
                })
                .collect();
            tracing::info!(peer = %id.fmt_short(), ?active, "linked device path in use");
        });
    }

    fn apply(&mut self, config: LanDiscoveryConfig) {
        self.keys.retain(&config.peers);
        self.dialled
            .retain(|device, _| config.peers.contains(device));
        self.answered
            .retain(|device, _| config.peers.contains(device));
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
/// the devices it names, and dial each one found through `cmd_tx`.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the runner lives for the whole discovery loop; it is used on every iteration"
)]
pub fn spawn(
    endpoint: iroh::Endpoint,
    device: &ed25519_dalek::SigningKey,
    cmd_tx: mpsc::Sender<NetCmd>,
) -> watch::Sender<LanDiscoveryConfig> {
    let (config_tx, mut config_rx) = watch::channel(LanDiscoveryConfig::default());
    let keys = linked::LinkKeys::new(device);
    tokio::spawn(async move {
        // Probes leave from, and replies return to, a socket of this
        // session's own; probes from other devices arrive on the shared
        // rendezvous port.
        let Some(asking) = tokio_socket(std::net::UdpSocket::bind((
            std::net::Ipv4Addr::UNSPECIFIED,
            0,
        )))
        .filter(|socket| socket.set_broadcast(true).is_ok()) else {
            tracing::warn!("linked-device discovery could not open its socket");
            return;
        };
        let listening = tokio_socket(nearby::bind_listener(nearby::NEARBY_PORT));
        if listening.is_none() {
            tracing::warn!(
                port = nearby::NEARBY_PORT,
                "linked-device discovery cannot listen; it can still find devices that listen"
            );
        }
        let mut runner = Runner {
            endpoint,
            keys,
            cmd_tx,
            config: LanDiscoveryConfig::default(),
            pending: VecDeque::new(),
            answered: HashMap::new(),
            dialled: HashMap::new(),
            started: Instant::now(),
        };
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
                        runner.answer(&asking, bytes, from).await;
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
}
