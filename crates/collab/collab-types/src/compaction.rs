//! Host-driven compaction checkpoint.
//!
//! A `CompactionCheckpoint` is the payload of a [`crate::op::SignedOp`]
//! whose `op_class` is [`crate::role::OpClass::Compaction`]. The
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

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, canonical_bytes, from_canonical_bytes};
use crate::channel::ArtifactId;
use crate::op::SignedOp;
use crate::role::OpClass;

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
    /// The op claimed `op_class == Compaction` but the payload didn't match
    /// the expected shape. Kept distinct so audit logs can tell "valid
    /// postcard, wrong type" from "garbage bytes".
    #[error("compaction op had the wrong op_class: {actual:?}")]
    WrongOpClass {
        /// The `op_class` the envelope actually carried, for audit-log triage.
        actual: OpClass,
    },
}

/// Encode a `CompactionCheckpoint` to canonical bytes — the format an
/// op's payload field should carry when `op_class == Compaction`.
///
/// # Errors
///
/// Returns [`CanonicalError`] if canonical serialisation fails.
pub fn encode_compaction_payload(cp: &CompactionCheckpoint) -> Result<Vec<u8>, CanonicalError> {
    canonical_bytes(cp)
}

/// Decode a `SignedOp::payload` as a `CompactionCheckpoint`. Verifies
/// `op.op_class == Compaction` so a caller gets a precise error rather
/// than a silently-decoded random struct. The returned struct is owned,
/// so callers can drop the original envelope.
///
/// # Errors
///
/// Returns [`CompactionError::WrongOpClass`] if `op.op_class != Compaction`,
/// or [`CompactionError::Malformed`] if the payload doesn't decode as a
/// `CompactionCheckpoint`.
pub fn parse_compaction_payload(op: &SignedOp) -> Result<CompactionCheckpoint, CompactionError> {
    if op.op_class != OpClass::Compaction {
        return Err(CompactionError::WrongOpClass {
            actual: op.op_class,
        });
    }
    from_canonical_bytes::<CompactionCheckpoint>(&op.payload).map_err(CompactionError::Malformed)
}

/// Compute the 32-byte digest of an op-log prefix: concatenates the
/// canonical bytes of each [`SignedOp`] in argument order (callers feed
/// ops in `seq` order) and SHA-256s the result.
///
/// Exposed so a host can mint a checkpoint with the same hashing rule
/// receivers use, avoiding drift.
///
/// # Errors
///
/// Returns [`CanonicalError`] if canonicalising any op in `ops` fails.
pub fn hash_op_prefix(ops: &[SignedOp]) -> Result<[u8; 32], CanonicalError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_keypair;
    use crate::identity::AttestationId;
    use std::time::SystemTime;

    fn dummy_op(seq: u64) -> SignedOp {
        let (_, pk) = generate_keypair();
        SignedOp::new_unsigned(
            vec![seq.to_le_bytes()[0]],
            ArtifactId::new(),
            pk,
            AttestationId::new(),
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            seq,
        )
    }

    #[test]
    fn parse_compaction_payload_round_trip() {
        let cp = CompactionCheckpoint {
            artifact: ArtifactId::new(),
            up_to_seq: 42,
            prefix_hash: [0xAB; 32],
        };
        let bytes = encode_compaction_payload(&cp).expect("encode");

        // Build a SignedOp carrying the encoded payload and the
        // matching op_class.
        let (_, pk) = generate_keypair();
        let op = SignedOp::new_unsigned(
            bytes,
            cp.artifact,
            pk,
            AttestationId::new(),
            OpClass::Compaction,
            SystemTime::UNIX_EPOCH,
            100,
        );
        let parsed = parse_compaction_payload(&op).expect("parse");
        assert_eq!(parsed, cp);
    }

    #[test]
    fn parse_compaction_payload_rejects_wrong_op_class() {
        // A SignedOp with op_class=Splice carrying a checkpoint-shaped
        // payload still must be rejected — the role table gives Editors
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
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            7,
        );
        match parse_compaction_payload(&op) {
            Err(CompactionError::WrongOpClass { actual }) => {
                assert_eq!(actual, OpClass::Splice);
            }
            other => panic!("expected WrongOpClass, got {other:?}"),
        }
    }

    #[test]
    fn hash_op_prefix_is_deterministic() {
        let a = dummy_op(1);
        let b = dummy_op(2);
        let h1 = hash_op_prefix(&[a.clone(), b.clone()]).unwrap();
        let h2 = hash_op_prefix(&[a, b]).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_op_prefix_order_sensitive() {
        // The hash must be sensitive to op order so a peer reordering
        // its log can't pass verification.
        let a = dummy_op(1);
        let b = dummy_op(2);
        let h_ab = hash_op_prefix(&[a.clone(), b.clone()]).unwrap();
        let h_ba = hash_op_prefix(&[b, a]).unwrap();
        assert_ne!(h_ab, h_ba);
    }

    #[test]
    fn hash_op_prefix_empty_is_stable() {
        // Edge case: an empty prefix (a checkpoint before any ops) — the
        // hash must be SHA-256 of the empty string, not a panic.
        let h = hash_op_prefix(&[]).unwrap();
        // SHA-256("") = e3b0c4...
        assert_eq!(h[0], 0xe3);
        assert_eq!(h[1], 0xb0);
    }
}
