use std::future::pending;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;
use tilo_server_client::HttpFuture;
use tilo_server_protocol::login_code::{CLAIM_ROUTE, WAITING_ROUTE};
use tilo_server_protocol::web::API_PREFIX;
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationPolicy, DeviceCapabilities, issue_device_cert,
};

use super::*;

fn request(user: &SigningKey, device: &SigningKey) -> CertificationRequest {
    let now = tilo_clock::unix_now_ns();
    CertificationRequest {
        version: CERTIFICATION_SCHEMA_VERSION,
        user_public_key: user.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Browser".into(),
        requested: DeviceCapabilities::member(),
        nonce: [1; 16],
        invite: [2; 16],
        issued_at_ns: now,
        expires_at_ns: now.saturating_add(60_000_000_000),
        signature: [0; 64],
    }
    .sign(device)
    .unwrap()
}

fn credentials() -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let device = SigningKey::from_bytes(&[2; 32]);
    let cert = issue_device_cert(
        &request(&root, &device),
        &root,
        &CertificationPolicy::browser(),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    (device, cert)
}

struct Host<F> {
    lifetime_ms: u64,
    waiting: F,
}

impl<F> HttpClient for Host<F>
where
    F: Fn() -> HttpFuture<'static> + Send + Sync,
{
    fn post(&self, path: &str, _body: Vec<u8>) -> HttpFuture<'_> {
        match path.strip_prefix(API_PREFIX) {
            Some(CLAIM_ROUTE) => Box::pin(async move {
                let expires_at_ms = tilo_clock::unix_now_ms().saturating_add(self.lifetime_ms);
                Ok((
                    200,
                    json!({ "expires_at_ms": expires_at_ms })
                        .to_string()
                        .into_bytes(),
                ))
            }),
            Some(WAITING_ROUTE) => (self.waiting)(),
            _ => panic!("unexpected request to {path}"),
        }
    }
}

#[tokio::test]
async fn a_claim_cannot_certify_a_request_pinned_to_another_person() {
    let (device, cert) = credentials();
    let stranger = SigningKey::from_bytes(&[3; 32]);
    let tab = SigningKey::from_bytes(&[4; 32]);
    let other = request(&stranger, &tab);
    let host = Host {
        lifetime_ms: CODE_LIFETIME_MS,
        waiting: || -> HttpFuture<'static> {
            let request = hex::encode(postcard::to_allocvec(&other).unwrap());
            Box::pin(
                async move { Ok((200, json!({ "request": request }).to_string().into_bytes())) },
            )
        },
    };
    let result = vouch_with_code(&host, "aa", &device, &cert, Some(&stranger), || {}).await;
    assert!(matches!(result, Err(VouchError::DifferentPerson)));
}

#[tokio::test]
async fn a_waiting_refusal_stops_the_sign_in() {
    let (device, cert) = credentials();
    let host = Host {
        lifetime_ms: CODE_LIFETIME_MS,
        waiting: || -> HttpFuture<'static> {
            Box::pin(async { Ok((409, br#"{"error":"claim closed"}"#.to_vec())) })
        },
    };
    let result = vouch_with_code(&host, "aa", &device, &cert, None, || {}).await;
    assert!(
        matches!(result, Err(VouchError::Waiting(refusal)) if refusal.status == 409 && refusal.message.contains("claim closed"))
    );
}

#[tokio::test]
async fn the_claim_expires_even_while_a_wait_request_is_in_flight() {
    let (device, cert) = credentials();
    let asked = AtomicBool::new(false);
    let host = Host {
        lifetime_ms: 500,
        waiting: || -> HttpFuture<'static> {
            asked.store(true, Ordering::SeqCst);
            Box::pin(pending())
        },
    };
    let result = vouch_with_code(&host, "aa", &device, &cert, None, || {}).await;
    assert!(asked.load(Ordering::SeqCst));
    assert!(matches!(result, Err(VouchError::Expired)));
}

#[tokio::test]
async fn dropping_a_vouch_cancels_its_in_flight_wait() {
    struct OnDrop(std::sync::Arc<AtomicBool>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let (device, cert) = credentials();
    let waiting = tokio::sync::Notify::new();
    let dropped = std::sync::Arc::new(AtomicBool::new(false));
    let host = Host {
        lifetime_ms: CODE_LIFETIME_MS,
        waiting: || -> HttpFuture<'static> {
            waiting.notify_one();
            let observed = OnDrop(dropped.clone());
            Box::pin(async move {
                let _observed = observed;
                pending().await
            })
        },
    };
    tokio::select! {
        result = vouch_with_code(&host, "aa", &device, &cert, None, || {}) => {
            panic!("vouch ended before cancellation: {result:?}");
        },
        () = waiting.notified() => {}
    }
    assert!(dropped.load(Ordering::SeqCst));
}
