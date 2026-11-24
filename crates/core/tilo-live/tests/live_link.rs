//! A desktop session's live session through its device's daemon: the session joins over a stream
//! of the daemon's socket ([`link::Daemon`]), and the daemon carries it ([`link::carry`]) into the
//! relay it runs on its own endpoint, or on to the relay of another device, which it dials from
//! its endpoint and splices to the link.
//!
//! The daemon's socket is an in-memory byte stream here; everything above it is the code a
//! session and a daemon run: the link's frames, the admission preface, `moq-lite`, the relay's
//! judgement and the live sessions' sealing. Only the authority is the test's, which reads the
//! grants as a daemon's does from device certificates and the workspace roster.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failed unwrap in a test is its assertion"
)]

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use iroh::{Endpoint, EndpointAddr, RelayMode};
use key_group::{ContentKeys, KeyGroup, MemberKey};
use tilo_live::grant;
use tilo_live::link::{self, Toward};
use tilo_live::live::{self, Namespace, Track};
use tilo_live::live_session::{Heard, Join, LiveError, LiveSession};
use tilo_live::native;
use tilo_live::relay::{
    Admission, AuthorizationStatus, Denied, Direction, Peer, Relay, RelayAuthority,
    SessionAuthorization,
};
use tilo_live::roster::Roster;
use tilo_live::scope::Scope;
use tilo_live::voice::{FRAME_SAMPLES, VoiceObject};
use tilo_test_env::wait_until;

const WORKSPACE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

fn scope() -> Scope {
    Scope::workspace(WORKSPACE)
}

fn keys() -> ContentKeys {
    let workspace = collab_types::WorkspaceId::parse(WORKSPACE).unwrap();
    KeyGroup::create(workspace, MemberKey::generate("owner").unwrap())
        .unwrap()
        .content_keys()
        .unwrap()
}

async fn endpoint() -> Endpoint {
    Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .transport_config(native::transport_config())
        .bind()
        .await
        .unwrap()
}

fn addr(endpoint: &Endpoint) -> EndpointAddr {
    let port = endpoint.bound_sockets().first().unwrap().port();
    EndpointAddr::new(endpoint.id()).with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

/// The workspace's devices and their certificates, as a daemon's authority reads them.
struct Authority {
    mates: Arc<Mutex<BTreeMap<[u8; 32], Vec<u8>>>>,
    changed: tokio::sync::watch::Sender<u64>,
}

impl Authority {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            mates: Arc::default(),
            changed: tokio::sync::watch::channel(0).0,
        })
    }

    fn admit_device(&self, device: [u8; 32]) -> Vec<u8> {
        let certificate = format!("cert-{device:02x?}").into_bytes();
        self.mates
            .lock()
            .unwrap()
            .insert(device, certificate.clone());
        certificate
    }

    fn roster(&self) -> Roster {
        Roster::new(self.mates.lock().unwrap().keys().copied())
    }
}

impl RelayAuthority for Authority {
    fn admit<'a>(&'a self, peer: Peer, proof: &'a [u8]) -> Admission<'a> {
        Box::pin(async move {
            let Peer(device) = peer;
            let grant = grant::Native::parse(proof).ok_or(Denied)?;
            if live::workspace_bytes(WORKSPACE) != Some(grant.workspace)
                || grant.scope != scope().id()
            {
                return Err(Denied);
            }
            let mates = self.mates.lock().unwrap();
            if mates.get(&device) != Some(&grant.certificate) {
                return Err(Denied);
            }
            let subscriptions = mates
                .keys()
                .filter(|key| **key != device)
                .map(|key| Namespace::new(&scope(), *key).path())
                .collect();
            drop(mates);
            Ok(Arc::new(Permit {
                changed: self.changed.subscribe(),
                publication: Namespace::new(&scope(), device).path(),
                subscriptions,
            }) as Arc<dyn SessionAuthorization>)
        })
    }

    fn stranger(&self, peer: Peer) -> bool {
        let Peer(device) = peer;
        !self.mates.lock().unwrap().contains_key(&device)
    }
}

struct Permit {
    changed: tokio::sync::watch::Receiver<u64>,
    publication: String,
    subscriptions: Vec<String>,
}

impl SessionAuthorization for Permit {
    fn publication(&self) -> Option<&str> {
        Some(&self.publication)
    }
    fn subscriptions(&self) -> &[String] {
        &self.subscriptions
    }
    fn current(&self, _direction: Direction, namespace: &str) -> bool {
        namespace == self.publication || self.subscriptions.iter().any(|name| name == namespace)
    }
    fn status(&self, direction: Direction, namespace: &str) -> AuthorizationStatus {
        if self.current(direction, namespace) {
            AuthorizationStatus::Current
        } else {
            AuthorizationStatus::Denied
        }
    }
    fn changed(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        Some(self.changed.clone())
    }
}

fn grant_for(certificate: &[u8]) -> Vec<u8> {
    grant::Native::new(
        live::workspace_bytes(WORKSPACE).unwrap(),
        &scope().id(),
        1,
        certificate.to_vec(),
        Vec::new(),
    )
    .unwrap()
    .encode()
}

fn join(device: [u8; 32], roster: Roster, keys: &ContentKeys) -> Join {
    Join {
        scope: scope(),
        device,
        keys: keys.clone().into(),
        roster,
        publish: true,
        presence: true,
    }
}

/// A device's daemon: the relay it runs on its own endpoint, answering other devices there.
struct Daemon {
    relay: Relay,
    endpoint: Endpoint,
    _router: iroh::protocol::Router,
}

impl Daemon {
    async fn new(authority: &Arc<Authority>) -> Self {
        let relay = Relay::new(authority.clone());
        let endpoint = endpoint().await;
        let served = relay.clone();
        let router = iroh::protocol::Router::builder(endpoint.clone())
            .accept(
                native::ALPN,
                native::handler(move |session: native::Session| {
                    let relay = served.clone();
                    async move { native::serve_on(&relay, session).await }
                }),
            )
            .spawn();
        Self {
            relay,
            endpoint,
            _router: router,
        }
    }

    fn device(&self) -> [u8; 32] {
        *self.endpoint.id().as_bytes()
    }

    /// The desktop session's carrier to this daemon, taken `toward`: each dial opens one stream
    /// of the daemon's socket, which the daemon carries for as long as it lasts.
    fn link(&self, toward: Toward) -> Arc<link::Daemon> {
        let (relay, endpoint, own) = (self.relay.clone(), self.endpoint.clone(), self.device());
        Arc::new(link::Daemon::new(
            move || {
                let (relay, endpoint) = (relay.clone(), endpoint.clone());
                Box::pin(async move {
                    let (session_end, daemon_end) = tokio::io::duplex(1 << 16);
                    tokio::spawn(async move {
                        let (reader, writer) = tokio::io::split(daemon_end);
                        let _ = link::carry(reader, writer, &relay, &endpoint, own).await;
                    });
                    let (reader, writer) = tokio::io::split(session_end);
                    Ok((
                        Box::new(reader) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                        Box::new(writer) as Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
                    ))
                }) as link::Opening
            },
            toward,
        ))
    }
}

fn heard(live_session: &LiveSession, track: Track) -> Vec<Heard> {
    let mut bodies = Vec::new();
    while let Some(heard) = live_session.try_recv() {
        if heard.track == track {
            bodies.push(heard);
        }
    }
    bodies
}

/// Keep publishing presence from `from` until `to` hears it: presence repeats, and one object
/// sent before the relay attached its forwarder is not one a subscriber is owed.
fn hears_presence(from: &LiveSession, to: &LiveSession, sender: [u8; 32], body: &[u8]) {
    let mut sequence = 1;
    wait_until("the presence reaches the other device", || {
        from.publish(Track::Presence, sequence, body).unwrap();
        sequence = sequence.wrapping_add(1);
        heard(to, Track::Presence)
            .iter()
            .any(|object| object.body == body && object.sender == sender)
    });
}

fn in_call() -> Vec<u8> {
    collab_types::presence::Presence::desktop(
        "w",
        collab_types::presence::Payload::Availability {
            state: "available".into(),
            call: Some("standup".to_owned()),
            camera: false,
            screen: false,
        },
    )
    .encode()
    .unwrap()
}

fn voice(sequence: u32) -> Vec<u8> {
    VoiceObject {
        sequence: u64::from(sequence),
        timestamp: u64::from(sequence).saturating_mul(u64::from(FRAME_SAMPLES)),
        duration: FRAME_SAMPLES,
        speaker: None,
        payload: vec![u8::try_from(sequence).unwrap_or(0); 8],
    }
    .encode()
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_joins_through_its_daemon_into_the_relay_here_and_on_another_device() {
    let authority = Authority::new();
    let keys = keys();

    // The device that relays the workspace: its session joins through its daemon, into the relay
    // the daemon runs, as the daemon's own device.
    let here = Daemon::new(&authority).await;
    let here_cert = authority.admit_device(here.device());
    // Another device, whose daemon dials the relaying device for its session.
    let there = Daemon::new(&authority).await;
    let there_cert = authority.admit_device(there.device());
    let roster = authority.roster();

    let relaying = LiveSession::join(
        here.link(Toward::Here),
        &grant_for(&here_cert),
        join(here.device(), roster.clone(), &keys),
    )
    .await
    .unwrap();
    let dialling = LiveSession::join(
        there.link(Toward::Relay(addr(&here.endpoint))),
        &grant_for(&there_cert),
        join(there.device(), roster.clone(), &keys),
    )
    .await
    .unwrap();
    wait_until("the relay admitted both sessions", || {
        here.relay.counters().accepted == 2
    });
    assert_eq!(
        there.relay.counters().accepted,
        0,
        "the dialling daemon carries its session on and relays nothing itself"
    );

    // Sealed presence both ways, through the link and the splice.
    hears_presence(&relaying, &dialling, here.device(), b"here");
    hears_presence(&dialling, &relaying, there.device(), b"there");

    // Voice in a call both are in: carried as the call's objects, opened as sent.
    for live_session in [&relaying, &dialling] {
        live_session
            .publish(Track::Presence, 100, &in_call())
            .unwrap();
        live_session.set_call_keys("standup", 0, keys.clone());
    }
    wait_until("each is asked for its voice", || {
        relaying.voice_wanted() && dialling.voice_wanted()
    });
    let sent: Vec<Vec<u8>> = (0..5).map(voice).collect();
    for (sequence, body) in (0_u32..).zip(&sent) {
        dialling.publish(Track::Audio, sequence, body).unwrap();
    }
    let mut opened = Vec::new();
    wait_until("the voice reaches the relaying device", || {
        opened.extend(heard(&relaying, Track::Audio).into_iter().map(|o| o.body));
        sent.iter().all(|body| opened.contains(body))
    });

    // A device the workspace does not name is refused at the relay, and the refusal reaches its
    // session through the splice as the relay gave it.
    let stranger = Daemon::new(&authority).await;
    let refused = LiveSession::join(
        stranger.link(Toward::Relay(addr(&here.endpoint))),
        &grant_for(b"cert-not-on-the-roster"),
        join(stranger.device(), authority.roster(), &keys),
    )
    .await;
    assert!(
        matches!(refused, Err(LiveError::Denied)),
        "refused at the preface: {:?}",
        refused.as_ref().err()
    );
}
