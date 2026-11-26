//! The device VPN's peer-to-peer half, on the device's one endpoint: a route through another of
//! the person's devices leaves here, enters there, and carries packets both ways; this device's
//! exit answers the tunnel on its endpoint while a shell lends the TUN, and turns a device that is
//! not the person's away.
//!
//! The other device is an endpoint on this machine's loopback, serving the tunnel with an
//! in-memory stack; the shell is one of this daemon's shell links, handing a device-VPN link over
//! as the desktop session does.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use device_vpn::link::{self, Control, Frame};
use device_vpn::net::Ipv4Net;
use device_vpn::packet;
use device_vpn::tunnel::device::testing::{ChannelDevice, Stack, channel_device};
use device_vpn::tunnel::dns::DnsRelay;
use device_vpn::tunnel::{ALPN, Authority, Decision, Exit, ExitDevices, client, wire};
use iroh::{Endpoint, EndpointAddr};
use runtime_collab::ShellClient;

use crate::host;
use crate::host::Host;

/// Everyone may route through the other device: what is being tested here is the carriage, not
/// that device's policy (which is `device_vpn`'s own).
struct Everyones;

impl Authority for Everyones {
    fn decide(&self, _peer: &iroh::EndpointId) -> Decision {
        Decision::Allow
    }

    fn home_networks(&self) -> Vec<Ipv4Net> {
        vec!["192.168.1.0/24".parse().expect("a network")]
    }
}

/// The other device's tunnel, in memory: what the test's stack puts in and takes out.
struct Devices {
    stacks: tokio::sync::mpsc::UnboundedSender<Stack>,
}

impl ExitDevices for Devices {
    type Device = ChannelDevice;

    async fn open(&self, _mtu: u16) -> std::io::Result<ChannelDevice> {
        let (device, stack) = channel_device();
        let _ = self.stacks.send(stack);
        Ok(device)
    }

    async fn close(&self) {}
}

/// The other device, up on this machine's loopback and offering the network it is on.
struct OtherDevice {
    exit: Exit<Everyones, Devices>,
    stacks: tokio::sync::mpsc::UnboundedReceiver<Stack>,
    stack: Option<Stack>,
    router: iroh::protocol::Router,
    addr: EndpointAddr,
}

impl OtherDevice {
    async fn start() -> Self {
        let (stacks, receivers) = tokio::sync::mpsc::unbounded_channel();
        let exit = Exit::new(
            Everyones,
            Devices { stacks },
            DnsRelay::new(SocketAddr::from((Ipv4Addr::LOCALHOST, 9))),
            || {},
        );
        let server = Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback address")
            .bind()
            .await
            .expect("the other device's endpoint binds");
        let port = server
            .bound_sockets()
            .first()
            .expect("a bound socket")
            .port();
        let addr = EndpointAddr::new(server.id())
            .with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        let router = iroh::protocol::Router::builder(server)
            .accept(ALPN, exit.clone())
            .spawn();
        Self {
            exit,
            stacks: receivers,
            stack: None,
            router,
            addr,
        }
    }

    /// The next packet its stack received through the tunnel.
    async fn delivered(&mut self) -> Vec<u8> {
        if self.stack.is_none() {
            self.stack = Some(
                tokio::time::timeout(Duration::from_secs(10), self.stacks.recv())
                    .await
                    .expect("the other device opens its tunnel")
                    .expect("the other device opened its tunnel"),
            );
        }
        tokio::time::timeout(
            Duration::from_secs(10),
            self.stack.as_mut().unwrap().delivered(),
        )
        .await
        .expect("the packet reaches the other device")
        .expect("the other device keeps its stack")
    }
}

/// The shell's end of a device-VPN link, as the desktop session opens it.
async fn vpn_link(host: &Host) -> (tokio::net::UnixStream, ShellClient) {
    let link = host
        .daemon()
        .shell_link()
        .expect("the daemon hands its shell a link");
    link.set_nonblocking(true)
        .expect("the link is made nonblocking");
    let shell = ShellClient::open(tokio::net::UnixStream::from_std(link).expect("a socket")).0;
    let vpn = shell
        .device_vpn()
        .await
        .expect("the daemon serves the device VPN");
    (vpn, shell)
}

/// The person's route through another of their devices leaves this device's daemon, enters that
/// device by the tunnel, and carries their packets both ways; when that device stops it, the
/// route ends and says so. This device's exit, lent by the shell, answers the tunnel on this
/// device's endpoint, and a device that is not the person's is turned away.
#[test]
fn the_daemon_carries_the_route_and_answers_this_devices_exit() {
    host::run(|host| async move {
        let mut other = OtherDevice::start().await;
        let (stream, _shell) = vpn_link(&host).await;
        let (mut reader, mut writer) = tokio::io::split(stream);

        // What the person's other device offers, asked before routing through it.
        let ask = Control::Probe {
            exit: hex::encode(other.addr.id.as_bytes()),
            at: Some(other.addr.clone()),
        };
        link::write_control(&mut writer, &ask).await.unwrap();
        let Frame::Control(Control::Offer { offer, .. }) = link::read(&mut reader).await.unwrap()
        else {
            panic!("the daemon answers a probe with an offer");
        };
        assert_eq!(
            offer.home_networks,
            vec!["192.168.1.0/24".parse::<Ipv4Net>().unwrap()],
            "the networks the other device is on"
        );

        // The route: the daemon dials the other device on the tunnel and answers with its offer
        // and where the tunnel's own packets leave this device.
        let (stream, _shell) = vpn_link(&host).await;
        let (mut reader, mut writer) = tokio::io::split(stream);
        let ask = Control::Through {
            exit: hex::encode(other.addr.id.as_bytes()),
            at: Some(other.addr.clone()),
        };
        link::write_control(&mut writer, &ask).await.unwrap();
        let Frame::Control(Control::Offer {
            offer,
            carrier,
            mtu,
            ..
        }) = link::read(&mut reader).await.unwrap()
        else {
            panic!("the daemon answers a route with the exit's offer");
        };
        let address = offer
            .address
            .expect("the exit gives this device an address");
        assert_eq!(address, Ipv4Addr::new(100, 100, 64, 2));
        assert!(mtu >= device_vpn::net::MIN_MTU, "{mtu}");
        assert!(
            !carrier.udp_ports.is_empty(),
            "the tunnel's own packets leave this device outside it"
        );

        // A packet longer than a link carries is refused rather than cut short.
        assert!(
            link::write_packet(&mut writer, &vec![0_u8; device_vpn::link::MAX_PACKET + 1])
                .await
                .is_err(),
            "a packet past what a link carries is refused"
        );

        // A packet this device sends through the route reaches the other device as it went.
        let web = SocketAddrV4::new(Ipv4Addr::new(198, 51, 100, 7), 443);
        let outward = packet::udp_packet(SocketAddrV4::new(address, 50_000), web, b"hello")
            .expect("a packet to send through it");
        link::write_packet(&mut writer, &outward).await.unwrap();
        assert_eq!(
            other.delivered().await,
            outward,
            "the packet arrives at the other device as this one sent it"
        );
        assert_eq!(
            other.exit.sessions().len(),
            1,
            "the other device has this one's tunnel"
        );

        // What the other device sends comes back over the link as it went.
        let inward = packet::udp_packet(web, SocketAddrV4::new(address, 50_000), b"hello back")
            .expect("a packet to receive");
        other.stack.as_ref().unwrap().inject(inward.clone()).await;
        let seen = tokio::time::timeout(Duration::from_secs(10), link::read(&mut reader))
            .await
            .expect("the daemon passes the packet on")
            .unwrap();
        assert_eq!(seen, Frame::Packet(inward), "the packet arrives as sent");

        // The other device stops this one: the route ends, and says why.
        let peer = other.exit.sessions();
        for session in &peer {
            other.exit.stop(&session.peer);
        }
        let ended = tokio::time::timeout(Duration::from_secs(10), link::read(&mut reader))
            .await
            .expect("the route ends when the other device stops it")
            .unwrap();
        assert_eq!(
            ended,
            Frame::Control(Control::Ended(client::Ended::Refused(
                client::Refusal::Stopped
            )))
        );

        // This device's exit: the shell lends its TUN, and the daemon answers the tunnel on this
        // device's endpoint. A device that is not the person's asks to route through it, and is
        // turned away before it reads a byte.
        let (stream, _shell) = vpn_link(&host).await;
        let (mut reader, mut writer) = tokio::io::split(stream);
        link::write_control(&mut writer, &Control::Exit)
            .await
            .unwrap();
        assert_eq!(
            link::read(&mut reader).await.unwrap(),
            Frame::Control(Control::Ready),
            "the daemon takes the TUN the shell lends"
        );
        let stranger = Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback address")
            .bind()
            .await
            .expect("the stranger's endpoint binds");
        let dialled = stranger
            .connect(host.daemon().endpoint_addr(), ALPN)
            .await
            .expect("this device's endpoint answers the tunnel");
        let iroh::endpoint::ConnectionError::ApplicationClosed(closed) = dialled.closed().await
        else {
            panic!("the exit closes the tunnel with its reason");
        };
        assert_eq!(
            u32::try_from(closed.error_code.into_inner()).expect("a close code"),
            wire::close::NOT_YOURS,
            "a device that is not the person's is turned away"
        );
        drop(stranger);
        other
            .router
            .shutdown()
            .await
            .expect("the other device stops");
    });
}
