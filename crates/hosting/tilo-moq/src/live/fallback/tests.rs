//! Fallback carrier bridged against real localhost WebTransport `MoQ` peers
//! (pinned generated certificate, no TLS bypass). Isolated test identities.
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
use super::super::peer::Peer;
use super::*;
use crate::carrier;

const ROOM: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const RECORD: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Session {
    publisher: &'static str,
    publish: bool,
    active: Arc<AtomicBool>,
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
        true
    }
    fn status(&self) -> AuthorizationStatus {
        if self.active.load(Ordering::SeqCst) {
            AuthorizationStatus::Current
        } else {
            AuthorizationStatus::Denied
        }
    }
}
struct Authority {
    b: Arc<AtomicBool>,
}
impl LiveAuthority for Authority {
    fn try_admit(&self, token: &[u8]) -> Result<Arc<dyn LiveSession>, Denied> {
        let (publisher, publish, active) = match token {
            b"grant-a" => (A, true, Arc::new(AtomicBool::new(true))),
            b"grant-b" => (B, true, self.b.clone()),
            b"grant-viewer" => (C, false, Arc::new(AtomicBool::new(true))),
            _ => return Err(Denied),
        };
        Ok(Arc::new(Session {
            publisher,
            publish,
            active,
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
    live: super::super::LoopbackLive,
    trust: carrier::Trust,
    b: Arc<AtomicBool>,
}
fn fixture() -> Fixture {
    let b = Arc::new(AtomicBool::new(true));
    let live = super::super::LoopbackLive::start(Arc::new(Authority { b: b.clone() })).unwrap();
    let trust = carrier::Trust::Fingerprints(live.fingerprints.clone());
    Fixture { live, trust, b }
}
fn path(publisher: &str) -> String {
    namespace_path(ROOM, publisher).unwrap()
}
fn bytes32(value: &str) -> [u8; 32] {
    unhex32(value).unwrap()
}
fn presence(seq: u64, tail: &[u8]) -> Vec<u8> {
    let mut frame = seq.to_le_bytes().to_vec();
    frame.extend_from_slice(tail);
    frame
}

async fn connect(f: &Fixture, token: &[u8], publish_as: &str) -> Peer {
    let url: url::Url = f.live.endpoint.parse().unwrap();
    tokio::time::timeout(
        tilo_test_env::hang_bound(),
        Peer::connect(&url, &f.trust, token, Some(&path(publish_as))),
    )
    .await
    .unwrap()
    .unwrap()
}

/// Parsed server stream.
struct Stream {
    inner: FallbackStream,
    pending: Vec<u8>,
}
impl Stream {
    async fn frame(&mut self) -> (u8, Vec<u8>) {
        loop {
            if self.pending.len() >= 5 {
                let len = u32::from_be_bytes(self.pending[1..5].try_into().unwrap()) as usize;
                if self.pending.len() >= 5 + len {
                    let kind = self.pending[0];
                    let payload = self.pending[5..5 + len].to_vec();
                    self.pending.drain(..5 + len);
                    return (kind, payload);
                }
            }
            let chunk = tokio::time::timeout(tilo_test_env::hang_bound(), self.inner.next())
                .await
                .expect("stream frame deadline")
                .expect("stream open");
            self.pending.extend_from_slice(&chunk);
        }
    }
    /// Next frame of `kind`, skipping others.
    async fn until(&mut self, kind: u8) -> Vec<u8> {
        tokio::time::timeout(tilo_test_env::hang_bound(), async {
            loop {
                let (next, payload) = self.frame().await;
                if next == kind {
                    return payload;
                }
            }
        })
        .await
        .expect("frame kind deadline")
    }
}
async fn open(f: &Fixture, token: &[u8], id: u8) -> ([u8; 16], Stream) {
    let stream = f
        .live
        .relay
        .fallback_open(token, [id; 16])
        .expect("fallback admitted");
    let mut stream = Stream {
        inner: stream,
        pending: Vec::new(),
    };
    let (kind, hello) = stream.frame().await;
    assert_eq!(kind, FRAME_HELLO);
    assert_eq!(&hello[..16], &[id; 16]);
    (hello[..16].try_into().unwrap(), stream)
}

#[test]
fn frames_round_trip_and_session_ids_are_exact_lowercase_hex() {
    let batch = [
        encode_frame(FRAME_SEND_PRESENCE, &[b"abc"]),
        encode_frame(FRAME_SEND_OPS, &[&[7; 32], b"ok"]),
    ]
    .concat();
    let frames = decode_frames(&batch).unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0], (FRAME_SEND_PRESENCE, &b"abc"[..]));
    assert_eq!(frames[1].1.len(), 34);
    assert!(decode_frames(&batch[..batch.len() - 1]).is_none());
    assert_eq!(session_id(&"ab".repeat(16)), Some([0xab; 16]));
    assert!(session_id(&"AB".repeat(16)).is_none() && session_id("ab").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_tokens_and_sessions_are_refused() {
    let f = fixture();
    assert_eq!(
        f.live.relay.fallback_open(b"forged", [1; 16]).err(),
        Some(FallbackError::Denied)
    );
    assert_eq!(
        f.live.relay.fallback_send(&[9; 16], b"").err(),
        Some(FallbackError::Gone)
    );
    assert_eq!(
        f.live.relay.fallback_resume(&[9; 16]).err(),
        Some(FallbackError::Gone)
    );
    let (id, _stream) = open(&f, b"grant-viewer", 3).await;
    // A viewer may subscribe but never publish.
    assert_eq!(
        f.live
            .relay
            .fallback_send(
                &id,
                &encode_frame(FRAME_SEND_PRESENCE, &[&presence(1, b"")])
            )
            .err(),
        Some(FallbackError::Denied)
    );
    assert_eq!(
        f.live.relay.fallback_send(&id, b"\x11\x00").err(),
        Some(FallbackError::Invalid)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn presence_bridges_both_ways_between_webtransport_and_fallback() {
    let f = fixture();
    let mut a = connect(&f, b"grant-a", A).await;
    let (id, mut stream) = open(&f, b"grant-b", 2).await;
    // The fallback member sees the MoQ member announced, then its presence.
    let member = stream.until(FRAME_MEMBER).await;
    assert_eq!((&member[..32], member[32]), (&bytes32(A)[..], 1));
    let received = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        let mut seq = 10;
        loop {
            seq += 1;
            assert!(a.publish_presence(&presence(seq, b"from-a")));
            let next =
                tokio::time::timeout(Duration::from_millis(50), stream.until(FRAME_PRESENCE)).await;
            if let Ok(payload) = next {
                return payload;
            }
        }
    })
    .await
    .expect("MoQ presence reaches the fallback stream");
    assert_eq!(&received[..32], &bytes32(A)[..]);
    assert!(received[32..].ends_with(b"from-a"));

    // The MoQ member sees the fallback member announced and receives its presence.
    let from_b = tokio::time::timeout(tilo_test_env::hang_bound(), a.member(&path(B)))
        .await
        .unwrap()
        .expect("fallback publisher announced");
    let mut subscriber = from_b
        .track(PRESENCE_TRACK)
        .unwrap()
        .subscribe(None)
        .await
        .unwrap();
    let payload = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        let mut seq = 100;
        loop {
            seq += 1;
            f.live
                .relay
                .fallback_send(
                    &id,
                    &encode_frame(FRAME_SEND_PRESENCE, &[&presence(seq, b"from-b")]),
                )
                .unwrap();
            if let Ok(Ok(Some(datagram))) =
                tokio::time::timeout(Duration::from_millis(50), subscriber.recv_datagram()).await
            {
                return datagram.payload.to_vec();
            }
        }
    })
    .await
    .expect("fallback presence reaches the MoQ subscriber");
    assert!(payload.ends_with(b"from-b"));
    // Stale sequence numbers from the fallback publisher are dropped by the relay.
    f.live
        .relay
        .fallback_send(
            &id,
            &encode_frame(FRAME_SEND_PRESENCE, &[&presence(1, b"stale")]),
        )
        .unwrap();
    f.live
        .relay
        .fallback_send(
            &id,
            &encode_frame(FRAME_SEND_PRESENCE, &[&presence(100_000, b"fresh")]),
        )
        .unwrap();
    let next = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let datagram = subscriber.recv_datagram().await.unwrap().unwrap();
            if datagram.payload.ends_with(b"fresh") || datagram.payload.ends_with(b"stale") {
                return datagram.payload.to_vec();
            }
        }
    })
    .await
    .unwrap();
    assert!(next.ends_with(b"fresh"), "stale fallback presence relayed");
    drop(a);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ops_bridge_both_ways_in_order_and_invalid_frames_are_not_relayed() {
    let f = fixture();
    let mut a = connect(&f, b"grant-a", A).await;
    let a_ops = a.record(RECORD).unwrap().clone();
    let (id, mut stream) = open(&f, b"grant-b", 4).await;
    stream.until(FRAME_MEMBER).await;
    let subscribe = encode_frame(FRAME_SUBSCRIBE_OPS, &[&bytes32(A), &bytes32(RECORD)]);
    f.live.relay.fallback_send(&id, &subscribe).unwrap();
    // MoQ -> fallback: one ordered group.
    let frames = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut group = a_ops.append_group().unwrap();
            for i in 0..5 {
                group
                    .write_frame(
                        moq_net::Timestamp::ZERO,
                        format!("ok-{attempt}-{i}").into_bytes(),
                    )
                    .unwrap();
            }
            group.finish().unwrap();
            let mut out = Vec::new();
            while let Ok((kind, payload)) =
                tokio::time::timeout(Duration::from_millis(100), stream.frame()).await
            {
                if kind == FRAME_OPS {
                    assert_eq!(&payload[..64], &[bytes32(A), bytes32(RECORD)].concat()[..]);
                    out.push(String::from_utf8(payload[64..].to_vec()).unwrap());
                    if out.len() == 5 {
                        return out;
                    }
                } else if kind == FRAME_OPS_END {
                    f.live.relay.fallback_send(&id, &subscribe).unwrap();
                }
            }
        }
    })
    .await
    .expect("MoQ ops reach the fallback stream");
    let attempt = frames[0].split('-').nth(1).unwrap().to_owned();
    assert_eq!(
        frames,
        (0..5)
            .map(|i| format!("ok-{attempt}-{i}"))
            .collect::<Vec<_>>()
    );

    // fallback -> MoQ: B's record broadcast feeds the egress track on demand.
    let from_b = tokio::time::timeout(tilo_test_env::hang_bound(), a.member(&path(B)))
        .await
        .unwrap()
        .unwrap();
    let subscription = moq_net::track::Subscription::default().with_max_age(Duration::from_secs(2));
    let mut subscriber = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            if let Ok(subscriber) = from_b
                .track(&format!("ops/{RECORD}"))
                .unwrap()
                .subscribe(subscription.clone())
                .await
            {
                return subscriber;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let batch = |tag: &str| {
        [
            encode_frame(
                FRAME_SEND_OPS,
                &[&bytes32(RECORD), format!("ok-{tag}-0").as_bytes()],
            ),
            encode_frame(
                FRAME_SEND_OPS,
                &[&bytes32(RECORD), format!("ok-{tag}-1").as_bytes()],
            ),
        ]
        .concat()
    };
    let received = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        let mut round = 0;
        loop {
            round += 1;
            f.live
                .relay
                .fallback_send(&id, &batch(&round.to_string()))
                .unwrap();
            if let Ok(Ok(Some(mut group))) =
                tokio::time::timeout(Duration::from_millis(100), subscriber.recv_group()).await
            {
                let mut frames = Vec::new();
                while let Ok(Some(frame)) = group.read_frame().await {
                    frames.push(String::from_utf8(frame.payload.to_vec()).unwrap());
                }
                if frames.len() == 2 {
                    return frames;
                }
            }
        }
    })
    .await
    .expect("fallback ops reach the MoQ subscriber");
    let tag = received[0].split('-').nth(1).unwrap().to_owned();
    assert_eq!(received, [format!("ok-{tag}-0"), format!("ok-{tag}-1")]);
    // Invalid frames (the relay's codec check) never pass.
    let forged = encode_frame(FRAME_SEND_OPS, &[&bytes32(RECORD), b"forged"]);
    f.live.relay.fallback_send(&id, &forged).unwrap();
    let ok = encode_frame(FRAME_SEND_OPS, &[&bytes32(RECORD), b"ok-after"]);
    f.live.relay.fallback_send(&id, &ok).unwrap();
    let after = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let Some(mut group) = subscriber.recv_group().await.unwrap() else {
                continue;
            };
            let mut frames = Vec::new();
            while let Ok(Some(frame)) = group.read_frame().await {
                frames.push(frame.payload.to_vec());
            }
            assert!(
                !frames.contains(&b"forged".to_vec()),
                "invalid fallback frame relayed"
            );
            if frames == [b"ok-after".to_vec()] {
                return true;
            }
        }
    })
    .await
    .unwrap();
    assert!(after);
    drop(a);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_keeps_the_session_and_revocation_ends_it() {
    let f = fixture();
    let a = connect(&f, b"grant-a", A).await;
    let (id, stream) = open(&f, b"grant-b", 5).await;
    drop(stream);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut resumed = Stream {
        inner: f
            .live
            .relay
            .fallback_resume(&id)
            .expect("resume within grace"),
        pending: Vec::new(),
    };
    assert_eq!(resumed.frame().await.0, FRAME_HELLO);
    // A MoQ subscriber sees the fallback publisher; revocation withdraws it.
    let from_b = tokio::time::timeout(tilo_test_env::hang_bound(), a.member(&path(B)))
        .await
        .unwrap()
        .unwrap();
    f.b.store(false, Ordering::SeqCst);
    assert_eq!(resumed.until(FRAME_END).await, [END_REVOKED]);
    tokio::time::timeout(tilo_test_env::hang_bound(), from_b.closed())
        .await
        .expect("revoked namespace withdrawn");
    assert_eq!(
        f.live.relay.fallback_send(&id, b"").err(),
        Some(FallbackError::Gone)
    );
    assert!(f.live.relay.counters().revoked >= 1);
    drop(a);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_collection_preserves_an_in_flight_namespace() {
    let f = fixture();
    let namespace = path(A);
    let reservation = f.live.relay.0.slot(&namespace, true).unwrap();
    f.live.relay.maintain();
    assert!(
        f.live.relay.0.slot(&namespace, false).is_some(),
        "maintenance must not withdraw a namespace while its owner is being attached"
    );
    drop(reservation);
    f.live.relay.maintain();
    assert!(
        f.live.relay.0.slot(&namespace, false).is_none(),
        "an abandoned namespace remains collectable"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_collection_rechecks_current_client_ownership() {
    let f = fixture();
    let (_id, _stream) = open(&f, b"grant-a", 44).await;
    let namespace = path(A);
    // Commit a maintenance candidate collected before this client attached.
    f.live.relay.0.remove_slot(&namespace, None);
    assert!(
        f.live.relay.0.slot(&namespace, false).is_some(),
        "a stale idle candidate must not withdraw a currently authorized client"
    );
    let (_subscriber, mut stream) = open(&f, b"grant-b", 45).await;
    let member = stream.until(FRAME_MEMBER).await;
    assert_eq!((&member[..32], member[32]), (&bytes32(A)[..], 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fallback_to_fallback_presence_is_under_ten_milliseconds() {
    let f = fixture();
    let (a, _a_stream) = open(&f, b"grant-a", 6).await;
    let (_b, mut b_stream) = open(&f, b"grant-b", 7).await;
    b_stream.until(FRAME_MEMBER).await;
    let mut samples = Vec::new();
    let mut seq = 0;
    while samples.len() < 20 {
        seq += 1;
        let sent = Instant::now();
        f.live
            .relay
            .fallback_send(
                &a,
                &encode_frame(FRAME_SEND_PRESENCE, &[&presence(seq, b"t")]),
            )
            .unwrap();
        if tokio::time::timeout(Duration::from_millis(200), b_stream.until(FRAME_PRESENCE))
            .await
            .is_ok()
        {
            samples.push(sent.elapsed());
        }
        // A move after the stream's presence gap goes at once.
        tokio::time::sleep(PRESENCE_GAP + Duration::from_millis(10)).await;
    }
    samples.sort();
    let median = samples[samples.len() / 2];
    assert!(median < Duration::from_millis(10), "median {median:?}");
}

/// Sessions watching the same member share one presence subscription, each
/// still gets every move, and a session that ends gives up its seat.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watchers_of_a_member_share_one_presence_feed() {
    let f = fixture();
    let (a, _a_stream) = open(&f, b"grant-a", 6).await;
    let (_b, mut b_stream) = open(&f, b"grant-b", 7).await;
    let (_viewer, mut viewer_stream) = open(&f, b"grant-viewer", 8).await;
    b_stream.until(FRAME_MEMBER).await;
    loop {
        let member = viewer_stream.until(FRAME_MEMBER).await;
        if member[..32] == bytes32(A) {
            break;
        }
    }
    let seated = || {
        f.live.relay.0.fallback.feeds.lock().unwrap()[&path(A)]
            .upgrade()
            .map_or(0, |feed| feed.sessions.lock().unwrap().len())
    };
    assert_eq!(seated(), 2);
    f.live
        .relay
        .fallback_send(
            &a,
            &encode_frame(FRAME_SEND_PRESENCE, &[&presence(1, b"t")]),
        )
        .unwrap();
    for stream in [&mut b_stream, &mut viewer_stream] {
        let payload = stream.until(FRAME_PRESENCE).await;
        assert_eq!(
            (&payload[..32], &payload[32..]),
            (&bytes32(A)[..], &presence(1, b"t")[..])
        );
    }
    f.b.store(false, Ordering::SeqCst);
    assert_eq!(b_stream.until(FRAME_END).await, [END_REVOKED]);
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while seated() != 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the ended session leaves the feed");
}

/// A member moving faster than the presence gap reaches a stream at most
/// once a gap, and its last position always arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fast_presence_is_paced_and_its_latest_value_arrives() {
    let f = fixture();
    let (a, _a_stream) = open(&f, b"grant-a", 6).await;
    let (_b, mut b_stream) = open(&f, b"grant-b", 7).await;
    b_stream.until(FRAME_MEMBER).await;
    let begun = Instant::now();
    let last = 50;
    for seq in 1..=last {
        f.live
            .relay
            .fallback_send(
                &a,
                &encode_frame(FRAME_SEND_PRESENCE, &[&presence(seq, b"t")]),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(4)).await;
    }
    let mut received = 0u32;
    loop {
        let payload = b_stream.until(FRAME_PRESENCE).await;
        received += 1;
        if payload[32..] == presence(last, b"t")[..] {
            break;
        }
    }
    let gaps = begun.elapsed().as_millis() / PRESENCE_GAP.as_millis() + 1;
    assert!(
        u128::from(received) <= gaps,
        "{received} presence writes over {gaps} gaps"
    );
}

/// Host presence injected into a host-fed namespace (an agent) reaches an
/// HTTPS fallback subscriber: the member is announced, then its presence.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn injected_host_presence_reaches_the_fallback_stream() {
    let f = fixture();
    let (_id, mut stream) = open(&f, b"grant-b", 4).await;
    let mut seq = 1;
    assert!(
        f.live
            .relay
            .inject_presence(ROOM, C, &presence(seq, b"agent"))
    );
    let member = stream.until(FRAME_MEMBER).await;
    assert_eq!((&member[..32], member[32]), (&bytes32(C)[..], 1));
    let received = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            seq += 1;
            assert!(
                f.live
                    .relay
                    .inject_presence(ROOM, C, &presence(seq, b"agent"))
            );
            let next =
                tokio::time::timeout(Duration::from_millis(50), stream.until(FRAME_PRESENCE)).await;
            if let Ok(payload) = next {
                return payload;
            }
        }
    })
    .await
    .expect("injected presence reaches the fallback stream");
    assert_eq!(&received[..32], &bytes32(C)[..]);
    assert!(received[32..].ends_with(b"agent"));
    assert!(
        !f.live
            .relay
            .inject_presence(ROOM, C, &presence(1, b"stale"))
    );
}
