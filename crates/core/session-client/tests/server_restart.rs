//! A server restarted under a computer's held session: the session hears the server go, dials
//! its address again through the packet relay that outlived it, and the computer's next action
//! is served within seconds, with nobody asking it to reconnect. The same when nothing of the
//! server's going reaches the session, behind a hop that outlives it, and the dial the session
//! makes meanwhile reaches something that never answers it: the session is served within seconds
//! of the server's return all the same. And a session its device gave up on while the server was
//! away stops dialing as it is closed: a device enters anew under the same key, and a server keeps
//! one connection per key, so a session left dialing would cut the new one off.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_session_client::{Outbox, TransportPhase};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{
    HostSettings, enrol, person_of, ready, restart_host_relayed, route, say, start_host_serving,
    start_peer,
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
    session
        .write(route(), &say(&mut note, person_of(&session), "before"))
        .unwrap();
    durable(&session, BACK_WITHIN).await.unwrap();

    drop(server.web);
    server.server.shutdown().await;
    let server = restart_host_relayed(&root, &server_device, state.path()).await;
    let back = Instant::now();

    // Nothing tells the session to reconnect: the computer's next action is simply served.
    session
        .write(route(), &say(&mut note, person_of(&session), "after"))
        .unwrap();
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

/// How soon after its server is back a session is served again when the dial it made while the
/// server was away reached something that never answered it: that dial gives up within the
/// handshake's deadline (twelve seconds), and the next comes within a capped, jittered backoff.
const BACK_AFTER_UNANSWERED: Duration = Duration::from_secs(35);

/// How long a session may take to give up a connection whose server went without a word: the
/// keep-alive silence it ends one after, and the dial that follows.
const NOTICED_WITHIN: Duration = Duration::from_secs(90);

/// A server that went without its going reaching the session, behind a hop that outlives it, as
/// the public relay does: the hop carries each connection to the server it was opened to, and
/// drops what is sent while nothing is there. The session gives the silent connection up and
/// dials again, and its dial lands on an endpoint at the server's address that takes the
/// connection and never answers it, as a server that is stopping or not serving yet does. Then the
/// server is back behind the hop. The session must not wait on that dial forever: the computer's
/// next action is served within seconds of the server's return, with nobody asking it to
/// reconnect.
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
    let hop = Hop::to(loopback_of(&server.server)).await;
    let session = start_peer(
        tilo_server_client::ServerAddress::at(
            iroh::EndpointAddr::new(server.server.node_addr().id).with_ip_addr(hop.address),
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
    session
        .write(route(), &say(&mut note, person_of(&session), "before"))
        .unwrap();
    durable(&session, BACK_WITHIN).await.unwrap();

    // The server goes, and nothing of its going reaches the session: the hop drops its farewell.
    hop.silence(true);
    drop(server.web);
    server.server.shutdown().await;
    let gone = Instant::now();
    hop.silence(false);
    // What the session dials next reaches an endpoint at the server's address that never answers.
    let unanswering = Unanswering::as_server(&server_device).await;
    hop.lead_to(unanswering.address);
    tokio::time::timeout(NOTICED_WITHIN, unanswering.dialed.notified())
        .await
        .expect("the session never gave its silent connection up and dialed again");
    eprintln!(
        "dialed again {} ms after the server went",
        gone.elapsed().as_millis()
    );

    // The server is back behind the hop, while the session waits on its unanswered dial.
    let server = start_host_serving(
        &root,
        &server_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        HostSettings::default(),
    )
    .await;
    hop.lead_to(loopback_of(&server.server));
    let back = Instant::now();

    // Nothing tells the session to reconnect: the computer's next action is simply served.
    session
        .write(route(), &say(&mut note, person_of(&session), "after"))
        .unwrap();
    let kept = durable(&session, BACK_AFTER_UNANSWERED).await;
    assert!(
        kept.is_ok(),
        "the computer's next action after its server came back was not served within {} s \
         (the session is {:?}: {:?})",
        BACK_AFTER_UNANSWERED.as_secs(),
        session.status().phase,
        session.status().failures,
    );
    eprintln!(
        "served again {} ms after the server was back",
        back.elapsed().as_millis()
    );
    session.close();
    server.server.shutdown().await;
    unanswering.endpoint.close().await;
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

/// An endpoint with a server's identity that takes the session protocol's connections and answers
/// nothing on them, keeping them open: a server that stopped serving but not listening.
struct Unanswering {
    endpoint: iroh::Endpoint,
    address: SocketAddr,
    /// Told each connection it took.
    dialed: Arc<tokio::sync::Notify>,
    accepting: tokio::task::JoinHandle<()>,
}

impl Unanswering {
    async fn as_server(server: &crate::support::Device) -> Self {
        let secret = iroh::SecretKey::from_bytes(&server.key.to_bytes());
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .secret_key(secret)
            .alpns(vec![tilo_server_protocol::transport::DIRECT_ALPN.to_vec()])
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let port = endpoint
            .addr()
            .ip_addrs()
            .find(|address| address.is_ipv4())
            .map(SocketAddr::port)
            .unwrap();
        let dialed = Arc::new(tokio::sync::Notify::new());
        let accepting = tokio::spawn({
            let (endpoint, dialed) = (endpoint.clone(), Arc::clone(&dialed));
            async move {
                while let Some(incoming) = endpoint.accept().await {
                    if let Ok(connection) = incoming.await {
                        dialed.notify_one();
                        // Held open, unanswered, until the device itself gives it up.
                        tokio::spawn(async move { connection.closed().await });
                    }
                }
            }
        });
        Self {
            endpoint,
            address: SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)),
            dialed,
            accepting,
        }
    }
}

impl Drop for Unanswering {
    fn drop(&mut self) {
        self.accepting.abort();
    }
}

/// A UDP hop between a device and its server, as a packet relay that outlives the server is: it
/// carries each connection to the server it was opened to for as long as the connection lives,
/// and only connections opened afresh to the one it now leads to. It can drop every packet both
/// ways, as a server that is gone receives nothing and says nothing.
struct Hop {
    address: SocketAddr,
    state: Arc<std::sync::Mutex<HopState>>,
    carrying: tokio::task::JoinHandle<()>,
}

struct HopState {
    /// Where connections opened from now on go.
    upstream: SocketAddr,
    silent: bool,
    /// The server each connection was opened to, by the start of a connection id it is sent to
    /// (an 8-byte prefix, which every id iroh issues is at least).
    opened_to: std::collections::HashMap<[u8; 8], SocketAddr>,
}

impl Hop {
    async fn to(upstream: SocketAddr) -> Self {
        let socket = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let address = socket.local_addr().unwrap();
        let state = Arc::new(std::sync::Mutex::new(HopState {
            upstream,
            silent: false,
            opened_to: std::collections::HashMap::new(),
        }));
        let carrying = tokio::spawn({
            let state = Arc::clone(&state);
            async move {
                let mut device: Option<SocketAddr> = None;
                let mut packet = vec![0_u8; 65_536];
                loop {
                    // A server gone from under its port answers with an error, which ends nothing.
                    let Ok((length, from)) = socket.recv_from(&mut packet).await else {
                        continue;
                    };
                    let packet = &packet[..length];
                    let to = {
                        let mut state = state.lock().unwrap();
                        if state.silent {
                            continue;
                        }
                        if Some(from) == device || device.is_none() && !is_server(&state, from) {
                            device = Some(from);
                            let id = destination(packet);
                            let known = id.and_then(|id| state.opened_to.get(&id).copied());
                            if let Some(server) = known {
                                server
                            } else {
                                let upstream = state.upstream;
                                if let Some(id) = id {
                                    state.opened_to.insert(id, upstream);
                                }
                                upstream
                            }
                        } else {
                            // The server names the id the device sends to from then on.
                            if let Some(id) = source(packet) {
                                state.opened_to.insert(id, from);
                            }
                            let Some(device) = device else { continue };
                            device
                        }
                    };
                    let _ = socket.send_to(packet, to).await;
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

    /// Lead the connections opened from now on to `upstream`.
    fn lead_to(&self, upstream: SocketAddr) {
        self.state.lock().unwrap().upstream = upstream;
    }
}

fn is_server(state: &HopState, from: SocketAddr) -> bool {
    from == state.upstream || state.opened_to.values().any(|server| *server == from)
}

/// The start of a QUIC packet's destination connection id: in a long header after its version
/// and length, in a short header right after its first byte.
fn destination(packet: &[u8]) -> Option<[u8; 8]> {
    let first = *packet.first()?;
    let start = if first & 0x80 == 0 { 1 } else { 6 };
    packet.get(start..start + 8)?.try_into().ok()
}

/// The start of a long-header QUIC packet's source connection id: the id its sender takes
/// packets at, which the other end sends to from then on.
fn source(packet: &[u8]) -> Option<[u8; 8]> {
    let first = *packet.first()?;
    if first & 0x80 == 0 {
        return None;
    }
    let destination_length = usize::from(*packet.get(5)?);
    let length_at = 6 + destination_length;
    let source_length = usize::from(*packet.get(length_at)?);
    if source_length < 8 {
        return None;
    }
    packet.get(length_at + 1..length_at + 9)?.try_into().ok()
}

impl Drop for Hop {
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
