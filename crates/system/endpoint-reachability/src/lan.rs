//! Finding the person's devices on the local network, without a relay.
//!
//! A Tilo endpoint dials a peer by its key. Number 0's directory answers
//! that over the internet, and only while this endpoint has a home relay to
//! be found at; a device whose relays rest, or a network without the
//! internet, leaves two of the person's devices holding each other's keys
//! and no way to turn one into an address. So the session endpoint also
//! advertises its direct addresses on the local network under Tilo's own
//! service name and answers a lookup of any key it hears advertised there.
//!
//! The service name is Tilo's, not iroh's, so only another Tilo endpoint
//! answers; direct addresses only, since a relay advertised here would need
//! the internet this lookup exists without. What crosses the network is the
//! key and the addresses — the same pair a directory record holds — and the
//! connection it leads to is authenticated as every iroh connection is.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::address_lookup::{
    AddrFilter, AddressLookup, AddressLookupBuilder, AddressLookupBuilderError, EndpointData,
    EndpointInfo, Error, Item,
};
use iroh::{Endpoint, EndpointId};
use n0_future::boxed::BoxStream;
use swarm_discovery::{Discoverer, DropGuard, IpClass, Peer};
use tilo_lock::MutexRecover as _;
use tokio::sync::broadcast;

pub const SERVICE_NAME: &str = "tilo-v1";

/// The networks a session's local discovery speaks on, as IPv4 addresses separated by commas,
/// when it is told some: everywhere the endpoint is otherwise.
pub const NETWORKS_ENV: &str = "TILO_LAN_NETWORKS";

const PROVENANCE: &str = "tilo-lan";

const LOOKUP_FOR: Duration = Duration::from_secs(10);

/// The endpoints this device hears on its local network, kept by the lookup
/// a session's endpoint takes ([`Nearby::lookup`]): what says which of the
/// person's devices are reachable here without a relay. Cloning shares it.
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

impl Nearby {
    /// Nothing heard yet; `changed` is told each time an endpoint is first
    /// heard or its announcement lapses, from the discovery's own thread.
    #[must_use]
    pub fn new(changed: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::hearing(changed, None)
    }

    /// Nothing heard yet, with this device's lookup speaking only on
    /// `networks` (IPv4), whatever networks the endpoint is on: a machine on
    /// more than one network can be told which one its person's other
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
    /// none: a machine on more than one network is told which one its person's other devices are
    /// on, and a test keeps a session's discovery on this machine's loopback instead of the
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

    fn hearing(changed: Arc<dyn Fn() + Send + Sync>, on: Option<Vec<Ipv4Addr>>) -> Self {
        let (news, _) = broadcast::channel(64);
        Self {
            shared: Arc::new(Shared {
                heard: Mutex::default(),
                news,
                changed,
                on,
            }),
        }
    }

    /// The lookup to bind the endpoint with: it advertises this endpoint's
    /// direct addresses on every network it is on, resolves another device's
    /// key to the address it advertises, and keeps what it heard here.
    #[must_use]
    pub fn lookup(&self) -> Lan {
        Lan {
            shared: Arc::clone(&self.shared),
        }
    }

    /// The endpoints heard on the local network now, this one aside. Only
    /// a key and its addresses are heard: whose device it is, the caller
    /// decides from what it already knows.
    #[must_use]
    pub fn heard(&self) -> BTreeSet<EndpointId> {
        self.shared.heard.lock_recover().keys().copied().collect()
    }
}

/// The local-network lookup of a [`Nearby`].
#[derive(Clone)]
pub struct Lan {
    shared: Arc<Shared>,
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
        LanLookup::spawn(endpoint.id(), self.shared)
    }
}

pub struct LanLookup {
    discoverer: DropGuard,
    shared: Arc<Shared>,
    networks: Mutex<BTreeSet<Ipv4Addr>>,
}

struct Shared {
    heard: Mutex<HashMap<EndpointId, EndpointData>>,
    news: broadcast::Sender<(EndpointId, EndpointData)>,
    changed: Arc<dyn Fn() + Send + Sync>,
    /// The networks its lookup speaks on, when it was told rather than
    /// taking them from what the endpoint publishes.
    on: Option<Vec<Ipv4Addr>>,
}

impl std::fmt::Debug for LanLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanLookup")
            .field("networks", &*self.networks.lock_recover())
            .finish_non_exhaustive()
    }
}

/// An endpoint id as the service names its peers: its key in lower-case
/// base32, as iroh's own local lookup names them.
fn peer_name(id: &EndpointId) -> String {
    data_encoding::BASE32_NOPAD
        .encode(id.as_bytes())
        .to_ascii_lowercase()
}

fn endpoint_of(name: &str) -> Option<EndpointId> {
    let bytes = data_encoding::BASE32_NOPAD
        .decode(name.to_ascii_uppercase().as_bytes())
        .ok()?;
    let key: [u8; 32] = bytes.try_into().ok()?;
    EndpointId::from_bytes(&key).ok()
}

fn advertised(peer: &Peer) -> EndpointData {
    EndpointData::from(
        peer.addrs()
            .iter()
            .map(|(ip, port)| SocketAddr::new(*ip, *port))
            .collect::<BTreeSet<_>>(),
    )
}

/// The device's own IPv4 addresses among `addrs`: one for each network it
/// is said on. Loopback is no network another device is on.
fn networks(addrs: &BTreeMap<u16, Vec<IpAddr>>) -> BTreeSet<Ipv4Addr> {
    addrs
        .values()
        .flatten()
        .filter_map(|ip| match ip {
            IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(*v4),
            _ => None,
        })
        .collect()
}

impl LanLookup {
    fn spawn(id: EndpointId, shared: Arc<Shared>) -> Result<Self, AddressLookupBuilderError> {
        let hearing = Arc::clone(&shared);
        let mut discoverer = Discoverer::new_interactive(SERVICE_NAME.to_owned(), peer_name(&id))
            .with_ip_class(IpClass::Auto)
            .with_callback(move |name: &str, peer: &Peer| {
                let Some(from) = endpoint_of(name) else {
                    tracing::debug!(name, "lan: an advertisement names no endpoint");
                    return;
                };
                if from == id {
                    return;
                }
                if peer.is_expiry() {
                    if hearing.heard.lock_recover().remove(&from).is_some() {
                        (hearing.changed)();
                    }
                    return;
                }
                let data = advertised(peer);
                let before = hearing.heard.lock_recover().insert(from, data.clone());
                if before.as_ref() != Some(&data) {
                    tracing::debug!(%from, ?data, "lan: heard an endpoint");
                    let _ = hearing.news.send((from, data));
                    if before.is_none() {
                        (hearing.changed)();
                    }
                }
            });
        if let Some(on) = &shared.on {
            // Only these networks are spoken on, and only over IPv4: the
            // machine may be on another network as well.
            discoverer = discoverer
                .with_ip_class(IpClass::V4Only)
                .with_multicast_interfaces_v4(on.clone());
        }
        let discoverer = discoverer
            .spawn(&tokio::runtime::Handle::current())
            .map_err(|error| AddressLookupBuilderError::from_err("tilo-lan", error))?;
        Ok(Self {
            discoverer,
            shared,
            networks: Mutex::default(),
        })
    }
}

impl AddressLookup for LanLookup {
    fn publish(&self, data: &EndpointData) {
        let data = data.apply_filter(&AddrFilter::ip_only()).into_owned();
        let mut by_port: BTreeMap<u16, Vec<IpAddr>> = BTreeMap::new();
        for addr in data.ip_addrs() {
            by_port.entry(addr.port()).or_default().push(addr.ip());
        }
        if self.shared.on.is_none() {
            // The networks this device's addresses say it is on, kept as the
            // endpoint's addresses come and go.
            let now = networks(&by_port);
            {
                let mut networks = self.networks.lock_recover();
                for gone in networks.difference(&now) {
                    self.discoverer.remove_interface_v4(*gone);
                }
                for new in now.difference(&networks) {
                    self.discoverer.add_interface_v4(*new);
                }
                *networks = now;
            }
        }
        self.discoverer.remove_all();
        for (port, ips) in by_port {
            self.discoverer.add(port, ips);
        }
    }

    fn resolve(&self, endpoint_id: EndpointId) -> Option<BoxStream<Result<Item, Error>>> {
        let item = move |data: EndpointData| {
            Item::new(
                EndpointInfo::from_parts(endpoint_id, data),
                PROVENANCE,
                None,
            )
        };
        let known = self
            .shared
            .heard
            .lock_recover()
            .get(&endpoint_id)
            .cloned()
            .map(item);
        let news = self.shared.news.subscribe();
        let now = tokio::time::Instant::now();
        let until = now.checked_add(LOOKUP_FOR).unwrap_or(now);
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
    use std::collections::BTreeMap;
    use std::net::{IpAddr, Ipv4Addr};

    use iroh::endpoint::presets;
    use iroh::{Endpoint, RelayMode, SecretKey};
    use tokio::time::{Duration, timeout};

    use super::{NETWORKS_ENV, Nearby, endpoint_of, networks, peer_name};

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

    const ALPN: &[u8] = b"tilo/lan-discovery-test";

    /// Two endpoints on this network find each other by key alone, with no
    /// relay and no directory: only what the local lookup hears. Both speak
    /// on this machine's loopback, so the test hears only its own two
    /// endpoints and needs no multicast on the network it runs on.
    #[tokio::test(flavor = "multi_thread")]
    async fn two_endpoints_find_each_other_on_the_local_network() {
        let binding = || {
            Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .alpns(vec![ALPN.to_vec()])
                .address_lookup(
                    Nearby::on(std::sync::Arc::new(|| {}), [Ipv4Addr::LOCALHOST]).lookup(),
                )
        };
        let host = binding().bind().await.expect("the host endpoint binds");
        let guest = binding().bind().await.expect("the guest endpoint binds");

        let accepting = tokio::spawn({
            let host = host.clone();
            async move {
                let incoming = host.accept().await.expect("the host hears a connection");
                let connection = incoming.await.expect("the connection authenticates");
                let (_, mut receive) = connection.accept_bi().await.expect("a stream");
                let said = receive.read_to_end(1024).await.expect("the stream reads");
                assert_eq!(said, b"hello");
            }
        });
        let connection = timeout(Duration::from_secs(30), guest.connect(host.id(), ALPN))
            .await
            .expect("found on the local network")
            .expect("dialed");
        let (mut send, _) = connection.open_bi().await.expect("a stream");
        send.write_all(b"hello").await.expect("sent");
        send.finish().expect("finished");
        timeout(Duration::from_secs(30), accepting)
            .await
            .expect("heard")
            .expect("the host's task");
        guest.close().await;
        host.close().await;
    }

    tilo_test_env::scenario! {
        fn the_local_lookup_names_its_networks_and_its_peers;

        phase the_lookup_is_said_on_every_network_the_device_is_on;
        // The lookup is said on each network the device has an IPv4 address
        // on, and on no loopback.
        {
            let addrs = BTreeMap::from([
                (
                    4711,
                    vec![
                        IpAddr::V4(Ipv4Addr::new(10, 0, 2, 15)),
                        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7)),
                        IpAddr::V4(Ipv4Addr::LOCALHOST),
                        "fe80::1".parse().unwrap(),
                    ],
                ),
                (4712, vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7))]),
            ]);
            assert_eq!(
                networks(&addrs).into_iter().collect::<Vec<_>>(),
                [Ipv4Addr::new(10, 0, 2, 15), Ipv4Addr::new(192, 168, 1, 7)]
            );
        }

        phase a_peer_is_named_by_its_key;
        // A peer's advertised name reads back as its endpoint, and nothing
        // else reads as one.
        {
            let id = SecretKey::from_bytes(&[7; 32]).public();
            assert_eq!(endpoint_of(&peer_name(&id)), Some(id));
            assert_eq!(endpoint_of("not-a-key"), None);
        }
    }
}
