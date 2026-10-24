//! Two endpoints on loopback, the exit's TUN and the client's both
//! in-memory: who the exit lets in, and what crosses.

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, EndpointId};
use tokio::sync::mpsc;

use super::ALPN;
use super::client::{self, ClientError, Ended, Refusal};
use super::device::testing::{ChannelDevice, Stack, channel_device};
use super::dns::DnsRelay;
use super::exit::{Authority, Decision, EXIT_MTU, Exit, ExitDevices};
use crate::net::{GATEWAY, Ipv4Net};
use crate::packet::{self, Ipv4Packet};

const WAIT: Duration = Duration::from_secs(20);

#[derive(Default)]
struct Person {
    devices: Mutex<HashSet<EndpointId>>,
    exit_on: AtomicBool,
}

#[derive(Clone, Default)]
struct TestAuthority(Arc<Person>);

impl Authority for TestAuthority {
    fn decide(&self, peer: &EndpointId) -> Decision {
        if !self.0.devices.lock().unwrap().contains(peer) {
            Decision::NotYours
        } else if !self.0.exit_on.load(Ordering::SeqCst) {
            Decision::ExitOff
        } else {
            Decision::Allow
        }
    }

    fn home_networks(&self) -> Vec<Ipv4Net> {
        vec![
            "192.168.1.0/24".parse().unwrap(),
            // Never offered: the tunnel's own network.
            "100.100.64.0/24".parse().unwrap(),
        ]
    }
}

/// Exit TUNs as in-memory devices; each one opened hands its stack to the
/// test.
struct TestDevices {
    stacks: mpsc::UnboundedSender<Stack>,
    opened: Arc<AtomicUsize>,
    closes: mpsc::UnboundedSender<()>,
}

impl ExitDevices for TestDevices {
    type Device = ChannelDevice;

    async fn open(&self, mtu: u16) -> std::io::Result<ChannelDevice> {
        assert_eq!(mtu, EXIT_MTU);
        let (device, stack) = channel_device();
        self.opened.fetch_add(1, Ordering::SeqCst);
        let _ = self.stacks.send(stack);
        Ok(device)
    }

    async fn close(&self) {
        let _ = self.closes.send(());
    }
}

async fn endpoint() -> Endpoint {
    Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

struct Pair {
    person: Arc<Person>,
    exit: Exit<TestAuthority, TestDevices>,
    exit_stacks: mpsc::UnboundedReceiver<Stack>,
    opened: Arc<AtomicUsize>,
    closes: mpsc::UnboundedReceiver<()>,
    changes: Arc<AtomicUsize>,
    router: iroh::protocol::Router,
    exit_addr: EndpointAddr,
    client: Endpoint,
}

impl Pair {
    async fn new(dns: DnsRelay) -> Self {
        let person = Arc::new(Person::default());
        let (stacks, exit_stacks) = mpsc::unbounded_channel();
        let opened = Arc::new(AtomicUsize::new(0));
        let (close_tx, closes) = mpsc::unbounded_channel();
        let changes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&changes);
        let exit = Exit::new(
            TestAuthority(Arc::clone(&person)),
            TestDevices {
                stacks,
                opened: Arc::clone(&opened),
                closes: close_tx,
            },
            dns,
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
            },
        );
        let server = endpoint().await;
        let exit_addr = EndpointAddr::new(server.id()).with_ip_addr(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            server.bound_sockets().first().unwrap().port(),
        )));
        let router = iroh::protocol::Router::builder(server)
            .accept(ALPN, exit.clone())
            .spawn();
        Self {
            person,
            exit,
            exit_stacks,
            opened,
            closes,
            changes,
            router,
            exit_addr,
            client: endpoint().await,
        }
    }

    fn enrol_client(&self) {
        self.person.devices.lock().unwrap().insert(self.client.id());
    }

    fn turn_exit(&self, on: bool) {
        self.person.exit_on.store(on, Ordering::SeqCst);
    }

    async fn shutdown(self) {
        self.client.close().await;
        self.router.shutdown().await.unwrap();
    }
}

fn unused_dns() -> DnsRelay {
    DnsRelay::new(SocketAddr::from((Ipv4Addr::LOCALHOST, 9)))
}

async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, future)
        .await
        .expect("within the test's bound")
}

fn udp(from: SocketAddrV4, to: SocketAddrV4, payload: &[u8]) -> Vec<u8> {
    packet::udp_packet(from, to, payload).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_persons_device_is_refused_and_nothing_opens() {
    let pair = Pair::new(unused_dns()).await;
    pair.turn_exit(true);
    // The client is not among the person's devices.
    let refused = within(client::connect(&pair.client, pair.exit_addr.clone())).await;
    assert_eq!(refused.err(), Some(ClientError::Refused(Refusal::NotYours)));
    let probed = within(client::probe(&pair.client, pair.exit_addr.clone())).await;
    assert_eq!(probed.err(), Some(ClientError::Refused(Refusal::NotYours)));
    assert_eq!(pair.opened.load(Ordering::SeqCst), 0);
    assert!(pair.exit.sessions().is_empty());
    pair.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_persons_device_is_refused_while_the_exit_is_off() {
    let pair = Pair::new(unused_dns()).await;
    pair.enrol_client();
    let refused = within(client::connect(&pair.client, pair.exit_addr.clone())).await;
    assert_eq!(refused.err(), Some(ClientError::Refused(Refusal::ExitOff)));
    assert_eq!(pair.opened.load(Ordering::SeqCst), 0);
    pair.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_probe_learns_the_offer_without_routing() {
    let pair = Pair::new(unused_dns()).await;
    pair.enrol_client();
    pair.turn_exit(true);
    let offer = within(client::probe(&pair.client, pair.exit_addr.clone()))
        .await
        .unwrap();
    assert_eq!(offer.gateway, GATEWAY);
    assert_eq!(offer.address, None);
    assert_eq!(offer.mtu, EXIT_MTU);
    assert_eq!(offer.home_networks, vec!["192.168.1.0/24".parse().unwrap()]);
    assert_eq!(pair.opened.load(Ordering::SeqCst), 0);
    pair.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn packets_cross_both_ways_and_stop_ends_the_tunnel() {
    let mut pair = Pair::new(unused_dns()).await;
    pair.enrol_client();
    pair.turn_exit(true);
    let tunnel = within(client::connect(&pair.client, pair.exit_addr.clone()))
        .await
        .unwrap();
    let address = tunnel.address;
    assert_eq!(address, Ipv4Addr::new(100, 100, 64, 2));
    assert!(tunnel.mtu() <= EXIT_MTU && tunnel.mtu() >= crate::net::MIN_MTU);
    let mut exit_stack = within(pair.exit_stacks.recv()).await.unwrap();

    let (device, mut client_stack) = channel_device();
    let tunnel = Arc::new(tunnel);
    let running = tokio::spawn({
        let tunnel = Arc::clone(&tunnel);
        async move { client::run(&tunnel, Arc::new(device)).await }
    });

    let web = SocketAddrV4::new(Ipv4Addr::new(198, 51, 100, 7), 443);
    let local = SocketAddrV4::new(address, 50_000);
    let outward = udp(local, web, b"hello");
    client_stack.inject(outward.clone()).await;
    assert_eq!(within(exit_stack.delivered()).await.unwrap(), outward);

    // Traffic claiming another source never leaves the client.
    let spoofed = udp(
        SocketAddrV4::new(Ipv4Addr::new(100, 100, 64, 9), 1),
        web,
        b"spoof",
    );
    client_stack.inject(spoofed).await;

    let inward = udp(web, local, b"world");
    exit_stack.inject(inward.clone()).await;
    assert_eq!(within(client_stack.delivered()).await.unwrap(), inward);

    // The spoofed packet was dropped: the next packet the exit sees is this.
    let second = udp(local, web, b"again");
    client_stack.inject(second.clone()).await;
    assert_eq!(within(exit_stack.delivered()).await.unwrap(), second);

    let sessions = pair.exit.sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].peer, pair.client.id());
    assert_eq!(sessions[0].address, address);
    assert!(pair.changes.load(Ordering::SeqCst) >= 1);

    pair.exit.stop(&pair.client.id());
    assert_eq!(
        within(running).await.unwrap(),
        Ended::Refused(Refusal::Stopped)
    );
    // The last session gone, the exit TUN closes.
    within(pair.closes.recv()).await.unwrap();
    assert!(pair.exit.sessions().is_empty());
    pair.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turning_the_exit_off_ends_a_running_tunnel() {
    let mut pair = Pair::new(unused_dns()).await;
    pair.enrol_client();
    pair.turn_exit(true);
    let tunnel = within(client::connect(&pair.client, pair.exit_addr.clone()))
        .await
        .unwrap();
    let _exit_stack = within(pair.exit_stacks.recv()).await.unwrap();
    let (device, _client_stack) = channel_device();
    let tunnel = Arc::new(tunnel);
    let running = tokio::spawn({
        let tunnel = Arc::clone(&tunnel);
        async move { client::run(&tunnel, Arc::new(device)).await }
    });
    pair.turn_exit(false);
    pair.exit.recheck();
    assert_eq!(
        within(running).await.unwrap(),
        Ended::Refused(Refusal::ExitOff)
    );
    pair.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_exit_answers_dns_sent_to_the_gateway() {
    let resolver = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let resolver_addr = resolver.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0_u8; 512];
        let (length, from) = resolver.recv_from(&mut buf).await.unwrap();
        let mut answer = buf[..length].to_vec();
        answer.extend_from_slice(b"=192.168.1.20");
        resolver.send_to(&answer, from).await.unwrap();
    });
    let mut pair = Pair::new(DnsRelay::new(resolver_addr)).await;
    pair.enrol_client();
    pair.turn_exit(true);
    let tunnel = Arc::new(
        within(client::connect(&pair.client, pair.exit_addr.clone()))
            .await
            .unwrap(),
    );
    let _exit_stack = within(pair.exit_stacks.recv()).await.unwrap();
    let (device, mut client_stack) = channel_device();
    tokio::spawn({
        let tunnel = Arc::clone(&tunnel);
        async move { client::run(&tunnel, Arc::new(device)).await }
    });
    let local = SocketAddrV4::new(tunnel.address, 53_000);
    let gateway = SocketAddrV4::new(GATEWAY, super::dns::PORT);
    client_stack
        .inject(udp(local, gateway, b"\x01\x02nas.home"))
        .await;
    let answer = within(client_stack.delivered()).await.unwrap();
    let answer = Ipv4Packet::parse(&answer).unwrap();
    assert_eq!(answer.source(), GATEWAY);
    assert_eq!(
        answer.udp(),
        Some((
            super::dns::PORT,
            53_000,
            &b"\x01\x02nas.home=192.168.1.20"[..]
        ))
    );
    tunnel.close();
    pair.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_packet_too_large_for_the_path_is_answered_with_the_size_that_fits() {
    let mut pair = Pair::new(unused_dns()).await;
    pair.enrol_client();
    pair.turn_exit(true);
    let tunnel = Arc::new(
        within(client::connect(&pair.client, pair.exit_addr.clone()))
            .await
            .unwrap(),
    );
    let _exit_stack = within(pair.exit_stacks.recv()).await.unwrap();
    let (device, mut client_stack) = channel_device();
    tokio::spawn({
        let tunnel = Arc::clone(&tunnel);
        async move { client::run(&tunnel, Arc::new(device)).await }
    });
    let fits = tunnel.connection.max_datagram_size().unwrap();
    // A full Ethernet-sized packet: more than any datagram the path carries.
    assert!(fits < 1500);
    let payload = vec![0_u8; 1500 - 28];
    let local = SocketAddrV4::new(tunnel.address, 50_000);
    let big = udp(
        local,
        SocketAddrV4::new(Ipv4Addr::new(198, 51, 100, 7), 9),
        &payload,
    );
    client_stack.inject(big).await;
    let icmp = within(client_stack.delivered()).await.unwrap();
    let icmp = Ipv4Packet::parse(&icmp).unwrap();
    assert_eq!(icmp.protocol(), 1);
    assert_eq!(icmp.destination(), tunnel.address);
    // ICMP type 3 code 4, with the next-hop MTU in bytes 6..8.
    let body = &icmp.bytes()[20..];
    assert_eq!((body[0], body[1]), (3, 4));
    // The size that fits now, which path MTU discovery may have raised
    // since: never the packet's own size, never below the IPv4 minimum.
    let mtu = u16::from_be_bytes([body[6], body[7]]);
    assert!(mtu < 1500, "{mtu} is smaller than the packet");
    assert!(mtu >= crate::net::MIN_MTU, "{mtu}");
    tunnel.close();
    pair.shutdown().await;
}
