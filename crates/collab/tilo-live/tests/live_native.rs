//! The carrier over iroh: two native endpoints and one in-process relay.
//!
//! Nothing here mocks the transport. Both endpoints are real iroh endpoints on
//! loopback, the relay is the same [`Relay`] a host runs, and the two tracks
//! are sealed with a call's transport cipher: the relay copies bytes it cannot
//! read, exactly as it does for the browser's WebTransport carrier.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion; the scenario is one connection's life"
)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, RelayMode};
use moq_net::Timestamp;
use tilo_live::carrier::{self, AdmissionError, Dial as _};
use tilo_live::native;
use tilo_live::relay::{
    Admission, Denied, Direction, Peer, Relay, RelayAuthority, SessionAuthorization,
};
use tilo_live::voice::{FRAME_SAMPLES, VoiceObject};
use tilo_seal::discord_voice::{TransportCipher, XCHACHA};

/// The carrier's admission, as the host decides it from a scope's proof.
struct Authority;

impl RelayAuthority for Authority {
    fn admit<'a>(&'a self, _peer: Peer, proof: &'a [u8]) -> Admission<'a> {
        Box::pin(std::future::ready(Self::decide(proof)))
    }
}

impl Authority {
    fn decide(proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let (publish, subscribe) = match proof {
            b"scope-a" => ("call/a", "call/b"),
            b"scope-b" => ("call/b", "call/a"),
            _ => return Err(Denied),
        };
        Ok(Arc::new(Permit {
            publish,
            subscriptions: vec![subscribe.to_owned()],
        }))
    }
}

struct Permit {
    publish: &'static str,
    subscriptions: Vec<String>,
}

impl SessionAuthorization for Permit {
    fn publication(&self) -> Option<&str> {
        Some(self.publish)
    }

    fn subscriptions(&self) -> &[String] {
        &self.subscriptions
    }

    fn current(&self, direction: Direction, namespace: &str) -> bool {
        match direction {
            Direction::Publish => namespace == self.publish,
            Direction::Subscribe => self.subscriptions.iter().any(|name| name == namespace),
        }
    }
}

/// One loopback endpoint that carries calls.
async fn endpoint() -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .transport_config(native::transport_config())
        .bind()
        .await
        .unwrap()
}

/// Where the other endpoint reaches `endpoint` on loopback.
fn addr(endpoint: &Endpoint) -> EndpointAddr {
    let port = endpoint.bound_sockets().first().unwrap().port();
    EndpointAddr::new(endpoint.id()).with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

/// One in-process origin whose driver runs until its task drops.
fn origin() -> (moq_net::origin::Producer, carrier::OriginTask) {
    carrier::OriginTask::produce(carrier::origin_config(1024 * 1024, Duration::from_secs(2)))
}

/// One scope's publication at `path`: a sealed voice track and a sealed
/// presence track.
struct Published {
    _broadcast: moq_net::broadcast::Producer,
    audio: moq_net::track::Producer,
    presence: moq_net::track::Producer,
}

fn publish(origin: &moq_net::origin::Producer, path: &str) -> Published {
    let broadcast = origin.create_broadcast(path).unwrap();
    let audio = broadcast.create_track("audio", None).unwrap();
    let presence = broadcast.create_track("presence", None).unwrap();
    broadcast
        .announce(moq_net::origin::Route::default())
        .unwrap();
    Published {
        _broadcast: broadcast,
        audio,
        presence,
    }
}

async fn dial(
    carrier: &native::Iroh,
    grant: &[u8],
    publish: Option<&moq_net::origin::Producer>,
    subscribe: Option<&moq_net::origin::Producer>,
) -> Result<carrier::Connected, AdmissionError> {
    carrier
        .dial(
            grant,
            publish.map(moq_net::origin::Producer::consume),
            subscribe.cloned(),
            tilo_test_env::hang_bound(),
        )
        .await
}

/// Poll `done` on this test's runtime until it holds, bounded by the hang
/// bound. A blocking wait would stall the relay's own tasks.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    tilo_test_env::within(what, async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
}

fn voice(sequence: u64, payload: &[u8]) -> Vec<u8> {
    VoiceObject {
        sequence,
        timestamp: sequence.saturating_mul(u64::from(FRAME_SAMPLES)),
        duration: FRAME_SAMPLES,
        payload: payload.to_vec(),
    }
    .encode()
    .unwrap()
}

#[tokio::test]
async fn native_carrier_admits_scopes_and_relays_their_sealed_tracks() {
    let relay_endpoint = endpoint().await;
    let relay = Relay::new(Arc::new(Authority));
    let _router = native::serve(relay_endpoint.clone(), {
        let relay = relay.clone();
        move |session| {
            let relay = relay.clone();
            async move {
                let peer = Peer(session.remote());
                relay.serve_session(session, peer).await;
            }
        }
    })
    .unwrap();
    let client_endpoint = endpoint().await;
    let carrier = native::Iroh::new(client_endpoint.clone(), addr(&relay_endpoint));

    assert!(matches!(
        dial(&carrier, b"not-a-scope", None, None).await,
        Err(AdmissionError::Denied)
    ));

    let (a_out, _a_out_task) = origin();
    let (a_in, _a_in_task) = origin();
    let (b_out, _b_out_task) = origin();
    let (b_in, _b_in_task) = origin();
    let mut a = publish(&a_out, "call/a");
    let _b = publish(&b_out, "call/b");
    let a_session = dial(&carrier, b"scope-a", Some(&a_out), Some(&a_in))
        .await
        .unwrap();
    let b_session = dial(&carrier, b"scope-b", Some(&b_out), Some(&b_in))
        .await
        .unwrap();
    let lite = carrier::version().unwrap();
    assert_eq!(a_session.session.version(), lite);
    assert_eq!(b_session.session.version(), lite);
    until("both native sessions admitted", || {
        relay.counters().accepted == 2
    })
    .await;

    let key = [7u8; 32];
    let cipher = TransportCipher::new(XCHACHA, &key).unwrap();
    let voice_aad = b"tilo-voice/1";
    let presence_aad = b"tilo-presence/1";
    let raw_voice = b"\xf8\xff\xfe\xfd".as_slice();
    let raw_presence = br#"{"state":"present"}"#.as_slice();
    let sealed_voice = cipher.seal(1, voice_aad, raw_voice).unwrap();
    let sealed_presence = cipher.seal(2, presence_aad, raw_presence).unwrap();

    let incoming = tilo_test_env::within(
        "the relay mirrors the other scope's call",
        b_in.consume().routed_broadcast("call/a"),
    )
    .await
    .unwrap();
    let mut audio = incoming
        .track("audio")
        .unwrap()
        .subscribe(None)
        .await
        .unwrap();
    let mut presence = incoming
        .track("presence")
        .unwrap()
        .subscribe(None)
        .await
        .unwrap();

    // Both tracks are written before the subscription has settled at the
    // publisher, so repeat until the relay's copy reaches this scope.
    tilo_test_env::within("the sealed voice object relays", async {
        loop {
            a.audio
                .write_frame(Timestamp::ZERO, voice(1, &sealed_voice))
                .unwrap();
            let Ok(Ok(Some(mut group))) =
                tokio::time::timeout(Duration::from_millis(50), audio.recv_group()).await
            else {
                continue;
            };
            let frame = group.read_frame().await.unwrap().expect("one voice object");
            let object = VoiceObject::decode(&frame.payload).unwrap();
            assert_eq!(
                cipher.open(1, voice_aad, &object.payload).unwrap(),
                raw_voice
            );
            break;
        }
    })
    .await;

    tilo_test_env::within("the sealed presence datagram relays", async {
        let mut sequence = 0_u64;
        loop {
            a.presence
                .insert_datagram(sequence, Timestamp::ZERO, sealed_presence.clone())
                .unwrap();
            sequence = sequence.saturating_add(1);
            let Ok(Ok(Some(datagram))) =
                tokio::time::timeout(Duration::from_millis(50), presence.recv_datagram()).await
            else {
                continue;
            };
            assert_eq!(
                cipher.open(2, presence_aad, &datagram.payload).unwrap(),
                raw_presence
            );
            break;
        }
    })
    .await;

    // The iroh adapter reports the connection's counters as wire statistics.
    let sent = a_session.wire();
    let received = b_session.wire();
    assert!(sent.tx_bytes > 0, "the publisher put bytes on the wire");
    assert!(
        sent.tx_datagrams > 0,
        "the presence datagram left the endpoint"
    );
    assert_eq!(sent.sent_packets, sent.tx_datagrams);
    assert!(received.rx_bytes > 0, "the subscriber received bytes");
    assert!(
        received.rx_datagrams > 0,
        "the subscriber received datagrams"
    );
}
