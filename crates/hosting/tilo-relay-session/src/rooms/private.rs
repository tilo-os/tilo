//! Closed governor RPCs over authenticated encrypted transport.
//! Private payloads never enter the room log; the trusted host reads plaintext.
//! Variant order is append-only across independently updated devices.
use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

/// Dedicated authenticated Iroh protocol, not a broadcast action envelope.
pub const PRIVATE_ALPN: &[u8] = b"tilo-governor-private/1";
/// Maximum encoded request, including the bounded private message body.
pub const MAX_REQUEST_BYTES: usize = 256 * 1024;
/// Maximum encoded answer, including a bounded private conversation page.
/// Requests remain independently bounded; exceeding this limit fails, never truncates.
pub const MAX_REPLY_BYTES: usize = 2 * 1024 * 1024;

/// Authenticated private governor operations; variant ordering is stable on the wire.
#[derive(Clone, Serialize, Deserialize)]
pub enum PrivateOperation {
    /// List contacts permitted by current membership.
    Contacts,
    /// Open the exact private conversation.
    Open {
        /// Exact other participant identifier.
        peer: String,
    },
    /// Read a bounded private message page.
    Read {
        /// Exact other participant identifier.
        peer: String,
        /// Exclusive sequence cursor.
        after_sequence: u64,
        /// Maximum returned messages.
        limit: u16,
    },
    /// Append an idempotent private message.
    Send {
        /// Exact other participant identifier.
        peer: String,
        /// Idempotency identifier.
        invocation: uuid::Uuid,
        /// Owner-private body text.
        text: String,
    },
    /// Wait for the conversation sequence to change.
    Watch {
        /// Exact other participant identifier.
        peer: String,
        /// Exclusive sequence cursor.
        after_sequence: u64,
    },
}
impl std::fmt::Debug for PrivateOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (operation, len) = match self {
            Self::Contacts => ("contacts", 0),
            Self::Open { .. } => ("open", 0),
            Self::Read { .. } => ("read", 0),
            Self::Send { text, .. } => ("send", text.len()),
            Self::Watch { .. } => ("watch", 0),
        };
        f.debug_struct("PrivateOperation")
            .field("operation", &operation)
            .field("len", &len)
            .finish()
    }
}
/// An authorized direct-conversation contact.
#[derive(Clone, Serialize, Deserialize)]
pub struct PrivateContact {
    /// Canonical participant public key.
    pub public_key: String,
    /// Optional participant display name.
    pub display_name: Option<String>,
}
/// A retained owner-private direct message.
#[derive(Clone, Serialize, Deserialize)]
pub struct PrivateMessage {
    /// Stable record identifier.
    pub id: String,
    /// Authenticated sender identifier.
    pub sender: String,
    /// Monotonic conversation sequence.
    pub sequence: u64,
    /// Receipt timestamp in seconds since the Unix epoch.
    pub received_at: i64,
    /// Owner-private body text.
    pub text: String,
}
/// One bounded page of a private conversation.
#[derive(Clone, Serialize, Deserialize)]
pub struct PrivateConversation {
    /// Stable record identifier.
    pub id: String,
    /// Exact other participant identifier.
    pub peer: String,
    /// Bounded private message page.
    pub messages: Vec<PrivateMessage>,
    /// Cursor for the next page.
    pub next_sequence: u64,
    /// Whether another page is available.
    pub has_more: bool,
}
/// Private governor reply; debug formatting redacts contents.
#[derive(Clone, Serialize, Deserialize)]
pub enum PrivateReply {
    /// List contacts permitted by current membership.
    Contacts(Vec<PrivateContact>),
    /// A private conversation page.
    Conversation(PrivateConversation),
    /// The direct message was accepted.
    Sent,
    /// The conversation sequence changed.
    Changed(u64),
    /// The operation was refused without exposing private data.
    Refused,
}
impl std::fmt::Debug for PrivateReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (op, len) = match self {
            Self::Contacts(v) => ("contacts", v.len()),
            Self::Conversation(v) => ("conversation", v.messages.len()),
            Self::Sent => ("sent", 0),
            Self::Changed(_) => ("changed", 0),
            Self::Refused => ("refused", 0),
        };
        f.debug_struct("PrivateReply")
            .field("operation", &op)
            .field("len", &len)
            .finish()
    }
}
/// Certificate-bound private request.
#[derive(Serialize, Deserialize)]
pub struct PrivateRequest {
    /// Current device certificate.
    pub device_cert: DeviceCert,
    /// Exact workspace identifier.
    pub room: String,
    /// Requested private operation.
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
    /// # Errors
    /// Returns a redacted transport or codec failure.
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn debug_never_formats_private_payload() {
        let marker = "private-wire-marker";
        let send = PrivateOperation::Send {
            peer: marker.into(),
            invocation: uuid::Uuid::new_v4(),
            text: marker.into(),
        };
        assert!(!format!("{send:?}").contains(marker));
        let reply = PrivateReply::Conversation(PrivateConversation {
            id: marker.into(),
            peer: marker.into(),
            messages: vec![PrivateMessage {
                id: marker.into(),
                sender: marker.into(),
                sequence: 1,
                received_at: 1,
                text: marker.into(),
            }],
            next_sequence: 1,
            has_more: false,
        });
        assert!(!format!("{reply:?}").contains(marker));
    }
}
