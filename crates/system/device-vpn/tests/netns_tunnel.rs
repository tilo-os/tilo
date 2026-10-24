//! A UDP exchange between two network namespaces, through real TUN devices
//! the helper's own Linux backend sets up and a loopback pair of iroh
//! endpoints carrying the packets.
//!
//! The test runs itself twice more under `unshare --user --map-root-user
//! --net`: once as the client device, once as the exit. Each child opens its
//! side through `Service<LinuxBackend>`, the way the root helper does, and
//! hands the TUN descriptor back over a Unix socket. The parent, outside
//! both namespaces, runs the tunnel between the two descriptors.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::os::fd::OwnedFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use device_vpn::fdpass::{read_line_with_fd, write_line_with_fd};
use device_vpn::net::Ipv4Net;
use device_vpn::protocol::{Carrier, ClientLink, ExitLink, Request, Response, Routes};
use device_vpn::service::linux::{Config, LinuxBackend};
use device_vpn::service::{Caller, Service};
use device_vpn::tunnel::device::TunDevice;
use device_vpn::tunnel::exit::EXIT_MTU;
use device_vpn::tunnel::{ALPN, Authority, Decision, Exit, ExitDevices, client};
use iroh::{Endpoint, EndpointAddr, EndpointId};

const ROLE: &str = "TILO_DEVICE_VPN_NETNS_ROLE";
const SOCKET: &str = "TILO_DEVICE_VPN_NETNS_SOCKET";
const STATE: &str = "TILO_DEVICE_VPN_NETNS_STATE";
const BOUND: Duration = Duration::from_secs(30);
const CLIENT_ADDRESS: Ipv4Addr = Ipv4Addr::new(100, 100, 64, 2);
/// An address on a network only the exit's namespace is attached to.
const SERVER: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(198, 51, 100, 1), 7777);

/// `ip` where the helper finds it.
fn ip(args: &str) {
    let program = Config::default().ip;
    let status = Command::new(&program)
        .args(args.split_whitespace())
        .status()
        .unwrap_or_else(|error| panic!("{} {args}: {error}", program.display()));
    assert!(status.success(), "ip {args}: {status}");
}

fn service(state: &Path, role: &str) -> Service<LinuxBackend> {
    let config = Config {
        record: state.join(format!("{role}-record.json")),
        dns_marker: state.join(format!("{role}-dns")),
        seat: seat_peer::Seat {
            trusted_ui: PathBuf::from("/nonexistent"),
            seat: state.join("seat0"),
            users: state.join("users"),
        },
        ..Config::default()
    };
    Service::new(
        LinuxBackend::new(config),
        device_vpn::service::record::Record::default(),
    )
}

fn opened(answer: (Response, Option<OwnedFd>)) -> OwnedFd {
    match answer {
        (Response::Opened { .. }, Some(fd)) => fd,
        (other, _) => panic!("the helper did not open the tunnel: {other:?}"),
    }
}

fn line(stream: &UnixStream) -> String {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    line.trim().to_owned()
}

/// One side, inside its own user and network namespace.
#[test]
#[ignore = "run by packets_cross_two_network_namespaces inside a namespace"]
fn netns_role() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let parent = UnixStream::connect(std::env::var(SOCKET).unwrap()).unwrap();
    parent.set_read_timeout(Some(BOUND)).unwrap();
    let state = PathBuf::from(std::env::var(STATE).unwrap());
    let caller = Caller { uid: 0 };
    ip("link set lo up");
    let mut service = service(&state, &role);
    match role.as_str() {
        "exit" => {
            // The far side of the exit: an address of its own, which the
            // client's packets reach once the exit has forwarded them.
            ip(&format!("address add {}/32 dev lo", SERVER.ip()));
            let fd = opened(service.handle(
                caller,
                Request::OpenExit {
                    link: ExitLink { mtu: EXIT_MTU },
                },
            ));
            let server = UdpSocket::bind(SERVER).unwrap();
            server.set_read_timeout(Some(BOUND)).unwrap();
            write_line_with_fd(&parent, b"exit\n", Some(std::os::fd::AsFd::as_fd(&fd))).unwrap();
            drop(fd);
            let mut buf = [0_u8; 512];
            let (length, from) = server.recv_from(&mut buf).unwrap();
            // Through the tunnel, the client's own address arrives.
            assert_eq!(from.ip(), std::net::IpAddr::V4(CLIENT_ADDRESS));
            let mut answer = b"pong:".to_vec();
            answer.extend_from_slice(&buf[..length]);
            server.send_to(&answer, from).unwrap();
            assert_eq!(line(&parent), "done");
            assert_eq!(service.handle(caller, Request::CloseExit).0, Response::Done);
        }
        "client" => {
            let link = ClientLink {
                address: CLIENT_ADDRESS,
                mtu: 1280,
                routes: Routes::All,
                // NetworkManager is not in the namespace.
                dns: false,
                kill_switch: true,
                carrier: Carrier {
                    udp_ports: vec![40_000],
                    relays: vec![],
                },
            };
            let fd = opened(service.handle(caller, Request::OpenClient { link }));
            write_line_with_fd(&parent, b"client\n", Some(std::os::fd::AsFd::as_fd(&fd))).unwrap();
            drop(fd);
            assert_eq!(line(&parent), "go");
            let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
            socket.set_read_timeout(Some(BOUND)).unwrap();
            socket.send_to(b"ping", SERVER).unwrap();
            let mut buf = [0_u8; 512];
            let (length, from) = socket.recv_from(&mut buf).unwrap();
            assert_eq!(from, SocketAddr::V4(SERVER));
            assert_eq!(&buf[..length], b"pong:ping");
            assert_eq!(
                service.handle(caller, Request::CloseClient).0,
                Response::Done
            );
            (&parent).write_all(b"crossed\n").unwrap();
        }
        other => panic!("no role {other}"),
    }
}

fn spawn_role(role: &str, socket: &Path, state: &Path) -> Child {
    Command::new("unshare")
        .args(["--user", "--map-root-user", "--net", "--"])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "netns_role",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROLE, role)
        .env(SOCKET, socket)
        .env(STATE, state)
        .stdin(Stdio::null())
        .spawn()
        .unwrap_or_else(|error| panic!("unshare: {error}"))
}

fn accept_fd(listener: &UnixListener, role: &str) -> (UnixStream, OwnedFd) {
    // A namespace that cannot be made never connects; fail instead of
    // waiting on it.
    let waiting = listener.try_clone().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(waiting.accept());
    });
    let (stream, _) = rx
        .recv_timeout(BOUND)
        .unwrap_or_else(|_| panic!("the {role} namespace never started"))
        .unwrap();
    stream.set_read_timeout(Some(BOUND)).unwrap();
    let (said, fd) = read_line_with_fd(&stream, 64).unwrap();
    assert_eq!(
        said,
        role.as_bytes(),
        "the {role} namespace failed to open its side"
    );
    (
        stream,
        fd.unwrap_or_else(|| panic!("the {role} sent no descriptor")),
    )
}

struct Allow(EndpointId);

impl Authority for Allow {
    fn decide(&self, peer: &EndpointId) -> Decision {
        if *peer == self.0 {
            Decision::Allow
        } else {
            Decision::NotYours
        }
    }

    fn home_networks(&self) -> Vec<Ipv4Net> {
        Vec::new()
    }
}

/// The exit TUN the exit namespace opened, handed over once.
struct Handed(Mutex<Option<OwnedFd>>);

impl ExitDevices for Handed {
    type Device = TunDevice;

    async fn open(&self, _mtu: u16) -> std::io::Result<TunDevice> {
        let fd = self
            .0
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| std::io::Error::other("opened twice"))?;
        TunDevice::new(fd)
    }

    async fn close(&self) {}
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

/// What the test needs from the machine, or what is missing.
fn machine_ready() -> Result<(), String> {
    let config = Config::default();
    for (name, path) in [("ip", &config.ip), ("nft", &config.nft)] {
        if !path.exists() {
            return Err(format!("no {name} at {}", path.display()));
        }
    }
    if !Path::new("/dev/net/tun").exists() {
        return Err("no /dev/net/tun".into());
    }
    // nftables over netlink is what sandboxes most often refuse, so ask for
    // it in the very namespaces the roles run in.
    let status = Command::new("unshare")
        .args(["--user", "--map-root-user", "--net", "--"])
        .arg(&config.nft)
        .args(["list", "ruleset"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("unshare: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "nft in a namespace of unshare --user --map-root-user --net: {status}"
        ))
    }
}

#[test]
fn packets_cross_two_network_namespaces() {
    tilo_test_env::require_capability!(
        NetworkTunnel,
        machine_ready(),
        "the namespaces need TUN devices, ip and nft"
    );
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("roles.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut exit_child = spawn_role("exit", &socket, dir.path());
    let (exit_stream, exit_fd) = accept_fd(&listener, "exit");
    let mut client_child = spawn_role("client", &socket, dir.path());
    let (client_stream, client_fd) = accept_fd(&listener, "client");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let exit_endpoint = endpoint().await;
        let client_endpoint = endpoint().await;
        let exit = Exit::new(
            Allow(client_endpoint.id()),
            Handed(Mutex::new(Some(exit_fd))),
            device_vpn::tunnel::dns::DnsRelay::new(SocketAddr::from((Ipv4Addr::LOCALHOST, 9))),
            || {},
        );
        let exit_addr = EndpointAddr::new(exit_endpoint.id()).with_ip_addr(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            exit_endpoint.bound_sockets().first().unwrap().port(),
        )));
        let router = iroh::protocol::Router::builder(exit_endpoint)
            .accept(ALPN, exit)
            .spawn();
        let tunnel = Arc::new(
            tokio::time::timeout(BOUND, client::connect(&client_endpoint, exit_addr))
                .await
                .unwrap()
                .unwrap(),
        );
        // The exit gives the address the client's helper configured.
        assert_eq!(tunnel.address, CLIENT_ADDRESS);
        let device = Arc::new(TunDevice::new(client_fd).unwrap());
        let running = tokio::spawn({
            let tunnel = Arc::clone(&tunnel);
            async move { client::run(&tunnel, device).await }
        });

        (&client_stream).write_all(b"go\n").unwrap();
        let said = tokio::task::spawn_blocking(move || line(&client_stream))
            .await
            .unwrap();
        assert_eq!(said, "crossed");
        (&exit_stream).write_all(b"done\n").unwrap();

        tunnel.close();
        let _ = tokio::time::timeout(BOUND, running).await;
        client_endpoint.close().await;
        router.shutdown().await.unwrap();
    });
    assert!(
        client_child.wait().unwrap().success(),
        "the client namespace failed"
    );
    assert!(
        exit_child.wait().unwrap().success(),
        "the exit namespace failed"
    );
}
