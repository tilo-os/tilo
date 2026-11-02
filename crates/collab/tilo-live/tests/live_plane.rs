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

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use iroh::{Endpoint, EndpointAddr, RelayMode};
use tilo_call_lane::Roster;
use tilo_live::grant;
use tilo_live::live::{self, Namespace, Track};
use tilo_live::native;
use tilo_live::plane::{self, Heard, Join, Plane, PlaneError};
use tilo_live::relay::{
    AuthorizationStatus, Denied, Direction, Peer, Relay, RelayAuthority, SessionAuthorization,
    VoiceRoute,
};
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
    fn admit(&self, peer: Peer, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let Peer::Native(device) = peer else {
            return Err(Denied);
        };
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
    )
    .unwrap()
    .encode()
}

fn join(device: [u8; 32], roster: Roster, keys: ChannelContentKeys) -> Join {
    Join {
        scope: scope(),
        device,
        keys,
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
                    let peer = Peer::Native(session.remote());
                    relay
                        .serve_session(VoiceRoute::negotiated(), session, peer)
                        .await;
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
        &native::Iroh::new(a_endpoint.clone(), addr(&relay_endpoint)),
        &grant_for(&a_cert),
        join(a_key, roster.clone(), keys.clone()),
    )
    .await
    .unwrap();
    let b = Plane::join(
        &native::Iroh::new(b_endpoint.clone(), addr(&relay_endpoint)),
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
        &native::Iroh::new(c_endpoint, addr(&relay_endpoint)),
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
    // the endpoint and the connection with it, so it lives as long as the
    // plane dialled through it.
    let carrier = native::Iroh::new(a_endpoint, addr(&moved_endpoint));
    let a_again = Plane::join(
        &carrier,
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
