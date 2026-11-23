//! The live session between native devices: a relay role on one device's own
//! endpoint, live sessions on the others, sealed presence and audio through it, and
//! a member removed from the workspace cut on the event.
//!
//! The relay and the live sessions here are the same code a session runs: the
//! relay's ALPN handler is [`tilo_live::native::handler`] over
//! [`Relay::serve_session`], the live sessions are [`LiveSession::join`] over
//! [`tilo_live::native::Iroh`], and every object is sealed under the scope's
//! channel content keys. Only the authority is the test's: it reads the grants
//! the way a session's authority does from device certificates and the
//! workspace roster.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
#![allow(
    clippy::too_many_lines,
    reason = "each test is one workspace session end to end"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::{Connection, VarInt};
use iroh::{Endpoint, EndpointAddr, RelayMode};
use key_group::{ContentKeys, KeyGroup, MemberKey};
use tilo_live::frame::LiveKeys;
use tilo_live::grant;
use tilo_live::live::{self, Namespace, Track};
use tilo_live::live_session::{self, Heard, Join, LiveError, LiveSession, MAX_HEARD};
use tilo_live::native;
use tilo_live::relay::{
    Admission, AuthorizationStatus, Denied, Direction, MAX_ADMITTING, MAX_ADMITTING_OF_ONE,
    MAX_ADMITTING_STRANGERS, Peer, Relay, RelayAuthority, SessionAuthorization,
};
use tilo_live::roster::Roster;
use tilo_live::scope::Scope;
use tilo_live::voice::{FRAME_SAMPLES, VoiceObject};
use tilo_test_env::wait_until;

const WORKSPACE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

/// The scope every device of the test joins: the workspace itself.
fn scope() -> Scope {
    Scope::workspace(WORKSPACE)
}

/// The workspace's content keys at epoch 1, as its group hands them to its
/// members.
fn keys() -> ContentKeys {
    let workspace = collab_types::WorkspaceId::parse(WORKSPACE).unwrap();
    let owner = MemberKey::generate("owner").unwrap();
    KeyGroup::create(workspace, owner)
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

/// The authority a session's relay role holds: who is in the workspace now,
/// what certificate each of them holds, and the watch its roster changes on.
struct Authority {
    epoch: u64,
    mates: Arc<Mutex<BTreeMap<[u8; 32], Vec<u8>>>>,
    changed: tokio::sync::watch::Sender<u64>,
    /// How many sessions the relay asked this authority to admit.
    asked: AtomicUsize,
    /// What a session waits for before it is judged, in an authority that holds them
    /// ([`Self::holding`]): one permit lets one session through.
    held: Option<tokio::sync::Semaphore>,
}

impl Authority {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: 1,
            mates: Arc::default(),
            changed: tokio::sync::watch::channel(0).0,
            asked: AtomicUsize::new(0),
            held: None,
        })
    }

    /// An authority that holds every session asking to be admitted until the test lets it through
    /// ([`Self::let_through`]): a workspace whose governance another task is changing, which an
    /// admission waits for. A session it holds is one the relay is still admitting.
    fn holding() -> Arc<Self> {
        Arc::new(Self {
            epoch: 1,
            mates: Arc::default(),
            changed: tokio::sync::watch::channel(0).0,
            asked: AtomicUsize::new(0),
            held: Some(tokio::sync::Semaphore::new(0)),
        })
    }

    /// How many sessions the relay asked this authority to admit so far: a session the relay had
    /// no room for is not among them.
    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }

    /// Judge `sessions` of the sessions held in admission, or the next ones to ask.
    fn let_through(&self, sessions: usize) {
        self.held
            .as_ref()
            .expect("an authority that holds its sessions")
            .add_permits(sessions);
    }

    fn admit_device(&self, device: [u8; 32]) -> Vec<u8> {
        let certificate = format!("cert-{device:02x?}").into_bytes();
        self.mates
            .lock()
            .unwrap()
            .insert(device, certificate.clone());
        certificate
    }

    /// The roster the live sessions open with.
    fn roster(&self) -> Roster {
        Roster::new(self.mates.lock().unwrap().keys().copied())
    }

    /// Remove `device` from the workspace and announce the change: the event
    /// a live session is cut on.
    fn remove(&self, device: [u8; 32]) {
        self.mates.lock().unwrap().remove(&device);
        self.changed
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }
}

impl RelayAuthority for Authority {
    fn admit<'a>(&'a self, peer: Peer, proof: &'a [u8]) -> Admission<'a> {
        Box::pin(async move {
            self.asked.fetch_add(1, Ordering::SeqCst);
            if let Some(held) = &self.held {
                held.acquire().await.map_err(|_| Denied)?.forget();
            }
            self.decide(peer, proof)
        })
    }

    /// An endpoint the workspace's roster does not name, as a session's authority tells it from
    /// the rosters it holds.
    fn stranger(&self, peer: Peer) -> bool {
        let Peer(endpoint) = peer;
        !self.mates.lock().unwrap().contains_key(&endpoint)
    }
}

impl Authority {
    fn decide(&self, peer: Peer, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let Peer(device) = peer;
        let grant = grant::Native::parse(proof).ok_or(Denied)?;
        if live::workspace_bytes(WORKSPACE) != Some(grant.workspace) {
            return Err(Denied);
        }
        if grant.scope != scope().id() {
            return Err(Denied);
        }
        if grant.epoch > self.epoch {
            return Err(Denied);
        }
        let mates = self.mates.lock().unwrap();
        let certificate = mates.get(&device).ok_or(Denied)?;
        // The certificate binds the handshake's key to the roster: a device
        // presenting another device's certificate is refused.
        if certificate != &grant.certificate {
            return Err(Denied);
        }
        let subscriptions = mates
            .keys()
            .filter(|key| **key != device)
            .map(|key| Namespace::new(&scope(), *key).path())
            .collect();
        drop(mates);
        Ok(Arc::new(Permit {
            mates: self.mates.clone(),
            changed: self.changed.subscribe(),
            device,
            publication: Namespace::new(&scope(), device).path(),
            subscriptions,
        }))
    }
}

/// One admitted session: its publication, what it hears, and the roster it is
/// cut by.
struct Permit {
    mates: Arc<Mutex<BTreeMap<[u8; 32], Vec<u8>>>>,
    changed: tokio::sync::watch::Receiver<u64>,
    device: [u8; 32],
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
        self.mates.lock().unwrap().contains_key(&self.device)
            && (namespace == self.publication
                || self.subscriptions.iter().any(|name| name == namespace))
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

/// The grant `device` presents for the workspace scope.
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

fn join(device: [u8; 32], roster: Roster, keys: impl Into<LiveKeys>) -> Join {
    Join {
        scope: scope(),
        device,
        keys: keys.into(),
        roster,
        publish: true,
        presence: true,
    }
}

/// The relay role on its own endpoint: the live session's ALPN handler on the
/// session's router.
fn serve(relay: &Relay, endpoint: &Endpoint) -> iroh::protocol::Router {
    let relay = relay.clone();
    iroh::protocol::Router::builder(endpoint.clone())
        .accept(
            native::ALPN,
            native::handler(move |session: native::Session| {
                let relay = relay.clone();
                async move { native::serve_on(&relay, session).await }
            }),
        )
        .spawn()
}

/// The connections one relay accepted, by the device at the other end: what a
/// test cuts to take a carrier away mid-session, which is what a network that
/// dropped does to a real one.
#[derive(Clone, Default)]
struct Carriers(Arc<Mutex<BTreeMap<[u8; 32], Connection>>>);

impl Carriers {
    /// Cut `device`'s connection, as a network that went away does.
    fn cut(&self, device: [u8; 32]) {
        let connection = self
            .0
            .lock()
            .unwrap()
            .get(&device)
            .cloned()
            .expect("the device dialled this relay");
        connection.close(VarInt::from_u32(0), b"the network went away");
    }

    /// The relay's end of `device`'s connection.
    fn connection(&self, device: [u8; 32]) -> Connection {
        self.0
            .lock()
            .unwrap()
            .get(&device)
            .cloned()
            .expect("the device dialled this relay")
    }
}

/// The relay role as [`serve`], remembering each connection it accepted so a
/// test can cut one.
fn serve_cuttable(
    relay: &Relay,
    endpoint: &Endpoint,
    carriers: &Carriers,
) -> iroh::protocol::Router {
    let relay = relay.clone();
    let carriers = carriers.clone();
    iroh::protocol::Router::builder(endpoint.clone())
        .accept(
            native::ALPN,
            native::handler(move |session: native::Session| {
                let relay = relay.clone();
                let carriers = carriers.clone();
                async move {
                    carriers
                        .0
                        .lock()
                        .unwrap()
                        .insert(session.remote(), session.connection().clone());
                    native::serve_on(&relay, session).await;
                }
            }),
        )
        .spawn()
}

fn voice(sequence: u32, payload: &[u8]) -> Vec<u8> {
    VoiceObject {
        sequence: u64::from(sequence),
        timestamp: u64::from(sequence).saturating_mul(u64::from(FRAME_SAMPLES)),
        duration: FRAME_SAMPLES,
        speaker: None,
        payload: payload.to_vec(),
    }
    .encode()
    .unwrap()
}

/// The call the tests' devices talk in.
const CALL: &str = "standup";

/// A presence object that says its device is in [`CALL`].
fn in_call() -> Vec<u8> {
    collab_types::presence::Presence::desktop(
        "w",
        collab_types::presence::Payload::Availability {
            state: "available".into(),
            call: Some(CALL.to_owned()),
            camera: false,
            screen: false,
        },
    )
    .encode()
    .unwrap()
}

/// Put each of `live_sessions` in [`CALL`] (its presence says so, at `sequence`) under the call's
/// `keys`, and wait until each is asked for its voice by the others there.
fn talk(live_sessions: &[&LiveSession], keys: &ContentKeys, sequence: u32) {
    for live_session in live_sessions {
        live_session
            .publish(Track::Presence, sequence, &in_call())
            .unwrap();
        assert_eq!(live_session.call_keys_wanted(), Some((CALL.to_owned(), 0)));
        live_session.set_call_keys(CALL, 0, keys.clone());
    }
    wait_until("each device in the call is asked for its voice", || {
        live_sessions
            .iter()
            .all(|live_session| live_session.voice_wanted())
    });
}

/// Everything `live_session` heard on `track` so far.
fn heard(live_session: &LiveSession, track: Track) -> Vec<Heard> {
    let mut bodies = Vec::new();
    while let Some(heard) = live_session.try_recv() {
        if heard.track == track {
            bodies.push(heard);
        }
    }
    bodies
}

#[tokio::test(flavor = "multi_thread")]
async fn three_native_devices_exchange_sealed_presence_and_audio_through_a_relay() {
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);

    // The relay device takes part in the workspace it relays: no connection
    // to itself, the same sealed objects through the shared origin.
    let relay_key = *relay_endpoint.id().as_bytes();
    authority.admit_device(relay_key);
    let keys = keys();
    let relay_live_session =
        LiveSession::local(&relay, &join(relay_key, authority.roster(), keys.clone())).unwrap();

    let a = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = LiveSession::join(
        Arc::new(native::Iroh::new(b_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);

    // Sealed presence: the relay copies it unread, and both the dialled
    // device and the relay's own live session open it under the scope's keys.
    a.publish(Track::Presence, 1, b"role=member").unwrap();
    wait_until("presence reaches the other device", || {
        heard(&b, Track::Presence)
            .iter()
            .any(|object| object.body == b"role=member" && object.sender == a_key)
    });
    wait_until("presence reaches the relay's own device", || {
        heard(&relay_live_session, Track::Presence)
            .iter()
            .any(|object| object.body == b"role=member" && object.sender == a_key)
    });

    // Sealed audio in the call both are in: five 20 ms objects, one group
    // each, opened on the far side exactly as they were published.
    talk(&[&a, &b], &keys, 2);
    let bodies: Vec<Vec<u8>> = (0..5_u32)
        .map(|seq| voice(seq, &[u8::try_from(seq).unwrap_or(0); 8][..]))
        .collect();
    for (seq, body) in bodies.iter().enumerate() {
        a.publish(Track::Audio, u32::try_from(seq).unwrap(), body)
            .unwrap();
    }
    let mut opened: Vec<Vec<u8>> = Vec::new();
    wait_until("the audio objects arrive", || {
        opened.extend(
            heard(&b, Track::Audio)
                .into_iter()
                .map(|object| object.body),
        );
        opened.len() >= bodies.len()
    });
    for body in &bodies {
        assert!(
            opened.contains(body),
            "an audio object was not opened as sent"
        );
    }
    assert!(
        relay.counters().forwarded >= 5,
        "the relay forwarded the objects"
    );

    // A member removed from the workspace is cut on the change itself: the
    // authority's watch fires, the relay re-observes, and the session ends
    // after the goaway grace.
    authority.remove(b_key);
    wait_until("the removed member's session ends", || b.ended());
    assert_eq!(
        a.publish(Track::Presence, 3, b"role=member"),
        Ok(()),
        "the remaining device keeps publishing"
    );

    // A device removed before it dials is refused at the preface and never
    // reaches MoQ.
    let c_endpoint = endpoint().await;
    let c_key = *c_endpoint.id().as_bytes();
    let refused = LiveSession::join(
        Arc::new(native::Iroh::new(c_endpoint, addr(&relay_endpoint))),
        &grant_for(b"cert-not-on-the-roster"),
        join(c_key, authority.roster(), keys.clone()),
    )
    .await;
    assert!(
        matches!(refused, Err(LiveError::Denied)),
        "refused at the preface"
    );

    // The live session is not tied to one relay: when the appetite moves, the same
    // device joins the new relay and is heard there.
    let moved_endpoint = endpoint().await;
    let moved = Relay::new(authority.clone());
    let _moved_router = serve(&moved, &moved_endpoint);
    // The dial is the only handle to `a`'s iroh endpoint: dropping it closes
    // the endpoint and the connection with it, so the live session keeps it for as
    // long as it runs.
    let a_again = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint, addr(&moved_endpoint))),
        &grant_for(&a_cert),
        join(a_key, authority.roster(), keys.clone()),
    )
    .await
    .unwrap();
    wait_until("the moved relay admits the device", || {
        moved.counters().accepted == 1
    });
    let moved_key = *moved_endpoint.id().as_bytes();
    authority.admit_device(moved_key);
    let moved_live_session =
        LiveSession::local(&moved, &join(moved_key, authority.roster(), keys.clone())).unwrap();
    // Presence is a signal that repeats: one object published in the instant
    // between admission and the relay's forwarder attaching is not one a
    // subscriber is owed, so the test keeps sending while it waits.
    let mut sequence = 3;
    wait_until("the moved relay carries the sealed presence", || {
        a_again
            .publish(Track::Presence, sequence, b"relay=moved")
            .unwrap();
        sequence = sequence.saturating_add(1);
        heard(&moved_live_session, Track::Presence)
            .iter()
            .any(|object| object.body == b"relay=moved")
    });
    assert_eq!(live_session::TRACKS.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_session_seals_under_the_keys_it_now_holds() {
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);

    // `a` joins under the channel's first keys, `b` under the ones the
    // channel moved to. The dials hold each device's endpoint, and each live session
    // keeps its dial: dropping one closes the endpoint and the session with
    // it, so they live as long as the live sessions.
    let first = keys();
    let next = keys();
    let a = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint, addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), first),
    )
    .await
    .unwrap();
    let b = LiveSession::join(
        Arc::new(native::Iroh::new(b_endpoint, addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster, next.clone()),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);

    // Once the channel's epoch moves, what `a` publishes is sealed under the
    // keys it now holds, not the ones it joined with.
    a.set_keys(next);
    let mut sequence = 1;
    wait_until(
        "the re-keyed object opens on the member at the new keys",
        || {
            a.publish(Track::Presence, sequence, b"epoch=moved")
                .unwrap();
            sequence = sequence.saturating_add(1);
            heard(&b, Track::Presence)
                .iter()
                .any(|object| object.body == b"epoch=moved" && object.sender == a_key)
        },
    );
}

/// A device where nothing shows people is sent none of their presence: its
/// live session holds no subscription to it, while the activity note still arrives.
/// Once something shows people it reads presence again, from the newest word,
/// and stops when that goes.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_session_that_hears_no_presence_is_sent_none() {
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let keys = keys();
    let a = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint, addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = LiveSession::join(
        Arc::new(native::Iroh::new(b_endpoint, addr(&relay_endpoint))),
        &grant_for(&b_cert),
        Join {
            presence: false,
            ..join(b_key, roster, keys)
        },
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);

    // Presence goes first, at the higher priority: a live session subscribed to it
    // would have it by the time the note arrives.
    let said = |sequence: u32, at: &[u8], note: &[u8]| {
        a.publish(Track::Presence, sequence, at).unwrap();
        a.publish(Track::Activity, sequence, note).unwrap();
        let mut presence = Vec::new();
        wait_until("the activity note arrives", || {
            let mut noted = false;
            while let Some(object) = b.try_recv() {
                match object.track {
                    Track::Presence => presence.push(object.body),
                    Track::Activity => noted |= object.body == note,
                    _ => {}
                }
            }
            noted
        });
        presence
    };
    let presence = said(1, b"at=first", b"note=first");
    assert!(presence.is_empty(), "no presence reaches b: {presence:?}");

    b.hear_presence(true);
    wait_until("b reads the newest presence once it hears it", || {
        heard(&b, Track::Presence)
            .iter()
            .any(|object| object.body == b"at=first" && object.sender == a_key)
    });

    b.hear_presence(false);
    let presence = said(2, b"at=second", b"note=second");
    assert!(
        presence.is_empty(),
        "no presence reaches b once it stopped hearing it: {presence:?}"
    );
}

/// A device the workspace admits while a live session is running is heard from
/// then on: the live session started with the roster of the moment it joined, and
/// the member arrives through [`LiveSession::set_roster`].
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_joins_the_roster_after_the_live_session_started_is_heard() {
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let keys = keys();

    // `a` opens knowing only itself: the workspace had no other device
    // when it joined. `b` is admitted afterwards, and `a` is told so.
    let a = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint, addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, Roster::new([a_key]), keys.clone()),
    )
    .await
    .unwrap();
    let b = LiveSession::join(
        Arc::new(native::Iroh::new(b_endpoint, addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, Roster::new([a_key, b_key]), keys.clone()),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);

    a.set_roster(Roster::new([a_key, b_key]));
    // Presence repeats, so a subscription that attached a moment after the
    // roster moved is still proven by the next one.
    let mut sequence = 1;
    wait_until(
        "the live session hears the device that joined its roster",
        || {
            b.publish(Track::Presence, sequence, b"joined=late")
                .unwrap();
            sequence = sequence.saturating_add(1);
            heard(&a, Track::Presence)
                .iter()
                .any(|object| object.body == b"joined=late" && object.sender == b_key)
        },
    );
}

/// A device the roster names before it has joined is heard as soon as it says itself: the member
/// already here waits for its broadcast and reads it when it comes, not when it comes back. An
/// invited person's page is in the workspace's governance a moment before it reaches the live
/// session, and the member who invited them sees them arrive at once.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_the_roster_names_before_it_joins_is_heard_once_it_says_itself() {
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let keys = keys();

    let a = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint, addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, Roster::new([a_key]), keys.clone()),
    )
    .await
    .unwrap();
    wait_until("the first device is admitted", || {
        relay.counters().accepted == 1
    });
    // The governance names `b` before `b` reaches the live session.
    a.set_roster(Roster::new([a_key, b_key]));
    tokio::time::sleep(Duration::from_millis(500)).await;

    let b = LiveSession::join(
        Arc::new(native::Iroh::new(b_endpoint, addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, Roster::new([a_key, b_key]), keys),
    )
    .await
    .unwrap();
    b.publish(Track::Presence, 1, b"arrived").unwrap();
    // Said once, as a page says itself as it joins: the next restatement is seconds away, so
    // what is heard within one came from that word.
    let since = std::time::Instant::now();
    let mut drawn = Vec::new();
    while since.elapsed() < Duration::from_secs(1) {
        drawn.extend(heard(&a, Track::Presence));
        if drawn.iter().any(|object| object.sender == b_key) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        drawn
            .iter()
            .any(|object| object.body == b"arrived" && object.sender == b_key),
        "the member here did not hear the device its roster named once it joined, in {:?}: \
         {drawn:?}",
        since.elapsed()
    );
}

/// A device that joins after another has said its presence hears it
/// within one frame of subscribing: the reader starts at the track's latest
/// group, not at whatever is said next. Nothing is published again here, so a
/// live session that only heard the next heartbeat could not pass.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_session_that_joins_after_presence_was_said_hears_it_without_a_new_one() {
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let keys = keys();

    let a = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    wait_until("the first device is admitted", || {
        relay.counters().accepted == 1
    });
    a.publish(Track::Presence, 1, b"role=member").unwrap();
    // The relay holds the frame and its author rests: this workspace's
    // presence heartbeat is twenty seconds apart, so anything the joiner sees
    // within two seconds came from where it subscribed, not from a new frame.
    wait_until("the relay carried the presence", || {
        relay.counters().forwarded >= 1
    });

    let joined = std::time::Instant::now();
    let b = LiveSession::join(
        Arc::new(native::Iroh::new(b_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster, keys),
    )
    .await
    .unwrap();
    // The joiner draws the frame its author said before the join, with no
    // further word from that author: the presence heartbeat is twenty seconds
    // apart, so anything within a second came from where it subscribed.
    let mut drawn = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while tokio::time::Instant::now() < deadline {
        drawn.extend(heard(&b, Track::Presence));
        if drawn.iter().any(|object| object.body == b"role=member") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        drawn
            .iter()
            .any(|object| object.body == b"role=member" && object.sender == a_key),
        "the joiner drew nothing of the presence said before it joined, in {:?}: {drawn:?}",
        joined.elapsed()
    );
    // Exactly the one frame its author sent: the joiner subscribed where the
    // relay held it, not to what came next.
    let from_a = drawn.iter().filter(|object| object.sender == a_key).count();
    assert_eq!(
        from_a, 1,
        "the joiner drew {from_a} frames of a presence its author said once: {drawn:?}"
    );
    drop(b);
}

/// Everything `live_session` heard on the presence track from `device` until `done`
/// holds of it, publishing through `say` between looks as a heartbeat would;
/// `what` names the wait.
async fn hear_until(
    live_session: &LiveSession,
    device: [u8; 32],
    what: &str,
    mut say: impl FnMut(),
    done: impl Fn(&[Heard]) -> bool,
) -> Vec<Heard> {
    let since = std::time::Instant::now();
    let mut drawn = Vec::new();
    while !done(&drawn) {
        assert!(
            since.elapsed() < Duration::from_secs(5),
            "{what} after {:?}: {drawn:?}",
            since.elapsed()
        );
        say();
        tokio::time::sleep(Duration::from_millis(50)).await;
        drawn.extend(
            heard(live_session, Track::Presence)
                .into_iter()
                .filter(|object| object.sender == device),
        );
    }
    drawn
}

/// A device whose session ends and that joins again — `tilo mcp` run again on
/// the same computer — is heard by a live session that stayed the whole time, every
/// time. Each new session numbers its presence from one again, under the same
/// name as the one before. Some sessions end before the next one joins, and
/// the live session that stayed hears them leave; others are still held when the next
/// one joins, as a killed process leaves its session until the relay notices,
/// and the new one is heard all the same, and nothing of the old one after.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_joins_again_is_heard_by_a_live_session_that_stayed() {
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let keys = keys();
    let session = || async {
        LiveSession::join(
            Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
            &grant_for(&a_cert),
            join(a_key, roster.clone(), keys.clone()),
        )
        .await
        .unwrap()
    };
    let first = session().await;
    let b = LiveSession::join(
        Arc::new(native::Iroh::new(b_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    // The session the next run starts with, and the sequence it says next.
    let mut current = Some((first, 1));
    // Whether each session is still held when the next one joins.
    for (run, lingers) in [false, true, false, true, false].into_iter().enumerate() {
        let (a, mut sequence) = match current.take() {
            Some(held) => held,
            None => (session().await, 1),
        };
        let said = format!("run={run}").into_bytes();
        let mut say = || {
            a.publish(Track::Presence, sequence, &said)
                .unwrap_or_else(|error| panic!("run {run}, frame {sequence}: {error}"));
            sequence += 1;
        };
        hear_until(
            &b,
            a_key,
            &format!("run {run}: the live session that stayed never heard the device"),
            &mut say,
            |drawn| drawn.iter().any(|object| object.body == said),
        )
        .await;
        if lingers {
            let next = session().await;
            let next_said = format!("run={}", run + 1).into_bytes();
            let mut next_sequence = 1;
            hear_until(
                &b,
                a_key,
                &format!(
                    "run {}: a session that joined while the last was held is unheard",
                    run + 1
                ),
                || {
                    next.publish(Track::Presence, next_sequence, &next_said)
                        .unwrap();
                    next_sequence += 1;
                },
                |drawn| drawn.iter().any(|object| object.body == next_said),
            )
            .await;
            drop(a);
            // The old session goes as the relay notices it, and the new one is
            // what the live session that stayed hears: the device is still here.
            let mut later = Vec::new();
            let held = std::time::Instant::now();
            while held.elapsed() < Duration::from_secs(1) {
                next.publish(Track::Presence, next_sequence, &next_said)
                    .unwrap();
                next_sequence += 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
                later.extend(
                    heard(&b, Track::Presence)
                        .into_iter()
                        .filter(|object| object.sender == a_key),
                );
            }
            assert!(
                later.last().is_some_and(|object| object.body == next_said),
                "run {run}: once the old session went, the device is not heard as the new \
                 one: {later:?}"
            );
            current = Some((next, next_sequence));
            continue;
        }
        drop(a);
        hear_until(
            &b,
            a_key,
            &format!("run {run}: the live session that stayed never heard the device leave"),
            || {},
            |drawn| drawn.iter().any(|object| object.body.is_empty()),
        )
        .await;
    }
}

/// A carrier taken away under a live session mid-session — a Wi-Fi network that moved, a laptop that slept:
/// the live session dials its relay again by itself, and while it is gone nobody it was reading is drawn as
/// if they were still there. The member's next word arrives as soon as the carrier is back.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_session_whose_carrier_goes_away_dials_its_relay_again() {
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let carriers = Carriers::default();
    let _router = serve_cuttable(&relay, &relay_endpoint, &carriers);
    let keys = keys();

    let a = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = LiveSession::join(
        Arc::new(native::Iroh::new(b_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);

    // The member says where they are, and the other live session draws them there.
    a.publish(Track::Presence, 1, b"at=12,34").unwrap();
    wait_until("the member is heard", || {
        heard(&b, Track::Presence)
            .iter()
            .any(|object| object.body == b"at=12,34" && object.sender == a_key)
    });

    // The connection under the second live session goes away, as a dropped network does. It is not drawn
    // where they last were — they are said to have left as the session carrying them did — and the
    // live session itself is not over.
    carriers.cut(b_key);
    wait_until("the member is no longer drawn", || {
        heard(&b, Track::Presence)
            .iter()
            .any(|object| object.sender == a_key && object.body.is_empty())
    });
    assert!(
        !b.ended(),
        "the live session outlasts the carrier it was opened on"
    );

    // It dials the relay again, and the member's next word is drawn on the same live session.
    wait_until("the live session dialled its relay again", || {
        relay.counters().accepted >= 3
    });
    let mut drawn: Vec<Heard> = Vec::new();
    let mut sequence = 2;
    let since = std::time::Instant::now();
    while !drawn
        .iter()
        .any(|object| object.body == b"at=40,55" && object.sender == a_key)
    {
        assert!(
            since.elapsed() < Duration::from_secs(10),
            "the member is not drawn again on the live_session whose carrier came back, in {:?}: \
             {drawn:?}",
            since.elapsed()
        );
        a.publish(Track::Presence, sequence, b"at=40,55").unwrap();
        sequence = sequence.saturating_add(1);
        tokio::time::sleep(Duration::from_millis(50)).await;
        drawn.extend(
            heard(&b, Track::Presence)
                .into_iter()
                .filter(|object| object.sender == a_key),
        );
    }
    assert!(
        !b.ended(),
        "the live session the member is drawn on is the one that lost its carrier"
    );

    // And the live session says again on the carrier it dialled: what this device publishes reaches the
    // member over the connection that replaced the one that went away.
    let mut sequence = 1;
    let since = std::time::Instant::now();
    while !heard(&a, Track::Presence)
        .iter()
        .any(|object| object.body == b"shell=here" && object.sender == b_key)
    {
        assert!(
            since.elapsed() < Duration::from_secs(10),
            "the live session does not say on the carrier it dialled again, in {:?}",
            since.elapsed()
        );
        b.publish(Track::Presence, sequence, b"shell=here").unwrap();
        sequence = sequence.saturating_add(1);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_session_let_go_closes_its_carrier() {
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint) = (endpoint().await, endpoint().await);
    let a_key = *a_endpoint.id().as_bytes();
    let a_cert = authority.admit_device(a_key);
    let relay = Relay::new(authority.clone());
    let carriers = Carriers::default();
    let _router = serve_cuttable(&relay, &relay_endpoint, &carriers);

    let a = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, authority.roster(), keys()),
    )
    .await
    .unwrap();
    wait_until("the session admitted", || relay.counters().accepted == 1);
    let connection = carriers.connection(a_key);

    // A device leaves the live session (the workspace went, its session started over): the connection it
    // dialled goes with it, long before the connection would idle out. One left open is kept,
    // with all it buffers, on both ends for as long as the device runs.
    drop(a);
    let since = std::time::Instant::now();
    while connection.close_reason().is_none() {
        assert!(
            since.elapsed() < Duration::from_secs(5),
            "the carrier of a live session let go is still open after {:?}",
            since.elapsed()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// One camera picture of `layer`, sealed and sent through `live_session`.
fn picture(
    layer: tilo_live::video::Layer,
    source: tilo_live::video::Source,
    sequence: u64,
    keyframe: bool,
) -> tilo_live::video::VideoObject {
    let (width, height) = layer.size(source);
    tilo_live::video::VideoObject {
        keyframe,
        hdr: false,
        codec: tilo_live::video::Codec::H264,
        layer,
        sequence,
        timestamp_us: sequence.saturating_mul(33_333),
        width,
        height,
        payload: vec![0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }],
    }
}

/// One object of a window's page, as its publisher sends it.
fn page_object(sequence: u64, whole: bool) -> tilo_live::page::PageObject {
    tilo_live::page::PageObject {
        whole,
        sequence,
        timestamp_us: sequence.saturating_mul(100_000),
        payload: vec![0, 9, 9],
    }
}

/// Everything `live_session` received on `track` so far, decoded.
fn pages(live_session: &LiveSession, track: Track) -> Vec<tilo_live::page::PageObject> {
    let mut objects = Vec::new();
    while let Some(heard) = live_session.try_recv() {
        if heard.track == track
            && let Ok(object) = tilo_live::page::PageObject::decode(&heard.body)
        {
            objects.push(object);
        }
    }
    objects
}

/// Everything `live_session` received on `track` so far, decoded.
fn pictures(live_session: &LiveSession, track: Track) -> Vec<tilo_live::video::VideoObject> {
    let mut objects = Vec::new();
    while let Some(heard) = live_session.try_recv() {
        if heard.track == track
            && let Ok(object) = tilo_live::video::VideoObject::decode(&heard.body)
        {
            objects.push(object);
        }
    }
    objects
}

/// A camera layer exists only while a peer shows it: the viewer asks for one
/// layer at a time, the device encodes exactly that, a picture of a layer
/// nobody shows is never sent, and showing nothing stops the encoding.
#[tokio::test(flavor = "multi_thread")]
async fn a_peer_encodes_only_the_camera_layers_someone_shows() {
    use tilo_live::live::Track;
    use tilo_live::video::{Layer, Source};
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let keys = keys();
    let a = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = LiveSession::join(
        Arc::new(native::Iroh::new(b_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster, keys.clone()),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);
    // Pictures are call media: both are in the call.
    talk(&[&a, &b], &keys, 1);
    assert!(
        a.wanted_pictures().is_empty(),
        "nobody shows the camera yet: nothing is encoded"
    );

    // The viewer shows the camera in a tile: the peer asks for that layer,
    // and starts it with a keyframe.
    b.show(a_key, &[Track::Camera(Layer::Mid)]);
    wait_until("the middle layer is asked for", || {
        a.wanted_pictures() == [(Source::Camera, Layer::Mid)]
    });
    let mut received = Vec::new();
    let mut sequence = 0_u64;
    wait_until("the picture reaches the viewer", || {
        let _ = a.send_picture(
            Source::Camera,
            &picture(Layer::Mid, Source::Camera, sequence, true),
        );
        sequence = sequence.saturating_add(1);
        received.extend(pictures(&b, Track::Camera(Layer::Mid)));
        !received.is_empty()
    });
    assert_eq!(received[0].layer, Layer::Mid);
    assert!(received[0].keyframe);
    assert_eq!(
        received[0].payload,
        vec![0, 0, 0, 1, 0x65],
        "the viewer opened the payload the camera sealed"
    );

    // The tile shrinks to a filmstrip face: only the small layer is encoded.
    b.show(a_key, &[Track::Camera(Layer::Low)]);
    wait_until("the small layer replaces the middle one", || {
        a.wanted_pictures() == [(Source::Camera, Layer::Low)]
    });
    let mut small = Vec::new();
    wait_until("the small picture reaches the viewer", || {
        let _ = a.send_picture(
            Source::Camera,
            &picture(Layer::Low, Source::Camera, sequence, true),
        );
        sequence = sequence.saturating_add(1);
        small.extend(pictures(&b, Track::Camera(Layer::Low)));
        !small.is_empty()
    });
    assert_eq!(small[0].width, 320);
    // A layer nobody shows is not sent: there is no encoder to make one.
    assert_eq!(
        a.send_picture(
            Source::Camera,
            &picture(Layer::High, Source::Camera, sequence, true)
        ),
        Err(live_session::LiveError::Unwanted)
    );

    // Showing nothing stops the layer's encoding and its track.
    b.show(a_key, &[]);
    wait_until("nothing is asked for", || a.wanted_pictures().is_empty());
    assert_eq!(
        a.send_picture(
            Source::Camera,
            &picture(Layer::Low, Source::Camera, sequence, true)
        ),
        Err(live_session::LiveError::Unwanted)
    );
}

/// A shared screen travels the live session as its own sealed track at the layer
/// shown, and a picture is published only while someone shows it: nothing of
/// the screen is sent after the last viewer stops, which is what a locked
/// screen's withdrawn share relies on.
#[tokio::test(flavor = "multi_thread")]
async fn a_shared_screen_is_published_only_at_the_layer_shown() {
    use tilo_live::live::Track;
    use tilo_live::video::{Layer, Source};
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let keys = keys();
    let a = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = LiveSession::join(
        Arc::new(native::Iroh::new(b_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster, keys.clone()),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);
    // Pictures are call media: both are in the call.
    talk(&[&a, &b], &keys, 1);

    b.show(a_key, &[Track::Screen(Layer::Low)]);
    wait_until("the screen's layer is asked for", || {
        a.wanted_pictures() == [(Source::Screen, Layer::Low)]
    });
    let mut received = Vec::new();
    let mut sequence = 0_u64;
    wait_until("the screen reaches the viewer", || {
        let _ = a.send_picture(
            Source::Screen,
            &picture(Layer::Low, Source::Screen, sequence, true),
        );
        sequence = sequence.saturating_add(1);
        received.extend(pictures(&b, Track::Screen(Layer::Low)));
        !received.is_empty()
    });
    assert_eq!(received[0].layer, Layer::Low);
    assert!(received[0].keyframe);
    // The screen's own stream: the same bytes do not open as a camera frame,
    // so a viewer showing a camera layer is never handed a screen's picture.
    b.show(
        a_key,
        &[Track::Screen(Layer::Low), Track::Camera(Layer::Low)],
    );
    wait_until("the camera layer is asked for too", || {
        a.wanted_pictures().len() == 2
    });
    let mut cameras = Vec::new();
    wait_until("a camera picture travels its own track", || {
        let _ = a.send_picture(
            Source::Camera,
            &picture(Layer::Low, Source::Camera, sequence, true),
        );
        sequence = sequence.saturating_add(1);
        cameras.extend(pictures(&b, Track::Camera(Layer::Low)));
        !cameras.is_empty()
    });
    assert_eq!(cameras[0].layer, Layer::Low);

    // The person stops sharing, or locks their screen: the share is
    // withdrawn, no viewer shows the screen any more, and nothing is encoded
    // or sent of it from here on.
    b.show(a_key, &[Track::Camera(Layer::Low)]);
    wait_until("the screen is no longer asked for", || {
        a.wanted_pictures() == [(Source::Camera, Layer::Low)]
    });
    assert_eq!(
        a.send_picture(
            Source::Screen,
            &picture(Layer::Low, Source::Screen, sequence, true)
        ),
        Err(live_session::LiveError::Unwanted)
    );
}

/// One window's page travels the live session as its own sealed track: the publisher
/// sends it only while a member shows it, the member's first object is a whole
/// frame, and a member that joins while the page is up starts at a whole frame
/// too — never at a patch of a frame it does not hold.
#[tokio::test(flavor = "multi_thread")]
async fn a_windows_page_travels_only_while_a_member_shows_it() {
    /// The window whose page the test publishes.
    const TAB: u64 = 0x5EED_0000_0000_0007;
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint, c_endpoint) = (
        endpoint().await,
        endpoint().await,
        endpoint().await,
        endpoint().await,
    );
    let (a_key, b_key, c_key) = (
        *a_endpoint.id().as_bytes(),
        *b_endpoint.id().as_bytes(),
        *c_endpoint.id().as_bytes(),
    );
    let (a_cert, b_cert, c_cert) = (
        authority.admit_device(a_key),
        authority.admit_device(b_key),
        authority.admit_device(c_key),
    );
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let keys = keys();
    let a = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = LiveSession::join(
        Arc::new(native::Iroh::new(b_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);
    assert!(
        a.wanted_pages().is_empty(),
        "nobody shows the page yet: nothing is captured"
    );

    // The viewer shows the window's page: its track is asked for, and it
    // starts with a whole frame.
    b.show(a_key, &[Track::Page(TAB)]);
    wait_until("the page is asked for", || a.wanted_pages() == [TAB]);
    assert_eq!(a.take_page_keyframes(), [TAB]);
    let mut received = Vec::new();
    let mut sequence = 0_u64;
    wait_until("the whole frame reaches the viewer", || {
        let _ = a.send_page(TAB, &page_object(sequence, true));
        sequence = sequence.saturating_add(1);
        received.extend(pages(&b, Track::Page(TAB)));
        !received.is_empty()
    });
    assert!(received[0].whole);
    assert_eq!(received[0].payload, vec![0, 9, 9]);

    // Its changes follow on the same track, without another whole frame.
    let mut patches = Vec::new();
    wait_until("the changes reach the viewer", || {
        let _ = a.send_page(TAB, &page_object(sequence, false));
        sequence = sequence.saturating_add(1);
        patches.extend(pages(&b, Track::Page(TAB)));
        patches.iter().any(|object| !object.whole)
    });
    assert!(
        patches.iter().any(|object| !object.whole),
        "a change travels without a new whole frame"
    );

    // A member that joins while the page is up starts at a whole frame: it
    // could not apply a change to a frame it never had.
    let c = LiveSession::join(
        Arc::new(native::Iroh::new(c_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&c_cert),
        join(c_key, roster, keys),
    )
    .await
    .unwrap();
    wait_until("the late member is admitted", || {
        relay.counters().accepted == 3
    });
    c.show(a_key, &[Track::Page(TAB)]);
    let mut late = Vec::new();
    wait_until("the late viewer is handed a whole frame", || {
        late.extend(pages(&c, Track::Page(TAB)));
        !late.is_empty()
    });
    assert!(late[0].whole, "a viewer that joined late starts at a frame");

    // Nobody watches any more: the page is not captured, and nothing of it is
    // sent.
    b.show(a_key, &[]);
    c.show(a_key, &[]);
    wait_until("the page is no longer shown", || {
        a.wanted_pages().is_empty()
    });
    assert_eq!(
        a.send_page(TAB, &page_object(sequence, false)),
        Err(LiveError::Unwanted)
    );
}

/// A shared window travels as its own sealed track: published only while a
/// member shows it, starting at a keyframe whose header says what a decoder
/// starts from, and a member that joins while it is up asks for a keyframe of
/// its own.
#[tokio::test(flavor = "multi_thread")]
async fn a_shared_window_travels_only_while_a_member_shows_it() {
    use tilo_live::video::{Layer, Source};
    /// The window the test shares.
    const TAB: u64 = 0x5EED_0000_0000_0009;
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let keys = keys();
    let a = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = LiveSession::join(
        Arc::new(native::Iroh::new(b_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster, keys),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);
    let window = |sequence: u64, keyframe: bool| tilo_live::video::VideoObject {
        width: 1000,
        height: 700,
        ..picture(Layer::High, Source::Screen, sequence, keyframe)
    };
    assert_eq!(
        a.send_window(TAB, &window(0, true)),
        Err(LiveError::Unwanted),
        "nobody shows the window: nothing of it is sent"
    );

    b.show(a_key, &[Track::Window(TAB, Layer::High)]);
    wait_until("the window is asked for", || a.wanted_windows() == [TAB]);
    assert_eq!(
        a.take_window_keyframes(),
        [TAB],
        "a new viewer starts at a keyframe"
    );
    let mut received = Vec::new();
    let mut sequence = 0_u64;
    wait_until("the window reaches the viewer", || {
        let _ = a.send_window(TAB, &window(sequence, sequence == 0));
        sequence = sequence.saturating_add(1);
        received.extend(pictures(&b, Track::Window(TAB, Layer::High)));
        received.len() >= 2
    });
    assert!(received[0].keyframe, "the viewer starts at the keyframe");
    assert_eq!(
        (received[0].width, received[0].height, received[0].codec),
        (1000, 700, tilo_live::video::Codec::H264),
        "the keyframe says the window's own size and codec"
    );
    assert!(
        a.wanted_pictures().is_empty(),
        "a window is no call picture"
    );

    b.show(a_key, &[]);
    wait_until("the window is no longer shown", || {
        a.wanted_windows().is_empty()
    });
    assert_eq!(
        a.send_window(TAB, &window(sequence, true)),
        Err(LiveError::Unwanted)
    );
}

/// A device that publishes to a workspace without holding any of its content
/// keys — a hosted machine confined to its own record — is handed the media
/// keys of exactly the streams it publishes. What it seals reaches a member
/// that holds the content keys, it cannot seal a stream it was not handed,
/// and nothing the member seals is opened here.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_on_its_own_media_keys_publishes_to_the_members_and_opens_nothing() {
    use tilo_live::frame::{Refusal, Stream};

    /// The window whose page the device publishes.
    const TAB: u64 = 0x5EED_0000_0000_0007;

    let authority = Authority::new();
    let (relay_endpoint, member_endpoint, confined_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (member_key, confined_key) = (
        *member_endpoint.id().as_bytes(),
        *confined_endpoint.id().as_bytes(),
    );
    let (member_cert, confined_cert) = (
        authority.admit_device(member_key),
        authority.admit_device(confined_key),
    );
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let channel = keys();

    // The member holds the workspace's content keys and derives this device's
    // own ones from them, for the presence and the page it publishes.
    let handed = channel
        .sender_media_keys(
            channel.epoch(),
            confined_key,
            &[Stream::Presence.tag(), Stream::Page.tag()],
        )
        .unwrap();
    let member = LiveSession::join(
        Arc::new(native::Iroh::new(member_endpoint, addr(&relay_endpoint))),
        &grant_for(&member_cert),
        join(member_key, roster.clone(), channel.clone()),
    )
    .await
    .unwrap();
    let confined = LiveSession::join(
        Arc::new(native::Iroh::new(confined_endpoint, addr(&relay_endpoint))),
        &grant_for(&confined_cert),
        join(confined_key, roster, handed.clone()),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);

    // Presence sealed under the keys it was handed opens on the member, under
    // the keys the member derived them from.
    confined
        .publish(Track::Presence, 1, b"role=hosted")
        .unwrap();
    wait_until("the member opens the device's presence", || {
        heard(&member, Track::Presence)
            .iter()
            .any(|object| object.sender == confined_key && object.body == b"role=hosted")
    });

    // A page it publishes the same way, on a track the member asks for, seals
    // under the same handed keys.
    member.show(confined_key, &[Track::Page(TAB)]);
    wait_until("the page is asked for", || confined.wanted_pages() == [TAB]);
    let mut page = 0_u64;
    wait_until("the member opens the device's page", || {
        let _ = confined.send_page(TAB, &page_object(page, true));
        page = page.saturating_add(1);
        !pages(&member, Track::Page(TAB)).is_empty()
    });

    // A stream it was not handed is not one it can seal: its call audio is
    // refused here rather than sealed under another device's key.
    confined.publish(Track::Presence, 2, &in_call()).unwrap();
    confined.set_call_keys(CALL, 0, handed);
    assert_eq!(
        confined.publish(Track::Audio, 1, &voice(0, b"hello")),
        Err(LiveError::Refused(Refusal::NoKey(channel.epoch())))
    );

    // Nothing the member seals is opened here, its presence included: the
    // device holds nobody's keys but its own, and those seal only.
    let mut sequence = 1;
    wait_until("the device refuses what it reads", || {
        member
            .publish(Track::Presence, sequence, b"role=member")
            .unwrap();
        sequence = sequence.saturating_add(1);
        !confined.refusals().is_empty()
    });
    assert_eq!(confined.heard_total(), 0, "it opens nobody's frames");
    assert_eq!(
        confined.publish(Track::Presence, sequence, b"role=hosted"),
        Ok(()),
        "it keeps publishing what it was handed"
    );
}

/// One device of the workspace: its endpoint, its key and the certificate the workspace holds of
/// it.
#[derive(Clone)]
struct Member {
    endpoint: Endpoint,
    key: [u8; 32],
    certificate: Vec<u8>,
}

/// `count` devices the workspace admitted, each on an endpoint of its own.
async fn members(authority: &Authority, count: usize) -> Vec<Member> {
    let mut members = Vec::new();
    for _ in 0..count {
        let endpoint = endpoint().await;
        let key = *endpoint.id().as_bytes();
        let certificate = authority.admit_device(key);
        members.push(Member {
            endpoint,
            key,
            certificate,
        });
    }
    members
}

/// `member` asks the relay at `relay` for the workspace's live session, and gives it `within`.
async fn ask(
    member: Member,
    relay: EndpointAddr,
    roster: Roster,
    keys: ContentKeys,
    within: Duration,
) -> Result<LiveSession, LiveError> {
    LiveSession::join_within(
        Arc::new(native::Iroh::new(member.endpoint, relay)),
        &grant_for(&member.certificate),
        join(member.key, roster, keys),
        within,
    )
    .await
}

/// How many sessions `relay` admitted so far.
fn admitted(relay: &Relay) -> usize {
    usize::try_from(relay.counters().accepted).unwrap()
}

/// `count` devices the workspace does not hold, each on an endpoint of its own.
async fn strangers(count: usize) -> Vec<Member> {
    let mut strangers = Vec::new();
    for _ in 0..count {
        let endpoint = endpoint().await;
        strangers.push(Member {
            key: *endpoint.id().as_bytes(),
            endpoint,
            certificate: b"cert-not-on-the-roster".to_vec(),
        });
    }
    strangers
}

/// The relay takes [`MAX_ADMITTING`] sessions of the workspace's devices through admission at
/// once. One more is closed as busy with nothing of it read: the relay asks its authority nothing
/// about it, so busy is no answer about a grant and never reads as a refusal. Devices the
/// workspace does not hold wait in places of their own, and one more of those is told busy the
/// same way, whoever it is. Once one of the sixteen is through, the next that asks is taken, and
/// a device the workspace does not hold is refused as soon as the relay has room to judge it.
#[tokio::test(flavor = "multi_thread")]
async fn a_relay_admitting_as_many_sessions_as_it_takes_tells_the_next_it_is_busy() {
    let authority = Authority::holding();
    let relay_endpoint = endpoint().await;
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let at = addr(&relay_endpoint);
    let keys = keys();
    let all = members(&authority, MAX_ADMITTING + 2).await;
    let (first, rest) = all.split_at(MAX_ADMITTING);
    let (seventeenth, eighteenth) = (&rest[0], &rest[1]);
    // Every live session reads the two devices this test listens to at its end.
    let roster = Roster::new([first[0].key, seventeenth.key]);
    // Devices the workspace does not hold: as many as wait in the strangers' places, and one more.
    let unknown = strangers(MAX_ADMITTING_STRANGERS + 1).await;
    let (waiting, stranger) = unknown.split_at(MAX_ADMITTING_STRANGERS);
    let stranger = &stranger[0];
    // No wait of the test's own ends a dial: what a device is told is the relay's answer.
    let long = tilo_test_env::hang_bound();
    let asks = |member: &Member| {
        ask(
            member.clone(),
            at.clone(),
            roster.clone(),
            keys.clone(),
            long,
        )
    };

    // Sixteen devices dial at once. The relay takes each into admission, where its authority
    // holds it: none is admitted yet, and none was turned back.
    let admitting: Vec<_> = first
        .iter()
        .map(|member| tokio::spawn(asks(member)))
        .collect();
    wait_until("sixteen sessions are in admission", || {
        authority.asked() == MAX_ADMITTING
    });
    assert_eq!(relay.counters().busy, 0);

    // One more is told the relay is busy, and the relay asked its authority nothing about it.
    assert_eq!(
        asks(seventeenth).await.err(),
        Some(LiveError::Busy),
        "a member that arrives while sixteen are in admission is told the relay is busy"
    );
    assert_eq!(
        authority.asked(),
        MAX_ADMITTING,
        "the relay asks its authority nothing about a session it has no room for"
    );
    let counters = relay.counters();
    assert_eq!(
        (counters.busy, counters.rejected, counters.accepted),
        (1, 0, 0),
        "a session closed as busy is counted as that, and as nobody's refusal"
    );
    // Devices the workspace does not hold are taken into places of their own, though every place
    // of the workspace's devices is taken: they hold none of those, and are judged like anyone.
    let unjudged: Vec<_> = waiting
        .iter()
        .map(|stranger| tokio::spawn(asks(stranger)))
        .collect();
    wait_until("the strangers are in admission beside the sixteen", || {
        authority.asked() == MAX_ADMITTING + MAX_ADMITTING_STRANGERS
    });
    // One more of them is told busy: busy says nothing about who asked.
    assert_eq!(
        asks(stranger).await.err(),
        Some(LiveError::Busy),
        "a busy relay judges nobody, a device it would refuse included"
    );
    assert_eq!(authority.asked(), MAX_ADMITTING + MAX_ADMITTING_STRANGERS);
    assert_eq!(
        asks(seventeenth).await.err(),
        Some(LiveError::Busy),
        "the strangers' places are no room for a member, as a member's are none for a stranger"
    );

    // One of the sixteen gets through its admission: the next that asks is taken, which makes
    // sixteen in admission again, so the one after it is told busy.
    authority.let_through(1);
    wait_until("one of the sixteen is through its admission", || {
        admitted(&relay) == 1
    });
    let next = tokio::spawn(asks(seventeenth));
    wait_until("the seventeenth is taken into admission", || {
        authority.asked() == MAX_ADMITTING + MAX_ADMITTING_STRANGERS + 1
    });
    assert_eq!(
        asks(eighteenth).await.err(),
        Some(LiveError::Busy),
        "the relay is at its bound again"
    );

    // Everyone the relay took into admission is judged: the members are let in, and so is the
    // member that was told busy, when it asks again; the strangers are refused.
    authority.let_through(4096);
    let mut live_sessions = Vec::new();
    for joining in admitting {
        live_sessions.push(
            joining
                .await
                .unwrap()
                .expect("a member the relay took into admission is admitted"),
        );
    }
    live_sessions.push(
        next.await
            .unwrap()
            .expect("the member told busy is admitted when it asks again"),
    );
    live_sessions.push(
        asks(eighteenth)
            .await
            .expect("the second member told busy is admitted when it asks again"),
    );
    wait_until("every member is in the live session", || {
        admitted(&relay) == MAX_ADMITTING + 2
    });
    for judged in unjudged {
        assert_eq!(
            judged.await.unwrap().err(),
            Some(LiveError::Denied),
            "a device the workspace does not hold is refused once it is judged"
        );
    }
    // The device the workspace does not hold that was told busy is refused now that the relay
    // judges it: being told busy before changed nothing about who is let in.
    assert_eq!(
        asks(stranger).await.err(),
        Some(LiveError::Denied),
        "a device the workspace does not hold is refused once the relay has room to judge it"
    );
    wait_until("the refusals are counted", || {
        usize::try_from(relay.counters().rejected).unwrap() == MAX_ADMITTING_STRANGERS + 1
    });
    let counters = relay.counters();
    assert_eq!(
        (counters.busy, admitted(&relay)),
        (4, MAX_ADMITTING + 2),
        "four sessions were told busy, and every member is in"
    );

    // The member that was told busy first is in the live session like any other: what it says reaches
    // the first device.
    let mut sequence = 1;
    wait_until("the member told busy is heard", || {
        live_sessions[MAX_ADMITTING]
            .publish(Track::Presence, sequence, b"in=after-busy")
            .unwrap();
        sequence = sequence.saturating_add(1);
        heard(&live_sessions[0], Track::Presence)
            .iter()
            .any(|object| object.body == b"in=after-busy" && object.sender == seventeenth.key)
    });
}

/// A relay that goes away while it admits a session was not reached: the device reads neither a
/// refusal nor a busy relay, and asks whichever device relays the workspace then.
#[tokio::test(flavor = "multi_thread")]
async fn a_relay_that_goes_away_while_it_admits_a_session_was_not_reached() {
    let authority = Authority::holding();
    let relay_endpoint = endpoint().await;
    let relay = Relay::new(authority.clone());
    let router = serve(&relay, &relay_endpoint);
    let member = members(&authority, 1).await.remove(0);
    let joining = tokio::spawn(ask(
        member,
        addr(&relay_endpoint),
        authority.roster(),
        keys(),
        tilo_test_env::hang_bound(),
    ));
    wait_until("the session is in admission", || authority.asked() == 1);
    router.shutdown().await.unwrap();
    assert_eq!(
        joining.await.unwrap().err(),
        Some(LiveError::Unavailable),
        "a relay that went away mid-admission was not reached"
    );
    assert_eq!(relay.counters().busy, 0);
}

/// A live session whose carrier went away dials its relay again. A relay that is admitting as many
/// sessions as it takes just then judged nobody, so the live session is not over: it keeps asking,
/// waiting longer each time, and is carried again once the relay has room.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_session_that_dials_a_busy_relay_again_keeps_asking_until_it_is_carried() {
    let authority = Authority::holding();
    let relay_endpoint = endpoint().await;
    let relay = Relay::new(authority.clone());
    let carriers = Carriers::default();
    let _router = serve_cuttable(&relay, &relay_endpoint, &carriers);
    let at = addr(&relay_endpoint);
    let keys = keys();
    let all = members(&authority, MAX_ADMITTING + 1).await;
    let long = tilo_test_env::hang_bound();
    let (stayed, others) = all.split_first().unwrap();
    // Every live session reads the two devices this test listens to at its end.
    let roster = Roster::new([stayed.key, others[0].key]);

    // One device is in the live session.
    authority.let_through(1);
    let live_session = ask(
        stayed.clone(),
        at.clone(),
        roster.clone(),
        keys.clone(),
        long,
    )
    .await
    .unwrap();
    wait_until("the device is in the live session", || {
        admitted(&relay) == 1
    });

    // Sixteen more arrive at once and are in admission when its carrier goes away, as when a
    // workspace's devices all come back together.
    let admitting: Vec<_> = others
        .iter()
        .map(|member| {
            tokio::spawn(ask(
                member.clone(),
                at.clone(),
                roster.clone(),
                keys.clone(),
                long,
            ))
        })
        .collect();
    wait_until("sixteen sessions are in admission", || {
        authority.asked() == MAX_ADMITTING + 1
    });
    carriers.cut(stayed.key);
    wait_until(
        "the live session dialled the busy relay again, twice",
        || relay.counters().busy >= 2,
    );
    assert!(
        !live_session.ended(),
        "a live session outlasts a relay that had no room for it when it dialled again"
    );
    assert_eq!(
        (authority.asked(), relay.counters().rejected),
        (MAX_ADMITTING + 1, 0),
        "the relay judged nobody while it was busy"
    );

    // The relay has room again: the sixteen are let in, and the live session's next dial is taken.
    authority.let_through(4096);
    let mut live_sessions = Vec::new();
    for joining in admitting {
        live_sessions.push(joining.await.unwrap().unwrap());
    }
    wait_until("the live session is carried again", || {
        admitted(&relay) == MAX_ADMITTING + 2
    });
    assert!(
        !live_session.ended(),
        "the live session that is carried again is the one that lost its carrier"
    );
    let mut sequence = 1;
    wait_until("the live session says on the carrier it dialled", || {
        live_session
            .publish(Track::Presence, sequence, b"shell=back")
            .unwrap();
        sequence = sequence.saturating_add(1);
        heard(&live_sessions[0], Track::Presence)
            .iter()
            .any(|object| object.body == b"shell=back" && object.sender == stayed.key)
    });
}

/// `member` asks the relay at `relay` for the workspace's live session until it is in: a relay that is
/// admitting as many sessions as it takes judged nobody, and is asked again, as a session asks it.
async fn ask_until_in(
    member: Member,
    relay: EndpointAddr,
    roster: Roster,
    keys: ContentKeys,
) -> LiveSession {
    let mut wait = Duration::from_millis(20);
    loop {
        let asked = ask(
            member.clone(),
            relay.clone(),
            roster.clone(),
            keys.clone(),
            tilo_test_env::hang_bound(),
        )
        .await;
        match asked {
            Ok(live_session) => return live_session,
            Err(LiveError::Busy) => {
                tokio::time::sleep(wait).await;
                wait = wait.saturating_mul(2).min(Duration::from_millis(500));
            }
            Err(error) => panic!("a member was not taken into the live session: {error}"),
        }
    }
}

/// The devices `live_session` heard say something on their presence track since it was last asked,
/// added to `heard`.
fn hears(live_session: &LiveSession, heard: &mut BTreeSet<[u8; 32]>) {
    while let Some(object) = live_session.try_recv() {
        if object.track == Track::Presence && !object.body.is_empty() {
            heard.insert(object.sender);
        }
    }
}

/// Have every live session of `live_sessions` say where it is, again and again, until `done` says of what each
/// of them heard that it is enough: who each live session heard, in the live sessions' order. `sequence`
/// numbers what they say, across every call for the same live sessions. A wait past the suite's bound
/// fails with how many devices each heard and how many it says it does not hear.
async fn said_until(
    what: &str,
    live_sessions: &[LiveSession],
    sequence: &mut u32,
    mut done: impl FnMut(&[BTreeSet<[u8; 32]>]) -> bool,
) -> Vec<BTreeSet<[u8; 32]>> {
    let deadline = tilo_test_env::hang_deadline();
    let mut heard = vec![BTreeSet::new(); live_sessions.len()];
    let mut said: Option<std::time::Instant> = None;
    loop {
        if said.is_none_or(|at| at.elapsed() >= Duration::from_secs(1)) {
            // A track's objects are numbered upwards for as long as its live session lives.
            *sequence = sequence.saturating_add(1);
            for live_session in live_sessions {
                live_session
                    .publish(Track::Presence, *sequence, b"here")
                    .unwrap();
            }
            said = Some(std::time::Instant::now());
        }
        for (live_session, heard) in live_sessions.iter().zip(heard.iter_mut()) {
            hears(live_session, heard);
        }
        if done(&heard) {
            return heard;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{what}: each live session heard {:?} devices and says of the rest {:?}",
            heard.iter().map(BTreeSet::len).collect::<Vec<_>>(),
            live_sessions
                .iter()
                .map(LiveSession::unheard)
                .collect::<Vec<_>>(),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// `devices` devices of one workspace in its live session through one relay, every one asking at once
/// as the devices of a workspace do when its host comes back: the relay, which must outlive the
/// live sessions, the devices and their live sessions.
async fn a_live_session_of(
    devices: usize,
) -> (
    (Relay, iroh::protocol::Router, Arc<Authority>),
    Vec<Member>,
    Vec<LiveSession>,
) {
    let authority = Authority::new();
    let relay_endpoint = endpoint().await;
    let relay = Relay::new(authority.clone());
    let serving = serve(&relay, &relay_endpoint);
    let at = addr(&relay_endpoint);
    let keys = keys();
    let all = members(&authority, devices).await;
    let roster = authority.roster();
    let joining: Vec<_> = all
        .iter()
        .map(|member| {
            tokio::spawn(ask_until_in(
                member.clone(),
                at.clone(),
                roster.clone(),
                keys.clone(),
            ))
        })
        .collect();
    let mut live_sessions = Vec::new();
    for joining in joining {
        live_sessions.push(joining.await.unwrap());
    }
    ((relay, serving, authority), all, live_sessions)
}

/// As many devices as one live session reads, and the one that reads them: every one of them hears
/// every other, through a relay their endpoints reach with the few streams a connection is
/// allowed before its session is admitted. Sixteen streams a connection, which is what the
/// carrier's transport allowed before, let a device hear four others and left the rest unread
/// without a word.
#[tokio::test(flavor = "multi_thread")]
async fn every_device_of_a_full_live_session_hears_every_other() {
    let devices = MAX_HEARD + 1;
    let (_relay, members, live_sessions) = a_live_session_of(devices).await;
    let mut sequence = 0;
    let heard = said_until(
        "every device hears every other",
        &live_sessions,
        &mut sequence,
        |heard| heard.iter().all(|heard| heard.len() == devices - 1),
    )
    .await;
    for (index, (member, heard)) in members.iter().zip(&heard).enumerate() {
        let others: BTreeSet<[u8; 32]> = members
            .iter()
            .map(|other| other.key)
            .filter(|key| *key != member.key)
            .collect();
        assert_eq!(heard, &others, "device {index} hears exactly the others");
    }
    for (index, live_session) in live_sessions.iter().enumerate() {
        assert_eq!(
            live_session.unheard(),
            0,
            "device {index} reads every device that is in the live session"
        );
    }
}

/// A live session reads [`MAX_HEARD`] devices at once. With more of them in the workspace, each device
/// hears that many and says how many it does not hear and that they are beyond what it reads:
/// nobody is absent without a word. Once devices go, the ones that were beyond are read in their
/// place, and nothing is left unheard.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_beyond_what_a_live_session_reads_is_counted_and_read_once_there_is_room() {
    let beyond = 3;
    let devices = MAX_HEARD + 1 + beyond;
    let (_relay, members, mut live_sessions) = a_live_session_of(devices).await;
    let told = Arc::new(AtomicUsize::new(0));
    let counting = told.clone();
    live_sessions[0].set_unheard_changed(Arc::new(move || {
        counting.fetch_add(1, Ordering::SeqCst);
    }));
    let mut sequence = 0;
    let heard = said_until(
        "every device hears as many as it reads and counts the rest",
        &live_sessions,
        &mut sequence,
        |heard| {
            heard.iter().all(|heard| heard.len() == MAX_HEARD)
                && live_sessions
                    .iter()
                    .all(|live_session| live_session.unheard() == beyond)
        },
    )
    .await;
    for (index, heard) in heard.iter().enumerate() {
        assert_eq!(
            heard.len(),
            MAX_HEARD,
            "device {index} hears as many devices as a live session reads, and no more"
        );
    }

    // As many devices as were beyond leave. Whoever read them reads the ones it did not hear in
    // their place, and whoever did not hear them has nobody left that it does not hear.
    live_sessions.truncate(devices - beyond);
    let stayed: BTreeSet<[u8; 32]> = members[..devices - beyond]
        .iter()
        .map(|member| member.key)
        .collect();
    said_until(
        "every device that stayed hears every other, and counts nobody as unheard",
        &live_sessions,
        &mut sequence,
        |heard| {
            heard.iter().zip(&members).all(|(heard, member)| {
                stayed
                    .iter()
                    .filter(|key| **key != member.key)
                    .all(|key| heard.contains(key))
            }) && live_sessions
                .iter()
                .all(|live_session| live_session.unheard() == 0)
        },
    )
    .await;
    assert!(
        told.load(Ordering::SeqCst) >= 2,
        "a live session tells whoever reads it when the devices it does not hear change"
    );
}

/// Anyone who reaches a relay can open its protocol and say nothing, and the relay holds each
/// such session in admission for as long as a preface may take. Endpoints the workspace's roster
/// does not name hold only the strangers' places that way: sixteen of them at once take four, the
/// rest are told busy, and every device of the workspace that asks meanwhile is taken in at once,
/// sixteen at a time as before. The relay judged nobody but the members.
#[tokio::test(flavor = "multi_thread")]
async fn endpoints_no_roster_names_cannot_keep_a_workspaces_devices_out() {
    let authority = Authority::new();
    let relay_endpoint = endpoint().await;
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let at = addr(&relay_endpoint);
    let keys = keys();
    let all = members(&authority, MAX_ADMITTING).await;
    let roster = authority.roster();

    // Sixteen endpoints nobody knows open the live session's protocol and say nothing.
    let silent = strangers(MAX_ADMITTING).await;
    let mut holding = Vec::new();
    for stranger in &silent {
        holding.push(
            stranger
                .endpoint
                .connect(at.clone(), native::ALPN)
                .await
                .expect("anyone reaches the relay's endpoint"),
        );
    }
    wait_until("the strangers past their places are told busy", || {
        usize::try_from(relay.counters().busy).unwrap() == MAX_ADMITTING - MAX_ADMITTING_STRANGERS
    });
    assert_eq!(
        authority.asked(),
        0,
        "a session that says nothing is nobody the authority is asked about"
    );

    // Every device of the workspace asks at once, one ask each: none is told busy.
    let joining: Vec<_> = all
        .iter()
        .map(|member| {
            tokio::spawn(ask(
                member.clone(),
                at.clone(),
                roster.clone(),
                keys.clone(),
                tilo_test_env::hang_bound(),
            ))
        })
        .collect();
    let mut live_sessions = Vec::new();
    for (index, joining) in joining.into_iter().enumerate() {
        live_sessions.push(joining.await.unwrap().unwrap_or_else(|error| {
            panic!("member {index} was kept out by endpoints nobody knows: {error}")
        }));
    }
    wait_until("every member is in the live session", || {
        admitted(&relay) == MAX_ADMITTING
    });
    assert_eq!(
        authority.asked(),
        MAX_ADMITTING,
        "the relay judged the members and nobody else"
    );
    assert_eq!(
        usize::try_from(relay.counters().busy).unwrap(),
        MAX_ADMITTING - MAX_ADMITTING_STRANGERS,
        "no member was told busy"
    );
    assert!(
        live_sessions
            .iter()
            .all(|live_session| !live_session.ended()),
        "every member's live session is carried"
    );
    assert_eq!(
        holding.len(),
        MAX_ADMITTING,
        "the endpoints nobody knows kept their sessions open all along"
    );
}

/// One endpoint holds at most [`MAX_ADMITTING_OF_ONE`] places in admission, however many
/// sessions it opens: a device of the workspace that opens sixteen at once has four taken and the
/// rest told busy, the other devices are taken into the places it could not hold, and the relay
/// is at its bound only once they fill them.
#[tokio::test(flavor = "multi_thread")]
async fn one_endpoint_holds_few_places_however_many_sessions_it_opens() {
    let authority = Authority::holding();
    let relay_endpoint = endpoint().await;
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let at = addr(&relay_endpoint);
    let keys = keys();
    let rest = MAX_ADMITTING - MAX_ADMITTING_OF_ONE;
    let all = members(&authority, 1 + rest + 1).await;
    let (one, others) = all.split_first().unwrap();
    let (others, last) = others.split_at(rest);
    let roster = Roster::new([one.key]);
    let long = tilo_test_env::hang_bound();
    let asks = |member: &Member| {
        ask(
            member.clone(),
            at.clone(),
            roster.clone(),
            keys.clone(),
            long,
        )
    };

    // One device opens sixteen sessions at once. Four are taken into admission, where the
    // authority holds them; the others are told busy.
    let opened: Vec<_> = (0..MAX_ADMITTING)
        .map(|_| tokio::spawn(asks(one)))
        .collect();
    wait_until(
        "the device's sessions past its places are told busy",
        || {
            usize::try_from(relay.counters().busy).unwrap() == rest
                && authority.asked() == MAX_ADMITTING_OF_ONE
                && opened
                    .iter()
                    .filter(|session| session.is_finished())
                    .count()
                    == rest
        },
    );
    let mut told_busy = 0;
    let mut waiting = Vec::new();
    for session in opened {
        if session.is_finished() {
            assert_eq!(session.await.unwrap().err(), Some(LiveError::Busy));
            told_busy += 1;
        } else {
            waiting.push(session);
        }
    }
    assert_eq!(
        (told_busy, waiting.len()),
        (rest, MAX_ADMITTING_OF_ONE),
        "one endpoint holds four places, and its further sessions are told busy"
    );

    // The other devices of the workspace are taken into the places it could not hold.
    let admitting: Vec<_> = others
        .iter()
        .map(|member| tokio::spawn(asks(member)))
        .collect();
    wait_until("the other devices are in admission", || {
        authority.asked() == MAX_ADMITTING
    });
    assert_eq!(
        usize::try_from(relay.counters().busy).unwrap(),
        rest,
        "no other device was told busy"
    );
    // Only now is the relay at its bound.
    assert_eq!(
        asks(&last[0]).await.err(),
        Some(LiveError::Busy),
        "sixteen sessions of the workspace's devices are in admission"
    );

    authority.let_through(4096);
    for session in waiting.into_iter().chain(admitting) {
        session
            .await
            .unwrap()
            .expect("a session the relay took into admission is admitted");
    }
    asks(&last[0])
        .await
        .expect("the device told busy is admitted when it asks again");
}

/// The workspace removes one member of a call and its media stops end to end: the relay cuts its
/// session, it can publish nothing more and the remaining device hears nothing more of it, while
/// its own session stays open and keeps publishing.
#[tokio::test(flavor = "multi_thread")]
async fn a_removed_members_media_stops_end_to_end_and_the_other_session_carries_on() {
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let keys = keys();
    let a = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint, addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = LiveSession::join(
        Arc::new(native::Iroh::new(b_endpoint, addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster, keys.clone()),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);

    // Media flows both ways before the workspace changes.
    talk(&[&a, &b], &keys, 1);
    for sequence in 1..=5_u32 {
        a.publish(Track::Audio, sequence, &voice(sequence, &[1; 8]))
            .unwrap();
        b.publish(Track::Audio, sequence, &voice(sequence, &[2; 8]))
            .unwrap();
    }
    a.publish(Track::Presence, 2, b"member=a").unwrap();
    b.publish(Track::Presence, 2, b"member=b").unwrap();
    let (mut a_heard, mut b_heard) = (0, 0);
    wait_until("each member's media reaches the other", || {
        a_heard += heard(&a, Track::Audio)
            .into_iter()
            .filter(|object| object.sender == b_key)
            .count();
        b_heard += heard(&b, Track::Audio)
            .into_iter()
            .filter(|object| object.sender == a_key)
            .count();
        a_heard >= 5 && b_heard >= 5
    });
    wait_until("the other member's presence is heard", || {
        a.presence_from()
            .get(&b_key)
            .is_some_and(|(said, _)| *said > 0)
    });

    // The workspace removes B. The relay cuts its session, A is told its broadcast ended, and both
    // sides can carry nothing more of it.
    authority.remove(b_key);
    wait_until("the relay cuts the removed member's session", || {
        relay.counters().expired >= 1
    });
    wait_until(
        "the removed member's broadcast ends at the other device",
        || {
            a.presence_from()
                .get(&b_key)
                .is_some_and(|(_, left)| *left > 0)
        },
    );
    wait_until("the removed member's session ends", || b.ended());
    // Everything of it that was on its way before the cut has arrived (all five objects did), so
    // from here on nothing of it can be heard or sent.
    assert!(
        !heard(&a, Track::Audio)
            .into_iter()
            .any(|object| object.sender == b_key && object.sequence > 5),
        "the removed member's media arrived after its removal"
    );
    assert_eq!(
        b.publish(Track::Audio, 6, &voice(6, &[3; 8])),
        Err(LiveError::Ended),
        "the removed member's session is over"
    );

    // The remaining device is untouched: its session stays open and it keeps publishing.
    assert!(
        !a.ended(),
        "the other member's session closed with the removal"
    );
    assert_eq!(
        a.publish(Track::Presence, 3, b"still=here"),
        Ok(()),
        "the remaining member keeps publishing"
    );
}

/// A silent voice stream costs the relay a fraction of a speaking one: Opus DTX sends a
/// comfort-noise update every 400 ms while nobody speaks, where speech is one object every 20 ms,
/// and the relay forwards what the publisher sends once and adds nothing on a clean hop. A second
/// of silence is therefore a small multiple less than a second of speech, by objects and by bytes.
#[tokio::test(flavor = "multi_thread")]
async fn a_silent_voice_stream_costs_the_relay_a_fraction_of_a_speaking_one() {
    // One second of each, as the encoder sends them: fifty 20 ms speech frames, and the three
    // comfort-noise updates DTX keeps the receiver's noise floor with.
    const SPEECH_OBJECTS: usize = 50;
    const SPEECH_BYTES: usize = 80;
    const SILENCE_OBJECTS: usize = 3;
    const SILENCE_BYTES: usize = 40;

    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let keys = keys();
    let a = LiveSession::join(
        Arc::new(native::Iroh::new(a_endpoint, addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = LiveSession::join(
        Arc::new(native::Iroh::new(b_endpoint, addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster, keys.clone()),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);

    // Each object is waited for before the next, so what arrives is exactly what was sent: a burst
    // on a loaded machine cannot hide a drop or pass one off as two.
    talk(&[&a, &b], &keys, 1);
    let mut sequence = 1_u32;
    let mut speaking = Vec::new();
    for _ in 0..SPEECH_OBJECTS {
        let before = speaking.len();
        a.publish(Track::Audio, sequence, &voice(sequence, &[7; SPEECH_BYTES]))
            .unwrap();
        sequence = sequence.saturating_add(1);
        wait_until("a speech frame reaches the other device", || {
            speaking.extend(
                heard(&b, Track::Audio)
                    .into_iter()
                    .filter(|object| object.sender == a_key),
            );
            speaking.len() > before
        });
    }
    let mut silent = Vec::new();
    for _ in 0..SILENCE_OBJECTS {
        let before = silent.len();
        a.publish(
            Track::Audio,
            sequence,
            &voice(sequence, &[0; SILENCE_BYTES]),
        )
        .unwrap();
        sequence = sequence.saturating_add(1);
        wait_until("a comfort-noise update reaches the other device", || {
            silent.extend(
                heard(&b, Track::Audio)
                    .into_iter()
                    .filter(|object| object.sender == a_key),
            );
            silent.len() > before
        });
    }

    let bytes =
        |objects: &[Heard]| -> usize { objects.iter().map(|object| object.body.len()).sum() };
    assert_eq!(
        (speaking.len(), silent.len()),
        (SPEECH_OBJECTS, SILENCE_OBJECTS),
        "the relay forwarded each object once"
    );
    assert!(
        bytes(&silent).saturating_mul(10) < bytes(&speaking),
        "silence cost {} bytes against speech's {}",
        bytes(&silent),
        bytes(&speaking)
    );
}

/// A call's rule changed to leave one device out: the devices it still admits seal what they say
/// from then on under the keys of the rule's new generation, which the device left out was never
/// given. Carried to it all the same (this relay lets every call through) and held to nothing by
/// its own client, it opens none of it with every key it had, while the others hear each other.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_a_call_left_out_opens_nothing_said_after_with_every_key_it_had() {
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint, c_endpoint) = (
        endpoint().await,
        endpoint().await,
        endpoint().await,
        endpoint().await,
    );
    let (a_key, b_key, c_key) = (
        *a_endpoint.id().as_bytes(),
        *b_endpoint.id().as_bytes(),
        *c_endpoint.id().as_bytes(),
    );
    let (a_cert, b_cert, c_cert) = (
        authority.admit_device(a_key),
        authority.admit_device(b_key),
        authority.admit_device(c_key),
    );
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let (workspace_keys, call) = (keys(), keys());
    let mut sessions = Vec::new();
    for (endpoint, key, cert) in [
        (a_endpoint, a_key, &a_cert),
        (b_endpoint, b_key, &b_cert),
        (c_endpoint, c_key, &c_cert),
    ] {
        sessions.push(
            LiveSession::join(
                Arc::new(native::Iroh::new(endpoint, addr(&relay_endpoint))),
                &grant_for(cert),
                join(key, roster.clone(), workspace_keys.clone()),
            )
            .await
            .unwrap(),
        );
    }
    let [a, b, c] = <[LiveSession; 3]>::try_from(sessions).ok().unwrap();
    wait_until("the sessions admitted", || relay.counters().accepted == 3);
    talk(&[&a, &b, &c], &call, 1);
    // What A says, and the last thing it said before the call moved on: only what it says after
    // counts as said after.
    let (sequence, before_the_change) = (std::cell::Cell::new(0_u32), std::cell::Cell::new(0));
    let speak = |heard_by: &LiveSession| {
        sequence.set(sequence.get().saturating_add(1));
        let _ = a.publish(
            Track::Audio,
            sequence.get(),
            &voice(sequence.get(), &[5; 8]),
        );
        heard(heard_by, Track::Audio)
            .into_iter()
            .filter(|object| object.sender == a_key && object.sequence > before_the_change.get())
            .count()
    };
    wait_until("the device later left out hears the call", || speak(&c) > 0);

    // The call denies C's person: A and B are told, and handed the keys of the rule's next
    // generation. C keeps the call's keys it had, and the workspace's.
    let persons: BTreeMap<[u8; 32], String> = [(a_key, "a"), (b_key, "b"), (c_key, "c")]
        .into_iter()
        .map(|(device, person)| (device, person.to_owned()))
        .collect();
    let rules = BTreeMap::from([(
        CALL.to_owned(),
        collab_types::calls::CallRule {
            denied: BTreeMap::from([("c".to_owned(), collab_types::calls::Directions::BOTH)]),
            generation: 1,
            ..collab_types::calls::CallRule::default()
        },
    )]);
    let next = keys();
    before_the_change.set(sequence.get());
    for honest in [&a, &b] {
        honest.set_calls(rules.clone(), persons.clone());
        assert_eq!(
            honest.call_keys_wanted(),
            Some((CALL.to_owned(), 1)),
            "the change asks for the next generation's keys"
        );
        assert_eq!(
            honest.publish(Track::Audio, 1, &voice(1, &[6; 8])),
            Err(LiveError::Withheld),
            "nothing is sealed under the keys the change left behind"
        );
        honest.set_call_keys(CALL, 1, next.clone());
    }
    wait_until("B hears A under the new keys", || speak(&b) > 0);
    let before = c.refusals().values().sum::<usize>();
    let mut opened = 0;
    wait_until("C is carried what A says and opens none of it", || {
        opened += speak(&c);
        c.refusals().values().sum::<usize>() > before.saturating_add(3)
    });
    assert_eq!(opened, 0, "C opened what was said after it was left out");
}
