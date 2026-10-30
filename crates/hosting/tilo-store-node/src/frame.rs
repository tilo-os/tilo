//! One retained frame: what a store needs to route, order and retire a
//! member's sealed op, and nothing that would let it open the op.

use collab_types::{ContentRetention, EncryptionScheme, verify_op};
use neighbour_wire::NeighbourMsg;
use web_time::UNIX_EPOCH;

/// One document channel and one author device's ops on it: the unit a store
/// compacts and a subscriber resumes within.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Lineage {
    /// The channel id the author sealed the op for, its UUID bytes.
    pub channel: [u8; 16],
    /// Device key of the op's author.
    pub author: [u8; 32],
}

/// Why a message is not a frame a store keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The message is not a document frame: a store keeps workspace history,
    /// not presence, surfaces or live traffic.
    #[error("not a document frame")]
    NotADocument,
    /// The op's signature does not verify against its declared author.
    #[error("the op is not signed by its author")]
    Unsigned,
    /// The op is a single-use MLS application message, which nothing can
    /// re-deliver, or it carries no channel at all: a store keeps what a
    /// member can read again later, sealed for a channel under its content
    /// key.
    #[error("the op is not sealed for a channel under a content key")]
    NotSealed,
}

/// A message a store keeps: the sealed op with the routing keys and the
/// claims its author signed beside it.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Where the frame belongs.
    pub lineage: Lineage,
    /// The author's sequence number on that lineage, one more than the frame
    /// before it.
    pub seq: u64,
    /// When the author signed the op, for the oldest-first retirement a
    /// bounded appetite does.
    pub authored_at_ns: u64,
    /// Whether the op is a state baseline or a change after one, as its
    /// author declared and signed it in the clear.
    pub retention: ContentRetention,
    /// The message itself, kept byte for byte: whatever it carried is sealed.
    pub message: NeighbourMsg,
}

impl Frame {
    /// The frame `message` is, if a store keeps it.
    ///
    /// # Errors
    ///
    /// [`Refusal::NotADocument`] for any other message, [`Refusal::Unsigned`]
    /// when the op's signature does not verify, and [`Refusal::NotSealed`]
    /// when the op is not content-key ciphertext.
    pub fn of(message: &NeighbourMsg) -> Result<Self, Refusal> {
        let NeighbourMsg::AppDocument { envelope, .. } = message else {
            return Err(Refusal::NotADocument);
        };
        verify_op(envelope).map_err(|_| Refusal::Unsigned)?;
        let metadata = envelope.encryption.ok_or(Refusal::NotSealed)?;
        let EncryptionScheme::ContentKey { retention } = metadata.scheme else {
            return Err(Refusal::NotSealed);
        };
        let authored_at_ns = envelope
            .timestamp
            .duration_since(UNIX_EPOCH)
            .map(|since| u64::try_from(since.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or_default();
        Ok(Self {
            lineage: Lineage {
                channel: *metadata.channel.0.as_bytes(),
                author: *envelope.author.as_bytes(),
            },
            seq: envelope.seq,
            authored_at_ns,
            retention,
            message: message.clone(),
        })
    }
}
