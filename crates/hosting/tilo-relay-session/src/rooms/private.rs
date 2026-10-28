//! Closed governor RPCs over authenticated encrypted transport.
//! Private payloads never enter the room log; the trusted host reads plaintext.
//! Variant order is append-only across independently updated devices.
use serde::{Deserialize, Serialize};
use tilo_relay_protocol::rooms::private::{
    MAX_REPLY_BYTES, MAX_REQUEST_BYTES, PrivateOperation, PrivateReply,
};
use tilo_user_identity::DeviceCert;

/// Dedicated authenticated Iroh protocol, not a broadcast action envelope.
pub const PRIVATE_ALPN: &[u8] = b"tilo-governor-private/1";
/// Certificate-bound private request.
#[derive(Serialize, Deserialize)]
pub struct PrivateRequest {
    /// Current device certificate.
    pub device_cert: DeviceCert,
    /// Exact workspace identifier.
    pub room: String,
    pub operation: PrivateOperation,
}
impl std::fmt::Debug for PrivateRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.operation.fmt(f)
    }
}
/// Uses the actual endpoint's QUIC certificate, checked against `device_cert`
/// by the governor. No asserted sender or on-behalf-of principal crosses this door.
#[derive(Clone)]
pub struct PrivateClient {
    endpoint: iroh::Endpoint,
    device_cert: DeviceCert,
}
impl PrivateClient {
    /// Bind private exchanges to the endpoint's certified device identity.
    #[must_use]
    pub fn new(endpoint: iroh::Endpoint, device_cert: DeviceCert) -> Self {
        Self {
            endpoint,
            device_cert,
        }
    }
    /// One exchange. Watches remain pending until a real pair or authority change.
    pub async fn send(
        &self,
        relay: iroh::EndpointAddr,
        room: String,
        operation: PrivateOperation,
    ) -> Result<PrivateReply, String> {
        if self.endpoint.id().as_bytes() != &self.device_cert.device_public_key {
            return Err("private endpoint certificate mismatch".into());
        }
        let bytes = postcard::to_stdvec(&PrivateRequest {
            device_cert: self.device_cert.clone(),
            room,
            operation,
        })
        .map_err(|_| "private request encoding failed")?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err("private request exceeds bound".into());
        }
        let connection = self
            .endpoint
            .connect(relay, PRIVATE_ALPN)
            .await
            .map_err(|_| "private connection failed")?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|_| "private stream failed")?;
        tilo_relay_client::frame::write_frame(&mut send, &bytes)
            .await
            .map_err(|_| "private write failed")?;
        send.finish().map_err(|_| "private stream finish failed")?;
        let reply = tilo_relay_client::frame::read_frame(&mut recv, MAX_REPLY_BYTES)
            .await
            .map_err(|_| "private read failed")?;
        connection.close(0u32.into(), b"done");
        postcard::from_bytes(&reply).map_err(|_| "private reply decoding failed".into())
    }
}
