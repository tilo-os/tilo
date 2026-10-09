//! Real localhost WebTransport tests: no mocked carrier or TLS bypass.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "in a test these are the assertion; fixture arithmetic is small and bounded"
)]
#![allow(
    clippy::similar_names,
    clippy::collection_is_never_read,
    clippy::unused_async,
    clippy::too_many_lines,
    clippy::items_after_statements,
    reason = "scenario-shaped test code"
)]

mod support;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use support::{Dialer, Origin};
use tilo_moq::relay::{
    Denied, Direction, LoopbackRelay, Relay, RelayAuthority, SessionAuthorization, VOICE_PATH,
};
use tilo_moq::voice::{FRAME_SAMPLES, VoiceObject};

#[test]
fn shared_ingress_route_requires_exact_raw_authority_and_target() {
    let expected = "media.example.test:443";
    assert!(Relay::validate_route(expected, expected, VOICE_PATH).is_some());

    for target in [
        "/moq/voice/",
        "/moq/./voice",
        "/moq/%2e/voice",
        "/moq/other/../voice",
        "/moq/voice?alias=1",
        "/moq/voice#fragment",
        "/peer/v3/connect",
        "/voice",
    ] {
        assert!(Relay::validate_route(expected, expected, target).is_none());
    }
    for authority in [
        "MEDIA.example.test:443",
        "media.example.test",
        "user@media.example.test:443",
        "media.example.test:8443",
        "other.example.test:443",
    ] {
        assert!(Relay::validate_route(authority, expected, VOICE_PATH).is_none());
    }
}

#[test]
fn isolated_proof_endpoint_rejects_nonloopback_binding() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_moq-proof"))
        .args([
            "--bind",
            "0.0.0.0:443",
            "--cert",
            "/unused",
            "--key",
            "/unused",
            "--identities",
            "/unused",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("loopback"));
}

struct Permit {
    publish: String,
    subscribe: Vec<String>,
    expires: Instant,
    active: Arc<AtomicBool>,
}

impl SessionAuthorization for Permit {
    fn publication(&self) -> Option<&str> {
        Some(&self.publish)
    }
    fn subscriptions(&self) -> &[String] {
        &self.subscribe
    }
    fn current(&self, direction: Direction, namespace: &str) -> bool {
        self.active.load(Ordering::SeqCst)
            && Instant::now() < self.expires
            && match direction {
                Direction::Publish => namespace == self.publish,
                Direction::Subscribe => self.subscribe.iter().any(|name| name == namespace),
            }
    }
}

struct Authority {
    active: Arc<AtomicBool>,
}

impl RelayAuthority for Authority {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        // Explicit isolated test identities. No workspace claim is made.
        let (publish, subscribe, ttl) = match proof {
            b"isolated-a" => ("opaque-call/a", "opaque-call/b", Duration::from_secs(30)),
            b"isolated-b" => ("opaque-call/b", "opaque-call/a", Duration::from_secs(30)),
            b"isolated-expiry" => (
                "opaque-call/expires",
                "opaque-call/a",
                Duration::from_millis(250),
            ),
            _ => return Err(Denied),
        };
        let mut subscriptions = vec![subscribe.into()];
        if proof == b"isolated-b" {
            subscriptions.extend(["opaque-call/a/child".into(), "opaque-call/a-other".into()]);
        }
        Ok(Arc::new(Permit {
            publish: publish.into(),
            subscribe: subscriptions,
            expires: Instant::now() + ttl,
            active: self.active.clone(),
        }))
    }
}

fn setup() -> (Dialer, Arc<AtomicBool>, LoopbackRelay) {
    let active = Arc::new(AtomicBool::new(true));
    let relay = LoopbackRelay::start(Arc::new(Authority {
        active: active.clone(),
    }))
    .unwrap();
    let dialer = Dialer::new(&relay.endpoint, &relay.fingerprints);
    (dialer, active, relay)
}

#[tokio::test]
async fn exact_voice_route_accepts_and_rejects_other_routes_and_queries() {
    let (dialer, _active, _relay) = setup();
    let admitted = dialer.connect(b"isolated-a", None, None).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), admitted.closed())
            .await
            .is_err()
    );
    drop(admitted);
    for path in [
        "/moq",
        "/moq/voice/",
        "/moq/v21/voice",
        "/moq/voice?token=invalid",
    ] {
        let changed = dialer.url.join(path).unwrap();
        assert!(
            dialer
                .connect_to(&changed, b"isolated-a", None, None)
                .await
                .is_err(),
            "{path}"
        );
    }
}

async fn read(origin: &Origin, path: &str) -> Vec<u8> {
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        let broadcast = origin.broadcast(path).await;
        let mut track = broadcast
            .track("audio")
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();
        let mut group = track.recv_group().await.unwrap().expect("voice group");
        group
            .read_frame()
            .await
            .unwrap()
            .expect("voice object")
            .payload
            .to_vec()
    })
    .await
    .expect("real WebTransport delivery deadline")
}

fn voice(sequence: u64) -> Vec<u8> {
    VoiceObject {
        sequence,
        timestamp: sequence * 960,
        duration: FRAME_SAMPLES,
        payload: vec![0xf8, 0xff, 0xfe],
    }
    .encode()
    .unwrap()
}

#[tokio::test]
async fn real_webtransport_relays_bidirectional_exact_voice_objects_and_reconnects() {
    let (dialer, _active, _relay) = setup();
    let (a_pub, a_sub, b_pub, b_sub) = (Origin::new(), Origin::new(), Origin::new(), Origin::new());
    let (_ab, mut at) = a_pub.publish("opaque-call/a");
    let (_bb, mut bt) = b_pub.publish("opaque-call/b");
    let a_session = tokio::time::timeout(
        tilo_test_env::hang_bound(),
        dialer.connect(b"isolated-a", Some(&a_pub), Some(&a_sub)),
    )
    .await
    .unwrap()
    .unwrap();
    let b_session = tokio::time::timeout(
        tilo_test_env::hang_bound(),
        dialer.connect(b"isolated-b", Some(&b_pub), Some(&b_sub)),
    )
    .await
    .unwrap()
    .unwrap();
    let lite = tilo_moq::carrier::version().unwrap();
    assert_eq!(a_session.session.version(), lite);
    assert_eq!(b_session.session.version(), lite);
    at.write_frame(moq_net::Timestamp::ZERO, voice(0)).unwrap();
    bt.write_frame(moq_net::Timestamp::ZERO, voice(1)).unwrap();
    let (a_rx, b_rx) = tokio::join!(read(&a_sub, "opaque-call/b"), read(&b_sub, "opaque-call/a"));
    assert_eq!(a_rx, voice(1));
    assert_eq!(b_rx, voice(0));
    drop(a_session);
    let again = tokio::time::timeout(
        tilo_test_env::hang_bound(),
        dialer.connect(b"isolated-a", Some(&a_pub), Some(&a_sub)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(again.session.version(), lite);
    at.write_frame(moq_net::Timestamp::ZERO, voice(5)).unwrap();
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while read(&b_sub, "opaque-call/a").await != voice(5) {
            at.write_frame(moq_net::Timestamp::ZERO, voice(5)).unwrap();
        }
    })
    .await
    .expect("reconnected publisher relayed");
    drop((again, b_session));
}

#[tokio::test]
async fn denied_proof_expiry_and_active_revocation_close_real_sessions() {
    let (dialer, active, _relay) = setup();
    assert!(dialer.connect(b"wrong-proof", None, None).await.is_err());
    let expired = dialer
        .connect(b"isolated-expiry", None, None)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), expired.closed())
            .await
            .is_err(),
        "valid short-lived grant must initially stay open"
    );
    let _ = tokio::time::timeout(Duration::from_secs(2), expired.closed())
        .await
        .expect("expired forwarding stops");
    let admitted = dialer.connect(b"isolated-a", None, None).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), admitted.closed())
            .await
            .is_err(),
        "valid grant must initially stay open"
    );
    active.store(false, Ordering::SeqCst);
    let _ = tokio::time::timeout(Duration::from_secs(1), admitted.closed())
        .await
        .expect("revocation stops established forwarding within one second");
}

#[tokio::test]
async fn a_publish_grant_does_not_authorize_descendants_or_changed_namespaces() {
    let (dialer, _active, _relay) = setup();
    let (incoming, outgoing) = (Origin::new(), Origin::new());
    let mut publications = Vec::new();
    {
        let (broadcast, mut track) = outgoing.publish("opaque-call/a");
        track
            .write_frame(moq_net::Timestamp::ZERO, voice(0))
            .unwrap();
        publications.push((broadcast, track));
    }
    let subscriber = dialer
        .connect(b"isolated-b", None, Some(&incoming))
        .await
        .unwrap();
    let publisher = dialer
        .connect(b"isolated-a", Some(&outgoing), None)
        .await
        .unwrap();
    assert_eq!(read(&incoming, "opaque-call/a").await, voice(0));
    let mut announced = incoming.producer.consume().announced();
    for path in ["opaque-call/a/child", "opaque-call/a-other"] {
        let (broadcast, mut track) = outgoing.publish(path);
        track
            .write_frame(moq_net::Timestamp::ZERO, voice(99))
            .unwrap();
        publications.push((broadcast, track));
    }
    // Only the exact granted namespace is ever announced to the subscriber...
    let seen = tokio::time::timeout(Duration::from_millis(250), async {
        let mut seen = Vec::new();
        while let Some(update) = announced.next().await {
            seen.push(update.prefix.as_str().to_owned());
        }
        seen
    })
    .await;
    if let Ok(seen) = seen {
        assert!(seen.iter().all(|path| path == "opaque-call/a"), "{seen:?}");
    }
    // ...and nothing published below or beside it reaches the subscriber.
    let child = incoming.broadcast("opaque-call/a/child").await;
    if let Ok(mut track) = child.track("audio").unwrap().subscribe(None).await {
        let delivered = tokio::time::timeout(Duration::from_millis(250), async {
            let mut group = track.recv_group().await.ok()??;
            group.read_frame().await.ok()?
        })
        .await;
        assert!(
            !matches!(delivered, Ok(Some(frame)) if frame.payload.as_ref() == voice(99)),
            "changed namespace was relayed"
        );
    }
    assert!(
        tokio::time::timeout(
            Duration::from_millis(250),
            incoming.broadcast("opaque-call/a-other")
        )
        .await
        .is_err(),
        "changed namespace was relayed"
    );
    drop((publisher, subscriber));
}

#[tokio::test]
async fn unauthorized_publisher_cannot_attach_media_before_admission() {
    let (dialer, _active, _relay) = setup();
    let (incoming, outgoing) = (Origin::new(), Origin::new());
    let (_broadcast, mut track) = outgoing.publish("opaque-call/a");
    track
        .write_frame(moq_net::Timestamp::ZERO, voice(0))
        .unwrap();
    let subscriber = dialer
        .connect(b"isolated-b", None, Some(&incoming))
        .await
        .unwrap();
    assert!(
        dialer
            .connect(b"wrong-proof", Some(&outgoing), None)
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(250),
            incoming.broadcast("opaque-call/a")
        )
        .await
        .is_err(),
        "unauthorized media reached the relay origin"
    );
    // An authorized publication subsequently proves the observer and carrier.
    let admitted = dialer
        .connect(b"isolated-a", Some(&outgoing), None)
        .await
        .unwrap();
    assert_eq!(read(&incoming, "opaque-call/a").await, voice(0));
    drop((admitted, subscriber));
}

#[tokio::test]
async fn real_webtransport_relays_voice_datagram_bundles_and_drops_invalid_ones() {
    let (dialer, _active, relay) = setup();
    let (a_pub, b_sub) = (Origin::new(), Origin::new());
    let (_ab, mut at) = a_pub.publish("opaque-call/a");
    let a_session = tokio::time::timeout(
        tilo_test_env::hang_bound(),
        dialer.connect(b"isolated-a", Some(&a_pub), None),
    )
    .await
    .unwrap()
    .unwrap();
    let b_session = tokio::time::timeout(
        tilo_test_env::hang_bound(),
        dialer.connect(b"isolated-b", None, Some(&b_sub)),
    )
    .await
    .unwrap()
    .unwrap();
    let mut bundle = voice(2);
    bundle.extend_from_slice(&voice(1));
    let received = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        let broadcast = b_sub.broadcast("opaque-call/a").await;
        let mut track = broadcast
            .track("audio")
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();
        loop {
            // Best-effort: keep offering until the relay subscription is attached.
            // An invalid object is refused at the relay without costing the session.
            at.append_datagram(moq_net::Timestamp::ZERO, vec![0u8; 40])
                .unwrap();
            at.append_datagram(moq_net::Timestamp::ZERO, bundle.clone())
                .unwrap();
            if let Ok(Ok(Some(datagram))) =
                tokio::time::timeout(Duration::from_millis(50), track.recv_datagram()).await
            {
                break datagram.payload.to_vec();
            }
        }
    })
    .await
    .expect("real WebTransport datagram delivery deadline");
    assert_eq!(received, bundle);
    assert!(
        relay.counters().dropped > 0,
        "invalid datagram was not refused"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), a_session.closed())
            .await
            .is_err(),
        "an invalid datagram must not close the publisher session"
    );
    drop((a_session, b_session));
}
