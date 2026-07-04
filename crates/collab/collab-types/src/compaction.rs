//! Phase 6: host-driven compaction checkpoint.
//!
//! A `CompactionCheckpoint` is the payload carried by a
//! [`crate::op::SignedOp`] whose `op_class` is
//! [`crate::role::OpClass::Compaction`]. The channel's host (initial:
//! the channel owner's primary device) emits one of these every N ops
//! or M bytes; recipients use the checkpoint to evict older op-log
//! entries from in-memory storage so memory doesn't grow forever.
//!
//! ## Verification
//!
//! The checkpoint carries a `prefix_hash` of the canonical-bytes of
//! every op with `seq <= up_to_seq`, concatenated in `seq` order.
//! Recipients recompute the hash from their own log; mismatch =>
//! either the checkpoint is forged or the recipient saw a different
//! op stream than the host. Either case is a hard error — we refuse
//! to evict.
//!
//! ## Why not store the artifact + seq inside the SignedOp directly?
//!
//! `SignedOp` already carries `artifact` and `seq` fields on its
//! envelope, but those describe the checkpoint *op itself* (i.e. "the
//! checkpoint mutation lands on this artifact, at this seq, by this
//! author"), not the range it covers. The `up_to_seq` inside the
//! payload is "evict every prior op up to and including seq N", which
//! is logically distinct from "this checkpoint op has seq N+1". By
//! keeping the range inside the payload, we don't bake the wire
//! envelope's seq semantics into the eviction contract.
//!
//! ## Wire stability
//!
//! Like every other wire type in this crate, the struct uses
//! `#[serde(deny_unknown_fields)]` so a forgotten field on an old
//! peer is a hard decode error rather than a silently-dropped value.
//! Postcard is the canonical encoder
//! ([`crate::canonical::canonical_bytes`]).

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, canonical_bytes, from_canonical_bytes};
use crate::channel::ArtifactId;
use crate::op::SignedOp;
use crate::role::OpClass;

/// A signed compaction checkpoint payload.
///
/// `(artifact, up_to_seq)` jointly identify "every op with this
/// artifact id and seq ≤ up_to_seq is eligible for eviction".
/// `prefix_hash` lets a recipient verify the checkpoint matches the
/// op stream they observed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionCheckpoint {
    /// The artifact whose op log this checkpoint covers. Same id as
    /// the enclosing `SignedOp::artifact` — recipients can short-
    /// circuit "which log to evict from" without parsing the payload,
    /// but the field stays inside the payload too so the checkpoint's
    /// scope is self-describing.
    pub artifact: ArtifactId,
    /// The inclusive upper bound: every op in the artifact's log with
    /// `seq <= up_to_seq` is eligible for eviction. Lower bound is
    /// "everything stored locally". The host picks this at minting
    /// time — typically the last seq observed before deciding to
    /// emit a checkpoint.
    pub up_to_seq: u64,
    /// Blake3-style digest over the canonical-bytes of every op the
    /// checkpoint covers, concatenated in `seq` order. The hash is
    /// 32 bytes — the same shape every other digest in this stack
    /// uses (FUSE iroh-blobs, identity attestation ids, …).
    ///
    /// Recomputation: receivers gather their own ops with seq in
    /// `[lower, up_to_seq]`, canonical-encode each `SignedOp`,
    /// concatenate the byte vectors, then SHA-256 (we pick SHA-256
    /// for v1; iroh-blobs uses blake3, but adding a dep on blake3
    /// purely for the checkpoint isn't worth it — postcard +
    /// `[u8; 32]` already satisfies the determinism requirement).
    pub prefix_hash: [u8; 32],
}

/// Reasons compacting an op log can fail.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CompactionError {
    /// The hash recomputed from the recipient's local op log does
    /// not match the checkpoint's `prefix_hash`. Either the checkpoint
    /// is forged, the recipient is missing ops, or the recipient
    /// has *extra* ops the host has not seen. Either way: do not
    /// evict.
    #[error("checkpoint prefix hash does not match local op log")]
    PrefixHashMismatch,
    /// The checkpoint's payload could not be decoded as a
    /// `CompactionCheckpoint`. Either the payload was forged for a
    /// different schema, or the op was misclassified by the author.
    #[error("malformed compaction payload: {0}")]
    Malformed(#[source] CanonicalError),
    /// The op claimed `op_class == Compaction` but the payload didn't
    /// match the expected wire shape — surfaced as a `Malformed`
    /// inside a hosting context. Kept as a separate variant so audit
    /// logs can distinguish "valid postcard, wrong type" from
    /// "garbage bytes".
    #[error("compaction op had the wrong op_class: {actual:?}")]
    WrongOpClass {
        /// The op_class the envelope actually carried. Surfaced for
        /// audit-log triage.
        actual: OpClass,
    },
}

/// Encode a `CompactionCheckpoint` to canonical bytes — the format an
/// op's payload field should carry when `op_class == Compaction`.
pub fn encode_compaction_payload(
    cp: &CompactionCheckpoint,
) -> Result<Vec<u8>, CanonicalError> {
    canonical_bytes(cp)
}

/// Decode a `SignedOp::payload` as a `CompactionCheckpoint`.
///
/// Verifies `op.op_class == Compaction` so a caller can pass any
/// `SignedOp` and get a precise error rather than a silently-decoded
/// random struct. The returned struct is owned (no borrow on `op`)
/// so callers can drop the original envelope and keep the checkpoint
/// around.
pub fn parse_compaction_payload(
    op: &SignedOp,
) -> Result<CompactionCheckpoint, CompactionError> {
    if op.op_class != OpClass::Compaction {
        return Err(CompactionError::WrongOpClass { actual: op.op_class });
    }
    from_canonical_bytes::<CompactionCheckpoint>(&op.payload)
        .map_err(CompactionError::Malformed)
}

/// Compute the 32-byte digest of an op-log prefix.
///
/// Concatenates the canonical bytes of each [`SignedOp`] in the input
/// (in argument order — callers are expected to feed ops in `seq`
/// order) and SHA-256 the result.
///
/// The function is exposed so a host can mint a checkpoint with the
/// same hashing rule receivers use to verify it — sharing the function
/// is the easiest way to avoid drift.
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
            vec![seq as u8],
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
        // payload still has to be rejected, because the role table
        // gives Editors Splice but NOT Compaction. Surfacing this as
        // a typed error lets the audit log distinguish forgery
        // attempts from genuine malformed payloads.
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
        // Edge case: an empty prefix (the host issues a checkpoint
        // before any ops). The hash must be the SHA-256 of the empty
        // string, not panic.
        let h = hash_op_prefix(&[]).unwrap();
        // SHA-256("") = e3b0c4...
        assert_eq!(h[0], 0xe3);
        assert_eq!(h[1], 0xb0);
    }
}
