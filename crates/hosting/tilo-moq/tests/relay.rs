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
    // Each attempt carries two objects the relay has not forwarded yet: it
    // forwards an object once, never a copy whose objects all went out.
    let mut sent = Vec::new();
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
            let newest = 2 * sent.len() as u64 + 2;
            let mut bundle = voice(newest);
            bundle.extend_from_slice(&voice(newest - 1));
            at.append_datagram(moq_net::Timestamp::ZERO, bundle.clone())
                .unwrap();
            sent.push(bundle);
            if let Ok(Ok(Some(datagram))) =
                tokio::time::timeout(Duration::from_millis(50), track.recv_datagram()).await
            {
                break datagram.payload.to_vec();
            }
        }
    })
    .await
    .expect("real WebTransport datagram delivery deadline");
    assert!(sent.contains(&received), "a valid bundle arrives intact");
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

/// One voice object of `payload` Opus bytes at media frame `frame`.
fn opus_object(sequence: u64, frame: u64, payload: usize) -> Vec<u8> {
    VoiceObject {
        sequence,
        timestamp: frame * u64::from(FRAME_SAMPLES),
        duration: FRAME_SAMPLES,
        payload: vec![0x5a; payload],
    }
    .encode()
    .unwrap()
}

/// How a browser publisher sends 20 ms of voice.
#[derive(Clone, Copy, Debug)]
enum Publisher {
    /// Before the change: every frame, each datagram carrying it and the two
    /// before it.
    TripleBundles,
    /// After: one object per datagram, and in silence only the encoder's
    /// DTX comfort-noise updates, one per 400 ms (20 frames).
    Dtx,
}

/// Real Opus sizes at 32 kbit/s, measured with `opusenc` (bitrate=32000,
/// audio-type=voice, frame-size=20): speech averages 80 bytes a frame,
/// silence 35 bytes, and with DTX silence sends one 35-byte comfort-noise
/// update per 20 frames.
const SPEECH_BYTES: usize = 80;
const SILENCE_BYTES: usize = 35;
const DTX_INTERVAL: u64 = 20;
const MEASURE: Duration = Duration::from_secs(2);

/// Per-second costs of one publisher's stream over [`MEASURE`].
#[derive(Debug)]
struct Cost {
    /// UDP plus IPv4 header bytes the subscriber received.
    subscriber_wire: f64,
    /// Voice object bytes the subscriber received.
    subscriber_objects: f64,
    subscriber_datagrams: f64,
    /// UDP plus IPv4 bytes the publisher sent: what a relay that forwards
    /// bundles verbatim (before the change) sends every subscriber.
    publisher_wire: f64,
}

const IPV4_UDP_HEADER: u64 = 28;

/// Wait until no voice datagram arrived for 100 ms: what was sent is in.
async fn quiet(received: &std::sync::Mutex<(u64, u64)>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut seen = *received.lock().unwrap();
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let now = *received.lock().unwrap();
        if now == seen || std::time::Instant::now() >= deadline {
            return;
        }
        seen = now;
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "byte counts over seconds, reported as rates"
)]
async fn measure(
    publisher: Publisher,
    speaking: bool,
    track: &mut moq_net::track::Producer,
    sessions: (&tilo_moq::carrier::Connected, &tilo_moq::carrier::Connected),
    received: &std::sync::Mutex<(u64, u64)>,
    sequence: &mut u64,
) -> Cost {
    let (a, b) = sessions;
    let wire = |stats: tilo_moq::carrier::WireStats, rx: bool| {
        if rx {
            stats.rx_bytes + IPV4_UDP_HEADER * stats.rx_datagrams
        } else {
            stats.tx_bytes + IPV4_UDP_HEADER * stats.tx_datagrams
        }
    };
    // The previous stream's last frames may still be on their way through a
    // loaded relay: they are not this stream's cost.
    quiet(received).await;
    let (a0, b0) = (wire(a.wire(), false), wire(b.wire(), true));
    let r0 = *received.lock().unwrap();
    let started = std::time::Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    let frames = MEASURE.as_millis() as u64 / 20;
    let first = *sequence;
    let mut recent: Vec<Vec<u8>> = Vec::new();
    for index in 0..frames {
        tick.tick().await;
        let frame = 10_000 + first + index;
        let payload = if speaking {
            SPEECH_BYTES
        } else {
            SILENCE_BYTES
        };
        match publisher {
            Publisher::TripleBundles => {
                let object = opus_object(*sequence, frame, payload);
                *sequence += 1;
                let mut bundle = object.clone();
                for earlier in recent.iter().rev().take(2) {
                    bundle.extend_from_slice(earlier);
                }
                recent.push(object);
                track
                    .append_datagram(moq_net::Timestamp::ZERO, bundle)
                    .unwrap();
            }
            Publisher::Dtx => {
                if speaking || index % DTX_INTERVAL == 0 {
                    let object = opus_object(*sequence, frame, payload);
                    *sequence += 1;
                    track
                        .append_datagram(moq_net::Timestamp::ZERO, object)
                        .unwrap();
                }
            }
        }
    }
    quiet(received).await;
    let r1 = *received.lock().unwrap();
    let (a1, b1) = (wire(a.wire(), false), wire(b.wire(), true));
    // Rates over the time the counters actually spanned: the frames' 20 ms
    // ticks, which run late on a loaded machine, and the wait for the last
    // frames to arrive. Dividing by MEASURE alone charged that extra time's
    // bytes to two seconds.
    let seconds = started.elapsed().as_secs_f64();
    Cost {
        subscriber_wire: (b1 - b0) as f64 / seconds,
        subscriber_objects: (r1.0 - r0.0) as f64 / seconds,
        subscriber_datagrams: (r1.1 - r0.1) as f64 / seconds,
        publisher_wire: (a1 - a0) as f64 / seconds,
    }
}

/// the relay's egress for one forwarded voice stream, measured on
/// a real QUIC connection (UDP payload bytes plus 28 bytes of IPv4/UDP
/// header per datagram) at the subscriber. Run with `--nocapture` for the
/// table.
#[tokio::test]
async fn forwarded_voice_costs_its_opus_bitrate_while_speaking_and_almost_nothing_while_silent() {
    let (dialer, _active, _relay) = setup();
    let (a_pub, b_sub) = (Origin::new(), Origin::new());
    let (_ab, mut at) = a_pub.publish("opaque-call/a");
    let a_session = dialer
        .connect(b"isolated-a", Some(&a_pub), None)
        .await
        .unwrap();
    let b_session = dialer
        .connect(b"isolated-b", None, Some(&b_sub))
        .await
        .unwrap();
    let received = Arc::new(std::sync::Mutex::new((0_u64, 0_u64)));
    let broadcast = b_sub.broadcast("opaque-call/a").await;
    let mut track = broadcast
        .track("audio")
        .unwrap()
        .subscribe(None)
        .await
        .unwrap();
    let (attached_tx, attached) = tokio::sync::oneshot::channel();
    let counter = received.clone();
    let reader = tokio::spawn(async move {
        let mut attached_tx = Some(attached_tx);
        while let Ok(Some(datagram)) = track.recv_datagram().await {
            {
                let mut counted = counter.lock().unwrap();
                counted.0 += datagram.payload.len() as u64;
                counted.1 += 1;
            }
            if let Some(attached) = attached_tx.take() {
                let _ = attached.send(());
            }
        }
    });
    // Offer frames until the relay's subscription is attached end to end.
    let mut sequence = 0;
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        tokio::pin!(attached);
        loop {
            at.append_datagram(
                moq_net::Timestamp::ZERO,
                opus_object(sequence, sequence, SPEECH_BYTES),
            )
            .unwrap();
            sequence += 1;
            if tokio::time::timeout(Duration::from_millis(20), &mut attached)
                .await
                .is_ok()
            {
                break;
            }
        }
    })
    .await
    .expect("voice datagrams reach the subscriber");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut costs = Vec::new();
    for (publisher, speaking) in [
        (Publisher::TripleBundles, true),
        (Publisher::TripleBundles, false),
        (Publisher::Dtx, true),
        (Publisher::Dtx, false),
    ] {
        let cost = measure(
            publisher,
            speaking,
            &mut at,
            (&a_session, &b_session),
            &received,
            &mut sequence,
        )
        .await;
        eprintln!(
            "{publisher:?} {}: publisher sends {:.0} B/s ({:.1} kbit/s); subscriber receives {:.0} B/s ({:.1} kbit/s) on the wire, {:.0} B/s of voice objects in {:.1} datagrams/s",
            if speaking { "speaking" } else { "silent" },
            cost.publisher_wire,
            cost.publisher_wire * 8.0 / 1000.0,
            cost.subscriber_wire,
            cost.subscriber_wire * 8.0 / 1000.0,
            cost.subscriber_objects,
            cost.subscriber_datagrams,
        );
        costs.push(cost);
    }
    reader.abort();
    let [legacy_speaking, legacy_silent, speaking, silent] = costs.try_into().unwrap();
    let kbit = |bytes: f64| bytes * 8.0 / 1000.0;
    // A speaking stream is one object per 20 ms frame: its Opus bitrate plus
    // the 32-byte object header (target <= 45 kbit/s)...
    let per_frame = speaking.subscriber_objects / speaking.subscriber_datagrams;
    assert!(kbit(per_frame * 50.0) <= 45.0, "{speaking:?}");
    // ...and a silent one a few DTX updates a second (target <= 5 kbit/s on
    // the wire, <= 5 packets/s, <= 10% of speaking).
    assert!(kbit(silent.subscriber_wire) <= 5.0, "{silent:?}");
    assert!(silent.subscriber_datagrams <= 5.0, "{silent:?}");
    assert!(silent.subscriber_wire <= speaking.subscriber_wire / 10.0);
    // A triple-bundling (older) publisher costs its subscribers no more than
    // a new one: the relay forwards each object once on a clean downlink.
    // (Compared in what the relay forwards; the QUIC overhead around the same
    // datagrams varies by several kbit/s with ACK timing on a loaded runner.)
    assert!(
        legacy_speaking.subscriber_objects <= speaking.subscriber_objects * 1.02
            && legacy_speaking.subscriber_datagrams <= speaking.subscriber_datagrams * 1.02,
        "{legacy_speaking:?} vs {speaking:?}"
    );
    assert!(
        legacy_silent.subscriber_wire < legacy_speaking.subscriber_wire,
        "{legacy_silent:?}"
    );
    // Before, the relay forwarded each publisher's bundles verbatim.
    assert!(legacy_speaking.publisher_wire > 1.8 * speaking.subscriber_wire);
    // A 4-person call with one speaker: each of 3 listeners receives the
    // speaker, and each silent participant reaches the 3 others.
    let call = 3.0 * speaking.subscriber_wire + 3.0 * 3.0 * silent.subscriber_wire;
    let before = 3.0 * legacy_speaking.publisher_wire + 3.0 * 3.0 * legacy_silent.publisher_wire;
    eprintln!(
        "4-person call, 1 speaker: relay egress {:.0} kbit/s (before: {:.0} kbit/s)",
        kbit(call),
        kbit(before)
    );
    assert!(kbit(call) <= 350.0, "{call}");
    drop((a_session, b_session));
}
