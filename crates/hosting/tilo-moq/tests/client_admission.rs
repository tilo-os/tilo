//! The native client reports Live only after the relay admitted the session
//! (the relay answers the admission preface only after host admission), and
//! hears a replacement publisher on the same namespace.
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

use std::sync::Arc;
use std::time::{Duration, Instant};
use tilo_moq::client::{Client, ClientAuthority, Connection, ConnectionScope, Status};
use tilo_moq::relay::{Denied, Direction, LoopbackRelay, RelayAuthority, SessionAuthorization};

fn name(byte: char) -> String {
    format!("{}/{}", "a".repeat(64), byte.to_string().repeat(64))
}

struct Permit {
    publish: String,
    subscribe: Vec<String>,
}
impl SessionAuthorization for Permit {
    fn publication(&self) -> Option<&str> {
        Some(&self.publish)
    }
    fn subscriptions(&self) -> &[String] {
        &self.subscribe
    }
    fn current(&self, _: Direction, _: &str) -> bool {
        true
    }
}

struct Relayed {
    admit: bool,
}
impl RelayAuthority for Relayed {
    fn admit(&self, _proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        if !self.admit {
            return Err(Denied);
        }
        Ok(Arc::new(Permit {
            publish: name('b'),
            subscribe: vec![name('c')],
        }))
    }
}

struct Owner;
impl ClientAuthority for Owner {
    fn current(&self, _: &ConnectionScope, _: Direction, _: &str) -> bool {
        true
    }
}

async fn observe(admit: bool) -> Vec<Status> {
    let relay = LoopbackRelay::start(Arc::new(Relayed { admit })).unwrap();
    let client = Client::start(
        Connection {
            scope: ConnectionScope {
                endpoint: relay.endpoint.clone(),
                fingerprints: relay.fingerprints.clone(),
                owner_epoch: [1; 32],
                carrier_generation: 1,
                publication: Some(name('b')),
                subscriptions: vec![name('c')],
            },
            authorization: b"proof".to_vec(),
        },
        Arc::new(Owner),
    )
    .unwrap();
    let mut seen = vec![client.status()];
    let deadline = tilo_test_env::hang_deadline();
    while Instant::now() < deadline {
        let status = client.status();
        if seen.last() != Some(&status) {
            seen.push(status);
        }
        if matches!(status, Status::Failed | Status::Denied | Status::Closed)
            || (admit && status == Status::Live)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    client.close();
    seen
}

#[tokio::test]
async fn refused_admission_never_reports_live() {
    let seen = observe(false).await;
    assert!(!seen.contains(&Status::Live), "{seen:?}");
    assert_eq!(seen.last(), Some(&Status::Failed), "{seen:?}");
}

#[tokio::test]
async fn admitted_session_reports_live() {
    let seen = observe(true).await;
    assert_eq!(seen.last(), Some(&Status::Live), "{seen:?}");
}

/// Grants by proof: `pub-b` publishes `b`, `sub` listens to `b`.
struct Roles;
impl RelayAuthority for Roles {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let (publish, subscribe) = match proof {
            b"pub-b" => (name('b'), vec![]),
            b"sub" => (name('c'), vec![name('b')]),
            _ => return Err(Denied),
        };
        Ok(Arc::new(Permit { publish, subscribe }))
    }
}

fn start(relay: &LoopbackRelay, proof: &[u8], generation: u64) -> Client {
    let (publication, subscriptions) = if proof == b"pub-b" {
        (Some(name('b')), vec![])
    } else {
        (Some(name('c')), vec![name('b')])
    };
    Client::start(
        Connection {
            scope: ConnectionScope {
                endpoint: relay.endpoint.clone(),
                fingerprints: relay.fingerprints.clone(),
                owner_epoch: [1; 32],
                carrier_generation: generation,
                publication,
                subscriptions,
            },
            authorization: proof.to_vec(),
        },
        Arc::new(Owner),
    )
    .unwrap()
}

async fn until_live(client: &Client) {
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while client.status() != Status::Live {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("live");
}

fn opus(timestamp_48k: u64) -> tilo_moq::client::CapturedOpus {
    tilo_moq::client::CapturedOpus {
        timestamp_48k,
        duration_samples: tilo_moq::voice::FRAME_SAMPLES,
        payload: vec![0xf8, 0xff, 0xfe],
        captured_at: Instant::now(),
    }
}

async fn received(client: &Client) -> tilo_moq::client::ReceivedOpus {
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            if let Some(value) = client.try_recv() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("object received")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replacement_publisher_is_heard_from_its_first_object() {
    let relay = LoopbackRelay::start(Arc::new(Roles)).unwrap();
    let listener = start(&relay, b"sub", 1);
    let publisher = start(&relay, b"pub-b", 1);
    until_live(&listener).await;
    until_live(&publisher).await;
    publisher.enable_publication().unwrap();
    publisher.try_send(opus(0)).unwrap();
    assert_eq!(received(&listener).await.object.timestamp, 0);
    publisher.close();
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while listener.status() == Status::Live {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("publisher loss observed");
    tokio::time::sleep(Duration::from_millis(120)).await;
    let replacement = start(&relay, b"pub-b", 2);
    let fresh = start(&relay, b"sub", 2);
    until_live(&replacement).await;
    until_live(&fresh).await;
    replacement.enable_publication().unwrap();
    replacement.try_send(opus(10 * 960)).unwrap();
    let object = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            if let Some(value) = fresh.try_recv() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("replacement not heard: relay {:?}", relay.counters()));
    assert_eq!(object.object.timestamp, 10 * 960);
}
