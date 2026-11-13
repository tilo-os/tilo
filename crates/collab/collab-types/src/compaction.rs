//! Host-driven compaction checkpoint.
//!
//! A `CompactionCheckpoint` is the payload of a [`crate::op::SignedOp`]
//! whose `kind` is [`crate::role::OpKind::Compaction`]. The
//! channel's host (initially the owner's primary device) emits one every
//! N ops or M bytes; recipients use it to evict older op-log entries so
//! memory doesn't grow forever.
//!
//! ## Verification
//!
//! The checkpoint carries a `prefix_hash` over the canonical bytes of
//! every op with `seq <= up_to_seq`, concatenated in `seq` order.
//! Recipients recompute it from their own log; a mismatch means the
//! checkpoint is forged or the recipient saw a different op stream — a
//! hard error either way, and eviction is refused.
//!
//! `SignedOp` already carries `artifact`/`seq` on its envelope, but those
//! describe the checkpoint op itself, not the range it covers — keeping
//! `up_to_seq` inside the payload avoids baking the envelope's seq
//! semantics into the eviction contract.
//!
//! Like every other wire type in this crate the struct uses
//! `#[serde(deny_unknown_fields)]`, so a forgotten field on an old peer
//! is a hard decode error, not a silently-dropped value.
//!
//! ## Sealed compaction
//!
//! An encrypted record's durable replicas that hold no channel key keep its
//! sealed writes as ciphertext, up to a bound. A [`SealedCompaction`] is how
//! a key holder lifts that bound: it opens the history, merges it into one
//! document, seals that document as a snapshot under the channel's current
//! epoch and signs the result as a `Compaction` op on the record's route.
//! A keyless replica never opens it; it recomputes which of its writes the
//! compaction `covers` and their `prefix_hash`, and replaces exactly those.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, canonical_bytes, from_canonical_bytes};
use crate::channel::ArtifactId;
use crate::crypto::Ed25519PublicKey;
use crate::op::{ContentRetention, SignedOp};
use crate::role::OpKind;

/// A signed compaction checkpoint payload. `(artifact, up_to_seq)`
/// jointly identify "every op with this artifact id and seq ≤ `up_to_seq`
/// is eligible for eviction"; `prefix_hash` lets a recipient verify it
/// matches the op stream they observed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionCheckpoint {
    /// The artifact whose op log this checkpoint covers — same id as the
    /// enclosing `SignedOp::artifact`, kept in the payload too so the
    /// checkpoint's scope is self-describing.
    pub artifact: ArtifactId,
    /// Inclusive upper bound: every op with `seq <= up_to_seq` is eligible
    /// for eviction. Lower bound is "everything stored locally". The host
    /// picks this at minting time — typically the last seq observed.
    pub up_to_seq: u64,
    /// SHA-256 digest over the canonical bytes of every op the checkpoint
    /// covers, concatenated in `seq` order, 32 bytes (same shape every other
    /// digest in this stack uses).
    ///
    /// Recomputation: gather ops with seq in `[lower, up_to_seq]`,
    /// canonical-encode each, concatenate, SHA-256. (SHA-256 for v1 —
    /// iroh-blobs uses blake3, but adding that dep purely for the checkpoint
    /// isn't worth it; postcard + `[u8; 32]` already satisfies determinism.)
    pub prefix_hash: [u8; 32],
}

/// Reasons compacting an op log can fail.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CompactionError {
    /// The hash recomputed from the recipient's local op log doesn't match
    /// `prefix_hash` — the checkpoint is forged, or ops differ between host
    /// and recipient. Either way: do not evict.
    #[error("checkpoint prefix hash does not match local op log")]
    PrefixHashMismatch,
    /// The checkpoint's payload didn't decode as a `CompactionCheckpoint` —
    /// forged for a different schema, or the op was misclassified.
    #[error("malformed compaction payload: {0}")]
    Malformed(#[source] CanonicalError),
    /// The op claimed `kind == Compaction` but the payload didn't match
    /// the expected shape. Kept distinct so audit logs can tell "valid
    /// postcard, wrong type" from "garbage bytes".
    #[error("compaction op had the wrong kind: {actual:?}")]
    WrongOpKind {
        /// The `kind` the envelope actually carried, for audit-log triage.
        actual: OpKind,
    },
    /// A sealed compaction that decoded but breaks its own contract.
    #[error("invalid sealed compaction: {0}")]
    InvalidSealed(&'static str),
}

/// Encode a `CompactionCheckpoint` to canonical bytes — the format an
/// op's payload field should carry when `kind == Compaction`.
pub fn encode_compaction_payload(cp: &CompactionCheckpoint) -> Result<Vec<u8>, CanonicalError> {
    canonical_bytes(cp)
}

/// Decode a `SignedOp::payload` as a `CompactionCheckpoint`. Verifies
/// `op.kind == Compaction` so a caller gets a precise error rather
/// than a silently-decoded random struct. The returned struct is owned,
/// so callers can drop the original envelope.
///
/// # Errors
///
/// Returns [`CompactionError::WrongOpKind`] if `op.kind != Compaction`,
/// or [`CompactionError::Malformed`] if the payload doesn't decode as a
/// `CompactionCheckpoint`.
pub fn parse_compaction_payload(op: &SignedOp) -> Result<CompactionCheckpoint, CompactionError> {
    if op.kind != OpKind::Compaction {
        return Err(CompactionError::WrongOpKind { actual: op.kind });
    }
    from_canonical_bytes::<CompactionCheckpoint>(&op.payload).map_err(CompactionError::Malformed)
}

/// Compute the 32-byte digest of an op-log prefix: concatenates the
/// canonical bytes of each [`SignedOp`] in argument order (callers feed
/// ops in `seq` order) and SHA-256s the result.
///
/// Exposed so a host can mint a checkpoint with the same hashing rule
/// receivers use, avoiding drift.
pub fn hash_op_prefix(ops: &[SignedOp]) -> Result<[u8; 32], CanonicalError> {
    hash_ops(ops)
}

fn hash_ops<'a>(ops: impl IntoIterator<Item = &'a SignedOp>) -> Result<[u8; 32], CanonicalError> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for op in ops {
        let bytes = canonical_bytes(op)?;
        hasher.update(&bytes);
    }
    let out = hasher.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    Ok(arr)
}

/// Version of a [`SealedCompaction`] whose snapshot is one op.
pub const SEALED_COMPACTION_VERSION: u8 = 1;
/// Version of a [`SealedCompaction`] whose snapshot spans its op and the
/// [`SealedCompactionSegment`]s it names.
pub const SEGMENTED_COMPACTION_VERSION: u8 = 2;
/// Version of a [`SealedCompactionSegment`].
pub const COMPACTION_SEGMENT_VERSION: u8 = 3;
const SEGMENT_HASH_DOMAIN: &[u8] = b"tilo-compaction-segment-v1\0";

/// Payload of a sealed record's `Compaction` op. The op itself is sealed
/// under the channel's content key with [`ContentRetention::Checkpoint`]
/// metadata; `ciphertext` is that sealed `DocumentMutation::Snapshot` of the
/// merged record, and opens exactly like a sealed snapshot's payload.
///
/// A snapshot larger than one op travels as a segmented document transfer
/// ([`crate::document_transfer`]): `ciphertext` opens to its first segment,
/// and `segments` names each later one, sealed in a
/// [`SealedCompactionSegment`] op of the same author and sequence that follows
/// the compaction in the record's history. A replica takes the compaction only
/// with every segment it names, at once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedCompaction {
    pub version: u8,
    /// Highest superseded sequence per author, sorted by author, unique.
    /// A write is superseded when its author is listed and its sequence is
    /// at most the listed one.
    pub covers: Vec<(Ed25519PublicKey, u64)>,
    /// [`hash_sealed_prefix`] of every superseded write.
    pub prefix_hash: [u8; 32],
    /// The sealed snapshot of the merged record, or its first segment.
    pub ciphertext: Vec<u8>,
    /// [`segment_hash`] of each later segment's payload, in order; empty for
    /// a snapshot in one op.
    pub segments: Vec<[u8; 32]>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WholeWire {
    version: u8,
    covers: Vec<(Ed25519PublicKey, u64)>,
    prefix_hash: [u8; 32],
    #[serde(with = "serde_bytes")]
    ciphertext: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SegmentedWire {
    version: u8,
    covers: Vec<(Ed25519PublicKey, u64)>,
    prefix_hash: [u8; 32],
    #[serde(with = "serde_bytes")]
    ciphertext: Vec<u8>,
    segments: Vec<[u8; 32]>,
}

/// Payload of one later segment of a segmented [`SealedCompaction`]: a
/// `Compaction` op of the compaction's author and sequence, sealed with
/// delta retention, whose `ciphertext` opens to one segment of the
/// compaction's snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedCompactionSegment {
    pub version: u8,
    #[serde(with = "serde_bytes")]
    pub ciphertext: Vec<u8>,
}

impl SealedCompactionSegment {
    /// The segment payload carrying `ciphertext`.
    #[must_use]
    pub fn new(ciphertext: Vec<u8>) -> Self {
        Self {
            version: COMPACTION_SEGMENT_VERSION,
            ciphertext,
        }
    }

    /// Canonical bytes, the payload of the segment op.
    pub fn encode(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(self)
    }

    /// Decode `op` as one segment of a segmented compaction.
    ///
    /// # Errors
    ///
    /// As [`SealedCompaction::parse`], for an op that is no segment.
    pub fn parse(op: &SignedOp) -> Result<Self, CompactionError> {
        if op.kind != OpKind::Compaction {
            return Err(CompactionError::WrongOpKind { actual: op.kind });
        }
        // It follows its compaction in a store's retained tail, as a later
        // segment of any snapshot does.
        if op.encryption.and_then(|meta| meta.retention()) != Some(ContentRetention::Delta) {
            return Err(CompactionError::InvalidSealed(
                "a compaction segment is a content-key delta",
            ));
        }
        if op.payload.first() != Some(&COMPACTION_SEGMENT_VERSION) {
            return Err(CompactionError::InvalidSealed("not a compaction segment"));
        }
        let segment =
            from_canonical_bytes::<Self>(&op.payload).map_err(CompactionError::Malformed)?;
        if segment.ciphertext.is_empty() {
            return Err(CompactionError::InvalidSealed("empty ciphertext"));
        }
        Ok(segment)
    }
}

/// Whether `op` is one later segment of a segmented compaction, by its
/// payload's version; [`SealedCompactionSegment::parse`] checks the rest.
#[must_use]
pub fn is_compaction_segment(op: &SignedOp) -> bool {
    op.kind == OpKind::Compaction && op.payload.first() == Some(&COMPACTION_SEGMENT_VERSION)
}

/// Whether `op` is a piece of a compaction that spans several ops: one of its
/// segments, or the compaction that names them. A replica takes such a piece
/// only with every other piece of its compaction.
#[must_use]
pub fn is_compaction_piece(op: &SignedOp) -> bool {
    is_compaction_segment(op)
        || (op.kind == OpKind::Compaction
            && op.payload.first() == Some(&SEGMENTED_COMPACTION_VERSION))
}

/// Digest a segmented compaction names a segment op's `payload` by.
#[must_use]
pub fn segment_hash(payload: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(SEGMENT_HASH_DOMAIN);
    hasher.update(payload);
    hasher.finalize().into()
}

fn sealed_checkpoint(op: &SignedOp) -> Result<(), CompactionError> {
    if op.kind != OpKind::Compaction {
        return Err(CompactionError::WrongOpKind { actual: op.kind });
    }
    if op.encryption.and_then(|meta| meta.retention()) != Some(ContentRetention::Checkpoint) {
        return Err(CompactionError::InvalidSealed(
            "a sealed compaction is a content-key checkpoint",
        ));
    }
    Ok(())
}

impl SealedCompaction {
    /// The compaction superseding exactly `superseded` with `ciphertext`.
    ///
    /// # Errors
    ///
    /// [`CompactionError::InvalidSealed`] when nothing is superseded or the
    /// ciphertext is empty; [`CompactionError::Malformed`] when an op does
    /// not canonicalise.
    pub fn new(superseded: &[SignedOp], ciphertext: Vec<u8>) -> Result<Self, CompactionError> {
        if superseded.is_empty() || ciphertext.is_empty() {
            return Err(CompactionError::InvalidSealed(
                "a compaction supersedes something with a sealed snapshot",
            ));
        }
        let mut covers = BTreeMap::<Ed25519PublicKey, u64>::new();
        for op in superseded {
            let seq = covers.entry(op.author).or_default();
            *seq = (*seq).max(op.seq);
        }
        Ok(Self {
            version: SEALED_COMPACTION_VERSION,
            covers: covers.into_iter().collect(),
            prefix_hash: hash_sealed_prefix(superseded).map_err(CompactionError::Malformed)?,
            ciphertext,
            segments: Vec::new(),
        })
    }

    /// This compaction with its snapshot continued in the segment ops whose
    /// payloads are `segments`, in order.
    #[must_use]
    pub fn segmented(mut self, segments: &[Vec<u8>]) -> Self {
        self.segments = segments
            .iter()
            .map(|payload| segment_hash(payload))
            .collect();
        self.version = if self.segments.is_empty() {
            SEALED_COMPACTION_VERSION
        } else {
            SEGMENTED_COMPACTION_VERSION
        };
        self
    }

    /// Canonical bytes, the payload of the compaction op.
    pub fn encode(&self) -> Result<Vec<u8>, CanonicalError> {
        if self.segments.is_empty() {
            canonical_bytes(&WholeWire {
                version: self.version,
                covers: self.covers.clone(),
                prefix_hash: self.prefix_hash,
                ciphertext: self.ciphertext.clone(),
            })
        } else {
            canonical_bytes(&SegmentedWire {
                version: self.version,
                covers: self.covers.clone(),
                prefix_hash: self.prefix_hash,
                ciphertext: self.ciphertext.clone(),
                segments: self.segments.clone(),
            })
        }
    }

    /// Decode `op` as a sealed compaction: a `Compaction` op sealed with
    /// checkpoint retention whose payload keeps this type's contract.
    ///
    /// # Errors
    ///
    /// [`CompactionError::WrongOpKind`], [`CompactionError::Malformed`] or
    /// [`CompactionError::InvalidSealed`].
    pub fn parse(op: &SignedOp) -> Result<Self, CompactionError> {
        sealed_checkpoint(op)?;
        let compaction = match op.payload.first() {
            Some(&SEALED_COMPACTION_VERSION) => {
                let wire = from_canonical_bytes::<WholeWire>(&op.payload)
                    .map_err(CompactionError::Malformed)?;
                Self {
                    version: wire.version,
                    covers: wire.covers,
                    prefix_hash: wire.prefix_hash,
                    ciphertext: wire.ciphertext,
                    segments: Vec::new(),
                }
            }
            Some(&SEGMENTED_COMPACTION_VERSION) => {
                let wire = from_canonical_bytes::<SegmentedWire>(&op.payload)
                    .map_err(CompactionError::Malformed)?;
                if wire.segments.is_empty() {
                    return Err(CompactionError::InvalidSealed(
                        "a segmented compaction names no segment",
                    ));
                }
                Self {
                    version: wire.version,
                    covers: wire.covers,
                    prefix_hash: wire.prefix_hash,
                    ciphertext: wire.ciphertext,
                    segments: wire.segments,
                }
            }
            _ => return Err(CompactionError::InvalidSealed("unknown version")),
        };
        if compaction.covers.is_empty()
            || !compaction
                .covers
                .iter()
                .zip(compaction.covers.iter().skip(1))
                .all(|(before, after)| before.0 < after.0)
        {
            return Err(CompactionError::InvalidSealed(
                "covers is empty, unsorted or repeats an author",
            ));
        }
        if compaction.ciphertext.is_empty() {
            return Err(CompactionError::InvalidSealed("empty ciphertext"));
        }
        Ok(compaction)
    }

    /// Whether `op` is one of the writes this compaction supersedes.
    pub fn covers(&self, op: &SignedOp) -> bool {
        self.covered(&op.author).is_some_and(|seq| op.seq <= seq)
    }

    /// The highest superseded sequence of `author`, if any is superseded.
    pub fn covered(&self, author: &Ed25519PublicKey) -> Option<u64> {
        self.covers
            .binary_search_by_key(author, |(listed, _)| *listed)
            .ok()
            .and_then(|index| self.covers.get(index))
            .map(|(_, seq)| *seq)
    }

    /// Whether `segment`, a segment op, is the `index`th this compaction
    /// (whose op is `head`) names: of its author and sequence, under its epoch.
    #[must_use]
    pub fn names_segment(&self, head: &SignedOp, index: usize, segment: &SignedOp) -> bool {
        self.segments
            .get(index)
            .is_some_and(|hash| *hash == segment_hash(&segment.payload))
            && segment.author == head.author
            && segment.seq == head.seq
            && segment.artifact == head.artifact
            && segment.encryption.map(|meta| (meta.channel, meta.epoch))
                == head.encryption.map(|meta| (meta.channel, meta.epoch))
            && SealedCompactionSegment::parse(segment).is_ok()
    }
}

/// Digest of a set of sealed writes, independent of the order a replica
/// admitted them in: each op's canonical bytes, sorted by `(author, seq)`,
/// SHA-256 over the concatenation (as [`hash_op_prefix`]).
pub fn hash_sealed_prefix<'a>(
    ops: impl IntoIterator<Item = &'a SignedOp>,
) -> Result<[u8; 32], CanonicalError> {
    let mut ops: Vec<&SignedOp> = ops.into_iter().collect();
    ops.sort_by_key(|op| (op.author, op.seq));
    hash_ops(ops)
}

/// The content-key ciphertext of a sealed op: a sealed compaction's
/// snapshot (or one segment of it), any other op's payload. Every content opener reads through
/// this, so a key holder opens a compaction like any sealed snapshot.
///
/// # Errors
///
/// As [`SealedCompaction::parse`], for a `Compaction` op.
pub fn sealed_ciphertext(op: &SignedOp) -> Result<Cow<'_, [u8]>, CompactionError> {
    if is_compaction_segment(op) {
        return SealedCompactionSegment::parse(op).map(|segment| Cow::Owned(segment.ciphertext));
    }
    if op.kind == OpKind::Compaction {
        return SealedCompaction::parse(op).map(|compaction| Cow::Owned(compaction.ciphertext));
    }
    Ok(Cow::Borrowed(&op.payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_keypair;
    use crate::identity::AttestationId;
    use web_time::SystemTime;

    fn dummy_op(seq: u64) -> SignedOp {
        let (_, pk) = generate_keypair();
        SignedOp::new_unsigned(
            vec![seq.to_le_bytes()[0]],
            ArtifactId::new(),
            pk,
            AttestationId::new(),
            OpKind::Change,
            SystemTime::UNIX_EPOCH,
            seq,
        )
    }

    /// Compaction payloads round-trip and reject the wrong op class; prefix hashes are deterministic,
    /// order-sensitive and stable for an empty prefix.
    #[test]
    fn compaction_payloads_and_prefix_hashes_are_exact() {
        // Phase: parse_compaction_payload_round_trip.
        {
            let cp = CompactionCheckpoint {
                artifact: ArtifactId::new(),
                up_to_seq: 42,
                prefix_hash: [0xAB; 32],
            };
            let bytes = encode_compaction_payload(&cp).expect("encode");

            let (_, pk) = generate_keypair();
            let op = SignedOp::new_unsigned(
                bytes,
                cp.artifact,
                pk,
                AttestationId::new(),
                OpKind::Compaction,
                SystemTime::UNIX_EPOCH,
                100,
            );
            let parsed = parse_compaction_payload(&op).expect("parse");
            assert_eq!(parsed, cp);
        }

        // Phase: parse_compaction_payload_rejects_wrong_op_kind.
        {
            // A SignedOp with kind=Splice carrying a checkpoint-shaped
            // payload still must be rejected — the grants give Editors
            // Splice but NOT Compaction, and a typed error here lets the audit
            // log distinguish forgery from a genuine malformed payload.
            let cp = CompactionCheckpoint {
                artifact: ArtifactId::new(),
                up_to_seq: 5,
                prefix_hash: [0; 32],
            };
            let bytes = encode_compaction_payload(&cp).expect("encode");
            let (_, pk) = generate_keypair();
            let op = SignedOp::new_unsigned(
                bytes,
                cp.artifact,
                pk,
                AttestationId::new(),
                OpKind::Change,
                SystemTime::UNIX_EPOCH,
                7,
            );
            match parse_compaction_payload(&op) {
                Err(CompactionError::WrongOpKind { actual }) => {
                    assert_eq!(actual, OpKind::Change);
                }
                other => panic!("expected WrongOpKind, got {other:?}"),
            }
        }

        // Phase: hash_op_prefix_is_deterministic.
        {
            let a = dummy_op(1);
            let b = dummy_op(2);
            let h1 = hash_op_prefix(&[a.clone(), b.clone()]).unwrap();
            let h2 = hash_op_prefix(&[a, b]).unwrap();
            assert_eq!(h1, h2);
        }

        // Phase: hash_op_prefix_order_sensitive.
        {
            // The hash must be sensitive to op order so a peer reordering
            // its log can't pass verification.
            let a = dummy_op(1);
            let b = dummy_op(2);
            let h_ab = hash_op_prefix(&[a.clone(), b.clone()]).unwrap();
            let h_ba = hash_op_prefix(&[b, a]).unwrap();
            assert_ne!(h_ab, h_ba);
        }

        // Phase: hash_op_prefix_empty_is_stable.
        {
            // Edge case: an empty prefix (a checkpoint before any ops) — the
            // hash must be SHA-256 of the empty string, not a panic.
            let h = hash_op_prefix(&[]).unwrap();
            // SHA-256("") = e3b0c4...
            assert_eq!(h[0], 0xe3);
            assert_eq!(h[1], 0xb0);
        }
    }

    fn sealed_op(payload: Vec<u8>, kind: OpKind, retention: ContentRetention) -> SignedOp {
        let (_, pk) = generate_keypair();
        SignedOp::new_unsigned_with_encryption(
            payload,
            ArtifactId::new(),
            pk,
            AttestationId::new(),
            kind,
            SystemTime::UNIX_EPOCH,
            9,
            crate::op::EncryptionMetadata::content_key(3, crate::ChannelId::new(), retention),
        )
    }

    /// A sealed compaction lists each author's highest superseded write once and hashes the
    /// superseded set independently of admission order.
    #[test]
    fn sealed_compaction_covers_exactly_its_writes() {
        let (a, b, c) = (dummy_op(1), dummy_op(2), dummy_op(3));
        let compaction =
            SealedCompaction::new(&[a.clone(), b.clone(), c.clone()], vec![7]).unwrap();
        assert!(compaction.covers(&a) && compaction.covers(&b) && compaction.covers(&c));
        assert!(
            compaction
                .covers
                .iter()
                .zip(compaction.covers.iter().skip(1))
                .all(|(x, y)| x.0 < y.0),
            "covers is sorted by author"
        );
        assert!(!compaction.covers(&dummy_op(1)), "another author's write");
        assert_eq!(
            hash_sealed_prefix([&c, &a, &b]).unwrap(),
            hash_sealed_prefix([&b, &c, &a]).unwrap(),
            "the digest does not depend on admission order"
        );
        assert_ne!(
            hash_sealed_prefix([&a, &b]).unwrap(),
            compaction.prefix_hash,
            "a different set hashes differently"
        );
        assert!(SealedCompaction::new(&[], vec![7]).is_err());
        assert!(SealedCompaction::new(std::slice::from_ref(&a), Vec::new()).is_err());
    }

    /// A sealed compaction decodes only from a sealed checkpoint `Compaction` op that keeps
    /// its contract, and only its ciphertext is what a key holder opens.
    #[test]
    fn sealed_compaction_parses_only_its_contract() {
        let a = dummy_op(1);
        let compaction =
            SealedCompaction::new(&[a.clone(), dummy_op(2), dummy_op(3)], vec![7]).unwrap();
        let payload = compaction.encode().unwrap();
        let op = sealed_op(
            payload.clone(),
            OpKind::Compaction,
            ContentRetention::Checkpoint,
        );
        assert_eq!(SealedCompaction::parse(&op).unwrap(), compaction);
        assert_eq!(sealed_ciphertext(&op).unwrap().as_ref(), &[7]);
        let write = sealed_op(vec![1, 2], OpKind::Change, ContentRetention::Delta);
        assert_eq!(sealed_ciphertext(&write).unwrap().as_ref(), &[1, 2]);

        for (op, why) in [
            (
                sealed_op(
                    payload.clone(),
                    OpKind::Change,
                    ContentRetention::Checkpoint,
                ),
                "another op class",
            ),
            (
                sealed_op(payload.clone(), OpKind::Compaction, ContentRetention::Delta),
                "delta retention",
            ),
            (
                SignedOp::new_unsigned(
                    payload.clone(),
                    ArtifactId::new(),
                    a.author,
                    AttestationId::new(),
                    OpKind::Compaction,
                    SystemTime::UNIX_EPOCH,
                    1,
                ),
                "a cleartext compaction",
            ),
        ] {
            assert!(SealedCompaction::parse(&op).is_err(), "{why} parses");
        }
        let mut extended = payload;
        extended.push(0);
        assert!(
            SealedCompaction::parse(&sealed_op(
                extended,
                OpKind::Compaction,
                ContentRetention::Checkpoint
            ))
            .is_err(),
            "trailing bytes"
        );
    }

    /// A compaction whose snapshot spans several ops names each segment by its payload, keeps
    /// the one-op encoding for one that does not, and opens each segment as its own ciphertext.
    #[test]
    fn a_segmented_compaction_names_exactly_its_segments() {
        let superseded = [dummy_op(1), dummy_op(2)];
        let whole = SealedCompaction::new(&superseded, vec![7]).unwrap();
        let encoded = whole.encode().unwrap();
        assert_eq!(encoded.first(), Some(&SEALED_COMPACTION_VERSION));
        let payloads: Vec<Vec<u8>> = [vec![8], vec![9]]
            .into_iter()
            .map(|ciphertext| SealedCompactionSegment::new(ciphertext).encode().unwrap())
            .collect();
        let segmented = whole.segmented(&payloads);
        let head = sealed_op(
            segmented.encode().unwrap(),
            OpKind::Compaction,
            ContentRetention::Checkpoint,
        );
        assert_eq!(SealedCompaction::parse(&head).unwrap(), segmented);
        assert!(is_compaction_piece(&head) && !is_compaction_segment(&head));
        let segment = |payload: &Vec<u8>, retention| {
            let mut op = sealed_op(payload.clone(), OpKind::Compaction, retention);
            op.author = head.author;
            op.seq = head.seq;
            op.artifact = head.artifact;
            op.encryption = head.encryption.map(|meta| {
                crate::op::EncryptionMetadata::content_key(meta.epoch, meta.channel, retention)
            });
            op
        };
        let first = segment(&payloads[0], ContentRetention::Delta);
        let second = segment(&payloads[1], ContentRetention::Delta);
        assert!(is_compaction_segment(&first) && is_compaction_piece(&first));
        assert!(segmented.names_segment(&head, 0, &first));
        assert!(segmented.names_segment(&head, 1, &second));
        assert!(!segmented.names_segment(&head, 0, &second), "out of order");
        assert!(
            !segmented.names_segment(
                &head,
                0,
                &segment(&payloads[0], ContentRetention::Checkpoint)
            ),
            "a segment follows its compaction as a delta"
        );
        assert_eq!(sealed_ciphertext(&second).unwrap().as_ref(), &[9]);
        let mut other = first;
        other.seq += 1;
        assert!(
            !segmented.names_segment(&head, 0, &other),
            "another sequence"
        );
    }

    /// A sealed compaction whose own fields break its contract neither parses nor opens.
    #[test]
    fn sealed_compaction_refuses_a_broken_contract() {
        let compaction =
            SealedCompaction::new(&[dummy_op(1), dummy_op(2), dummy_op(3)], vec![7]).unwrap();
        for (tampered, why) in [
            (
                SealedCompaction {
                    version: 2,
                    ..compaction.clone()
                },
                "an unknown version",
            ),
            (
                SealedCompaction {
                    covers: compaction.covers.iter().rev().copied().collect(),
                    ..compaction.clone()
                },
                "unsorted covers",
            ),
            (
                SealedCompaction {
                    covers: vec![compaction.covers[0], compaction.covers[0]],
                    ..compaction.clone()
                },
                "a repeated author",
            ),
            (
                SealedCompaction {
                    covers: Vec::new(),
                    ..compaction.clone()
                },
                "empty covers",
            ),
            (
                SealedCompaction {
                    ciphertext: Vec::new(),
                    ..compaction.clone()
                },
                "an empty ciphertext",
            ),
        ] {
            let op = sealed_op(
                tampered.encode().unwrap(),
                OpKind::Compaction,
                ContentRetention::Checkpoint,
            );
            assert!(SealedCompaction::parse(&op).is_err(), "{why} parses");
            assert!(sealed_ciphertext(&op).is_err(), "{why} opens");
        }
    }
}
