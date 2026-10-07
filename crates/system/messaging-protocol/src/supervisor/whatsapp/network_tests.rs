use super::*;
use crate::supervisor::whatsapp::WhatsappBootstrap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tilo_messaging::SendError;

pub(in super::super) struct Gate(pub(in super::super) AtomicBool);
impl AccountAuthority for Gate {
    fn check(&self) -> Result<(), SendError> {
        if self.0.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(SendError::Revoked)
        }
    }
}
struct Pipe {
    gate: Arc<Gate>,
    writes: AtomicUsize,
    closes: AtomicUsize,
    revoke_on_write: bool,
}
#[whatsapp_rust::async_trait]
impl Transport for Pipe {
    async fn send(&self, _: whatsapp_rust::bytes::Bytes) -> whatsapp_rust::anyhow::Result<()> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if self.revoke_on_write {
            self.gate.0.store(false, Ordering::SeqCst);
        }
        Ok(())
    }
    async fn disconnect(&self) {
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}
pub(in super::super) struct Provider {
    gate: Arc<Gate>,
    pipe: Arc<Pipe>,
    exchanges: AtomicUsize,
    creates: AtomicUsize,
    revoke_on_call: bool,
}
#[whatsapp_rust::async_trait]
impl HttpClient for Provider {
    async fn execute(&self, _: HttpRequest) -> whatsapp_rust::anyhow::Result<HttpResponse> {
        self.exchanges.fetch_add(1, Ordering::SeqCst);
        if self.revoke_on_call {
            self.gate.0.store(false, Ordering::SeqCst);
        }
        Ok(HttpResponse {
            status_code: 200,
            body: b"private provider response".to_vec(),
        })
    }
}
#[whatsapp_rust::async_trait]
impl TransportFactory for Provider {
    async fn create_transport(
        &self,
    ) -> whatsapp_rust::anyhow::Result<(
        Arc<dyn Transport>,
        whatsapp_rust::async_channel::Receiver<TransportEvent>,
    )> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        if self.revoke_on_call {
            self.gate.0.store(false, Ordering::SeqCst);
        }
        let (_, receiver) = whatsapp_rust::async_channel::bounded(1);
        Ok((self.pipe.clone(), receiver))
    }
}
pub(in super::super) fn fixture(
    revoke_on_call: bool,
    revoke_on_write: bool,
) -> (Arc<Gate>, Arc<Provider>, Network) {
    let gate = Arc::new(Gate(AtomicBool::new(true)));
    let pipe = Arc::new(Pipe {
        gate: gate.clone(),
        writes: AtomicUsize::new(0),
        closes: AtomicUsize::new(0),
        revoke_on_write,
    });
    let provider = Arc::new(Provider {
        gate: gate.clone(),
        pipe,
        exchanges: AtomicUsize::new(0),
        creates: AtomicUsize::new(0),
        revoke_on_call,
    });
    let network = Network {
        http: provider.clone(),
        transport: provider.clone(),
    }
    .guarded(gate.clone());
    (gate, provider, network)
}

#[tokio::test]
async fn native_profile_bootstrap_refuses_revocation_before_sdk_custody_or_network() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("sdk");
    let owner = crate::journal::Owner {
        principal: "11".repeat(32),
        device: "profile-device".into(),
        service: crate::Service::Whatsapp,
        account: "profile-account".into(),
        linking_generation: 1,
    };
    let (gate, provider, network) = fixture(false, false);
    gate.0.store(false, Ordering::SeqCst);
    assert!(
        WhatsappBootstrap::open_profile(
            &root,
            owner.clone(),
            secret_vault::VaultKey::from_bytes([23; 32]),
            network,
            gate,
        )
        .await
        .is_err()
    );
    assert!(!root.exists());
    assert_eq!(provider.exchanges.load(Ordering::SeqCst), 0);
    assert_eq!(provider.creates.load(Ordering::SeqCst), 0);
    let (gate, provider, network) = fixture(false, false);
    let bootstrap = WhatsappBootstrap::open_profile(
        &root,
        owner,
        secret_vault::VaultKey::from_bytes([23; 32]),
        network,
        gate,
    )
    .await
    .unwrap();
    assert!(!bootstrap.client().is_logged_in());
    assert!(!bootstrap.client().is_connected());
    assert!(root.join("state.enc").is_file());
    assert_eq!(provider.exchanges.load(Ordering::SeqCst), 0);
    assert_eq!(provider.creates.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn revoked_native_network_never_reaches_http_or_connection_factory() {
    let (gate, provider, network) = fixture(false, false);
    gate.0.store(false, Ordering::SeqCst);
    assert!(
        network
            .http
            .execute(HttpRequest::get("https://fixture.invalid"))
            .await
            .is_err()
    );
    assert!(network.transport.create_transport().await.is_err());
    assert_eq!(provider.exchanges.load(Ordering::SeqCst), 0);
    assert_eq!(provider.creates.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn http_reply_after_revocation_is_not_exposed_to_sdk() {
    let (_, provider, network) = fixture(true, false);
    assert!(
        network
            .http
            .execute(HttpRequest::get("https://fixture.invalid"))
            .await
            .is_err()
    );
    assert_eq!(provider.exchanges.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn transport_created_during_revocation_is_closed_and_not_lent() {
    let (_, provider, network) = fixture(true, false);
    assert!(network.transport.create_transport().await.is_err());
    assert_eq!(provider.creates.load(Ordering::SeqCst), 1);
    assert_eq!(provider.pipe.closes.load(Ordering::SeqCst), 1);
    assert_eq!(provider.pipe.writes.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn existing_sdk_transport_rechecks_each_write_and_permits_teardown() {
    let (gate, provider, network) = fixture(false, false);
    let (transport, _) = network.transport.create_transport().await.unwrap();
    transport
        .send(whatsapp_rust::bytes::Bytes::from_static(b"fixture frame"))
        .await
        .unwrap();
    gate.0.store(false, Ordering::SeqCst);
    assert!(
        transport
            .send(whatsapp_rust::bytes::Bytes::from_static(b"refused frame"))
            .await
            .is_err()
    );
    assert_eq!(provider.pipe.writes.load(Ordering::SeqCst), 1);
    assert_eq!(provider.pipe.closes.load(Ordering::SeqCst), 1);
    transport.disconnect().await;
    assert_eq!(provider.pipe.closes.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn write_admitted_before_revocation_is_not_reported_successful_or_replayed() {
    let (_, provider, network) = fixture(false, true);
    let (transport, _) = network.transport.create_transport().await.unwrap();
    let frame = whatsapp_rust::bytes::Bytes::from_static(b"fixture frame");
    assert!(transport.send(frame.clone()).await.is_err());
    assert!(transport.send(frame).await.is_err());
    assert_eq!(provider.pipe.writes.load(Ordering::SeqCst), 1);
    assert_eq!(provider.pipe.closes.load(Ordering::SeqCst), 2);
}
