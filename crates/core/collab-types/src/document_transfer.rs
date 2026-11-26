//! Segmented document transfer over the signed-op envelope.
//!
//! One [`SignedOp`] carries at most
//! [`MAX_DOCUMENT_MUTATION_BYTES`] of Automerge data. A larger snapshot or
//! incremental change set is split into [`DocumentSegment`] frames. Every
//! segment travels as its own signed envelope, so brokers validate each one
//! exactly like a small mutation and never buffer a whole transfer. A
//! receiver joins the segments of a transfer and reads them back with
//! [`DocumentMutation::of_transfer`].
//!
//! Small mutations keep the unchanged single-frame encoding, so peers that
//! predate segmentation still exchange them. Segments use a new kind tag in
//! the same versioned prefix; an older receiver rejects them explicitly as
//! an unknown kind rather than misreading them.

use sha2::{Digest, Sha256};

use crate::op::{
    ContentRetention, DOCUMENT_INCREMENTAL_TAG, DOCUMENT_MUTATION_V2_PREFIX, DOCUMENT_SEGMENT_TAG,
    DOCUMENT_SNAPSHOT_TAG, DocumentMutation, DocumentMutationEncodeError, DocumentMutationError,
    MAX_DOCUMENT_MUTATION_BYTES,
};

/// Automerge bytes carried by each segment of a large transfer. Mutations up
/// to this size are sent as one unsegmented frame.
pub const DOCUMENT_SEGMENT_BYTES: usize = 1024 * 1024;
/// Smallest segment a receiver accepts (except the final one). Bounds the
/// number of pieces a transfer header can claim, and so the bookkeeping a
/// receiver allocates for it.
pub const MIN_DOCUMENT_SEGMENT_BYTES: usize = 64 * 1024;
/// Largest complete snapshot or incremental change set the collaboration
/// transport carries. A larger mutation is refused with an explicit error
/// before anything is sent.
pub const MAX_DOCUMENT_TRANSFER_BYTES: usize = 64 * 1024 * 1024;

const SEGMENT_HEADER_BYTES: usize = 1 + 32 + 8 + 4 + 4;
const TRANSFER_DIGEST_DOMAIN: &[u8] = b"tilo-document-transfer-v1\0";

/// Whether a mutation replaces the document or extends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DocumentMutationKind {
    Snapshot,
    Incremental,
}

impl DocumentMutationKind {
    fn tag(self) -> u8 {
        match self {
            Self::Snapshot => DOCUMENT_SNAPSHOT_TAG,
            Self::Incremental => DOCUMENT_INCREMENTAL_TAG,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, DocumentMutationError> {
        match tag {
            DOCUMENT_SNAPSHOT_TAG => Ok(Self::Snapshot),
            DOCUMENT_INCREMENTAL_TAG => Ok(Self::Incremental),
            other => Err(DocumentMutationError::UnknownKind(other)),
        }
    }
}

/// One piece of a segmented document mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentSegment {
    pub kind: DocumentMutationKind,
    /// [`transfer_digest`] of the complete mutation. Identifies the transfer
    /// and authenticates the reassembled bytes.
    pub digest: [u8; 32],
    /// Length of the complete mutation's Automerge bytes.
    pub total_len: u64,
    /// Length of every segment except possibly the last.
    pub segment_len: u32,
    /// Zero-based position of this segment.
    pub index: u32,
    /// This segment's slice of the complete mutation.
    pub data: Vec<u8>,
}

impl DocumentSegment {
    /// Number of segments in the complete transfer.
    pub fn count(&self) -> u32 {
        segment_count(self.total_len, self.segment_len)
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            DOCUMENT_MUTATION_V2_PREFIX
                .len()
                .saturating_add(1)
                .saturating_add(SEGMENT_HEADER_BYTES)
                .saturating_add(self.data.len()),
        );
        out.extend_from_slice(DOCUMENT_MUTATION_V2_PREFIX);
        out.push(DOCUMENT_SEGMENT_TAG);
        out.push(self.kind.tag());
        out.extend_from_slice(&self.digest);
        out.extend_from_slice(&self.total_len.to_le_bytes());
        out.extend_from_slice(&self.segment_len.to_le_bytes());
        out.extend_from_slice(&self.index.to_le_bytes());
        out.extend_from_slice(&self.data);
        out
    }

    fn decode(body: &[u8]) -> Result<Self, DocumentMutationError> {
        let (header, data) = body
            .split_at_checked(SEGMENT_HEADER_BYTES)
            .ok_or(DocumentMutationError::Truncated)?;
        let (&kind, rest) = header
            .split_first()
            .ok_or(DocumentMutationError::Truncated)?;
        let (digest, rest) = rest
            .split_first_chunk::<32>()
            .ok_or(DocumentMutationError::Truncated)?;
        let (total_len, rest) = rest
            .split_first_chunk::<8>()
            .ok_or(DocumentMutationError::Truncated)?;
        let (segment_len, rest) = rest
            .split_first_chunk::<4>()
            .ok_or(DocumentMutationError::Truncated)?;
        let (index, _) = rest
            .split_first_chunk::<4>()
            .ok_or(DocumentMutationError::Truncated)?;
        let segment = Self {
            kind: DocumentMutationKind::from_tag(kind)?,
            digest: *digest,
            total_len: u64::from_le_bytes(*total_len),
            segment_len: u32::from_le_bytes(*segment_len),
            index: u32::from_le_bytes(*index),
            data: data.to_vec(),
        };
        segment.validate()?;
        Ok(segment)
    }

    /// Check the header describes a transfer within the transport limits and
    /// that this segment carries exactly the bytes its position implies.
    fn validate(&self) -> Result<(), DocumentMutationError> {
        let invalid = |reason: &'static str| DocumentMutationError::InvalidSegment(reason);
        let total = usize::try_from(self.total_len).unwrap_or(usize::MAX);
        if total == 0 {
            return Err(invalid("empty transfer"));
        }
        if total > MAX_DOCUMENT_TRANSFER_BYTES {
            return Err(DocumentMutationError::TooLarge {
                actual: total,
                max: MAX_DOCUMENT_TRANSFER_BYTES,
            });
        }
        let segment_len = self.segment_len as usize;
        if !(MIN_DOCUMENT_SEGMENT_BYTES..=MAX_DOCUMENT_MUTATION_BYTES).contains(&segment_len) {
            return Err(invalid("segment length outside transport bounds"));
        }
        if self.index >= self.count() {
            return Err(invalid("segment index beyond transfer"));
        }
        let offset = (self.index as usize).saturating_mul(segment_len);
        let expected = segment_len.min(total.saturating_sub(offset));
        if self.data.len() != expected {
            return Err(invalid("segment length does not match its position"));
        }
        Ok(())
    }
}

fn segment_count(total_len: u64, segment_len: u32) -> u32 {
    if segment_len == 0 {
        return 0;
    }
    u32::try_from(total_len.div_ceil(u64::from(segment_len))).unwrap_or(u32::MAX)
}

/// Digest binding a complete mutation's kind, length and bytes.
pub fn transfer_digest(kind: DocumentMutationKind, data: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(TRANSFER_DIGEST_DOMAIN);
    hash.update([kind.tag()]);
    hash.update(u64::try_from(data.len()).unwrap_or(u64::MAX).to_le_bytes());
    hash.update(data);
    hash.finalize().into()
}

/// A decoded wire frame: either a complete mutation or one segment of one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentFrame {
    Whole(DocumentMutation),
    Segment(DocumentSegment),
}

impl DocumentFrame {
    /// Decode one signed payload as it appears on the wire. A complete frame
    /// is limited to [`MAX_DOCUMENT_MUTATION_BYTES`]; larger mutations must
    /// arrive as segments.
    ///
    /// # Errors
    ///
    /// A payload carrying its action's commit ([`crate::WorkspaceCommit`]) is
    /// read as the frame inside it: the commit is checked here, so a frame
    /// whose commit does not decode never reaches a receiver.
    ///
    /// Returns the same errors as [`DocumentMutation::decode`], plus
    /// [`DocumentMutationError::InvalidSegment`] for a segment whose header
    /// is inconsistent with its data and
    /// [`DocumentMutationError::InvalidCommit`] for a commit that does not
    /// check.
    pub fn decode(payload: &[u8]) -> Result<Self, DocumentMutationError> {
        let payload = match crate::WorkspaceCommit::split(payload) {
            Ok((inner, _)) => inner,
            Err(error) => return Err(DocumentMutationError::InvalidCommit(error.to_string())),
        };
        let max_frame = DOCUMENT_MUTATION_V2_PREFIX
            .len()
            .saturating_add(1)
            .saturating_add(SEGMENT_HEADER_BYTES)
            .saturating_add(MAX_DOCUMENT_MUTATION_BYTES);
        if payload.len() > max_frame {
            return Err(DocumentMutationError::TooLarge {
                actual: payload.len(),
                max: max_frame,
            });
        }
        if let Some(body) = payload.strip_prefix(DOCUMENT_MUTATION_V2_PREFIX)
            && let Some((&DOCUMENT_SEGMENT_TAG, segment)) = body.split_first()
        {
            return DocumentSegment::decode(segment).map(Self::Segment);
        }
        // Anything that is not a segment must be a whole frame; `decode`
        // refuses anything unprefixed.
        let mutation = DocumentMutation::decode(payload)?;
        let actual = mutation.data().len();
        if actual > MAX_DOCUMENT_MUTATION_BYTES {
            return Err(DocumentMutationError::TooLarge {
                actual,
                max: MAX_DOCUMENT_MUTATION_BYTES,
            });
        }
        Ok(Self::Whole(mutation))
    }

    /// Storage class of this frame: a snapshot, or the first segment of
    /// one, starts a new retained baseline; everything else follows it. A
    /// segmented snapshot's later segments are the tail behind its first, so
    /// a replaying device receives every segment in order.
    #[must_use]
    pub fn retention(&self) -> ContentRetention {
        match self {
            Self::Whole(DocumentMutation::Snapshot(_)) => ContentRetention::Checkpoint,
            Self::Segment(segment)
                if segment.kind == DocumentMutationKind::Snapshot && segment.index == 0 =>
            {
                ContentRetention::Checkpoint
            }
            Self::Whole(DocumentMutation::Incremental(_)) | Self::Segment(_) => {
                ContentRetention::Delta
            }
        }
    }
}

impl ContentRetention {
    /// Storage class of one cleartext document frame
    /// ([`DocumentFrame::retention`]); a payload that is not a document
    /// frame starts no baseline.
    #[must_use]
    pub fn of_frame(cleartext: &[u8]) -> Self {
        DocumentFrame::decode(cleartext).map_or(Self::Delta, |frame| frame.retention())
    }
}

impl DocumentMutation {
    /// Kind of this mutation independent of its bytes.
    pub fn kind(&self) -> DocumentMutationKind {
        match self {
            Self::Snapshot(_) => DocumentMutationKind::Snapshot,
            Self::Incremental(_) => DocumentMutationKind::Incremental,
        }
    }

    /// The mutation a complete transfer of `kind` carries. `body` is the data of its segments
    /// joined in order: the bytes [`transfer_digest`] is over, which are the mutation's own
    /// ([`Self::encode_frames`]), so a reader that joins segments itself reads them with
    /// this, as it reads a receiver's own decode through [`Self::decode`].
    ///
    /// # Errors
    ///
    /// Returns [`DocumentMutationError::TooLarge`] above [`MAX_DOCUMENT_TRANSFER_BYTES`].
    pub fn of_transfer(
        kind: DocumentMutationKind,
        body: &[u8],
    ) -> Result<Self, DocumentMutationError> {
        let mutation = Self::of_wire_body(kind.tag(), body)?;
        let actual = mutation.data().len();
        if actual > MAX_DOCUMENT_TRANSFER_BYTES {
            return Err(DocumentMutationError::TooLarge {
                actual,
                max: MAX_DOCUMENT_TRANSFER_BYTES,
            });
        }
        Ok(mutation)
    }

    /// Encode this mutation as the signed payloads that carry it: one frame
    /// when it fits in [`DOCUMENT_SEGMENT_BYTES`], otherwise one frame per
    /// segment. Each returned payload must be signed as its own envelope and
    /// the envelopes sent in order.
    ///
    /// # Errors
    ///
    /// Returns [`DocumentMutationEncodeError::TooLarge`] when the Automerge
    /// bytes exceed [`MAX_DOCUMENT_TRANSFER_BYTES`].
    pub fn encode_frames(&self) -> Result<Vec<Vec<u8>>, DocumentMutationEncodeError> {
        self.encode_frames_with(DOCUMENT_SEGMENT_BYTES)
    }

    fn encode_frames_with(
        &self,
        segment_len: usize,
    ) -> Result<Vec<Vec<u8>>, DocumentMutationEncodeError> {
        let data = self.data();
        check_transfer_len(data.len())?;
        if data.len() <= segment_len {
            return self.encode().map(|frame| vec![frame]);
        }
        let kind = self.kind();
        let data = self.data();
        let digest = transfer_digest(kind, data);
        let total_len = u64::try_from(data.len()).unwrap_or(u64::MAX);
        let segment_len_u32 = u32::try_from(segment_len).unwrap_or(u32::MAX);
        Ok(data
            .chunks(segment_len)
            .enumerate()
            .map(|(index, chunk)| {
                DocumentSegment {
                    kind,
                    digest,
                    total_len,
                    segment_len: segment_len_u32,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    data: chunk.to_vec(),
                }
                .encode()
            })
            .collect())
    }
}

/// Refuse a mutation the transport will never carry, before any frame of it
/// is produced or any local frontier is advanced.
///
/// # Errors
///
/// Returns [`DocumentMutationEncodeError::TooLarge`] above
/// [`MAX_DOCUMENT_TRANSFER_BYTES`].
pub fn check_transfer_len(len: usize) -> Result<(), DocumentMutationEncodeError> {
    if len > MAX_DOCUMENT_TRANSFER_BYTES {
        return Err(DocumentMutationEncodeError::TooLarge {
            actual: len,
            max: MAX_DOCUMENT_TRANSFER_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: usize = 1024 * 1024;

    fn bytes(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.extend_from_slice(&state.to_le_bytes());
        }
        out.truncate(len);
        out
    }

    fn segments(frames: &[Vec<u8>]) -> Vec<DocumentSegment> {
        frames
            .iter()
            .map(
                |frame| match DocumentFrame::decode(frame).expect("wire frame decodes") {
                    DocumentFrame::Segment(segment) => segment,
                    DocumentFrame::Whole(_) => panic!("expected a segment"),
                },
            )
            .collect()
    }

    tilo_test_env::scenario! {
        /// Small mutations keep the single-frame wire format; over-limit mutations are refused before
        /// encoding; unsegmented frames above the limit and malformed segment headers are rejected.
        fn document_transfer_framing_refuses_what_it_cannot_carry;

        phase small_mutations_keep_the_single_frame_wire_format;
        {
            let mutation = DocumentMutation::Incremental(vec![1, 2, 3]);
            let frames = mutation.encode_frames().unwrap();
            assert_eq!(frames, vec![mutation.encode().unwrap()]);
            assert_eq!(
                DocumentFrame::decode(&frames[0]).unwrap(),
                DocumentFrame::Whole(mutation)
            );
        }

        phase over_limit_mutations_are_refused_before_encoding;
        {
            let mutation = DocumentMutation::Incremental(vec![
                0;
                MAX_DOCUMENT_TRANSFER_BYTES
                    .saturating_add(1)
            ]);
            assert_eq!(
                mutation.encode_frames(),
                Err(DocumentMutationEncodeError::TooLarge {
                    actual: MAX_DOCUMENT_TRANSFER_BYTES + 1,
                    max: MAX_DOCUMENT_TRANSFER_BYTES,
                })
            );
        }

        phase wire_rejects_unsegmented_frames_above_the_frame_limit;
        {
            let mut wire = DOCUMENT_MUTATION_V2_PREFIX.to_vec();
            wire.push(DOCUMENT_INCREMENTAL_TAG);
            wire.extend(vec![0; MAX_DOCUMENT_MUTATION_BYTES + 1]);
            assert!(matches!(
                DocumentFrame::decode(&wire),
                Err(DocumentMutationError::TooLarge { .. })
            ));
            assert!(DocumentMutation::decode(&wire).is_ok());
        }

        phase malformed_segment_headers_are_rejected;
        {
            let mutation = DocumentMutation::Snapshot(bytes(3 * MIB, 3));
            let frames = mutation.encode_frames().unwrap();
            let mut truncated = frames[0].clone();
            truncated.pop();
            assert!(matches!(
                DocumentFrame::decode(&truncated),
                Err(DocumentMutationError::InvalidSegment(_))
            ));

            let mut segment = segments(&frames).remove(0);
            segment.index = 99;
            assert!(matches!(
                DocumentFrame::decode(&segment.encode()),
                Err(DocumentMutationError::InvalidSegment(_))
            ));

            let mut tiny = segments(&frames).remove(0);
            tiny.segment_len = 16;
            tiny.data.truncate(16);
            assert!(
                DocumentFrame::decode(&tiny.encode()).is_err(),
                "a header cannot claim millions of pieces"
            );

            let mut huge = segments(&frames).remove(0);
            huge.total_len = (MAX_DOCUMENT_TRANSFER_BYTES + 1) as u64;
            assert!(matches!(
                DocumentFrame::decode(&huge.encode()),
                Err(DocumentMutationError::TooLarge { .. })
            ));
            assert_eq!(
                DocumentMutation::decode(&frames[0]),
                Err(DocumentMutationError::Segmented),
                "a segment is never mistaken for a complete mutation"
            );
        }
    }
}
