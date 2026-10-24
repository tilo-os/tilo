//! DNS through the exit: the exit's shell answers UDP queries to the tunnel
//! gateway by asking its own resolver, so a client resolves what the exit
//! resolves, home network names included.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;

use crate::net::GATEWAY;
use crate::packet::{self, Ipv4Packet};

/// The DNS port.
pub const PORT: u16 = 53;
/// The most queries in flight at once, from all clients together.
const MAX_IN_FLIGHT: usize = 64;
/// How long a resolver may take to answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest answer relayed: the largest a UDP packet in the tunnel carries.
const MAX_ANSWER_BYTES: usize = 1400;

/// A query to the gateway's DNS port, taken apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub client: SocketAddrV4,
    pub payload: Vec<u8>,
}

impl Query {
    /// The DNS query `packet` carries, if it is a UDP datagram to the
    /// gateway's DNS port.
    #[must_use]
    pub fn of(packet: &Ipv4Packet<'_>) -> Option<Self> {
        if packet.destination() != GATEWAY {
            return None;
        }
        let (source, destination, payload) = packet.udp()?;
        (destination == PORT && !payload.is_empty()).then(|| Self {
            client: SocketAddrV4::new(packet.source(), source),
            payload: payload.to_vec(),
        })
    }
}

/// Asks this machine's resolver on behalf of the tunnel's clients.
#[derive(Clone)]
pub struct DnsRelay {
    resolver: SocketAddr,
    permits: Arc<Semaphore>,
}

impl DnsRelay {
    #[must_use]
    pub fn new(resolver: SocketAddr) -> Self {
        Self {
            resolver,
            permits: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        }
    }

    /// A relay to the first resolver `/etc/resolv.conf` names, or to the
    /// local stub when it names none.
    #[must_use]
    pub fn system() -> Self {
        let text = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
        Self::new(resolver_of(&text))
    }

    /// Answer `query`, then hand the answer packet to `reply`. Queries beyond
    /// the in-flight bound are dropped, as a busy resolver would drop them;
    /// the client asks again.
    pub fn answer(&self, query: Query, reply: impl FnOnce(Vec<u8>) + Send + 'static) {
        let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() else {
            return;
        };
        let resolver = self.resolver;
        tokio::spawn(async move {
            let _permit = permit;
            if let Some(answer) = ask(resolver, &query.payload).await
                && let Some(packet) =
                    packet::udp_packet(SocketAddrV4::new(GATEWAY, PORT), query.client, &answer)
            {
                reply(packet);
            }
        });
    }
}

async fn ask(resolver: SocketAddr, payload: &[u8]) -> Option<Vec<u8>> {
    let local: SocketAddr = match resolver {
        SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        SocketAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let socket = tokio::net::UdpSocket::bind(local).await.ok()?;
    socket.connect(resolver).await.ok()?;
    socket.send(payload).await.ok()?;
    let mut answer = vec![0_u8; MAX_ANSWER_BYTES];
    let length = tokio::time::timeout(ANSWER_TIMEOUT, socket.recv(&mut answer))
        .await
        .ok()?
        .ok()?;
    answer.truncate(length);
    // An answer must answer this query: same transaction id.
    (answer.get(..2) == payload.get(..2)).then_some(answer)
}

/// The first `nameserver` of a resolv.conf, or the local stub.
fn resolver_of(text: &str) -> SocketAddr {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .filter_map(|rest| rest.trim().parse::<std::net::IpAddr>().ok())
        .map(|address| SocketAddr::new(address, PORT))
        .next()
        .unwrap_or_else(|| SocketAddr::from((Ipv4Addr::LOCALHOST, PORT)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resolver_is_the_first_nameserver() {
        assert_eq!(
            resolver_of(
                "# NetworkManager\nsearch home\nnameserver 127.0.0.1\nnameserver 9.9.9.9\n"
            ),
            "127.0.0.1:53".parse().unwrap()
        );
        assert_eq!(resolver_of("nameserver ::1\n"), "[::1]:53".parse().unwrap());
        assert_eq!(resolver_of(""), "127.0.0.1:53".parse().unwrap());
    }

    #[test]
    fn only_udp_to_the_gateways_dns_port_is_a_query() {
        let client = SocketAddrV4::new(Ipv4Addr::new(100, 100, 64, 2), 40_000);
        let query =
            packet::udp_packet(client, SocketAddrV4::new(GATEWAY, PORT), b"\x12\x34q").unwrap();
        assert_eq!(
            Query::of(&Ipv4Packet::parse(&query).unwrap()),
            Some(Query {
                client,
                payload: b"\x12\x34q".to_vec()
            })
        );
        let elsewhere =
            packet::udp_packet(client, "9.9.9.9:53".parse().unwrap(), b"\x12\x34q").unwrap();
        assert_eq!(Query::of(&Ipv4Packet::parse(&elsewhere).unwrap()), None);
        let other_port =
            packet::udp_packet(client, SocketAddrV4::new(GATEWAY, 5353), b"\x12\x34q").unwrap();
        assert_eq!(Query::of(&Ipv4Packet::parse(&other_port).unwrap()), None);
    }

    #[tokio::test]
    async fn a_query_is_answered_from_the_resolver() {
        let resolver = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = resolver.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0_u8; 512];
            let (length, from) = resolver.recv_from(&mut buf).await.unwrap();
            let mut answer = buf[..length].to_vec();
            answer.extend_from_slice(b" answered");
            resolver.send_to(&answer, from).await.unwrap();
        });
        let relay = DnsRelay::new(address);
        let client = SocketAddrV4::new(Ipv4Addr::new(100, 100, 64, 2), 40_000);
        let (tx, rx) = tokio::sync::oneshot::channel();
        relay.answer(
            Query {
                client,
                payload: b"\xab\xcdquery".to_vec(),
            },
            move |packet| {
                let _ = tx.send(packet);
            },
        );
        let packet = rx.await.unwrap();
        let parsed = Ipv4Packet::parse(&packet).unwrap();
        assert_eq!(parsed.source(), GATEWAY);
        assert_eq!(parsed.destination(), *client.ip());
        assert_eq!(
            parsed.udp(),
            Some((PORT, 40_000, &b"\xab\xcdquery answered"[..]))
        );
    }
}
