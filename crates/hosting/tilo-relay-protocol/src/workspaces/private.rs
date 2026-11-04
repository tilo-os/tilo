//! The retired private conversation vocabulary.
//!
//! A workspace's governor once served two people's direct messages here: a
//! native device over a dedicated iroh protocol, a page over its HTTPS
//! session. A conversation is an ordinary record of the workspace now
//! (`direct/<pair>`), which the app whose stores hold conversations reads and
//! writes itself, so nothing serves these operations: a host no longer
//! answers the protocol or the route.
//!
//! The variants stay in their places, retired, because the encoding names a
//! variant by its position: bytes already written read as what they meant.
//! New variants go at the end.
use serde::{Deserialize, Serialize};

/// A retired private governor operation.
#[derive(Clone, Serialize, Deserialize)]
pub enum PrivateOperation {
    /// Once: list contacts permitted by current membership.
    RetiredContacts,
    /// Once: open the exact private conversation.
    RetiredOpen {
        /// Exact other participant identifier.
        peer: String,
    },
    /// Once: read a bounded private message page.
    RetiredRead {
        /// Exact other participant identifier.
        peer: String,
        /// Exclusive sequence cursor.
        after_sequence: u64,
        /// Maximum returned messages.
        limit: u16,
    },
    /// Once: append an idempotent private message.
    RetiredSend {
        /// Exact other participant identifier.
        peer: String,
        /// Idempotency identifier.
        invocation: uuid::Uuid,
        /// Owner-private body text.
        text: String,
    },
    /// Once: wait for the conversation sequence to change.
    RetiredWatch {
        /// Exact other participant identifier.
        peer: String,
        /// Exclusive sequence cursor.
        after_sequence: u64,
    },
}
impl core::fmt::Debug for PrivateOperation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (operation, len) = match self {
            Self::RetiredContacts => ("contacts", 0),
            Self::RetiredOpen { .. } => ("open", 0),
            Self::RetiredRead { .. } => ("read", 0),
            Self::RetiredSend { text, .. } => ("send", text.len()),
            Self::RetiredWatch { .. } => ("watch", 0),
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
    /// The name the workspace keeps for the person.
    pub display_name: Option<String>,
    /// Whether a conversation between the caller and this person holds
    /// messages.
    pub conversation: bool,
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
    pub next_sequence: u64,
    /// Whether another page is available.
    pub has_more: bool,
}
/// A retired private governor reply.
#[derive(Clone, Serialize, Deserialize)]
pub enum PrivateReply {
    /// Once: contacts permitted by current membership.
    RetiredContacts(Vec<PrivateContact>),
    /// Once: a private conversation page.
    RetiredConversation(PrivateConversation),
    /// Once: the direct message was accepted.
    RetiredSent,
    /// Once: the conversation sequence changed.
    RetiredChanged(u64),
    /// Once: the operation was refused without exposing private data.
    RetiredRefused,
}
impl core::fmt::Debug for PrivateReply {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (op, len) = match self {
            Self::RetiredContacts(v) => ("contacts", v.len()),
            Self::RetiredConversation(v) => ("conversation", v.messages.len()),
            Self::RetiredSent => ("sent", 0),
            Self::RetiredChanged(_) => ("changed", 0),
            Self::RetiredRefused => ("refused", 0),
        };
        f.debug_struct("PrivateReply")
            .field("operation", &op)
            .field("len", &len)
            .finish()
    }
}
/// A retired page private operation in `workspace`. The caller is the device
/// its session authenticated; nothing in the body names one.
#[derive(Serialize, Deserialize)]
pub struct PrivateCall {
    /// Exact workspace identifier.
    #[serde(rename = "room")]
    pub workspace: String,
    pub operation: PrivateOperation,
}
impl core::fmt::Debug for PrivateCall {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.operation.fmt(f)
    }
}
