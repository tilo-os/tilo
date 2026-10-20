//! Segmented document transfer over the signed-op envelope.
//!
//! One [`SignedOp`] carries at most
//! [`MAX_DOCUMENT_MUTATION_BYTES`] of Automerge data. A larger snapshot or
//! incremental change set is split into [`DocumentSegment`] frames. Every
//! segment travels as its own signed envelope, so brokers validate each one
//! exactly like a small mutation and never buffer a whole transfer. The
//! receiving endpoint reassembles the segments with [`DocumentReassembler`],
//! which bounds memory, rejects inconsistent headers and verifies a digest
//! over the complete mutation before handing it to the application.
//!
//! Small mutations keep the unchanged single-frame encoding, so peers that
//! predate segmentation still exchange them. Segments use a new kind tag in
//! the same versioned prefix; an older receiver rejects them explicitly as
//! an unknown kind rather than misreading them.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::op::{
    ContentRetention, DOCUMENT_INCREMENTAL_TAG, DOCUMENT_MUTATION_V2_PREFIX, DOCUMENT_SEGMENT_TAG,
    DOCUMENT_SNAPSHOT_TAG, DocumentMutation, DocumentMutationEncodeError, DocumentMutationError,
    MAX_DOCUMENT_MUTATION_BYTES,
};
use crate::{ArtifactId, Ed25519PublicKey, SignedOp};

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
/// Upper bound on segment bytes a receiver holds for incomplete transfers.
pub const MAX_REASSEMBLY_BUFFER_BYTES: usize = 2 * MAX_DOCUMENT_TRANSFER_BYTES;
/// Upper bound on concurrently incomplete transfers a receiver tracks.
pub const MAX_CONCURRENT_TRANSFERS: usize = 16;
/// An incomplete transfer that receives no segment for this long is dropped.
pub const TRANSFER_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

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
    /// Kind of the complete mutation.
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
    /// Returns the same errors as [`DocumentMutation::decode`], plus
    /// [`DocumentMutationError::InvalidSegment`] for a segment whose header
    /// is inconsistent with its data.
    pub fn decode(payload: &[u8]) -> Result<Self, DocumentMutationError> {
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

/// Why a segment could not contribute to a complete mutation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReassemblyError {
    /// A segment disagrees with earlier segments of the same transfer.
    #[error("document segment contradicts its transfer header")]
    Inconsistent,
    /// Every segment arrived but the bytes do not match the signed digest.
    #[error("reassembled document does not match its transfer digest")]
    DigestMismatch,
}

/// A complete mutation rebuilt from its segments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReassembledDocument {
    /// Unsegmented typed payload, decodable with [`DocumentMutation::decode`].
    pub payload: Vec<u8>,
    /// Kind of the complete mutation.
    pub kind: DocumentMutationKind,
    /// Automerge bytes carried, excluding the typed prefix.
    pub len: usize,
}

/// An incomplete transfer dropped to stay within the receiver's limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbandonedTransfer {
    pub artifact: ArtifactId,
    pub author: Ed25519PublicKey,
    pub total_len: u64,
    pub received_segments: usize,
    pub segment_count: u32,
    pub reason: AbandonReason,
}

/// Why an incomplete transfer was dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbandonReason {
    /// No segment arrived within [`TRANSFER_IDLE_TIMEOUT`].
    Idle,
    /// Newer transfers needed the memory or tracking slot.
    Evicted,
}

/// Limits enforced by a [`DocumentReassembler`].
#[derive(Clone, Copy, Debug)]
pub struct ReassemblyLimits {
    pub max_buffered_bytes: usize,
    pub max_transfers: usize,
    pub idle_timeout: Duration,
}

impl Default for ReassemblyLimits {
    fn default() -> Self {
        Self {
            max_buffered_bytes: MAX_REASSEMBLY_BUFFER_BYTES,
            max_transfers: MAX_CONCURRENT_TRANSFERS,
            idle_timeout: TRANSFER_IDLE_TIMEOUT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TransferKey {
    artifact: ArtifactId,
    author: Ed25519PublicKey,
    digest: [u8; 32],
}

struct Transfer {
    kind: DocumentMutationKind,
    total_len: u64,
    segment_len: u32,
    segments: BTreeMap<u32, Vec<u8>>,
    bytes: usize,
    last_activity: Instant,
}

/// Endpoint-side reassembly of segmented document mutations.
///
/// Transfers are keyed by artifact, author and digest, so segments from
/// different routes or authors never mix. Only the segments actually
/// received are buffered; the total stays below
/// [`ReassemblyLimits::max_buffered_bytes`] by abandoning the least recently
/// active incomplete transfers. Abandonment is reported, never silent.
pub struct DocumentReassembler {
    limits: ReassemblyLimits,
    transfers: HashMap<TransferKey, Transfer>,
    buffered: usize,
    abandoned: Vec<AbandonedTransfer>,
}

impl Default for DocumentReassembler {
    fn default() -> Self {
        Self::new(ReassemblyLimits::default())
    }
}

impl DocumentReassembler {
    pub fn new(limits: ReassemblyLimits) -> Self {
        Self {
            limits,
            transfers: HashMap::new(),
            buffered: 0,
            abandoned: Vec::new(),
        }
    }

    /// Bytes currently held for incomplete transfers.
    pub fn buffered_bytes(&self) -> usize {
        self.buffered
    }

    /// Number of incomplete transfers currently tracked.
    pub fn pending_transfers(&self) -> usize {
        self.transfers.len()
    }

    /// Take the transfers abandoned since the last call.
    pub fn take_abandoned(&mut self) -> Vec<AbandonedTransfer> {
        std::mem::take(&mut self.abandoned)
    }

    /// Add one validated segment carried by `envelope`. Returns the complete
    /// mutation once its final missing segment arrives.
    ///
    /// # Errors
    ///
    /// Returns [`ReassemblyError::Inconsistent`] when the segment contradicts
    /// earlier segments of its transfer, and
    /// [`ReassemblyError::DigestMismatch`] when the complete bytes do not
    /// match the digest. Either way the transfer is discarded.
    pub fn accept(
        &mut self,
        envelope: &SignedOp,
        segment: DocumentSegment,
        now: Instant,
    ) -> Result<Option<ReassembledDocument>, ReassemblyError> {
        self.expire(now);
        let key = TransferKey {
            artifact: envelope.artifact,
            author: envelope.author,
            digest: segment.digest,
        };
        if let Some(existing) = self.transfers.get(&key)
            && (existing.kind != segment.kind
                || existing.total_len != segment.total_len
                || existing.segment_len != segment.segment_len)
        {
            self.remove(&key);
            return Err(ReassemblyError::Inconsistent);
        }
        if !self.transfers.contains_key(&key) {
            self.make_room(0, Some(&key));
            self.transfers.insert(
                key,
                Transfer {
                    kind: segment.kind,
                    total_len: segment.total_len,
                    segment_len: segment.segment_len,
                    segments: BTreeMap::new(),
                    bytes: 0,
                    last_activity: now,
                },
            );
        }
        let count = segment.count();
        let len = segment.data.len();
        let duplicate = self
            .transfers
            .get(&key)
            .is_some_and(|transfer| transfer.segments.contains_key(&segment.index));
        if !duplicate {
            self.make_room(len, Some(&key));
        }
        let Some(transfer) = self.transfers.get_mut(&key) else {
            return Ok(None);
        };
        transfer.last_activity = now;
        if duplicate {
            // A retransmitted segment carries the same signed digest, so its
            // bytes are either identical or rejected at completion.
            return Ok(None);
        }
        transfer.segments.insert(segment.index, segment.data);
        transfer.bytes = transfer.bytes.saturating_add(len);
        self.buffered = self.buffered.saturating_add(len);
        if transfer.segments.len() < count as usize {
            return Ok(None);
        }
        let Some(transfer) = self.remove(&key) else {
            return Ok(None);
        };
        let total = usize::try_from(transfer.total_len).unwrap_or(usize::MAX);
        let header = DOCUMENT_MUTATION_V2_PREFIX.len().saturating_add(1);
        let mut payload = Vec::with_capacity(header.saturating_add(total));
        payload.extend_from_slice(DOCUMENT_MUTATION_V2_PREFIX);
        payload.push(transfer.kind.tag());
        for chunk in transfer.segments.into_values() {
            payload.extend_from_slice(&chunk);
        }
        let body = payload.get(header..).unwrap_or_default();
        if body.len() != total || transfer_digest(transfer.kind, body) != key.digest {
            return Err(ReassemblyError::DigestMismatch);
        }
        Ok(Some(ReassembledDocument {
            payload,
            kind: transfer.kind,
            len: total,
        }))
    }

    /// Drop transfers that have been idle longer than the configured timeout.
    pub fn expire(&mut self, now: Instant) {
        let timeout = self.limits.idle_timeout;
        let idle: Vec<TransferKey> = self
            .transfers
            .iter()
            .filter(|(_, transfer)| now.saturating_duration_since(transfer.last_activity) > timeout)
            .map(|(key, _)| *key)
            .collect();
        for key in idle {
            self.abandon(&key, AbandonReason::Idle);
        }
    }

    /// Evict least recently active transfers other than `keep` until `extra`
    /// more bytes and (for a new transfer) one more slot fit.
    fn make_room(&mut self, extra: usize, keep: Option<&TransferKey>) {
        loop {
            let slot_needed = keep.is_some_and(|key| !self.transfers.contains_key(key))
                && self.transfers.len() >= self.limits.max_transfers;
            let bytes_needed = self.buffered.saturating_add(extra) > self.limits.max_buffered_bytes;
            if !slot_needed && !bytes_needed {
                return;
            }
            let victim = self
                .transfers
                .iter()
                .filter(|(key, _)| Some(*key) != keep)
                .min_by_key(|(_, transfer)| transfer.last_activity)
                .map(|(key, _)| *key);
            let Some(victim) = victim else {
                return;
            };
            self.abandon(&victim, AbandonReason::Evicted);
        }
    }

    fn abandon(&mut self, key: &TransferKey, reason: AbandonReason) {
        if let Some(transfer) = self.remove(key) {
            self.abandoned.push(AbandonedTransfer {
                artifact: key.artifact,
                author: key.author,
                total_len: transfer.total_len,
                received_segments: transfer.segments.len(),
                segment_count: segment_count(transfer.total_len, transfer.segment_len),
                reason,
            });
        }
    }

    fn remove(&mut self, key: &TransferKey) -> Option<Transfer> {
        let transfer = self.transfers.remove(key)?;
        self.buffered = self.buffered.saturating_sub(transfer.bytes);
        Some(transfer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AttestationId, OpKind};
    use std::time::SystemTime;

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

    fn envelope(artifact: ArtifactId, author: u8, payload: Vec<u8>) -> SignedOp {
        SignedOp::new_unsigned(
            payload,
            artifact,
            Ed25519PublicKey::from_bytes([author; 32]),
            AttestationId::new(),
            OpKind::Change,
            SystemTime::UNIX_EPOCH,
            1,
        )
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

    fn reassemble(
        reassembler: &mut DocumentReassembler,
        artifact: ArtifactId,
        frames: &[Vec<u8>],
    ) -> Option<DocumentMutation> {
        let now = Instant::now();
        let mut out = None;
        for (frame, segment) in frames.iter().zip(segments(frames)) {
            let op = envelope(artifact, 1, frame.clone());
            if let Some(done) = reassembler
                .accept(&op, segment, now)
                .expect("valid transfer")
            {
                assert!(out.is_none(), "a transfer completes exactly once");
                out = Some(DocumentMutation::decode(&done.payload).expect("reassembled decodes"));
            }
        }
        out
    }

    /// Small mutations keep the single-frame wire format; over-limit mutations are refused before
    /// encoding; unsegmented frames above the limit and malformed segment headers are rejected.
    #[test]
    fn document_transfer_framing_refuses_what_it_cannot_carry() {
        // Phase: small_mutations_keep_the_single_frame_wire_format.
        {
            let mutation = DocumentMutation::Incremental(vec![1, 2, 3]);
            let frames = mutation.encode_frames().unwrap();
            assert_eq!(frames, vec![mutation.encode().unwrap()]);
            assert_eq!(
                DocumentFrame::decode(&frames[0]).unwrap(),
                DocumentFrame::Whole(mutation)
            );
        }

        // Phase: over_limit_mutations_are_refused_before_encoding.
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

        // Phase: wire_rejects_unsegmented_frames_above_the_frame_limit.
        {
            let mut wire = DOCUMENT_MUTATION_V2_PREFIX.to_vec();
            wire.push(DOCUMENT_INCREMENTAL_TAG);
            wire.extend(vec![0; MAX_DOCUMENT_MUTATION_BYTES + 1]);
            assert!(matches!(
                DocumentFrame::decode(&wire),
                Err(DocumentMutationError::TooLarge { .. })
            ));
            // The endpoint decode of a reassembled mutation accepts it.
            assert!(DocumentMutation::decode(&wire).is_ok());
        }

        // Phase: malformed_segment_headers_are_rejected.
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

    /// Large mutations round-trip through signed segments, reassemble in any order ignoring
    /// duplicates, and are discarded when bytes are tampered or headers are inconsistent.
    #[test]
    fn segmented_transfers_reassemble_only_intact_consistent_segments() {
        // Phase: large_mutations_round_trip_through_signed_segments.
        {
            for (len, snapshot) in [
                (MIB + 1, false),
                (7 * MIB / 2, true),
                (8 * MIB, false),
                (32 * MIB, true),
            ] {
                let data = bytes(len, len as u64);
                let mutation = if snapshot {
                    DocumentMutation::Snapshot(data)
                } else {
                    DocumentMutation::Incremental(data)
                };
                let frames = mutation.encode_frames().unwrap();
                assert_eq!(frames.len(), len.div_ceil(DOCUMENT_SEGMENT_BYTES));
                assert!(
                    frames
                        .iter()
                        .all(|frame| frame.len() <= DOCUMENT_SEGMENT_BYTES + 128),
                    "every signed frame stays small"
                );
                let mut reassembler = DocumentReassembler::default();
                let got = reassemble(&mut reassembler, ArtifactId::new(), &frames);
                assert_eq!(got.as_ref(), Some(&mutation), "{len} bytes round trip");
                assert_eq!(reassembler.buffered_bytes(), 0);
                assert_eq!(reassembler.pending_transfers(), 0);
            }
        }

        // Phase: segments_reassemble_in_any_order_and_ignore_duplicates.
        {
            let mutation = DocumentMutation::Incremental(bytes(5 * MIB + 17, 9));
            let mut frames = mutation.encode_frames().unwrap();
            frames.reverse();
            let duplicate = frames[1].clone();
            frames.insert(2, duplicate);
            let got = reassemble(
                &mut DocumentReassembler::default(),
                ArtifactId::new(),
                &frames,
            );
            assert_eq!(got, Some(mutation));
        }

        // Phase: tampered_bytes_fail_the_transfer_digest.
        {
            let mutation = DocumentMutation::Incremental(bytes(2 * MIB + 5, 4));
            let frames = mutation.encode_frames().unwrap();
            let mut parts = segments(&frames);
            parts[1].data[7] ^= 0xff;
            let artifact = ArtifactId::new();
            let mut reassembler = DocumentReassembler::default();
            let now = Instant::now();
            let mut result = Ok(None);
            for (frame, segment) in frames.iter().zip(parts) {
                result = reassembler.accept(&envelope(artifact, 1, frame.clone()), segment, now);
            }
            assert_eq!(result, Err(ReassemblyError::DigestMismatch));
            assert_eq!(reassembler.buffered_bytes(), 0);
        }

        // Phase: inconsistent_headers_discard_the_transfer.
        {
            let mutation = DocumentMutation::Incremental(bytes(3 * MIB, 6));
            let frames = mutation.encode_frames().unwrap();
            let parts = segments(&frames);
            let artifact = ArtifactId::new();
            let mut reassembler = DocumentReassembler::default();
            let now = Instant::now();
            reassembler
                .accept(
                    &envelope(artifact, 1, frames[0].clone()),
                    parts[0].clone(),
                    now,
                )
                .unwrap();
            let mut liar = parts[1].clone();
            liar.kind = DocumentMutationKind::Snapshot;
            assert_eq!(
                reassembler.accept(&envelope(artifact, 1, frames[1].clone()), liar, now),
                Err(ReassemblyError::Inconsistent)
            );
            assert_eq!(reassembler.pending_transfers(), 0);
        }
    }

    /// Transfers from different authors or routes never mix, and memory stays bounded with
    /// abandonment reported.
    #[test]
    fn transfers_stay_isolated_and_bounded() {
        // Phase: transfers_from_different_authors_or_routes_never_mix.
        {
            let mutation = DocumentMutation::Incremental(bytes(2 * MIB + 1, 5));
            let frames = mutation.encode_frames().unwrap();
            let parts = segments(&frames);
            let artifact = ArtifactId::new();
            let mut reassembler = DocumentReassembler::default();
            let now = Instant::now();
            // Author 1 sends the first segments, author 2 the last: neither is
            // complete, because each author's transfer is tracked separately.
            let last = parts.len() - 1;
            for (index, segment) in parts.iter().enumerate() {
                let author = if index == last { 2 } else { 1 };
                let done = reassembler
                    .accept(
                        &envelope(artifact, author, frames[index].clone()),
                        segment.clone(),
                        now,
                    )
                    .unwrap();
                assert!(done.is_none());
            }
            assert_eq!(reassembler.pending_transfers(), 2);
            let other_route = reassembler
                .accept(
                    &envelope(ArtifactId::new(), 1, frames[last].clone()),
                    parts[last].clone(),
                    now,
                )
                .unwrap();
            assert!(other_route.is_none(), "another route cannot finish it");
        }

        // Phase: memory_stays_bounded_and_abandonment_is_reported.
        {
            let limits = ReassemblyLimits {
                max_buffered_bytes: 3 * MIB,
                max_transfers: 2,
                idle_timeout: Duration::from_secs(10),
            };
            let mut reassembler = DocumentReassembler::new(limits);
            let now = Instant::now();
            let artifact = ArtifactId::new();
            // Four incomplete transfers, each holding one 1 MiB segment.
            for seed in 0..4_u64 {
                let mutation = DocumentMutation::Incremental(bytes(2 * MIB, seed + 100));
                let frames = mutation.encode_frames().unwrap();
                let first = segments(&frames).remove(0);
                let at = now + Duration::from_millis(seed);
                reassembler
                    .accept(&envelope(artifact, 1, frames[0].clone()), first, at)
                    .unwrap();
                assert!(reassembler.buffered_bytes() <= limits.max_buffered_bytes);
                assert!(reassembler.pending_transfers() <= limits.max_transfers);
            }
            let abandoned = reassembler.take_abandoned();
            assert_eq!(abandoned.len(), 2, "the two oldest transfers were evicted");
            assert!(
                abandoned
                    .iter()
                    .all(|t| t.reason == AbandonReason::Evicted && t.received_segments == 1)
            );

            reassembler.expire(now + Duration::from_secs(60));
            assert_eq!(reassembler.pending_transfers(), 0);
            assert_eq!(reassembler.buffered_bytes(), 0);
            assert!(
                reassembler
                    .take_abandoned()
                    .iter()
                    .all(|t| t.reason == AbandonReason::Idle)
            );
        }
    }
}
