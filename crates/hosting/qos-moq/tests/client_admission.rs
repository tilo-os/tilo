//! The native client reports Live only after the relay admitted the session
//! (the relay sends its Draft 21 SETUP only after host admission).

use qos_moq::client::{Client, ClientAuthority, Connection, ConnectionScope, Status};
use qos_moq::relay::{Denied, Direction, LoopbackRelay, RelayAuthority, SessionAuthorization};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    let deadline = Instant::now() + Duration::from_secs(5);
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
