//! Revocation drains a real localhost WebTransport session: forwarding stops when
//! authority is lost, and GOAWAY makes the client close before the relay does. The
//! transport stays open for at most `GOAWAY_GRACE`, carrying no media.

use qos_moq::relay::{
    Denied, Direction, GOAWAY_GRACE, Relay, RelayAuthority, SessionAuthorization, VOICE_PATH,
};
use qos_moq::voice::{FRAME_SAMPLES, VoiceObject};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(5);
/// The relay's existing revocation bound (see tests/relay.rs).
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
fn receive(
    origin: moq_net::origin::Producer,
    path: &'static str,
    got: Log,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let broadcast = origin
            .consume()
            .announced_broadcast(path)
            .await
            .expect("authorized announce");
        let mut track = broadcast
            .track("audio")
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();
        while let Ok(Some(datagram)) = track.recv_datagram().await {
            let sequence = VoiceObject::decode(&datagram.payload).unwrap().sequence;
            got.lock().unwrap().push((sequence, Instant::now()));
        }
    })
}

fn sent_at(sent: &Log, sequence: u64) -> Instant {
    sent.lock()
        .unwrap()
        .iter()
        .find(|(s, _)| *s == sequence)
        .expect("received sequence was sent")
        .1
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revocation_stops_media_at_once_and_lets_the_client_close_first() {
    let mut config = moq_native::ServerConfig::default();
    config.bind = Some("127.0.0.1:0".into());
    config.version = vec!["moq-transport-21".parse().unwrap()];
    config.tls.generate = vec!["localhost".into()];
    let server = config.init().unwrap();
    let url: url::Url = format!(
        "https://localhost:{}{VOICE_PATH}",
        server.local_addr().unwrap().port()
    )
    .parse()
    .unwrap();
    let mut client = moq_native::ClientConfig::default();
    client.version = vec!["moq-transport-21".parse().unwrap()];
    client.tls.fingerprint = server.certificates().fingerprints();
    let client = client.init().unwrap();
    let a_active = Arc::new(AtomicBool::new(true));
    let relay = Relay::new(Arc::new(Authority {
        a: a_active.clone(),
    }));
    let serving = tokio::spawn(relay.clone().serve(server));

    let (a_pub, a_sub) = (
        moq_net::Origin::random().produce(),
        moq_net::Origin::random().produce(),
    );
    let (b_pub, b_sub) = (
        moq_net::Origin::random().produce(),
        moq_net::Origin::random().produce(),
    );
    let mut ab = a_pub
        .create_broadcast(
            "drain-call/a",
            moq_net::broadcast::Route::new().with_announce(true),
        )
        .unwrap();
    let mut bb = b_pub
        .create_broadcast(
            "drain-call/b",
            moq_net::broadcast::Route::new().with_announce(true),
        )
        .unwrap();
    let (a_sent, b_sent, a_got, b_got): (Log, Log, Log, Log) = Default::default();
    let a_session = tokio::time::timeout(
        TIMEOUT,
        client
            .clone()
            .with_authorization(b"drain-a".to_vec())
            .unwrap()
            .with_publisher(&a_pub)
            .with_subscriber(a_sub.clone())
            .connect(url.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    let b_session = tokio::time::timeout(
        TIMEOUT,
        client
            .with_authorization(b"drain-b".to_vec())
            .unwrap()
            .with_publisher(&b_pub)
            .with_subscriber(b_sub.clone())
            .connect(url),
    )
    .await
    .unwrap()
    .unwrap();
    let tasks = [
        publish(ab.create_track("audio", None).unwrap(), a_sent.clone()),
        publish(bb.create_track("audio", None).unwrap(), b_sent.clone()),
        receive(b_sub, "drain-call/a", b_got.clone()),
        receive(a_sub, "drain-call/b", a_got.clone()),
    ];

    // Media flows both ways before the revocation.
    tokio::time::timeout(TIMEOUT, async {
        while a_got.lock().unwrap().len() < 20 || b_got.lock().unwrap().len() < 20 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("media flows before revocation");

    let revoked = Instant::now();
    a_active.store(false, Ordering::SeqCst);
    let closed = tokio::time::timeout(REVOCATION_BOUND + GOAWAY_GRACE, a_session.closed())
        .await
        .expect("a revoked session closes");
    let closed_after = revoked.elapsed();
    // Keep publishing: nothing more may be forwarded after the drain.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let a_to_b = *b_got.lock().unwrap().last().unwrap();
    let b_to_a = *a_got.lock().unwrap().last().unwrap();
    // A->B: the newest frame B heard was offered before A's authority was dropped
    // at the relay; frames A offered later were not forwarded.
    let a_to_b_after = sent_at(&a_sent, a_to_b.0).saturating_duration_since(revoked);
    let b_to_a_after = b_to_a.1.saturating_duration_since(revoked);
    let a_offered_after = a_sent
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, t)| *t > revoked + REVOCATION_BOUND)
        .count();
    let b_heard_after = b_got
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, t)| *t > revoked + REVOCATION_BOUND)
        .count();
    println!(
        "a->b last forwarded frame offered {a_to_b_after:?} after revocation; \
         b->a last frame delivered {b_to_a_after:?} after revocation; \
         a closed {closed_after:?} after revocation ({closed}); \
         a offered {a_offered_after} frames after the bound, b heard {b_heard_after}; counters {:?}",
        relay.counters()
    );
    assert!(
        a_to_b_after < REVOCATION_BOUND,
        "revoked publisher forwarded {a_to_b_after:?} after revocation"
    );
    assert!(
        b_to_a_after < REVOCATION_BOUND,
        "revoked subscriber received {b_to_a_after:?} after revocation"
    );
    assert!(a_offered_after > 0 && b_heard_after == 0);
    // The client closed on GOAWAY, well before the relay's hard close.
    assert!(
        closed_after < GOAWAY_GRACE,
        "client did not close before the grace: {closed_after:?}"
    );
    // The unrevoked participant is untouched.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), b_session.closed())
            .await
            .is_err()
    );

    for task in tasks {
        task.abort();
    }
    b_session.abort(moq_net::Error::Cancel);
    serving.abort();
}
