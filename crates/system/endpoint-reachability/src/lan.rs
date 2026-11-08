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
//!
//! It is said and heard on every network the device is on, not only the
//! one its default route leaves by: a box with a VPN, a container bridge or
//! a second network card has its default route somewhere its other devices
//! are not, and a multicast sent and joined the default way goes out there
//! alone. The networks are the device's own IPv4 addresses, which are what
//! the endpoint hands this lookup to advertise ([`AddressLookup::publish`]):
//! as they change, the lookup is said on the new ones and no longer on those
//! gone.

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

/// The multicast service Tilo endpoints advertise under.
pub const SERVICE_NAME: &str = "tilo-v1";

/// What the lookup's results say they came from.
const PROVENANCE: &str = "tilo-lan";

/// How long a lookup of a key nobody has advertised yet listens for it.
const LOOKUP_FOR: Duration = Duration::from_secs(10);

/// The local-network lookup a session's endpoint takes beside number 0's:
/// it advertises this endpoint's direct addresses on every network it is on
/// and resolves another device's key to the address it advertises.
#[must_use]
pub fn lan() -> Lan {
    Lan
}

/// Builds the [`LanLookup`] an endpoint runs.
#[derive(Clone, Copy, Debug, Default)]
pub struct Lan;

impl AddressLookupBuilder for Lan {
    fn into_address_lookup(
        self,
        endpoint: &Endpoint,
    ) -> Result<impl AddressLookup, AddressLookupBuilderError> {
        LanLookup::spawn(endpoint.id())
    }
}

/// The endpoints heard on the local network, and this one's announcement.
pub struct LanLookup {
    discoverer: DropGuard,
    shared: Arc<Shared>,
    /// The networks this endpoint is said on, by its address on each.
    networks: Mutex<BTreeSet<Ipv4Addr>>,
}

struct Shared {
    /// Every endpoint heard now, with what it advertises.
    heard: Mutex<HashMap<EndpointId, EndpointData>>,
    /// Each endpoint as it is heard, for the lookups waiting on it.
    news: broadcast::Sender<(EndpointId, EndpointData)>,
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

/// What `peer` advertises, as a lookup answers it.
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
    fn spawn(id: EndpointId) -> Result<Self, AddressLookupBuilderError> {
        let (news, _) = broadcast::channel(64);
        let shared = Arc::new(Shared {
            heard: Mutex::default(),
            news,
        });
        let hearing = Arc::clone(&shared);
        let discoverer = Discoverer::new_interactive(SERVICE_NAME.to_owned(), peer_name(&id))
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
                    hearing.heard.lock_recover().remove(&from);
                    return;
                }
                let data = advertised(peer);
                let changed =
                    hearing.heard.lock_recover().insert(from, data.clone()) != Some(data.clone());
                if changed {
                    tracing::debug!(%from, ?data, "lan: heard an endpoint");
                    let _ = hearing.news.send((from, data));
                }
            })
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

    use super::{endpoint_of, lan, networks, peer_name};

    const ALPN: &[u8] = b"tilo/lan-discovery-test";

    /// Two endpoints on this network find each other by key alone, with no
    /// relay and no directory: only what the local lookup hears.
    #[tokio::test(flavor = "multi_thread")]
    async fn two_endpoints_find_each_other_on_the_local_network() {
        let binding = || {
            Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .alpns(vec![ALPN.to_vec()])
                .address_lookup(lan())
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

    /// The lookup is said on each network the device has an IPv4 address
    /// on, and on no loopback.
    #[test]
    fn the_lookup_is_said_on_every_network_the_device_is_on() {
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

    /// A peer's advertised name reads back as its endpoint, and nothing
    /// else reads as one.
    #[test]
    fn a_peer_is_named_by_its_key() {
        let id = SecretKey::from_bytes(&[7; 32]).public();
        assert_eq!(endpoint_of(&peer_name(&id)), Some(id));
        assert_eq!(endpoint_of("not-a-key"), None);
    }
}
