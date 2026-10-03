//! The admission preface over real localhost WebTransport: a session that
//! skips or botches the preface is closed and never reaches `MoQ` or the host
//! authority.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unchecked_time_subtraction,
    reason = "in a test these are the assertion; fixture arithmetic is small and bounded"
)]
use super::*;
use crate::relay::{
    Denied, Direction, LoopbackRelay, RelayAuthority, SessionAuthorization, VOICE_PATH,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const NAME: &str = "call/a";

struct Permit;
impl SessionAuthorization for Permit {
    fn publication(&self) -> Option<&str> {
        Some(NAME)
    }
    fn subscriptions(&self) -> &[String] {
        &[]
    }
    fn current(&self, _: Direction, _: &str) -> bool {
        true
    }
}

/// Admits exactly the grant `good`, counting every admission attempt.
#[derive(Default)]
struct Counting(AtomicUsize);
impl RelayAuthority for Counting {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if proof == b"good" {
            Ok(Arc::new(Permit))
        } else {
            Err(Denied)
        }
    }
}

async fn open(relay: &LoopbackRelay) -> web_transport_quinn::Session {
    let hashes = relay
        .fingerprints
        .iter()
        .map(|value| hex::decode(value).unwrap())
        .collect();
    let client = web_transport_quinn::ClientBuilder::new()
        .with_server_certificate_hashes(hashes)
        .unwrap();
    let url: url::Url = relay.endpoint.parse().unwrap();
    client
        .connect(web_transport_quinn::proto::ConnectRequest::new(url).with_protocol(PROTOCOL))
        .await
        .unwrap()
}

/// The WebTransport close code the host used, once the session closed.
async fn close_code(session: &web_transport_quinn::Session) -> Option<u32> {
    match tokio::time::timeout(PREFACE_BUDGET + Duration::from_secs(2), session.closed()).await {
        Ok(web_transport_quinn::SessionError::WebTransportError(
            web_transport_quinn::WebTransportError::Closed(code, _),
        )) => Some(code),
        _ => None,
    }
}

#[tokio::test]
async fn session_that_never_sends_a_preface_is_closed_after_the_budget() {
    let authority = Arc::new(Counting::default());
    let relay = LoopbackRelay::start(authority.clone()).unwrap();
    let session = open(&relay).await;
    // A MoQ client that skips the preface opens its SETUP stream directly.
    let mut setup = session.open_uni().await.unwrap();
    setup.write_all(&[0x2f, 0x00, 0x01]).await.unwrap();
    let started = std::time::Instant::now();
    assert_eq!(close_code(&session).await, Some(CLOSE_PREFACE));
    assert!(started.elapsed() >= PREFACE_BUDGET - Duration::from_millis(500));
    assert_eq!(authority.0.load(Ordering::SeqCst), 0);
    assert_eq!(relay.counters().accepted, 0);
}

#[tokio::test]
async fn malformed_prefaces_are_refused_before_any_admission() {
    let oversized = u16::try_from(MAX_GRANT_BYTES + 1).unwrap().to_be_bytes();
    let cases: [(&str, Vec<u8>); 4] = [
        ("wrong version", vec![2, 0, 4, b'g', b'o', b'o', b'd']),
        ("empty grant", vec![PREFACE_VERSION, 0, 0]),
        (
            "grant above the ceiling (refused on the header alone)",
            vec![PREFACE_VERSION, oversized[0], oversized[1]],
        ),
        ("truncated grant", vec![PREFACE_VERSION, 0, 4, b'g', b'o']),
    ];
    for (case, bytes) in cases {
        let authority = Arc::new(Counting::default());
        let relay = LoopbackRelay::start(authority.clone()).unwrap();
        let session = open(&relay).await;
        let (mut send, _recv) = session.open_bi().await.unwrap();
        send.write_all(&bytes).await.unwrap();
        send.finish().unwrap();
        let started = std::time::Instant::now();
        assert_eq!(close_code(&session).await, Some(CLOSE_PREFACE), "{case}");
        assert!(
            started.elapsed() < PREFACE_BUDGET,
            "{case} waited for the budget"
        );
        assert_eq!(authority.0.load(Ordering::SeqCst), 0, "{case}");
        assert_eq!(relay.counters().accepted, 0, "{case}");
    }
}

#[tokio::test]
async fn refused_grant_is_answered_denied_and_never_reaches_moq() {
    let authority = Arc::new(Counting::default());
    let relay = LoopbackRelay::start(authority.clone()).unwrap();
    let session = open(&relay).await;
    assert!(matches!(
        present(&session, b"forged").await,
        Err(AdmissionError::Denied)
    ));
    assert_eq!(close_code(&session).await, Some(CLOSE_DENIED));
    assert_eq!(authority.0.load(Ordering::SeqCst), 1);
    assert_eq!(relay.counters().accepted, 0);
}

#[tokio::test]
async fn admitted_grant_reaches_moq() {
    let authority = Arc::new(Counting::default());
    let relay = LoopbackRelay::start(authority.clone()).unwrap();
    let session = open(&relay).await;
    let connected = start(session, b"good", None, None).await.unwrap();
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while relay.counters().accepted == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(connected.session.version(), version().unwrap());
    assert!(relay.endpoint.ends_with(VOICE_PATH));
}

#[tokio::test]
async fn client_refuses_grants_outside_the_preface_bounds() {
    let relay = LoopbackRelay::start(Arc::new(Counting::default())).unwrap();
    let session = open(&relay).await;
    assert!(matches!(
        present(&session, b"").await,
        Err(AdmissionError::Invalid)
    ));
    assert!(matches!(
        present(&session, &vec![1; MAX_GRANT_BYTES + 1]).await,
        Err(AdmissionError::Invalid)
    ));
}
