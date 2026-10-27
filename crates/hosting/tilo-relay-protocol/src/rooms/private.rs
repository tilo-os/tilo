//! The private conversation vocabulary a room's governor serves: two
//! people's direct messages, which never enter the room's log or its
//! publication. A native device asks over the governor's dedicated iroh
//! protocol (`tilo_relay_session::rooms::private`), a page over its HTTPS
//! session at [`PRIVATE_PATH`]; both are authenticated by the device
//! certificate their transport proved, and both reach the same governor code.
//! Variant order is append-only across independently updated devices.
use serde::{Deserialize, Serialize};

/// Maximum encoded request, including the bounded private message body.
pub const MAX_REQUEST_BYTES: usize = 256 * 1024;
/// Maximum encoded answer, including a bounded private conversation page.
/// Requests remain independently bounded; exceeding this limit fails, never truncates.
pub const MAX_REPLY_BYTES: usize = 2 * 1024 * 1024;
/// Most messages a whole conversation ([`read_whole`]) holds.
pub const MAX_WHOLE_MESSAGES: usize = 2000;
/// Most text bytes a whole conversation ([`read_whole`]) holds.
pub const MAX_WHOLE_TEXT_BYTES: usize = 512 * 1024;
/// Messages asked for per page by [`read_whole`].
const PAGE: u16 = 100;

/// Route, below [`API_PREFIX`](crate::web::API_PREFIX), of a page's private
/// operations: a [`PrivateCall`] in, a [`PrivateReply`] out. A
/// [`PrivateOperation::Watch`] is a long-poll held at most
/// [`MAX_POLL_MS`](crate::web::MAX_POLL_MS); unchanged after that it answers
/// [`PrivateReply::Changed`] with the cursor it was given.
pub const PRIVATE_PATH: &str = "/private";

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
impl core::fmt::Debug for PrivateOperation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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
    /// The name the room keeps for the person; never one of their devices'
    /// labels.
    pub display_name: Option<String>,
    /// Whether a conversation between the caller and this person holds
    /// messages: whom a list of direct messages shows, of everyone the
    /// caller may write to.
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
impl core::fmt::Debug for PrivateReply {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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
/// A page's private operation in `room`. The caller is the device its
/// session authenticated; nothing in the body names one.
#[derive(Serialize, Deserialize)]
pub struct PrivateCall {
    /// Exact workspace identifier.
    pub room: String,
    pub operation: PrivateOperation,
}
impl core::fmt::Debug for PrivateCall {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.operation.fmt(f)
    }
}

/// The whole conversation with `peer`, read page by page through `send`
/// and checked as it arrives: every page binds the same conversation and
/// peer, sequences only grow, and the whole stays within
/// [`MAX_WHOLE_MESSAGES`] and [`MAX_WHOLE_TEXT_BYTES`]. Fails rather than
/// returning part of it.
///
/// # Errors
///
/// What `send` failed with, an unexpected reply, or a page breaking the
/// binding or the bounds.
pub async fn read_whole<F, Fut>(mut send: F, peer: &str) -> Result<PrivateConversation, String>
where
    F: FnMut(PrivateOperation) -> Fut,
    Fut: core::future::Future<Output = Result<PrivateReply, String>>,
{
    let mut result: Option<PrivateConversation> = None;
    let mut bytes = 0usize;
    loop {
        let after_sequence = result.as_ref().map_or(0, |page| page.next_sequence);
        let PrivateReply::Conversation(page) = send(PrivateOperation::Read {
            peer: peer.to_owned(),
            after_sequence,
            limit: PAGE,
        })
        .await?
        else {
            return Err("unexpected private read reply".into());
        };
        if page.peer != peer
            || page.messages.len() > usize::from(PAGE)
            || page.next_sequence < after_sequence
            || (page.has_more && page.next_sequence <= after_sequence)
            || result
                .as_ref()
                .is_some_and(|previous| previous.id != page.id)
        {
            return Err("private conversation page binding changed".into());
        }
        let mut sequence = after_sequence;
        for message in &page.messages {
            if message.sequence <= sequence || message.sequence > page.next_sequence {
                return Err("private conversation sequence invalid".into());
            }
            sequence = message.sequence;
            bytes = bytes
                .checked_add(message.text.len())
                .ok_or("private snapshot exceeds bound")?;
        }
        let messages = result
            .as_ref()
            .map_or(0, |page| page.messages.len())
            .checked_add(page.messages.len())
            .ok_or("private snapshot exceeds bound")?;
        if bytes > MAX_WHOLE_TEXT_BYTES || messages > MAX_WHOLE_MESSAGES {
            return Err("private conversation snapshot exceeds bound".into());
        }
        let more = page.has_more;
        match &mut result {
            Some(previous) => {
                previous.next_sequence = page.next_sequence;
                previous.has_more = page.has_more;
                previous.messages.extend(page.messages);
            }
            None => result = Some(page),
        }
        if !more {
            return result.ok_or_else(|| "private snapshot missing".into());
        }
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
