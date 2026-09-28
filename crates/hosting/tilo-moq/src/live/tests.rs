//! Real localhost WebTransport tests of the live relay: no mocked carrier, no
//! TLS bypass (the generated leaf certificate is pinned). Test identities are
//! isolated fixtures, never native workspace authorization.
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
use super::peer::Peer;
use super::*;
use std::sync::atomic::AtomicBool;

const ROOM: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const RECORD: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Session {
    publisher: &'static str,
    publish: bool,
    subscribe: bool,
    active: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}
impl LiveSession for Session {
    fn room(&self) -> &str {
        ROOM
    }
    fn publisher(&self) -> &str {
        self.publisher
    }
    fn publish(&self) -> bool {
        self.publish
    }
    fn subscribe(&self) -> bool {
        self.subscribe
    }
    fn status(&self) -> AuthorizationStatus {
        if !self.active.load(Ordering::SeqCst) {
            AuthorizationStatus::Denied
        } else if self.busy.load(Ordering::SeqCst) {
            // Owner commit in progress: unobservable, bounded by the lease.
            AuthorizationStatus::Unavailable {
                expires_at: Instant::now() + Duration::from_secs(60),
            }
        } else {
            AuthorizationStatus::Current
        }
    }
}

/// Presence test codec: 8-byte little-endian sequence, then opaque bytes.
/// Ops test codec: frames must start with `ok`.
struct Authority {
    a: Arc<AtomicBool>,
    b: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}
impl LiveAuthority for Authority {
    fn try_admit(&self, token: &[u8]) -> Result<Arc<dyn LiveSession>, Admission> {
        let (publisher, publish, subscribe, active) = match token {
            b"grant-a" => (A, true, true, self.a.clone()),
            b"grant-b" => (B, true, true, self.b.clone()),
            b"grant-viewer" => (C, false, true, Arc::new(AtomicBool::new(true))),
            _ => return Err(Admission::Denied),
        };
        Ok(Arc::new(Session {
            publisher,
            publish,
            subscribe,
            active,
            busy: self.busy.clone(),
        }))
    }
    fn presence_seq(&self, frame: &[u8]) -> Option<u64> {
        Some(u64::from_le_bytes(frame.get(..8)?.try_into().ok()?))
    }
    fn ops_valid(&self, record: &str, frame: &[u8]) -> bool {
        record == RECORD && frame.starts_with(b"ok")
    }
}

struct Fixture {
    live: LoopbackLive,
    trust: carrier::Trust,
    a: Arc<AtomicBool>,
    b: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}
fn fixture() -> Fixture {
    let a = Arc::new(AtomicBool::new(true));
    let b = Arc::new(AtomicBool::new(true));
    let busy = Arc::new(AtomicBool::new(false));
    let live = LoopbackLive::start(Arc::new(Authority {
        a: a.clone(),
        b: b.clone(),
        busy: busy.clone(),
    }))
    .unwrap();
    let trust = carrier::Trust::Fingerprints(live.fingerprints.clone());
    Fixture {
        live,
        trust,
        a,
        b,
        busy,
    }
}
fn path(publisher: &str) -> String {
    namespace_path(ROOM, publisher).unwrap()
}
fn url(f: &Fixture) -> url::Url {
    f.live.endpoint.parse().unwrap()
}

async fn try_connect(f: &Fixture, token: &[u8], publish_as: &str) -> Option<Peer> {
    tokio::time::timeout(
        tilo_test_env::hang_bound(),
        Peer::connect(&url(f), &f.trust, token, Some(&path(publish_as))),
    )
    .await
    .unwrap()
    .ok()
}
async fn connect(f: &Fixture, token: &[u8], publish_as: &str) -> Peer {
    try_connect(f, token, publish_as).await.expect("admitted")
}
async fn presence_subscriber(peer: &Peer, from: &str) -> moq_net::track::Subscriber {
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        let broadcast = peer.member(&path(from)).await.expect("announce");
        broadcast
            .track(PRESENCE_TRACK)
            .unwrap()
            .subscribe(None)
            .await
            .unwrap()
    })
    .await
    .expect("presence subscription")
}
fn presence(seq: u64, tail: &[u8]) -> Vec<u8> {
    let mut frame = seq.to_le_bytes().to_vec();
    frame.extend_from_slice(tail);
    frame
}
/// Offer frames until one arrives (the relay's upstream subscription attaches
/// asynchronously); returns the first received payload.
async fn first_presence(
    publisher: &mut Peer,
    subscriber: &mut moq_net::track::Subscriber,
    seq: &mut u64,
) -> Vec<u8> {
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            *seq += 1;
            assert!(publisher.publish_presence(&presence(*seq, b"hello")));
            if let Ok(Ok(Some(datagram))) =
                tokio::time::timeout(Duration::from_millis(50), subscriber.recv_datagram()).await
            {
                return datagram.payload.to_vec();
            }
        }
    })
    .await
    .expect("presence delivery deadline")
}

#[test]
fn route_is_exact_and_namespaces_are_canonical_hex() {
    let expected = "host.example:443";
    assert!(LiveRelay::validate_route(expected, expected, LIVE_PATH).is_some());
    for target in ["/moq/live/", "/moq/voice", "/moq/live?x=1", "/moq/%6cive"] {
        assert!(LiveRelay::validate_route(expected, expected, target).is_none());
    }
    assert!(LiveRelay::validate_route("HOST.example:443", expected, LIVE_PATH).is_none());
    assert_eq!(path(A), format!("tilo-live/{ROOM}/{A}"));
    assert!(namespace_path(ROOM, &A.to_uppercase()).is_none());
    assert!(namespace_path("room", A).is_none());
    assert_eq!(ops_record(&format!("ops/{RECORD}")), Some(RECORD));
    assert!(ops_record("ops/xyz").is_none() && ops_record(PRESENCE_TRACK).is_none());
    assert_eq!(
        record_path(&path(A), RECORD),
        Some(format!("tilo-live/{ROOM}/{A}/ops/{RECORD}"))
    );
    assert!(record_path(&path(A), "xyz").is_none());
}

#[test]
fn token_bucket_bounds_sustained_rate_and_burst() {
    let start = Instant::now();
    let mut bucket = Bucket::new(MAX_PRESENCE_RATE);
    bucket.last = start;
    let burst = (0..1000).filter(|_| bucket.take(start)).count();
    assert_eq!(burst, 30, "quarter-second burst");
    // One further second admits exactly the sustained rate (bucket capped at burst).
    let admitted = (1..=1000)
        .filter(|i| bucket.take(start + Duration::from_millis(*i)))
        .count();
    assert!((119..=121).contains(&admitted), "sustained {admitted}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grant_is_required_and_refused_tokens_close_before_attachment() {
    let f = fixture();
    for token in [&b"forged"[..], b""] {
        assert!(try_connect(&f, token, A).await.is_none());
    }
    let admitted = connect(&f, b"grant-a", A).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), admitted.session().closed())
            .await
            .is_err()
    );
    let counters = f.live.relay.counters();
    assert!(
        counters.rejected >= 1 && counters.accepted == 1,
        "{counters:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn presence_is_relayed_latest_value_and_foreign_namespaces_are_refused() {
    let f = fixture();
    let mut a = connect(&f, b"grant-a", A).await;
    // B tries to publish into A's namespace as well as its own.
    let mut forged = connect(&f, b"grant-b", A).await;
    let b = connect(&f, b"grant-b", B).await;
    let mut from_a = presence_subscriber(&b, A).await;
    let mut seq = 100;
    let first = first_presence(&mut a, &mut from_a, &mut seq).await;
    assert_eq!(first[..8], presence(seq, b"")[..8]);
    // Forged frames with a higher sequence under A's name never arrive.
    for extra in 0..20 {
        assert!(forged.publish_presence(&presence(10_000 + extra, b"forged")));
    }
    // Stale (lower) sequence numbers from A are dropped; a newer one passes.
    assert!(a.publish_presence(&presence(seq - 50, b"stale")));
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(a.publish_presence(&presence(seq + 1, b"fresh")));
    let next = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let datagram = from_a.recv_datagram().await.unwrap().unwrap();
            if datagram.payload[..8] != presence(seq, b"")[..8] {
                return datagram.payload.to_vec();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        next,
        presence(seq + 1, b"fresh"),
        "stale or forged frame was relayed"
    );
    drop((a, forged, b));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_presence_tap_sees_client_frames_but_not_injected_ones() {
    let f = fixture();
    let (tx, mut tapped) = tokio::sync::mpsc::unbounded_channel();
    f.live.relay.set_presence_tap(Arc::new(
        move |room: &str, publisher: &str, frame: &[u8]| {
            let _ = tx.send((room.to_owned(), publisher.to_owned(), frame.to_vec()));
        },
    ));
    let mut a = connect(&f, b"grant-a", A).await;
    let b = connect(&f, b"grant-b", B).await;
    let mut from_a = presence_subscriber(&b, A).await;
    let mut seq = 0;
    first_presence(&mut a, &mut from_a, &mut seq).await;
    let (room, publisher, frame) = tokio::time::timeout(tilo_test_env::hang_bound(), tapped.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((room.as_str(), publisher.as_str()), (ROOM, A));
    assert_eq!(frame[8..], *b"hello");
    while tapped.try_recv().is_ok() {}
    assert!(f.live.relay.inject_presence(ROOM, C, &presence(1, b"host")));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        tapped.try_recv().is_err(),
        "a host-injected frame must not come back through the tap"
    );
    drop((a, b));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn viewer_may_subscribe_but_never_publish() {
    let f = fixture();
    let mut a = connect(&f, b"grant-a", A).await;
    let mut viewer = connect(&f, b"grant-viewer", C).await;
    let mut from_a = presence_subscriber(&viewer, A).await;
    let mut seq = 0;
    first_presence(&mut a, &mut from_a, &mut seq).await;
    for i in 0..20 {
        assert!(viewer.publish_presence(&presence(i, b"viewer")));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(300), a.member(&path(C)))
            .await
            .is_err(),
        "a subscribe-only grant published a namespace"
    );
    drop(a);
    drop(viewer);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn presence_rate_limit_drops_excess_frames() {
    let f = fixture();
    let mut a = connect(&f, b"grant-a", A).await;
    let b = connect(&f, b"grant-b", B).await;
    let mut from_a = presence_subscriber(&b, A).await;
    let mut seq = 0;
    first_presence(&mut a, &mut from_a, &mut seq).await;
    tokio::time::sleep(Duration::from_millis(300)).await; // refill
    let before = f.live.relay.counters();
    let started = Instant::now();
    // 400 frames over ~200 ms: the ceiling admits the 30-frame burst plus
    // 120/s for the time actually elapsed (sleeps stretch under load).
    for i in 0..400u64 {
        assert!(a.publish_presence(&presence(seq + 1 + i, b"burst")));
        if i % 2 == 1 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after = f.live.relay.counters();
    let ceiling = 30 + (started.elapsed().as_secs_f64() * 120.0).ceil() as u64 + 1;
    let forwarded = after.presence_forwarded - before.presence_forwarded;
    assert!(
        forwarded <= ceiling,
        "rate ceiling exceeded: {forwarded} forwarded, ceiling {ceiling}"
    );
    assert!(forwarded < 400, "nothing was rate limited");
    assert!(after.presence_dropped > before.presence_dropped);
    drop(a);
    drop(b);
}

async fn ops_subscriber(peer: &Peer, from: &str) -> moq_net::track::Subscriber {
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let broadcast = peer.member(&path(from)).await.expect("announce");
            // Receivers keep late groups (stream reordering) for the relay window.
            let subscription =
                moq_net::track::Subscription::default().with_max_age(Duration::from_secs(2));
            if let Ok(subscriber) = broadcast
                .track(&format!("ops/{RECORD}"))
                .unwrap()
                .subscribe(subscription)
                .await
            {
                return subscriber;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("ops subscription")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ops_frames_stay_ordered_and_invalid_frames_reset_only_their_group() {
    let f = fixture();
    let mut a = connect(&f, b"grant-a", A).await;
    let ops = a.record(RECORD).unwrap().clone();
    let b = connect(&f, b"grant-b", B).await;
    let mut from_a = ops_subscriber(&b, A).await;
    let mut group = ops.append_group().unwrap();
    for i in 0..50u32 {
        group
            .write_frame(moq_net::Timestamp::ZERO, format!("ok-{i}").into_bytes())
            .unwrap();
    }
    group.finish().unwrap();
    let received = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        let mut group = from_a.recv_group().await.unwrap().unwrap();
        let mut frames = Vec::new();
        while let Ok(Some(frame)) = group.read_frame().await {
            frames.push(String::from_utf8(frame.payload.to_vec()).unwrap());
        }
        frames
    })
    .await
    .unwrap();
    assert_eq!(
        received,
        (0..50).map(|i| format!("ok-{i}")).collect::<Vec<_>>()
    );
    // An invalid frame resets its group; the next group still flows.
    let mut bad = ops.append_group().unwrap();
    bad.write_frame(moq_net::Timestamp::ZERO, b"ok-before".to_vec())
        .unwrap();
    bad.write_frame(moq_net::Timestamp::ZERO, b"forged".to_vec())
        .unwrap();
    bad.finish().unwrap();
    let mut good = ops.append_group().unwrap();
    good.write_frame(moq_net::Timestamp::ZERO, b"ok-after".to_vec())
        .unwrap();
    good.finish().unwrap();
    let after = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let mut group = from_a.recv_group().await.unwrap().unwrap();
            let mut frames = Vec::new();
            let complete = loop {
                match group.read_frame().await {
                    Ok(Some(frame)) => frames.push(frame.payload.to_vec()),
                    Ok(None) => break true,
                    Err(_) => break false,
                }
            };
            if complete && frames == [b"ok-after".to_vec()] {
                return true;
            }
            assert!(
                !complete || frames != [b"ok-before".to_vec(), b"forged".to_vec()],
                "invalid frame relayed"
            );
        }
    })
    .await
    .unwrap();
    assert!(after);
    assert!(f.live.relay.counters().ops_dropped >= 1);
    drop(a);
    drop(b);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_injection_reaches_current_subscribers_without_a_client() {
    let f = fixture();
    // Nobody subscribed and no namespace: dropped, nothing stored.
    assert!(!f.live.relay.inject_ops(ROOM, C, RECORD, b"ok-early"));
    let b = connect(&f, b"grant-b", B).await;
    let mut from_c = ops_subscriber(&b, C).await;
    // SUBSCRIBE_OK can precede the relay accepting the egress track; an
    // injection before that has no reader and is dropped, never queued.
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while !f
            .live
            .relay
            .inject_ops(ROOM, C, RECORD, b"ok-signed-change")
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("egress track accepted");
    let payload = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        let mut group = from_c.recv_group().await.unwrap().unwrap();
        group.read_frame().await.unwrap().unwrap().payload.to_vec()
    })
    .await
    .unwrap();
    assert_eq!(payload, b"ok-signed-change");
    assert!(!f.live.relay.inject_ops(ROOM, C, "not-hex", b"ok"));
    drop(b);
}

async fn group_frames(subscriber: &mut moq_net::track::Subscriber) -> Vec<Vec<u8>> {
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let mut group = subscriber.recv_group().await.unwrap().unwrap();
            let mut frames = Vec::new();
            while let Ok(Some(frame)) = group.read_frame().await {
                if !frame.payload.is_empty() {
                    frames.push(frame.payload.to_vec());
                }
            }
            if !frames.is_empty() {
                return frames;
            }
        }
    })
    .await
    .expect("a stream group")
}

/// A host stream keeps one group open per sequence, so a subscriber that
/// joins mid-sequence starts at its first frame (a video keyframe).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_streams_group_by_sequence_and_late_subscribers_start_at_its_head() {
    let f = fixture();
    let frames = |names: &[&'static str]| {
        names
            .iter()
            .map(|name| bytes::Bytes::from_static(name.as_bytes()))
            .collect::<Vec<_>>()
    };
    let b = connect(&f, b"grant-b", B).await;
    let viewer = connect(&f, b"grant-viewer", C).await;
    assert!(
        !f.live
            .relay
            .inject_stream(ROOM, A, RECORD, true, &frames(&["unread"])),
        "no subscriber yet"
    );
    let mut first = ops_subscriber(&b, A).await;
    // The egress track and this subscriber's view attach asynchronously, and
    // a view starts at the latest group: open sequences until one arrives.
    let mut open = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        let mut attempt = 0u32;
        loop {
            attempt = attempt.wrapping_add(1);
            let key = format!("key-1.{attempt}");
            let started = f.live.relay.inject_stream(
                ROOM,
                A,
                RECORD,
                true,
                &[bytes::Bytes::from(key.clone())],
            );
            if started
                && let Ok(Ok(Some(mut group))) =
                    tokio::time::timeout(Duration::from_millis(50), first.recv_group()).await
                && group
                    .read_frame()
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|frame| frame.payload == key.as_bytes())
            {
                return group;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a stream group reaches the subscriber");
    assert!(
        f.live
            .relay
            .inject_stream(ROOM, A, RECORD, false, &frames(&["delta-1"]))
    );
    async fn next(group: &mut moq_net::group::Consumer) -> Option<Vec<u8>> {
        tokio::time::timeout(tilo_test_env::hang_bound(), group.read_frame())
            .await
            .unwrap()
            .unwrap()
            .map(|frame| frame.payload.to_vec())
    }
    assert_eq!(
        next(&mut open).await,
        Some(b"delta-1".to_vec()),
        "a later frame joins the open group"
    );
    assert!(
        f.live
            .relay
            .inject_stream(ROOM, A, RECORD, true, &frames(&["key-2"]))
    );
    assert_eq!(
        next(&mut open).await,
        None,
        "a new sequence finishes the group"
    );
    let mut late = ops_subscriber(&viewer, A).await;
    assert!(
        f.live
            .relay
            .inject_stream(ROOM, A, RECORD, false, &frames(&["delta-2"]))
    );
    assert!(f.live.relay.stream_open(ROOM, A, RECORD));
    f.live.relay.end_stream(ROOM, A, RECORD);
    assert!(!f.live.relay.stream_open(ROOM, A, RECORD));
    let expected = vec![b"key-2".to_vec(), b"delta-2".to_vec()];
    assert_eq!(group_frames(&mut first).await, expected);
    assert_eq!(
        group_frames(&mut late).await,
        expected,
        "a late subscriber starts at the keyframe"
    );
    assert!(
        !f.live
            .relay
            .inject_stream(ROOM, A, RECORD, false, &frames(&["after-end"])),
        "an ended stream takes nothing until the next sequence starts"
    );
    drop(b);
    drop(viewer);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revocation_resets_only_the_revoked_publishers_tracks() {
    let f = fixture();
    let mut a = connect(&f, b"grant-a", A).await;
    let mut b = connect(&f, b"grant-b", B).await;
    let viewer = connect(&f, b"grant-viewer", C).await;
    let mut a_at_viewer = presence_subscriber(&viewer, A).await;
    let mut b_at_viewer = presence_subscriber(&viewer, B).await;
    let (mut sa, mut sb) = (0, 1000);
    first_presence(&mut a, &mut a_at_viewer, &mut sa).await;
    first_presence(&mut b, &mut b_at_viewer, &mut sb).await;
    let a_announced = viewer.member(&path(A)).await.unwrap();
    f.a.store(false, Ordering::SeqCst);
    // A's namespace is withdrawn at the viewer (its tracks end or reset); the
    // viewer's own session stays connected.
    let ended = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        tokio::select! {
            _ = a_announced.closed() => "broadcast",
            () = async {
                while let Ok(Some(_)) = a_at_viewer.recv_datagram().await {}
            } => "track",
        }
    })
    .await;
    ended.expect("revoked publisher's namespace ends at the viewer");
    // A's own session is told to go away and closes.
    tokio::time::timeout(tilo_test_env::hang_bound(), a.session().closed())
        .await
        .expect("revoked session closes");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), viewer.session().closed())
            .await
            .is_err()
    );
    // B keeps flowing to the viewer.
    first_presence(&mut b, &mut b_at_viewer, &mut sb).await;
    assert_eq!(f.live.relay.counters().revoked, 1);
    assert!(f.b.load(Ordering::SeqCst));
    drop(b);
    drop(viewer);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn open_records_per_publisher_are_bounded() {
    let f = fixture();
    let mut a = connect(&f, b"grant-a", A).await;
    for i in 0..(MAX_OPEN_RECORDS + 2) {
        assert!(a.record(&format!("{:064x}", i + 1)).is_some());
    }
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while f.live.relay.counters().records_refused < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("records beyond the bound are refused");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(f.live.relay.counters().records_refused, 2);
    // Closing a record frees its place for another.
    a.close_record(&format!("{:064x}", 1));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(a.record(&format!("{:064x}", 100)).is_some());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(f.live.relay.counters().records_refused, 2);
}

/// Two native clients exchange presence both ways over loopback; records
/// one-way relay latency (publisher append to subscriber receive, same clock).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loopback_presence_exchange_is_under_ten_milliseconds() {
    let f = fixture();
    let mut a = connect(&f, b"grant-a", A).await;
    let mut b = connect(&f, b"grant-b", B).await;
    let mut a_at_b = presence_subscriber(&b, A).await;
    let mut b_at_a = presence_subscriber(&a, B).await;
    let (mut sa, mut sb) = (0, 0);
    first_presence(&mut a, &mut a_at_b, &mut sa).await;
    first_presence(&mut b, &mut b_at_a, &mut sb).await;
    let epoch = Instant::now();
    let stamp = |seq: u64| {
        let mut tail = (epoch.elapsed().as_nanos() as u64).to_le_bytes().to_vec();
        tail.extend_from_slice(&[0x5a; 48]);
        presence(seq, &tail)
    };
    let receive = |mut subscriber: moq_net::track::Subscriber, count: usize| async move {
        let mut samples = Vec::new();
        while samples.len() < count {
            let Ok(Ok(Some(datagram))) =
                tokio::time::timeout(Duration::from_secs(2), subscriber.recv_datagram()).await
            else {
                break;
            };
            let sent = u64::from_le_bytes(datagram.payload[8..16].try_into().unwrap());
            if sent == 0 {
                continue;
            }
            samples.push(epoch.elapsed().as_nanos() as u64 - sent);
        }
        samples
    };
    const FRAMES: usize = 200;
    let rx_b = tokio::spawn(receive(a_at_b, FRAMES));
    let rx_a = tokio::spawn(receive(b_at_a, FRAMES));
    // 100 Hz each way, under the 120/s ceiling.
    for _ in 0..FRAMES {
        sa += 1;
        sb += 1;
        assert!(a.publish_presence(&stamp(sa)));
        assert!(b.publish_presence(&stamp(sb)));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut samples = rx_b.await.unwrap();
    samples.extend(rx_a.await.unwrap());
    assert!(
        samples.len() >= FRAMES * 2 * 9 / 10,
        "only {} of {} frames arrived",
        samples.len(),
        FRAMES * 2
    );
    samples.sort_unstable();
    let pct = |p: usize| Duration::from_nanos(samples[(samples.len() - 1) * p / 100]);
    eprintln!(
        "LIVE_LOOPBACK_LATENCY samples={} p50={:?} p90={:?} p99={:?} max={:?}",
        samples.len(),
        pct(50),
        pct(90),
        pct(99),
        pct(100)
    );
    assert!(
        pct(50) < Duration::from_millis(10),
        "median one-way {:?}",
        pct(50)
    );
    assert!(
        pct(90) < Duration::from_millis(10),
        "p90 one-way {:?}",
        pct(90)
    );
    drop(a);
    drop(b);
}

/// Inject until the egress track accepts (a fresh subscription may briefly
/// precede it), then read one frame from the next group.
async fn inject_and_read(
    f: &Fixture,
    sub: &mut moq_net::track::Subscriber,
    publisher: &str,
    frame: &[u8],
) -> Vec<u8> {
    let frame_bytes = frame;
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while !f.live.relay.inject_ops(ROOM, publisher, RECORD, frame) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        loop {
            let mut group = sub.recv_group().await.unwrap().expect("ops group");
            // Empty seal groups carry nothing; a re-subscription in the same
            // client session may first yield that session's locally cached
            // earlier group, which receivers de-duplicate.
            if let Ok(Some(frame)) = group.read_frame().await
                && frame.payload.as_ref() == frame_bytes
            {
                return frame.payload.to_vec();
            }
        }
    })
    .await
    .expect("injected frame delivered")
}

/// Regression: sustained owner commits make every authority
/// observation Unavailable for seconds. That is contention, not revocation:
/// the author's session must stay admitted and host injections for it must
/// keep flowing throughout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sustained_owner_contention_never_revokes_and_injection_keeps_flowing() {
    let f = fixture();
    let mut a = connect(&f, b"grant-a", A).await;
    let b = connect(&f, b"grant-b", B).await;
    let mut from_a = ops_subscriber(&b, A).await;
    let mut presence_a = presence_subscriber(&b, A).await;
    let mut seq = 0;
    first_presence(&mut a, &mut presence_a, &mut seq).await;
    f.busy.store(true, Ordering::SeqCst);
    // Far longer than the former 900 ms outage fence.
    let started = Instant::now();
    let mut delivered = 0;
    while started.elapsed() < Duration::from_millis(2500) {
        let frame = format!("ok-{delivered}");
        assert_eq!(
            inject_and_read(&f, &mut from_a, A, frame.as_bytes()).await,
            frame.as_bytes()
        );
        delivered += 1;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f.busy.store(false, Ordering::SeqCst);
    assert!(delivered > 50, "{delivered}");
    let counters = f.live.relay.counters();
    assert_eq!(
        counters.revoked, 0,
        "busy owner revoked a session: {counters:?}"
    );
    assert_eq!(counters.injected_dropped, 0, "{counters:?}");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), a.session().closed())
            .await
            .is_err()
    );
    first_presence(&mut a, &mut presence_a, &mut seq).await;
    drop(a);
    drop(b);
}

/// After an actual revocation the namespace is withdrawn; host injection must
/// re-announce it, a re-subscription on the same namespace must be served, and
/// the author's reconnect must take the namespace over again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn injection_reannounces_and_resubscribe_works_after_revocation_and_reconnect() {
    let f = fixture();
    let a = connect(&f, b"grant-a", A).await;
    let b = connect(&f, b"grant-b", B).await;
    let mut from_a = ops_subscriber(&b, A).await;
    assert_eq!(inject_and_read(&f, &mut from_a, A, b"ok-1").await, b"ok-1");
    let old = b.member(&path(A)).await.unwrap();
    f.a.store(false, Ordering::SeqCst);
    tokio::time::timeout(tilo_test_env::hang_bound(), a.session().closed())
        .await
        .expect("revoked");
    // The withdrawn namespace closes at the subscriber (clients must watch
    // this); then resubscribe on the re-announced host-fed namespace.
    tokio::time::timeout(tilo_test_env::hang_bound(), old.closed())
        .await
        .expect("old namespace withdrawn");
    drop(from_a);
    let mut from_a = ops_subscriber(&b, A).await;
    assert_eq!(inject_and_read(&f, &mut from_a, A, b"ok-2").await, b"ok-2");
    // Reconnect: the author takes the namespace over, injection continues.
    f.a.store(true, Ordering::SeqCst);
    let mut a = connect(&f, b"grant-a", A).await;
    let mut presence_a = presence_subscriber(&b, A).await;
    let mut seq = 500;
    first_presence(&mut a, &mut presence_a, &mut seq).await;
    let mut from_a = ops_subscriber(&b, A).await;
    assert_eq!(inject_and_read(&f, &mut from_a, A, b"ok-3").await, b"ok-3");
    drop(a);
    drop(b);
}

/// A late subscriber is not replayed an already delivered group: once a track
/// is quiet the relay seals it with an empty group.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_subscriber_is_not_replayed_stale_groups() {
    let f = fixture();
    let b = connect(&f, b"grant-b", B).await;
    let viewer = connect(&f, b"grant-viewer", C).await;
    // First host injection announces the host-fed namespace (nobody reads it yet).
    assert!(!f.live.relay.inject_ops(ROOM, A, RECORD, b"ok-unread"));
    let mut first = ops_subscriber(&b, A).await;
    assert_eq!(
        inject_and_read(&f, &mut first, A, b"ok-old").await,
        b"ok-old"
    );
    tokio::time::sleep(SEAL_QUIET + Duration::from_millis(100)).await;
    let mut late = ops_subscriber(&viewer, A).await;
    let replayed = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            let Ok(Some(mut group)) = late.recv_group().await else {
                return None;
            };
            // Seal groups hold one empty frame.
            if let Ok(Some(frame)) = group.read_frame().await
                && !frame.payload.is_empty()
            {
                return Some(frame.payload.to_vec());
            }
        }
    })
    .await;
    assert!(
        !matches!(replayed, Ok(Some(_))),
        "stale group replayed: {replayed:?}"
    );
    assert_eq!(
        inject_and_read(&f, &mut late, A, b"ok-new").await,
        b"ok-new"
    );
    drop(b);
    drop(viewer);
}

/// A final withdrawal (revoked author, no host-fed replacement) is announced
/// inactive and closes the member's broadcast; nothing re-announces it, so a
/// client drops the member instead of retrying. A clean disconnect likewise.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn final_withdrawal_is_announced_inactive_and_not_reannounced() {
    let f = fixture();
    let a = connect(&f, b"grant-a", A).await;
    let b = connect(&f, b"grant-b", B).await;
    let viewer = connect(&f, b"grant-viewer", C).await;
    let mut announces = viewer.announced();
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let update = announces.next().await.expect("announce stream");
            if update.prefix.as_str() == path(A) && update.kind.is_active() {
                return;
            }
        }
    })
    .await
    .expect("member announced active");
    let member = viewer.member(&path(A)).await.unwrap();
    f.a.store(false, Ordering::SeqCst);
    // Inactive announcement for exactly this member, and its broadcast closes.
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let update = announces.next().await.expect("announce stream");
            if update.prefix.as_str() == path(A) {
                assert!(!update.kind.is_active(), "revoked member re-announced");
                return;
            }
        }
    })
    .await
    .expect("member announced inactive");
    tokio::time::timeout(tilo_test_env::hang_bound(), member.closed())
        .await
        .expect("member broadcast closed");
    // Final: no re-announce (the host never fed this namespace).
    let again = tokio::time::timeout(Duration::from_millis(600), async {
        loop {
            let update = announces.next().await.expect("announce stream");
            if update.prefix.as_str() == path(A) {
                return update.kind.is_active();
            }
        }
    })
    .await;
    assert!(
        !matches!(again, Ok(true)),
        "final withdrawal was re-announced"
    );
    // A clean disconnect of B is a final withdrawal as well.
    drop(b);
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let update = announces.next().await.expect("announce stream");
            if update.prefix.as_str() == path(B) && !update.kind.is_active() {
                return;
            }
        }
    })
    .await
    .expect("disconnected member announced inactive");
    drop(viewer);
    drop(a);
}

/// Host presence injection into a host-fed namespace (an agent the host speaks
/// for): reaches a `MoQ` subscriber, keeps latest-value sequence order, and is
/// refused while a client session owns the namespace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_presence_injection_reaches_subscribers_and_never_speaks_for_a_client() {
    let f = fixture();
    let before = f.live.relay.counters();
    // Invalid frames never create a namespace.
    assert!(!f.live.relay.inject_presence(ROOM, C, b"short"));
    assert!(!f.live.relay.inject_presence("room", C, &presence(1, b"x")));
    assert!(
        !f.live
            .relay
            .inject_presence(ROOM, C, &vec![0; MAX_DATAGRAM_FRAME_BYTES + 1])
    );
    let b = connect(&f, b"grant-b", B).await;
    // The first valid injection announces the host-fed namespace.
    let mut seq = 1;
    assert!(
        f.live
            .relay
            .inject_presence(ROOM, C, &presence(seq, b"agent"))
    );
    let mut from_c = presence_subscriber(&b, C).await;
    let first = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            seq += 1;
            assert!(
                f.live
                    .relay
                    .inject_presence(ROOM, C, &presence(seq, b"agent"))
            );
            if let Ok(Ok(Some(datagram))) =
                tokio::time::timeout(Duration::from_millis(50), from_c.recv_datagram()).await
            {
                return datagram.payload.to_vec();
            }
        }
    })
    .await
    .expect("injected presence delivered");
    assert!(first.ends_with(b"agent"));
    // Stale and equal sequences are dropped; a newer one passes.
    assert!(
        !f.live
            .relay
            .inject_presence(ROOM, C, &presence(seq, b"equal"))
    );
    assert!(
        !f.live
            .relay
            .inject_presence(ROOM, C, &presence(1, b"stale"))
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        f.live
            .relay
            .inject_presence(ROOM, C, &presence(seq + 1, b"fresh"))
    );
    let next = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let datagram = from_c.recv_datagram().await.unwrap().unwrap();
            if !datagram.payload.ends_with(b"agent") {
                return datagram.payload.to_vec();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(next, presence(seq + 1, b"fresh"), "stale injection relayed");

    // A client owns A's namespace: the host may not inject presence there.
    let mut a = connect(&f, b"grant-a", A).await;
    let mut from_a = presence_subscriber(&b, A).await;
    let mut client_seq = 10;
    first_presence(&mut a, &mut from_a, &mut client_seq).await;
    assert!(
        !f.live
            .relay
            .inject_presence(ROOM, A, &presence(u64::MAX - 1, b"host"))
    );
    // The refused injection did not raise the namespace's sequence floor.
    client_seq += 1;
    assert!(a.publish_presence(&presence(client_seq, b"client-next")));
    let after = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let datagram = from_a.recv_datagram().await.unwrap().unwrap();
            assert!(
                !datagram.payload.ends_with(b"host"),
                "host spoke for a client"
            );
            if datagram.payload.ends_with(b"client-next") {
                return datagram.payload.to_vec();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(after, presence(client_seq, b"client-next"));
    let counters = f.live.relay.counters();
    assert!(counters.injected > before.injected, "{counters:?}");
    assert!(
        counters.injected_dropped >= before.injected_dropped + 6,
        "{counters:?}"
    );
    drop(a);
    drop(b);
}

/// A revoked member stops receiving the room at once: nothing reaches it once
/// the relay observed the lapse, while the other members keep flowing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_member_receives_nothing_after_revocation() {
    let f = fixture();
    let a = connect(&f, b"grant-a", A).await;
    let mut b = connect(&f, b"grant-b", B).await;
    let viewer = connect(&f, b"grant-viewer", C).await;
    let mut b_at_a = presence_subscriber(&a, B).await;
    let mut b_at_viewer = presence_subscriber(&viewer, B).await;
    let mut seq = 0;
    first_presence(&mut b, &mut b_at_a, &mut seq).await;
    first_presence(&mut b, &mut b_at_viewer, &mut seq).await;
    let received = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = received.clone();
    let reader = tokio::spawn(async move {
        while let Ok(Some(_)) = b_at_a.recv_datagram().await {
            log.lock().unwrap().push(Instant::now());
        }
    });
    let publisher = tokio::spawn(async move {
        let mut seq = 10_000;
        loop {
            seq += 1;
            if !b.publish_presence(&presence(seq, b"b")) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !received.lock().unwrap().is_empty(),
        "b reaches a before revocation"
    );
    f.a.store(false, Ordering::SeqCst);
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while f.live.relay.counters().revoked == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("revocation observed");
    let observed = Instant::now();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let late = received
        .lock()
        .unwrap()
        .iter()
        .filter(|at| **at > observed + Duration::from_millis(20))
        .count();
    assert_eq!(
        late, 0,
        "the revoked member received {late} frames after revocation"
    );
    // The other members are untouched.
    let mut still = 0;
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while still < 3 {
            if let Ok(Some(_)) = b_at_viewer.recv_datagram().await {
                still += 1;
            }
        }
    })
    .await
    .expect("b keeps reaching the viewer");
    publisher.abort();
    reader.abort();
    drop((a, viewer));
}
