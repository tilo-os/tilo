//! A server restarted under a computer's held session: the session hears the server go, dials
//! its address again through the packet relay that outlived it, and the computer's next action
//! is served within seconds, with nobody asking it to reconnect. The same when the path to the
//! server goes silent rather than closed: nothing tells the session the server went or came back,
//! and it is served within seconds of the server's return all the same. And a
//! session its device gave up on while the server was away stops dialing as it is closed: a
//! device enters anew under the same key, and a server keeps one connection per key, so a
//! session left dialing would cut the new one off.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_session_client::{Outbox, TransportPhase};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{
    HostSettings, enrol, ready, restart_host_relayed, route, say, start_host_serving, start_peer,
};

/// How long a restart may stay visible to a held session once the server is back: its next dial
/// comes within a backoff of a few seconds.
const BACK_WITHIN: Duration = Duration::from_secs(20);

/// How long a closed session may take to end: no further dial, and its endpoint's packet relay
/// given the three seconds iroh grants it to hear the endpoint go. A session left dialing takes
/// up to three dials of ten seconds.
const CLOSED_WITHIN: Duration = Duration::from_secs(5);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_session_is_served_again_within_seconds_of_its_server_restarting() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start_host_serving(
        &root,
        &server_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        HostSettings {
            relayed: true,
            ..HostSettings::default()
        },
    )
    .await;
    let session = start_peer(
        server.iroh(),
        laptop.cert.clone(),
        server_device.cert.clone(),
        laptop.key.clone(),
    );
    session
        .join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    ready(&[&session]).await;
    let mut note = automerge::AutoCommit::new();
    session.write(route(), &say(&mut note, "before")).unwrap();
    durable(&session, BACK_WITHIN).await.unwrap();

    drop(server.web);
    server.server.shutdown().await;
    let server = restart_host_relayed(&root, &server_device, state.path()).await;
    let back = Instant::now();

    // Nothing tells the session to reconnect: the computer's next action is simply served.
    session.write(route(), &say(&mut note, "after")).unwrap();
    let kept = durable(&session, BACK_WITHIN).await;
    assert!(
        kept.is_ok(),
        "the computer's next action after its server restarted was not served within {} s \
         (the session is {:?}: {:?})",
        BACK_WITHIN.as_secs(),
        session.status().phase,
        session.status().failures,
    );
    eprintln!(
        "served again {} ms after the restart",
        back.elapsed().as_millis()
    );
    session.close();
    server.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_closed_while_its_server_is_away_stops_dialing_at_once() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start_host_serving(
        &root,
        &server_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        HostSettings {
            relayed: true,
            ..HostSettings::default()
        },
    )
    .await;
    let address = server.iroh();
    drop(server.web);
    server.server.shutdown().await;

    // A device enters while its server is away, and gives up on the entry while it dials.
    let session = start_peer(
        address,
        laptop.cert.clone(),
        server_device.cert.clone(),
        laptop.key.clone(),
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let closing = Instant::now();
    session.close();
    let closed = tokio::time::timeout(CLOSED_WITHIN, async {
        while session.status().phase != TransportPhase::Closed {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "a session closed mid-dial was still {:?} {} ms later",
        session.status().phase,
        closing.elapsed().as_millis()
    );
    eprintln!(
        "closed {} ms after it was closed",
        closing.elapsed().as_millis()
    );
}

/// How long a server stays away under a session whose path to it went silent: longer than the
/// silence a session takes as a lost connection, as the outages people saw were (minutes), so the
/// session has given the old connection up and is dialing when the server comes back.
const SILENT_OUTAGE: Duration = Duration::from_secs(50);

/// How soon after its server is back a session that lost it silently is served again: a dial
/// waits at most a capped, jittered backoff of a few seconds.
const BACK_AFTER_SILENCE: Duration = Duration::from_secs(15);

/// A server that went away without its going reaching the session, as a packet relay that
/// outlived it leaves a device: every packet between the two is dropped for longer than a session
/// waits on a silent connection, then a restarted server is reached on the same path, which the
/// old connection never reaches. Nothing tells the session the server went or came back, and
/// the computer's next action is served within seconds of the server being back: the session
/// ended the silent connection and kept dialing at short, capped waits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_whose_server_went_silently_is_served_again_within_seconds_of_its_return() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start_host_serving(
        &root,
        &server_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        HostSettings::default(),
    )
    .await;
    let path = SilentPath::to(loopback_of(&server.server)).await;
    let session = start_peer(
        tilo_server_client::ServerAddress::at(
            iroh::EndpointAddr::new(server.server.node_addr().id).with_ip_addr(path.address),
        ),
        laptop.cert.clone(),
        server_device.cert.clone(),
        laptop.key.clone(),
    );
    session
        .join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    ready(&[&session]).await;
    let mut note = automerge::AutoCommit::new();
    session.write(route(), &say(&mut note, "before")).unwrap();
    durable(&session, BACK_WITHIN).await.unwrap();

    // The server goes, and nothing of its going reaches the session.
    path.silence(true);
    drop(server.web);
    server.server.shutdown().await;
    tokio::time::sleep(SILENT_OUTAGE).await;
    let server = start_host_serving(
        &root,
        &server_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        HostSettings::default(),
    )
    .await;
    path.lead_to(loopback_of(&server.server));
    path.silence(false);
    let back = Instant::now();

    // Nothing tells the session to reconnect: the computer's next action is simply served.
    session.write(route(), &say(&mut note, "after")).unwrap();
    let kept = durable(&session, BACK_AFTER_SILENCE).await;
    assert!(
        kept.is_ok(),
        "the computer's next action after its server came back was not served within {} s \
         (the session is {:?}: {:?})",
        BACK_AFTER_SILENCE.as_secs(),
        session.status().phase,
        session.status().failures,
    );
    eprintln!(
        "served again {} ms after the server was back",
        back.elapsed().as_millis()
    );
    session.close();
    server.server.shutdown().await;
}

/// Where `server`'s endpoint is reached on this machine's loopback: the port of the IPv4 address
/// it names, which it listens on for every address of the machine.
fn loopback_of(server: &tilo_server::daemon::Server) -> SocketAddr {
    let port = server
        .node_addr()
        .ip_addrs()
        .find(|address| address.is_ipv4())
        .map(SocketAddr::port)
        .expect("the server names an IPv4 address");
    SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port))
}

/// A UDP path between a device and its server, as a packet relay that outlives the server
/// carries one: it can drop every packet both ways while the server is gone, then lead to a
/// restarted server on the same address. The connections the device had before go nowhere from
/// then on: the relay that kept them open never hears of their end, and nothing of them reaches
/// the restarted server, which would refuse them out loud. Only connections dialed afresh reach it.
struct SilentPath {
    address: SocketAddr,
    state: Arc<std::sync::Mutex<PathState>>,
    carrying: tokio::task::JoinHandle<()>,
}

struct PathState {
    upstream: SocketAddr,
    silent: bool,
    /// The connections the device used, by the start of the destination connection id of its
    /// short-header packets (an 8-byte prefix, which every id iroh issues is at least).
    seen: std::collections::HashSet<[u8; 8]>,
    /// Connections that go nowhere any more: those seen before the server was restarted.
    gone: std::collections::HashSet<[u8; 8]>,
}

impl SilentPath {
    async fn to(upstream: SocketAddr) -> Self {
        let socket = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let address = socket.local_addr().unwrap();
        let state = Arc::new(std::sync::Mutex::new(PathState {
            upstream,
            silent: false,
            seen: std::collections::HashSet::new(),
            gone: std::collections::HashSet::new(),
        }));
        let carrying = tokio::spawn({
            let state = Arc::clone(&state);
            async move {
                let mut device: Option<SocketAddr> = None;
                let mut packet = vec![0_u8; 65_536];
                loop {
                    let Ok((length, from)) = socket.recv_from(&mut packet).await else {
                        return;
                    };
                    let to = {
                        let mut state = state.lock().unwrap();
                        if state.silent {
                            continue;
                        }
                        if from == state.upstream {
                            let Some(device) = device else { continue };
                            device
                        } else {
                            device = Some(from);
                            if let Some(connection) = short_header_connection(&packet[..length]) {
                                if state.gone.contains(&connection) {
                                    continue;
                                }
                                state.seen.insert(connection);
                            }
                            state.upstream
                        }
                    };
                    let _ = socket.send_to(&packet[..length], to).await;
                }
            }
        });
        Self {
            address,
            state,
            carrying,
        }
    }

    fn silence(&self, silent: bool) {
        self.state.lock().unwrap().silent = silent;
    }

    /// Lead to `upstream`, a restarted server: every connection the device had goes nowhere.
    fn lead_to(&self, upstream: SocketAddr) {
        let mut state = self.state.lock().unwrap();
        state.upstream = upstream;
        let seen = std::mem::take(&mut state.seen);
        state.gone.extend(seen);
    }
}

/// The start of a QUIC short-header packet's destination connection id: what tells one
/// connection's packets from another's once its handshake is done. `None` for a long-header
/// packet, which a connection being made sends.
fn short_header_connection(packet: &[u8]) -> Option<[u8; 8]> {
    let first = *packet.first()?;
    if first & 0x80 != 0 {
        return None;
    }
    packet.get(1..9)?.try_into().ok()
}

impl Drop for SilentPath {
    fn drop(&mut self) {
        self.carrying.abort();
    }
}

/// Whether everything `session` wrote is durable at its server within `within`.
async fn durable(session: &tilo_session_client::Link, within: Duration) -> Result<(), ()> {
    let receipt = session.barrier().map_err(drop)?;
    tokio::time::timeout(within, async move {
        loop {
            if let Ok(answer) = receipt.try_recv() {
                return answer.map_err(drop);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(drop)?
}
