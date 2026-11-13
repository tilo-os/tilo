//! The live plane between native devices: a relay role on one device's own
//! endpoint, planes on the others, sealed presence and audio through it, and
//! a member removed from the workspace cut on the event.
//!
//! The relay and the planes here are the same code a session runs: the
//! relay's ALPN handler is [`tilo_live::native::handler`] over
//! [`Relay::serve_session`], the planes are [`Plane::join`] over
//! [`tilo_live::native::Iroh`], and every object is sealed under the scope's
//! channel content keys. Only the authority is the test's: it reads the grants
//! the way a session's authority does from device certificates and the
//! workspace roster.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion; the scenario is a workspace's live session"
)]
#![allow(
    clippy::too_many_lines,
    reason = "each test is one workspace session end to end"
)]

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use iroh::endpoint::{Connection, VarInt};
use iroh::{Endpoint, EndpointAddr, RelayMode};
use tilo_live::frame::PlaneKeys;
use tilo_live::grant;
use tilo_live::live::{self, Namespace, Track};
use tilo_live::native;
use tilo_live::plane::{self, Heard, Join, Plane, PlaneError};
use tilo_live::relay::{
    Admission, AuthorizationStatus, Denied, Direction, Peer, Relay, RelayAuthority,
    SessionAuthorization,
};
use tilo_live::roster::Roster;
use tilo_live::scope::Scope;
use tilo_live::voice::{FRAME_SAMPLES, VoiceObject};
use tilo_test_env::wait_until;

const WORKSPACE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

/// The scope every participant of the test joins: the workspace itself.
fn scope() -> Scope {
    Scope::workspace(WORKSPACE)
}

/// The scope's channel content keys at epoch 1, as the workspace's own
/// channel hands them to its members.
fn keys() -> ChannelContentKeys {
    let channel = tilo_live::scope::Scope::workspace(WORKSPACE).channel_id();
    let owner = MemberKey::generate("owner").unwrap();
    ChannelGroup::create(channel, owner)
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
}

impl Authority {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: 1,
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

    /// The roster the planes open with.
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
        Box::pin(std::future::ready(self.decide(peer, proof)))
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

fn join(device: [u8; 32], roster: Roster, keys: impl Into<PlaneKeys>) -> Join {
    Join {
        scope: scope(),
        device,
        keys: keys.into(),
        roster,
        publish: true,
    }
}

/// The relay role on its own endpoint: the plane's ALPN handler on the
/// session's router.
fn serve(relay: &Relay, endpoint: &Endpoint) -> iroh::protocol::Router {
    let relay = relay.clone();
    iroh::protocol::Router::builder(endpoint.clone())
        .accept(
            native::ALPN,
            native::handler(move |session: native::Session| {
                let relay = relay.clone();
                async move {
                    let peer = Peer(session.remote());
                    relay.serve_session(session, peer).await;
                }
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
                    let peer = Peer(session.remote());
                    relay.serve_session(session, peer).await;
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
        payload: payload.to_vec(),
    }
    .encode()
    .unwrap()
}

/// Everything `plane` heard on `track` so far.
fn heard(plane: &Plane, track: Track) -> Vec<Heard> {
    let mut bodies = Vec::new();
    while let Some(heard) = plane.try_recv() {
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
    let relay_plane =
        Plane::local(&relay, &join(relay_key, authority.roster(), keys.clone())).unwrap();

    let a = Plane::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = Plane::join(
        Arc::new(native::Iroh::new(b_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);

    // Sealed presence: the relay copies it unread, and both the dialled
    // participant and the relay's own plane open it under the scope's keys.
    a.publish(Track::Presence, 1, b"role=member").unwrap();
    wait_until("presence reaches the other participant", || {
        heard(&b, Track::Presence)
            .iter()
            .any(|object| object.body == b"role=member" && object.sender == a_key)
    });
    wait_until("presence reaches the relay's own participant", || {
        heard(&relay_plane, Track::Presence)
            .iter()
            .any(|object| object.body == b"role=member" && object.sender == a_key)
    });

    // Sealed audio: five 20 ms objects, one group each, opened on the far
    // side exactly as they were published.
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
        a.publish(Track::Presence, 2, b"role=member"),
        Ok(()),
        "the remaining participant keeps publishing"
    );

    // A device removed before it dials is refused at the preface and never
    // reaches MoQ.
    let c_endpoint = endpoint().await;
    let c_key = *c_endpoint.id().as_bytes();
    let refused = Plane::join(
        Arc::new(native::Iroh::new(c_endpoint, addr(&relay_endpoint))),
        &grant_for(b"cert-not-on-the-roster"),
        join(c_key, authority.roster(), keys.clone()),
    )
    .await;
    assert!(
        matches!(refused, Err(PlaneError::Denied)),
        "refused at the preface"
    );

    // The plane is not tied to one relay: when the appetite moves, the same
    // device joins the new relay and is heard there.
    let moved_endpoint = endpoint().await;
    let moved = Relay::new(authority.clone());
    let _moved_router = serve(&moved, &moved_endpoint);
    // The dial is the only handle to `a`'s iroh endpoint: dropping it closes
    // the endpoint and the connection with it, so the plane keeps it for as
    // long as it runs.
    let a_again = Plane::join(
        Arc::new(native::Iroh::new(a_endpoint, addr(&moved_endpoint))),
        &grant_for(&a_cert),
        join(a_key, authority.roster(), keys.clone()),
    )
    .await
    .unwrap();
    wait_until("the moved relay admits the participant", || {
        moved.counters().accepted == 1
    });
    let moved_key = *moved_endpoint.id().as_bytes();
    authority.admit_device(moved_key);
    let moved_plane =
        Plane::local(&moved, &join(moved_key, authority.roster(), keys.clone())).unwrap();
    // Presence is a signal that repeats: one object published in the instant
    // between admission and the relay's forwarder attaching is not one a
    // subscriber is owed, so the test keeps sending while it waits.
    let mut sequence = 3;
    wait_until("the moved relay carries the sealed presence", || {
        a_again
            .publish(Track::Presence, sequence, b"relay=moved")
            .unwrap();
        sequence = sequence.saturating_add(1);
        heard(&moved_plane, Track::Presence)
            .iter()
            .any(|object| object.body == b"relay=moved")
    });
    assert_eq!(plane::TRACKS.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plane_seals_under_the_keys_it_now_holds() {
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);

    // `a` joins under the channel's first keys, `b` under the ones the
    // channel moved to. The dials hold each device's endpoint, and each plane
    // keeps its dial: dropping one closes the endpoint and the session with
    // it, so they live as long as the planes.
    let first = keys();
    let next = keys();
    let a = Plane::join(
        Arc::new(native::Iroh::new(a_endpoint, addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), first),
    )
    .await
    .unwrap();
    let b = Plane::join(
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

/// A participant the workspace admits while a plane is running is heard from
/// then on: the plane started with the roster of the moment it joined, and
/// the member arrives through [`Plane::set_roster`].
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_joins_the_roster_after_the_plane_started_is_heard() {
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let keys = keys();

    // `a` opens knowing only itself: the workspace had no other participant
    // when it joined. `b` is admitted afterwards, and `a` is told so.
    let a = Plane::join(
        Arc::new(native::Iroh::new(a_endpoint, addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, Roster::new([a_key]), keys.clone()),
    )
    .await
    .unwrap();
    let b = Plane::join(
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
    wait_until("the plane hears the device that joined its roster", || {
        b.publish(Track::Presence, sequence, b"joined=late")
            .unwrap();
        sequence = sequence.saturating_add(1);
        heard(&a, Track::Presence)
            .iter()
            .any(|object| object.body == b"joined=late" && object.sender == b_key)
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_that_reached_another_relay_is_heard_here() {
    let authority = Authority::new();
    let (alpha_endpoint, beta_endpoint, publisher_endpoint, member_endpoint) = (
        endpoint().await,
        endpoint().await,
        endpoint().await,
        endpoint().await,
    );
    let (alpha_key, beta_key) = (
        *alpha_endpoint.id().as_bytes(),
        *beta_endpoint.id().as_bytes(),
    );
    let (publisher_key, member_key) = (
        *publisher_endpoint.id().as_bytes(),
        *member_endpoint.id().as_bytes(),
    );
    let publisher_cert = authority.admit_device(publisher_key);
    let member_cert = authority.admit_device(member_key);
    let beta_cert = authority.admit_device(beta_key);
    authority.admit_device(alpha_key);
    let roster = authority.roster();

    // Two members of the workspace relay it. A member that reaches the first
    // of them and one that reaches the second are still one workspace.
    let alpha = Relay::new(authority.clone());
    let beta = Relay::new(authority.clone());
    let _alpha_router = serve(&alpha, &alpha_endpoint);
    let _beta_router = serve(&beta, &beta_endpoint);
    let _bridge = tilo_live::bridge::Bridge::join(
        &native::Iroh::new(beta_endpoint.clone(), addr(&alpha_endpoint)),
        &grant_for(&beta_cert),
        &scope(),
        &beta,
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    let keys = keys();
    let publisher = Plane::join(
        Arc::new(native::Iroh::new(
            publisher_endpoint.clone(),
            addr(&alpha_endpoint),
        )),
        &grant_for(&publisher_cert),
        join(publisher_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let member = Plane::join(
        Arc::new(native::Iroh::new(
            member_endpoint.clone(),
            addr(&beta_endpoint),
        )),
        &grant_for(&member_cert),
        join(member_key, roster, keys),
    )
    .await
    .unwrap();
    // Alpha carries the publisher and the bridge beta keeps into it; beta
    // carries the member that reached it.
    wait_until("both sessions admitted", || {
        alpha.counters().accepted >= 2 && beta.counters().accepted >= 1
    });

    publisher
        .publish(Track::Presence, 1, b"relay=alpha")
        .unwrap();
    wait_until("the member on the other relay hears the publisher", || {
        heard(&member, Track::Presence)
            .iter()
            .any(|object| object.body == b"relay=alpha" && object.sender == publisher_key)
    });
}

/// A participant that joins after another has said its presence hears it
/// within one frame of subscribing: the reader starts at the track's latest
/// group, not at whatever is said next. Nothing is published again here, so a
/// plane that only heard the next heartbeat could not pass.
#[tokio::test(flavor = "multi_thread")]
async fn a_plane_that_joins_after_presence_was_said_hears_it_without_a_new_one() {
    let authority = Authority::new();
    let (relay_endpoint, a_endpoint, b_endpoint) =
        (endpoint().await, endpoint().await, endpoint().await);
    let (a_key, b_key) = (*a_endpoint.id().as_bytes(), *b_endpoint.id().as_bytes());
    let (a_cert, b_cert) = (authority.admit_device(a_key), authority.admit_device(b_key));
    let roster = authority.roster();
    let relay = Relay::new(authority.clone());
    let _router = serve(&relay, &relay_endpoint);
    let keys = keys();

    let a = Plane::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    wait_until("the first participant is admitted", || {
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
    let b = Plane::join(
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

/// Everything `plane` heard on the presence track from `device` until `done`
/// holds of it, publishing through `say` between looks as a heartbeat would;
/// `what` names the wait.
async fn hear_until(
    plane: &Plane,
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
            heard(plane, Track::Presence)
                .into_iter()
                .filter(|object| object.sender == device),
        );
    }
    drawn
}

/// A device whose session ends and that joins again — `tilo mcp` run again on
/// the same computer — is heard by a plane that stayed the whole time, every
/// time. Each new session numbers its presence from one again, under the same
/// name as the one before. Some sessions end before the next one joins, and
/// the plane that stayed hears them leave; others are still held when the next
/// one joins, as a killed process leaves its session until the relay notices,
/// and the new one is heard all the same, and nothing of the old one after.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_joins_again_is_heard_by_a_plane_that_stayed() {
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
        Plane::join(
            Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
            &grant_for(&a_cert),
            join(a_key, roster.clone(), keys.clone()),
        )
        .await
        .unwrap()
    };
    let first = session().await;
    let b = Plane::join(
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
            &format!("run {run}: the plane that stayed never heard the device"),
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
            // what the plane that stayed hears: the device is still here.
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
            &format!("run {run}: the plane that stayed never heard the device leave"),
            || {},
            |drawn| drawn.iter().any(|object| object.body.is_empty()),
        )
        .await;
    }
}

/// A carrier taken away under a plane mid-session — a Wi-Fi network that moved, a laptop that slept:
/// the plane dials its relay again by itself, and while it is gone nobody it was reading is drawn as
/// if they were still there. The member's next word arrives as soon as the carrier is back.
#[tokio::test(flavor = "multi_thread")]
async fn a_plane_whose_carrier_goes_away_dials_its_relay_again() {
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

    let a = Plane::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = Plane::join(
        Arc::new(native::Iroh::new(b_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);

    // The member says where they are, and the other plane draws them there.
    a.publish(Track::Presence, 1, b"at=12,34").unwrap();
    wait_until("the member is heard", || {
        heard(&b, Track::Presence)
            .iter()
            .any(|object| object.body == b"at=12,34" && object.sender == a_key)
    });

    // The connection under the second plane goes away, as a dropped network does. It is not drawn
    // where they last were — they are said to have left as the session carrying them did — and the
    // plane itself is not over.
    carriers.cut(b_key);
    wait_until("the member is no longer drawn", || {
        heard(&b, Track::Presence)
            .iter()
            .any(|object| object.sender == a_key && object.body.is_empty())
    });
    assert!(
        !b.ended(),
        "the plane outlasts the carrier it was opened on"
    );

    // It dials the relay again, and the member's next word is drawn on the same plane.
    wait_until("the plane dialled its relay again", || {
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
            "the member is not drawn again on the plane whose carrier came back, in {:?}: \
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
        "the plane the member is drawn on is the one that lost its carrier"
    );

    // And the plane says again on the carrier it dialled: what this device publishes reaches the
    // member over the connection that replaced the one that went away.
    let mut sequence = 1;
    let since = std::time::Instant::now();
    while !heard(&a, Track::Presence)
        .iter()
        .any(|object| object.body == b"desk=here" && object.sender == b_key)
    {
        assert!(
            since.elapsed() < Duration::from_secs(10),
            "the plane does not say on the carrier it dialled again, in {:?}",
            since.elapsed()
        );
        b.publish(Track::Presence, sequence, b"desk=here").unwrap();
        sequence = sequence.saturating_add(1);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// One camera picture of `layer`, sealed and sent through `plane`.
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

/// Everything `plane` received on `track` so far, decoded.
fn pages(plane: &Plane, track: Track) -> Vec<tilo_live::page::PageObject> {
    let mut objects = Vec::new();
    while let Some(heard) = plane.try_recv() {
        if heard.track == track
            && let Ok(object) = tilo_live::page::PageObject::decode(&heard.body)
        {
            objects.push(object);
        }
    }
    objects
}

/// Everything `plane` received on `track` so far, decoded.
fn pictures(plane: &Plane, track: Track) -> Vec<tilo_live::video::VideoObject> {
    let mut objects = Vec::new();
    while let Some(heard) = plane.try_recv() {
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
    let a = Plane::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = Plane::join(
        Arc::new(native::Iroh::new(b_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster, keys),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);
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
        Err(plane::PlaneError::Unwanted)
    );

    // Showing nothing stops the layer's encoding and its track.
    b.show(a_key, &[]);
    wait_until("nothing is asked for", || a.wanted_pictures().is_empty());
    assert_eq!(
        a.send_picture(
            Source::Camera,
            &picture(Layer::Low, Source::Camera, sequence, true)
        ),
        Err(plane::PlaneError::Unwanted)
    );
}

/// A shared screen travels the plane as its own sealed track at the layer
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
    let a = Plane::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = Plane::join(
        Arc::new(native::Iroh::new(b_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&b_cert),
        join(b_key, roster, keys),
    )
    .await
    .unwrap();
    wait_until("both sessions admitted", || relay.counters().accepted == 2);

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
        Err(plane::PlaneError::Unwanted)
    );
}

/// One window's page travels the plane as its own sealed track: the publisher
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
    let a = Plane::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = Plane::join(
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
    let c = Plane::join(
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
        Err(PlaneError::Unwanted)
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
    let a = Plane::join(
        Arc::new(native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint))),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = Plane::join(
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
        Err(PlaneError::Unwanted),
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
        Err(PlaneError::Unwanted)
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
    let member = Plane::join(
        Arc::new(native::Iroh::new(member_endpoint, addr(&relay_endpoint))),
        &grant_for(&member_cert),
        join(member_key, roster.clone(), channel.clone()),
    )
    .await
    .unwrap();
    let confined = Plane::join(
        Arc::new(native::Iroh::new(confined_endpoint, addr(&relay_endpoint))),
        &grant_for(&confined_cert),
        join(confined_key, roster, handed),
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
    assert_eq!(
        confined.publish(Track::Audio, 1, &voice(0, b"hello")),
        Err(PlaneError::Refused(Refusal::NoKey(channel.epoch())))
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
