//! Revocation drains a real localhost WebTransport session: forwarding of the
//! revoked publication stops when authority is lost, and the revocation notice
//! on the admission stream makes the client close before the relay does. The
//! transport stays open for at most `GOAWAY_GRACE`. How fast all that happens
//! is a measurement for a quiet machine, apart from what must hold however
//! slow the run.
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
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use support::{Dialer, Origin};
use tilo_moq::relay::{
    Denied, Direction, GOAWAY_GRACE, LoopbackRelay, RelayAuthority, SessionAuthorization,
};
use tilo_moq::voice::{FRAME_SAMPLES, VoiceObject};

/// How soon the relay should observe a revocation and stop its media.
const REVOCATION_BOUND: Duration = Duration::from_secs(1);

struct Permit {
    publish: String,
    subscribe: Vec<String>,
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
            && match direction {
                Direction::Publish => namespace == self.publish,
                Direction::Subscribe => self.subscribe.iter().any(|name| name == namespace),
            }
    }
}

/// `drain-a` can be revoked alone; `drain-b` stays current.
struct Authority {
    a: Arc<AtomicBool>,
}

impl RelayAuthority for Authority {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let (publish, subscribe, active) = match proof {
            b"drain-a" => ("drain-call/a", "drain-call/b", self.a.clone()),
            b"drain-b" => (
                "drain-call/b",
                "drain-call/a",
                Arc::new(AtomicBool::new(true)),
            ),
            _ => return Err(Denied),
        };
        Ok(Arc::new(Permit {
            publish: publish.into(),
            subscribe: vec![subscribe.into()],
            active,
        }))
    }
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

type Log = Arc<Mutex<Vec<(u64, Instant)>>>;

/// Publish one voice datagram every 10 ms, logging when each was offered.
fn publish(mut track: moq_net::track::Producer, sent: Log) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        for sequence in 0.. {
            tick.tick().await;
            if track
                .append_datagram(moq_net::Timestamp::ZERO, voice(sequence))
                .is_err()
            {
                break;
            }
            sent.lock().unwrap().push((sequence, Instant::now()));
        }
    })
}

/// Log every voice datagram received on `path`.
fn receive(origin: Arc<Origin>, path: &'static str, got: Log) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let broadcast = origin.broadcast(path).await;
        let mut track = broadcast
            .track("audio")
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();
        while let Ok(Some(datagram)) = track.recv_datagram().await {
            // The relay adds earlier objects to a datagram while it measures
            // loss on this downlink (a loaded runner can show some on
            // loopback): log each object once, oldest first, as a receiver
            // plays it.
            for object in tilo_moq::voice::bundle_objects(&datagram.payload)
                .unwrap()
                .into_iter()
                .rev()
            {
                let sequence = VoiceObject::decode(object).unwrap().sequence;
                let mut got = got.lock().unwrap();
                if got.iter().all(|(seen, _)| *seen != sequence) {
                    got.push((sequence, Instant::now()));
                }
            }
        }
    })
}

fn last_sequence(log: &Log) -> u64 {
    log.lock()
        .unwrap()
        .last()
        .map_or(0, |(sequence, _)| *sequence)
}

fn sent_at(sent: &Log, sequence: u64) -> Instant {
    sent.lock()
        .unwrap()
        .iter()
        .find(|(s, _)| *s == sequence)
        .expect("received sequence was sent")
        .1
}

/// What one revocation of A, while A and B exchange voice, looked like.
struct Drain {
    /// When the test revoked A's authority.
    revoked: Instant,
    /// The newest objects A and B had offered once the relay counted the lapse.
    a_cut: u64,
    b_cut: u64,
    /// How A's session ended.
    closed: moq_net::Error,
    /// When A's session closed.
    closed_at: Instant,
    a_sent: Log,
    b_sent: Log,
    a_got: Log,
    b_got: Log,
    /// B's session was still open once A's had closed and every receiver
    /// of A's media had ended.
    b_open: bool,
    counters: tilo_moq::relay::CounterSnapshot,
}

async fn drain() -> Drain {
    let a_active = Arc::new(AtomicBool::new(true));
    let relay = LoopbackRelay::start(Arc::new(Authority {
        a: a_active.clone(),
    }))
    .unwrap();
    let dialer = Dialer::new(&relay.endpoint, &relay.fingerprints);
    let (a_pub, a_sub) = (Origin::new(), Arc::new(Origin::new()));
    let (b_pub, b_sub) = (Origin::new(), Arc::new(Origin::new()));
    let (_ab, a_track) = a_pub.publish("drain-call/a");
    let (_bb, b_track) = b_pub.publish("drain-call/b");
    let (a_sent, b_sent, a_got, b_got): (Log, Log, Log, Log) = Default::default();
    let a_session = tilo_test_env::within(
        "A connects",
        dialer.connect(b"drain-a", Some(&a_pub), Some(&a_sub)),
    )
    .await
    .unwrap();
    let b_session = tilo_test_env::within(
        "B connects",
        dialer.connect(b"drain-b", Some(&b_pub), Some(&b_sub)),
    )
    .await
    .unwrap();
    let publishers = [
        publish(a_track, a_sent.clone()),
        publish(b_track, b_sent.clone()),
    ];
    let a_to_b = receive(b_sub, "drain-call/a", b_got.clone());
    let b_to_a = receive(a_sub, "drain-call/b", a_got.clone());

    // Media flows both ways before the revocation.
    tilo_test_env::within("media flows before revocation", async {
        while a_got.lock().unwrap().len() < 20 || b_got.lock().unwrap().len() < 20 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    let revoked = Instant::now();
    a_active.store(false, Ordering::SeqCst);
    // The relay counts the lapse once A's media is cut off both ways.
    tilo_test_env::wait_until("revocation observed", || relay.counters().expired == 1);
    let (a_cut, b_cut) = (last_sequence(&a_sent), last_sequence(&b_sent));
    let closed = tilo_test_env::within("a revoked session closes", a_session.closed()).await;
    let closed_at = Instant::now();
    // Both keep publishing past the cut while their receivers end: A's
    // media ends at B once its publication is gone, B's at A with A's session.
    tilo_test_env::wait_until("both publish past the revocation", || {
        last_sequence(&a_sent) > a_cut + 5 && last_sequence(&b_sent) > b_cut + 5
    });
    tilo_test_env::within("A's media ends at B", a_to_b)
        .await
        .unwrap();
    tilo_test_env::within("B's media ends at A", b_to_a)
        .await
        .unwrap();
    let b_open = {
        let mut b_closed = std::pin::pin!(b_session.closed());
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::future::Future::poll(b_closed.as_mut(), cx).is_pending())
        })
        .await
    };
    for task in publishers {
        task.abort();
    }
    let counters = relay.counters();
    drop(b_session);
    Drain {
        revoked,
        a_cut,
        b_cut,
        closed,
        closed_at,
        a_sent,
        b_sent,
        a_got,
        b_got,
        b_open,
        counters,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revocation_stops_media_and_leaves_the_other_participant_alone() {
    let drain = drain().await;
    // Nothing either side offered once the relay counted the lapse crossed it.
    let late = |got: &Log, cut: u64| {
        got.lock()
            .unwrap()
            .iter()
            .filter(|(sequence, _)| *sequence > cut)
            .count()
    };
    let a_to_b_late = late(&drain.b_got, drain.a_cut);
    let b_to_a_late = late(&drain.a_got, drain.b_cut);
    assert_eq!(
        a_to_b_late, 0,
        "the revoked publisher's media was forwarded after revocation"
    );
    assert_eq!(
        b_to_a_late, 0,
        "the revoked subscriber received media after revocation"
    );
    assert!(last_sequence(&drain.a_sent) > drain.a_cut);
    assert!(last_sequence(&drain.b_sent) > drain.b_cut);
    // The unrevoked participant is untouched.
    assert!(drain.b_open, "B's session closed with A's revocation");
    assert_eq!(drain.counters.expired, 1, "{:?}", drain.counters);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "measurement; run with --ignored --nocapture"]
async fn revocation_drain_timing() {
    let drain = drain().await;
    let revoked = drain.revoked;
    let a_to_b = *drain.b_got.lock().unwrap().last().unwrap();
    let b_to_a = *drain.a_got.lock().unwrap().last().unwrap();
    // A->B: the newest frame B heard was offered before A's authority was
    // dropped at the relay; B->A: the last frame A heard.
    let a_to_b_after = sent_at(&drain.a_sent, a_to_b.0).saturating_duration_since(revoked);
    let b_to_a_after = b_to_a.1.saturating_duration_since(revoked);
    let closed_after = drain.closed_at.saturating_duration_since(revoked);
    let b_offered_after = drain
        .b_sent
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, t)| *t > revoked + REVOCATION_BOUND)
        .count();
    println!(
        "a->b last forwarded frame offered {a_to_b_after:?} after revocation; \
         b->a last frame delivered {b_to_a_after:?} after revocation; \
         a closed {closed_after:?} after revocation ({}); \
         b offered {b_offered_after} frames after the bound; counters {:?}",
        drain.closed, drain.counters
    );
    assert!(
        a_to_b_after < REVOCATION_BOUND,
        "revoked publisher forwarded {a_to_b_after:?} after revocation"
    );
    assert!(
        b_to_a_after < REVOCATION_BOUND,
        "revoked subscriber received {b_to_a_after:?} after revocation"
    );
    // The client closed on the revocation notice, well before the relay's
    // hard close.
    assert!(
        closed_after < GOAWAY_GRACE,
        "client did not close before the grace: {closed_after:?}"
    );
}
