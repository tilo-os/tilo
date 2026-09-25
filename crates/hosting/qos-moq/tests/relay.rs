//! Real localhost WebTransport tests: no mocked carrier or TLS bypass.

use qos_moq::relay::{Denied, Direction, Relay, RelayAuthority, SessionAuthorization, VOICE_PATH};
use qos_moq::voice::{FRAME_SAMPLES, VoiceObject};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn shared_ingress_route_requires_exact_raw_authority_and_target() {
    let expected = "media.example.test:443";
    assert!(Relay::validate_route(expected, expected, VOICE_PATH).is_some());

    for target in [
        "/moq/v21/voice/",
        "/moq/v21/./voice",
        "/moq/v21/%2e/voice",
        "/moq/v21/other/../voice",
        "/moq/v21/voice?alias=1",
        "/moq/v21/voice#fragment",
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

fn draft() -> moq_net::Version {
    "moq-transport-21".parse().unwrap()
}

async fn setup() -> (
    url::Url,
    moq_native::Client,
    Arc<AtomicBool>,
    tokio::task::JoinHandle<()>,
) {
    let mut config = moq_native::ServerConfig::default();
    config.bind = Some("127.0.0.1:0".into());
    config.version = vec![draft()];
    config.tls.generate = vec!["localhost".into()];
    let server = config.init().unwrap();
    let url = format!(
        "https://localhost:{}{VOICE_PATH}",
        server.local_addr().unwrap().port()
    )
    .parse()
    .unwrap();
    let mut config = moq_native::ClientConfig::default();
    config.version = vec![draft()];
    config.tls.fingerprint = server.certificates().fingerprints();
    let client = config.init().unwrap();
    let active = Arc::new(AtomicBool::new(true));
    let relay = Relay::new(Arc::new(Authority {
        active: active.clone(),
    }));
    let task = tokio::spawn(relay.serve(server));
    (url, client, active, task)
}

#[tokio::test]
async fn exact_voice_route_accepts_and_rejects_other_routes_and_queries() {
    let (url, client, _active, server) = setup().await;
    let client = client.with_authorization(b"isolated-a".to_vec()).unwrap();
    let admitted = client.clone().connect(url.clone()).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), admitted.closed())
            .await
            .is_err()
    );
    admitted.abort(moq_net::Error::Cancel);
    for path in [
        "/moq",
        "/moq/v21/voice/",
        "/moq/v20/voice",
        "/moq/v21/voice?token=invalid",
    ] {
        let changed = url.join(path).unwrap();
        if let Ok(session) = client.clone().connect(changed).await {
            let _ = tokio::time::timeout(TIMEOUT, session.closed())
                .await
                .expect("wrong route closes");
        }
    }
    server.abort();
}

async fn read(origin: &moq_net::origin::Producer, path: &str) -> Vec<u8> {
    tokio::time::timeout(TIMEOUT, async {
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
    let (url, client, _active, server) = setup().await;
    let a_pub = moq_net::Origin::random().produce();
    let a_sub = moq_net::Origin::random().produce();
    let b_pub = moq_net::Origin::random().produce();
    let b_sub = moq_net::Origin::random().produce();
    let mut ab = a_pub
        .create_broadcast(
            "opaque-call/a",
            moq_net::broadcast::Route::new().with_announce(true),
        )
        .unwrap();
    let mut at = ab.create_track("audio", None).unwrap();
    let mut bb = b_pub
        .create_broadcast(
            "opaque-call/b",
            moq_net::broadcast::Route::new().with_announce(true),
        )
        .unwrap();
    let mut bt = bb.create_track("audio", None).unwrap();
    let a = client
        .clone()
        .with_authorization(b"isolated-a".to_vec())
        .unwrap()
        .with_publisher(&a_pub)
        .with_subscriber(a_sub.clone());
    let b = client
        .clone()
        .with_authorization(b"isolated-b".to_vec())
        .unwrap()
        .with_publisher(&b_pub)
        .with_subscriber(b_sub.clone());
    let a_session = tokio::time::timeout(TIMEOUT, a.connect(url.clone()))
        .await
        .unwrap()
        .unwrap();
    let b_session = tokio::time::timeout(TIMEOUT, b.connect(url.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a_session.version(), draft());
    assert_eq!(b_session.version(), draft());
    at.write_frame(moq_net::Timestamp::ZERO, voice(0)).unwrap();
    bt.write_frame(moq_net::Timestamp::ZERO, voice(1)).unwrap();
    let (a_rx, b_rx) = tokio::join!(read(&a_sub, "opaque-call/b"), read(&b_sub, "opaque-call/a"));
    assert_eq!(a_rx, voice(1));
    assert_eq!(b_rx, voice(0));
    a_session.abort(moq_net::Error::Cancel);
    drop(a_session);
    let again = tokio::time::timeout(TIMEOUT, a.connect(url))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.version(), draft());
    at.write_frame(moq_net::Timestamp::ZERO, voice(5)).unwrap();
    assert_eq!(read(&b_sub, "opaque-call/a").await, voice(5));
    again.abort(moq_net::Error::Cancel);
    b_session.abort(moq_net::Error::Cancel);
    server.abort();
}

#[tokio::test]
async fn denied_proof_expiry_and_active_revocation_close_real_sessions() {
    let (url, client, active, server) = setup().await;
    let denied = client
        .clone()
        .with_authorization(b"wrong-proof".to_vec())
        .unwrap()
        .connect(url.clone())
        .await;
    if let Ok(session) = denied {
        let _ = tokio::time::timeout(TIMEOUT, session.closed())
            .await
            .expect("denied before origin attachment");
    }
    let expired = client
        .clone()
        .with_authorization(b"isolated-expiry".to_vec())
        .unwrap()
        .connect(url.clone())
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
    let admitted = client
        .clone()
        .with_authorization(b"isolated-a".to_vec())
        .unwrap()
        .connect(url)
        .await
        .unwrap();
    // Authentication may finish just after the peer sees SETUP. Keep authority
    // valid until the session has had a chance to enter its forwarding loop.
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
    server.abort();
}

#[tokio::test]
async fn a_publish_grant_does_not_authorize_descendants_or_changed_namespaces() {
    let (url, client, _active, server) = setup().await;
    let incoming = moq_net::Origin::random().produce();
    let outgoing = moq_net::Origin::random().produce();
    let mut publications = Vec::new();
    for path in ["opaque-call/a"] {
        let mut broadcast = outgoing
            .create_broadcast(path, moq_net::broadcast::Route::new().with_announce(true))
            .unwrap();
        let mut track = broadcast.create_track("audio", None).unwrap();
        track
            .write_frame(moq_net::Timestamp::ZERO, voice(0))
            .unwrap();
        publications.push((broadcast, track));
    }
    let subscriber = client
        .clone()
        .with_authorization(b"isolated-b".to_vec())
        .unwrap()
        .with_subscriber(incoming.clone())
        .connect(url.clone())
        .await
        .unwrap();
    let publisher = client
        .clone()
        .with_authorization(b"isolated-a".to_vec())
        .unwrap()
        .with_publisher(&outgoing)
        .connect(url)
        .await
        .unwrap();
    assert_eq!(read(&incoming, "opaque-call/a").await, voice(0));
    // Establish the authorized path before injecting malformed publications:
    // rejecting those is permitted to close the whole publisher session.
    for path in ["opaque-call/a/child", "opaque-call/a-other"] {
        let mut broadcast = outgoing
            .create_broadcast(path, moq_net::broadcast::Route::new().with_announce(true))
            .unwrap();
        let mut track = broadcast.create_track("audio", None).unwrap();
        track
            .write_frame(moq_net::Timestamp::ZERO, voice(0))
            .unwrap();
        publications.push((broadcast, track));
    }
    for path in ["opaque-call/a/child", "opaque-call/a-other"] {
        assert!(
            tokio::time::timeout(
                Duration::from_millis(250),
                incoming.consume().announced_broadcast(path)
            )
            .await
            .is_err(),
            "changed namespace was relayed"
        );
    }
    publisher.abort(moq_net::Error::Cancel);
    subscriber.abort(moq_net::Error::Cancel);
    server.abort();
}

#[tokio::test]
async fn unauthorized_publisher_cannot_attach_media_before_admission() {
    let (url, client, _active, server) = setup().await;
    let outgoing = moq_net::Origin::random().produce();
    let incoming = moq_net::Origin::random().produce();
    let mut broadcast = outgoing
        .create_broadcast(
            "opaque-call/a",
            moq_net::broadcast::Route::new().with_announce(true),
        )
        .unwrap();
    let mut track = broadcast.create_track("audio", None).unwrap();
    track
        .write_frame(moq_net::Timestamp::ZERO, voice(0))
        .unwrap();
    let subscriber = client
        .clone()
        .with_authorization(b"isolated-b".to_vec())
        .unwrap()
        .with_subscriber(incoming.clone())
        .connect(url.clone())
        .await
        .unwrap();
    let denied = client
        .clone()
        .with_authorization(b"wrong-proof".to_vec())
        .unwrap()
        .with_publisher(&outgoing)
        .connect(url.clone())
        .await;
    if let Ok(session) = denied {
        let _ = tokio::time::timeout(TIMEOUT, session.closed())
            .await
            .expect("unauthorized publisher closes");
    }
    assert!(
        tokio::time::timeout(
            Duration::from_millis(250),
            incoming.consume().announced_broadcast("opaque-call/a")
        )
        .await
        .is_err(),
        "unauthorized media reached the relay origin"
    );
    // An authorized publication subsequently proves the observer and carrier.
    let admitted = client
        .with_authorization(b"isolated-a".to_vec())
        .unwrap()
        .with_publisher(&outgoing)
        .connect(url)
        .await
        .unwrap();
    assert_eq!(read(&incoming, "opaque-call/a").await, voice(0));
    admitted.abort(moq_net::Error::Cancel);
    subscriber.abort(moq_net::Error::Cancel);
    server.abort();
}

#[tokio::test]
async fn admitted_transient_authority_contention_survives_but_new_admission_denies() {
    use qos_moq::relay::AuthorizationStatus;
    struct Evidence {
        busy: Arc<AtomicBool>,
        expires: Instant,
    }
    impl SessionAuthorization for Evidence {
        fn publication(&self) -> Option<&str> {
            Some("test/a")
        }
        fn subscriptions(&self) -> &[String] {
            &[]
        }
        fn current(&self, _: Direction, _: &str) -> bool {
            !self.busy.load(Ordering::SeqCst)
        }
        fn status(&self, d: Direction, n: &str) -> AuthorizationStatus {
            if self.current(d, n) {
                AuthorizationStatus::Current
            } else {
                AuthorizationStatus::Unavailable {
                    expires_at: self.expires,
                }
            }
        }
    }
    struct Auth(Arc<Evidence>);
    impl RelayAuthority for Auth {
        fn admit(&self, _: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
            Ok(self.0.clone())
        }
    }
    let mut config = moq_native::ServerConfig::default();
    config.bind = Some("127.0.0.1:0".into());
    config.version = vec![draft()];
    config.tls.generate = vec!["localhost".into()];
    let server = config.init().unwrap();
    let url: url::Url = format!(
        "https://localhost:{}{VOICE_PATH}",
        server.local_addr().unwrap().port()
    )
    .parse()
    .unwrap();
    let mut config = moq_native::ClientConfig::default();
    config.version = vec![draft()];
    config.tls.fingerprint = server.certificates().fingerprints();
    let client = config.init().unwrap().with_authorization(vec![1]).unwrap();
    let busy = Arc::new(AtomicBool::new(false));
    let relay = Relay::new(Arc::new(Auth(Arc::new(Evidence {
        busy: busy.clone(),
        expires: Instant::now() + Duration::from_secs(30),
    }))));
    let task = tokio::spawn(relay.serve(server));
    let session = client.clone().connect(url.clone()).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), session.closed())
            .await
            .is_err()
    );
    busy.store(true, Ordering::SeqCst);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), session.closed())
            .await
            .is_err(),
        "ordinary owner commit must not disconnect admitted media"
    );
    // Contention that outlasts the bounded admission wait never admits
    // (total contention here stays below the admitted session's 900 ms fence).
    if let Ok(denied) = client.clone().connect(url.clone()).await {
        let _ = tokio::time::timeout(TIMEOUT, denied.closed())
            .await
            .expect("contention never admits");
    }
    busy.store(false, Ordering::SeqCst);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), session.closed())
            .await
            .is_err()
    );
    // Brief contention during a new join is waited out, then admitted only on
    // a Current observation; an honest join no longer fails on a busy owner.
    busy.store(true, Ordering::SeqCst);
    let release = {
        let busy = busy.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            busy.store(false, Ordering::SeqCst);
        })
    };
    let joined = client.connect(url).await.expect("brief contention admits");
    release.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(300), joined.closed())
            .await
            .is_err(),
        "session admitted after contention cleared must stay open"
    );
    task.abort();
}

async fn status_session(
    lease: Duration,
) -> (
    moq_net::Session,
    Arc<std::sync::atomic::AtomicU8>,
    tokio::task::JoinHandle<()>,
) {
    use qos_moq::relay::AuthorizationStatus;
    struct Evidence {
        state: Arc<std::sync::atomic::AtomicU8>,
        expires: std::sync::Mutex<Instant>,
    }
    impl SessionAuthorization for Evidence {
        fn publication(&self) -> Option<&str> {
            Some("bounded/a")
        }
        fn subscriptions(&self) -> &[String] {
            &[]
        }
        fn current(&self, _: Direction, _: &str) -> bool {
            self.state.load(Ordering::SeqCst) == 0
        }
        fn status(&self, _: Direction, _: &str) -> AuthorizationStatus {
            match self.state.load(Ordering::SeqCst) {
                0 => AuthorizationStatus::Current,
                1 => AuthorizationStatus::Unavailable {
                    expires_at: *self.expires.lock().unwrap(),
                },
                3 => {
                    // Deliberately let an in-progress successful observation
                    // return after the existing unavailable deadline. The
                    // validity branch was selected while that deadline was
                    // still in the future, making this race deterministic.
                    std::thread::sleep(Duration::from_millis(300));
                    self.state.store(0, Ordering::SeqCst);
                    AuthorizationStatus::Current
                }
                _ => AuthorizationStatus::Denied,
            }
        }
    }
    struct Auth(Arc<Evidence>, Duration);
    impl RelayAuthority for Auth {
        fn admit(&self, _: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
            *self.0.expires.lock().unwrap() = Instant::now() + self.1;
            Ok(self.0.clone())
        }
    }
    let mut config = moq_native::ServerConfig::default();
    config.bind = Some("127.0.0.1:0".into());
    config.version = vec![draft()];
    config.tls.generate = vec!["localhost".into()];
    let server = config.init().unwrap();
    let url: url::Url = format!(
        "https://localhost:{}{VOICE_PATH}",
        server.local_addr().unwrap().port()
    )
    .parse()
    .unwrap();
    let mut config = moq_native::ClientConfig::default();
    config.version = vec![draft()];
    config.tls.fingerprint = server.certificates().fingerprints();
    let client = config.init().unwrap().with_authorization(vec![1]).unwrap();
    let state = Arc::new(std::sync::atomic::AtomicU8::new(0));
    let relay = Relay::new(Arc::new(Auth(
        Arc::new(Evidence {
            state: state.clone(),
            expires: std::sync::Mutex::new(Instant::now()),
        }),
        lease,
    )));
    let task = tokio::spawn(relay.serve(server));
    let session = client.connect(url).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), session.closed())
            .await
            .is_err()
    );
    (session, state, task)
}
#[tokio::test]
async fn repeated_unavailable_cannot_renew_the_last_confirmed_authority_deadline() {
    let (session, state, task) = status_session(Duration::from_secs(30)).await;
    state.store(1, Ordering::SeqCst);
    assert!(
        tokio::time::timeout(Duration::from_millis(350), session.closed())
            .await
            .is_err()
    );
    let _ = tokio::time::timeout(Duration::from_millis(650), session.closed())
        .await
        .expect("repeated pending must close within900ms since last confirmed authority");
    task.abort();
}
#[tokio::test]
async fn unavailable_never_outlives_the_verified_short_grant() {
    let (session, state, task) = status_session(Duration::from_millis(400)).await;
    state.store(1, Ordering::SeqCst);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), session.closed())
            .await
            .is_err()
    );
    let _ = tokio::time::timeout(Duration::from_millis(300), session.closed())
        .await
        .expect("grant expiry precedes900ms grace");
    task.abort();
}
#[tokio::test]
async fn revocation_observed_after_contention_closes_without_another_grace_period() {
    let (session, state, task) = status_session(Duration::from_secs(30)).await;
    state.store(1, Ordering::SeqCst);
    assert!(
        tokio::time::timeout(Duration::from_millis(250), session.closed())
            .await
            .is_err()
    );
    state.store(2, Ordering::SeqCst);
    let _ = tokio::time::timeout(Duration::from_millis(200), session.closed())
        .await
        .expect("explicit deny closes on next observation");
    task.abort();
}

#[tokio::test]
async fn expired_unavailable_deadline_is_terminal_even_when_revalidation_returns_current() {
    for (lease, wait) in [
        (Duration::from_millis(450), Duration::from_millis(150)),
        (Duration::from_secs(30), Duration::from_millis(650)),
    ] {
        let (session, state, task) = status_session(lease).await;
        state.store(1, Ordering::SeqCst);
        assert!(tokio::time::timeout(wait, session.closed()).await.is_err());
        state.store(3, Ordering::SeqCst);
        let _ = tokio::time::timeout(Duration::from_millis(700), session.closed())
            .await
            .expect("a late Current must not revive an expired unavailable deadline");
        task.abort();
    }
}

#[tokio::test]
async fn current_observed_before_the_terminal_deadline_keeps_the_session_alive() {
    let (session, state, task) = status_session(Duration::from_secs(30)).await;
    state.store(1, Ordering::SeqCst);
    assert!(
        tokio::time::timeout(Duration::from_millis(350), session.closed())
            .await
            .is_err()
    );
    // The 300ms observation returns around800ms, before the900ms fence.
    // Keep observing past the old fence to prove valid recovery reset it.
    state.store(3, Ordering::SeqCst);
    assert!(
        tokio::time::timeout(Duration::from_millis(900), session.closed())
            .await
            .is_err()
    );
    task.abort();
}

#[tokio::test]
async fn real_webtransport_relays_voice_datagram_bundles_and_drops_invalid_ones() {
    let (url, client, _active, server) = setup().await;
    let a_pub = moq_net::Origin::random().produce();
    let b_sub = moq_net::Origin::random().produce();
    let mut ab = a_pub
        .create_broadcast(
            "opaque-call/a",
            moq_net::broadcast::Route::new().with_announce(true),
        )
        .unwrap();
    let mut at = ab.create_track("audio", None).unwrap();
    let a = client
        .clone()
        .with_authorization(b"isolated-a".to_vec())
        .unwrap()
        .with_publisher(&a_pub);
    let b = client
        .clone()
        .with_authorization(b"isolated-b".to_vec())
        .unwrap()
        .with_subscriber(b_sub.clone());
    let a_session = tokio::time::timeout(TIMEOUT, a.connect(url.clone()))
        .await
        .unwrap()
        .unwrap();
    let b_session = tokio::time::timeout(TIMEOUT, b.connect(url.clone()))
        .await
        .unwrap()
        .unwrap();
    let mut bundle = voice(2);
    bundle.extend_from_slice(&voice(1));
    let received = tokio::time::timeout(TIMEOUT, async {
        let broadcast = b_sub
            .consume()
            .announced_broadcast("opaque-call/a")
            .await
            .expect("authorized announce");
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
        tokio::time::timeout(Duration::from_millis(100), a_session.closed())
            .await
            .is_err(),
        "an invalid datagram must not close the publisher session"
    );
    a_session.abort(moq_net::Error::Cancel);
    b_session.abort(moq_net::Error::Cancel);
    server.abort();
}
