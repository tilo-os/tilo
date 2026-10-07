//! Direct private Messages traffic on the session's original Iroh endpoint.
//! This lane carries no room id, shared record, account grant or sender choice.
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointId};
use serde::{Deserialize, Serialize};
use tilo_messaging::SendError;
use tilo_user_identity::DeviceCert;
use zeroize::Zeroize;

pub(crate) const ALPN: &[u8] = b"tilo-private-messages/1";
const MAX_FRAME: usize = 64 * 1024;
const MAX_TEXT: usize = 16 * 1024;
const BUDGET: Duration = Duration::from_secs(20);

type Completion<'a> = Pin<Box<dyn Future<Output = Result<(), SendError>> + Send + 'a>>;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Material {
    pub cert: DeviceCert,
    pub revocations: Vec<tilo_user_identity::RevocationEntry>,
}

/// Implemented only by the protected private account owner. Every call checks
/// original personal custody, the current Contacts route and live revocations.
/// `receive` returns only after encrypted history has durably accepted the id.
pub(crate) trait Account: Send + Sync {
    fn certificate(&self) -> Result<Material, SendError>;
    fn check_peer(&self, certificate: &Material) -> Result<(), SendError>;
    fn receive<'a>(&'a self, certificate: &'a Material, text: &'a Text) -> Completion<'a>;
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Text {
    pub id: uuid::Uuid,
    pub timestamp_ms: u64,
    pub body: String,
}
impl Drop for Text {
    fn drop(&mut self) {
        self.body.zeroize();
    }
}
impl Text {
    fn valid(&self) -> bool {
        !self.id.is_nil()
            && self.timestamp_ms > 0
            && !self.body.trim().is_empty()
            && self.body.len() <= MAX_TEXT
    }
}
#[derive(Serialize, Deserialize)]
struct Request {
    certificate: Material,
    destination: [u8; 32],
    text: Text,
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    certificate: Material,
    id: uuid::Uuid,
}

#[derive(Clone)]
pub(crate) struct Registry {
    local: EndpointId,
    account: Arc<Mutex<Option<Weak<dyn Account>>>>,
    incoming: Arc<tokio::sync::Semaphore>,
}
impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateMessagesRegistry")
    }
}
impl Registry {
    pub(crate) fn new(local: EndpointId) -> Self {
        Self {
            local,
            account: Arc::new(Mutex::new(None)),
            incoming: Arc::new(tokio::sync::Semaphore::new(8)),
        }
    }
    pub(crate) fn bind(&self, account: &Arc<dyn Account>) -> Result<(), SendError> {
        let mut slot = self.account.lock().map_err(|_| SendError::Unavailable)?;
        if slot.as_ref().and_then(Weak::upgrade).is_some() {
            return Err(SendError::Unavailable);
        }
        *slot = Some(Arc::downgrade(account));
        drop(slot);
        Ok(())
    }
    fn account(&self) -> Result<Arc<dyn Account>, SendError> {
        self.account
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or(SendError::Unavailable)
    }
}

#[derive(Clone, Debug)]
pub struct Transport {
    endpoint: Endpoint,
    registry: Registry,
}
impl Transport {
    pub(crate) fn new(endpoint: Endpoint, registry: Registry) -> Self {
        Self { endpoint, registry }
    }
    pub(crate) fn bind(&self, account: &Arc<dyn Account>) -> Result<(), SendError> {
        let certificate = account.certificate()?;
        verify(
            &certificate,
            self.endpoint.id(),
            certificate.cert.user_public_key,
        )?;
        self.registry.bind(account)
    }
    /// The caller pins both recipient root and device from the selected route.
    /// A timeout is uncertain and must never cause an automatic resend.
    pub(crate) async fn send(
        &self,
        peer: EndpointId,
        root: [u8; 32],
        text: Text,
    ) -> Result<(), SendError> {
        tokio::time::timeout(BUDGET, self.send_inner(peer, root, text))
            .await
            .map_err(|_| SendError::Unavailable)?
    }
    async fn send_inner(
        &self,
        peer: EndpointId,
        root: [u8; 32],
        text: Text,
    ) -> Result<(), SendError> {
        if !text.valid() {
            return Err(SendError::Unavailable);
        }
        let account = self.registry.account()?;
        let certificate = current_certificate(Arc::clone(&account)).await?;
        verify(
            &certificate,
            self.endpoint.id(),
            certificate.cert.user_public_key,
        )?;
        let id = text.id;
        let connection = self
            .endpoint
            .connect(peer, ALPN)
            .await
            .map_err(|_| SendError::Unavailable)?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|_| SendError::Unavailable)?;
        let result = async {
            // QUIC exposes a bidirectional stream only after its first write.
            // This fixed prelude carries no identity, route or private text.
            send.write_all(&[1])
                .await
                .map_err(|_| SendError::Unavailable)?;
            // Authenticate the recipient before transmitting private text.
            let bytes = recv
                .read_to_end(MAX_FRAME)
                .await
                .map_err(|_| SendError::Unavailable)?;
            let recipient: Material =
                postcard::from_bytes(&bytes).map_err(|_| SendError::Unavailable)?;
            verify(&recipient, connection.remote_id(), root)?;
            check_peer(Arc::clone(&account), recipient.clone()).await?;
            let certificate = current_certificate(Arc::clone(&account)).await?;
            let request = Request {
                certificate,
                destination: root,
                text,
            };
            let bytes = zeroize::Zeroizing::new(
                postcard::to_allocvec(&request).map_err(|_| SendError::Unavailable)?,
            );
            if bytes.len() > MAX_FRAME {
                return Err(SendError::Unavailable);
            }
            send.write_all(&bytes)
                .await
                .map_err(|_| SendError::Unavailable)?;
            send.finish().map_err(|_| SendError::Unavailable)?;
            let (mut receipt_send, mut receipt_recv) = connection
                .accept_bi()
                .await
                .map_err(|_| SendError::Unavailable)?;
            receipt_send.finish().map_err(|_| SendError::Unavailable)?;
            let bytes = receipt_recv
                .read_to_end(MAX_FRAME)
                .await
                .map_err(|_| SendError::Unavailable)?;
            let receipt: Receipt =
                postcard::from_bytes(&bytes).map_err(|_| SendError::Unavailable)?;
            verify(&receipt.certificate, connection.remote_id(), root)?;
            check_peer(Arc::clone(&account), receipt.certificate.clone()).await?;
            current_certificate(Arc::clone(&account)).await?;
            if receipt.id != id {
                return Err(SendError::Unavailable);
            }
            Ok(())
        }
        .await;
        connection.close(0u32.into(), b"finished");
        result
    }
}

impl ProtocolHandler for Registry {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let Ok(permit) = Arc::clone(&self.incoming).try_acquire_owned() else {
            connection.close(0u32.into(), b"busy");
            return Ok(());
        };
        let registry = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = tokio::time::timeout(BUDGET, registry.receive(&connection)).await;
            connection.close(0u32.into(), b"finished");
        });
        Ok(())
    }
}
impl Registry {
    async fn receive(&self, connection: &iroh::endpoint::Connection) -> Result<(), SendError> {
        let account = self.account()?;
        let certificate = current_certificate(Arc::clone(&account)).await?;
        verify(&certificate, self.local, certificate.cert.user_public_key)?;
        let (mut send, mut recv) = connection
            .accept_bi()
            .await
            .map_err(|_| SendError::Unavailable)?;
        let mut prelude = [0];
        recv.read_exact(&mut prelude)
            .await
            .map_err(|_| SendError::Unavailable)?;
        if prelude != [1] {
            return Err(SendError::InvalidRequest);
        }
        let bytes = postcard::to_allocvec(&certificate).map_err(|_| SendError::Unavailable)?;
        send.write_all(&bytes)
            .await
            .map_err(|_| SendError::Unavailable)?;
        send.finish().map_err(|_| SendError::Unavailable)?;
        let bytes = zeroize::Zeroizing::new(
            recv.read_to_end(MAX_FRAME)
                .await
                .map_err(|_| SendError::Unavailable)?,
        );
        let request: Request = postcard::from_bytes(&bytes).map_err(|_| SendError::Unavailable)?;
        verify(
            &request.certificate,
            connection.remote_id(),
            request.certificate.cert.user_public_key,
        )?;
        if request.destination != certificate.cert.user_public_key || !request.text.valid() {
            return Err(SendError::Revoked);
        }
        check_peer(Arc::clone(&account), request.certificate.clone()).await?;
        current_certificate(Arc::clone(&account)).await?;
        account.receive(&request.certificate, &request.text).await?;
        check_peer(Arc::clone(&account), request.certificate.clone()).await?;
        let certificate = current_certificate(Arc::clone(&account)).await?;
        let receipt = Receipt {
            certificate,
            id: request.text.id,
        };
        let (mut send, _) = connection
            .open_bi()
            .await
            .map_err(|_| SendError::Unavailable)?;
        let bytes = postcard::to_allocvec(&receipt).map_err(|_| SendError::Unavailable)?;
        send.write_all(&bytes)
            .await
            .map_err(|_| SendError::Unavailable)?;
        send.finish().map_err(|_| SendError::Unavailable)?;
        send.stopped().await.map_err(|_| SendError::Unavailable)?;
        Ok(())
    }
}
async fn current_certificate(account: Arc<dyn Account>) -> Result<Material, SendError> {
    tokio::task::spawn_blocking(move || account.certificate())
        .await
        .map_err(|_| SendError::Unavailable)?
}
async fn check_peer(account: Arc<dyn Account>, certificate: Material) -> Result<(), SendError> {
    tokio::task::spawn_blocking(move || account.check_peer(&certificate))
        .await
        .map_err(|_| SendError::Unavailable)?
}
fn verify(certificate: &Material, peer: EndpointId, root: [u8; 32]) -> Result<(), SendError> {
    let now_ns = tilo_clock::unix_now_ns();
    if certificate.cert.device_public_key != *peer.as_bytes()
        || certificate.cert.user_public_key != root
        || certificate.cert.capabilities.foreign
    {
        return Err(SendError::Revoked);
    }
    tilo_user_identity::verify_device_cert_at(&certificate.cert, now_ns)
        .map_err(|_| SendError::Revoked)?;
    if certificate.revocations.len() > 128 {
        return Err(SendError::Revoked);
    }
    let mut revocations = tilo_user_identity::RevocationList::new();
    for entry in &certificate.revocations {
        if entry.user_public_key != root {
            return Err(SendError::Revoked);
        }
        revocations
            .insert(entry.clone())
            .map_err(|_| SendError::Revoked)?;
    }
    if revocations.is_revoked_at(&certificate.cert.device_public_key, now_ns) {
        return Err(SendError::Revoked);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::address_lookup::memory::MemoryLookup;
    use iroh::endpoint::presets;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

    struct Protected {
        certificate: DeviceCert,
        peer: [u8; 32],
        revoked: AtomicBool,
        rows: Mutex<Vec<uuid::Uuid>>,
    }
    impl Account for Protected {
        fn certificate(&self) -> Result<Material, SendError> {
            if self.revoked.load(Ordering::Acquire) {
                return Err(SendError::Revoked);
            }
            Ok(Material {
                cert: self.certificate.clone(),
                revocations: Vec::new(),
            })
        }
        fn check_peer(&self, certificate: &Material) -> Result<(), SendError> {
            self.certificate()?;
            if certificate.cert.user_public_key != self.peer {
                return Err(SendError::Revoked);
            }
            Ok(())
        }
        fn receive<'a>(&'a self, certificate: &'a Material, text: &'a Text) -> Completion<'a> {
            Box::pin(async move {
                self.check_peer(certificate)?;
                let mut rows = self.rows.lock().map_err(|_| SendError::Unavailable)?;
                self.check_peer(certificate)?;
                if !rows.contains(&text.id) {
                    rows.push(text.id);
                }
                drop(rows);
                Ok(())
            })
        }
    }
    async fn endpoint(seed: u8) -> (Endpoint, MemoryLookup, DeviceCert) {
        let root = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[seed.checked_add(1).unwrap(); 32]);
        let lookup = MemoryLookup::new();
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(iroh::SecretKey::from_bytes(&device.to_bytes()))
            .address_lookup(lookup.clone())
            .bind()
            .await
            .unwrap();
        let mut certificate = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "private fixture".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut certificate, &root).unwrap();
        (endpoint, lookup, certificate)
    }
    async fn transport(commands: &tokio::sync::mpsc::Sender<super::super::NetCmd>) -> Transport {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        commands
            .send(super::super::NetCmd::PrivateMessages(reply))
            .await
            .unwrap();
        receiver.await.unwrap()
    }
    fn text(id: uuid::Uuid) -> Text {
        Text {
            id,
            timestamp_ms: 1,
            body: "private fixture text".into(),
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn actual_router_private_text_reconnect_pin_and_revocation() {
        let (a, lookup_a, cert_a) = endpoint(31).await;
        let (b, lookup_b, cert_b) = endpoint(41).await;
        let mut foreign = cert_b.clone();
        foreign.capabilities.foreign = true;
        sign_device_cert(
            &mut foreign,
            &ed25519_dalek::SigningKey::from_bytes(&[41; 32]),
        )
        .unwrap();
        assert!(
            tilo_user_identity::verify_device_cert_at(&foreign, tilo_clock::unix_now_ns()).is_ok()
        );
        assert_eq!(
            verify(
                &Material {
                    cert: foreign,
                    revocations: Vec::new()
                },
                b.id(),
                cert_b.user_public_key
            ),
            Err(SendError::Revoked)
        );
        lookup_a.add_endpoint_info(b.addr());
        lookup_b.add_endpoint_info(a.addr());
        let (cmd_a, task_a) = super::super::Actor::spawn(a.clone(), lookup_a);
        let (cmd_b, task_b) = super::super::Actor::spawn(b.clone(), lookup_b);
        let transport_a = transport(&cmd_a).await;
        let transport_b = transport(&cmd_b).await;
        let account_a = Arc::new(Protected {
            certificate: cert_a.clone(),
            peer: cert_b.user_public_key,
            revoked: AtomicBool::new(false),
            rows: Mutex::new(Vec::new()),
        });
        let account_b = Arc::new(Protected {
            certificate: cert_b.clone(),
            peer: cert_a.user_public_key,
            revoked: AtomicBool::new(false),
            rows: Mutex::new(Vec::new()),
        });
        transport_a
            .bind(&(account_a.clone() as Arc<dyn Account>))
            .unwrap();
        transport_b
            .bind(&(account_b.clone() as Arc<dyn Account>))
            .unwrap();
        let id = uuid::Uuid::new_v4();
        transport_a
            .send(b.id(), cert_b.user_public_key, text(id))
            .await
            .unwrap();
        transport_a
            .send(b.id(), cert_b.user_public_key, text(id))
            .await
            .unwrap();
        assert_eq!(*account_b.rows.lock().unwrap(), vec![id]);
        let reply = uuid::Uuid::new_v4();
        transport_b
            .send(a.id(), cert_a.user_public_key, text(reply))
            .await
            .unwrap();
        assert_eq!(*account_a.rows.lock().unwrap(), vec![reply]);
        assert!(
            transport_a
                .send(b.id(), cert_a.user_public_key, text(uuid::Uuid::new_v4()))
                .await
                .is_err()
        );
        assert_eq!(*account_b.rows.lock().unwrap(), vec![id]);
        account_b.revoked.store(true, Ordering::Release);
        assert!(
            transport_a
                .send(b.id(), cert_b.user_public_key, text(uuid::Uuid::new_v4()))
                .await
                .is_err()
        );
        assert_eq!(*account_b.rows.lock().unwrap(), vec![id]);
        task_a.abort();
        task_b.abort();
        a.close().await;
        b.close().await;
    }
}
