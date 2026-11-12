//! Content gets: how a caller reads bytes back out of its workspace's
//! content store, a piece at a time, by the reference a put or an action
//! answered with (`tilo.content:<hash>`).
//!
//! A put is how bytes get in ([`super::ContentPuts`]); a get is how they
//! come out, for a caller that keeps no copy of the workspace — an agent's
//! MCP session through its own device. Reads are bounded like puts: one
//! request answers one piece a frame carries, and the caller asks again at
//! the offset the piece ends at.
//!
//! Only the store of the workspace the caller is in answers: a hash held by
//! another workspace, or by no workspace, reads as no such content.

use super::{ActionHostError, Caller};

/// What a host without a content store answers.
pub const CONTENT_GETS_UNAVAILABLE: &str = "reading content is not available here";

/// One piece of content, as a host answers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentPiece {
    /// The whole content's length.
    pub size: u64,
    /// Where this piece starts.
    pub offset: u64,
    /// The piece.
    pub bytes: Vec<u8>,
}

/// A workspace's content store as a caller reads it.
pub trait ContentGets: Send + Sync {
    /// The bytes of `content` (`tilo.content:<hash>`) from `offset`, at most
    /// `length` of them; `length` of zero asks for as many as one piece
    /// carries.
    ///
    /// # Errors
    ///
    /// [`ActionHostError::InvalidRequest`] for a reference that is not one,
    /// content the caller's workspace does not hold, an offset past its end,
    /// or a piece longer than [`super::MAX_CONTENT_FRAME_BYTES`].
    fn get(
        &self,
        caller: &Caller,
        content: &str,
        offset: u64,
        length: u64,
    ) -> Result<ContentPiece, ActionHostError>;
}

/// What a request for content answers: the whole content's size, where the
/// piece starts, and the piece itself.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentGetAnswer {
    /// The whole content's length.
    pub size: u64,
    /// Where this piece starts.
    pub offset: u64,
    /// The piece, hex-encoded so the frame stays self-describing text.
    bytes: String,
}

impl ContentGetAnswer {
    /// The answer for `piece`.
    ///
    /// # Errors
    ///
    /// A piece longer than [`super::MAX_CONTENT_FRAME_BYTES`], which no
    /// frame carries.
    pub fn new(piece: ContentPiece) -> Result<Self, ActionHostError> {
        if piece.bytes.len() > super::MAX_CONTENT_FRAME_BYTES {
            return Err(ActionHostError::InvalidRequest {
                reason: format!(
                    "a piece carries at most {} bytes",
                    super::MAX_CONTENT_FRAME_BYTES
                ),
            });
        }
        Ok(Self {
            size: piece.size,
            offset: piece.offset,
            bytes: hex::encode(piece.bytes),
        })
    }

    /// The piece's bytes; `None` when they are not well-formed.
    #[must_use]
    pub fn bytes(&self) -> Option<Vec<u8>> {
        if !self.bytes.len().is_multiple_of(2)
            || self.bytes.len() > super::MAX_CONTENT_FRAME_BYTES * 2
        {
            return None;
        }
        hex::decode(&self.bytes).ok()
    }

    /// Where a next piece starts, or `None` when this one ends the content.
    #[must_use]
    pub fn next_offset(&self) -> Option<u64> {
        let end = self
            .offset
            .saturating_add(self.bytes.len().div_ceil(2) as u64);
        (end < self.size).then_some(end)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    #[test]
    fn a_piece_names_where_the_next_one_starts() {
        let answer = ContentGetAnswer::new(ContentPiece {
            size: 10,
            offset: 0,
            bytes: b"abcd".to_vec(),
        })
        .unwrap();
        assert_eq!(answer.bytes(), Some(b"abcd".to_vec()));
        assert_eq!(answer.next_offset(), Some(4));

        let last = ContentGetAnswer::new(ContentPiece {
            size: 6,
            offset: 4,
            bytes: b"ef".to_vec(),
        })
        .unwrap();
        assert_eq!(last.next_offset(), None, "the end of the content");
        assert_eq!(last.bytes(), Some(b"ef".to_vec()));

        let empty = ContentGetAnswer::new(ContentPiece {
            size: 0,
            offset: 0,
            bytes: Vec::new(),
        })
        .unwrap();
        assert_eq!(empty.bytes(), Some(Vec::new()));
        assert_eq!(empty.next_offset(), None);
    }
}
