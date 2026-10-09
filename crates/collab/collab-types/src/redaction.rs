//! Cooperative cryptographic redaction.
//!
//! A `RedactRequest` is the payload of a [`crate::op::SignedOp`] whose
//! `kind` is [`crate::role::OpKind::Redaction`]. Only the channel owner
//! (see [`crate::role::role_permits`]) can mint one, declaring "this
//! earlier op is now compliance-required to be removed; peers, please honour it."
//!
//! Redaction is **cooperative**, not adversarial: an honest peer redacts
//! in-memory state, rewrites its op-log copy to a fixed marker, and
//! records the request in its audit log; a rogue peer who refuses cannot
//! be forced.
//!
//! [`OpId`] is a stable 16-byte identifier derived from a `SignedOp`'s
//! (author, seq, canonical-bytes-prefix), so peers can address an op with
//! no explicit id field — needed for forward-compat with ops minted
//! before the Redact op-class existed.
//!
//! Like every other wire type in this crate the struct uses
//! `#[serde(deny_unknown_fields)]`, so a forgotten field on an old peer is
//! a hard decode error, not a silently-dropped value.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use web_time::SystemTime;

use crate::canonical::{CanonicalError, canonical_bytes, from_canonical_bytes};
use crate::channel::ArtifactId;
use crate::crypto::Ed25519PublicKey;
use crate::op::SignedOp;
use crate::role::OpKind;

/// Hard upper bound on `RedactRequest::reason`. Long enough for a
/// compliance reference plus a sentence of context; larger justifications
/// belong in the compliance system, not the audit log.
pub const MAX_REDACTION_REASON_BYTES: usize = 512;

/// Stable opaque identifier for one signed op — a SHA-256-derived prefix
/// of `(author pubkey, seq, op canonical-bytes excluding signature)`, so
/// two peers running the same code derive the same id.
///
/// Used by `RedactRequest::target_op_id` and `ChannelState::redacted_ops`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpId(pub [u8; 16]);

impl OpId {
    /// Construct directly from a byte array — mostly for tests and
    /// audit-log readers ingesting an id off the wire.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Derive an `OpId` from a [`SignedOp`], hashing the author's public key,
    /// the seq, and the op's signing-bytes (canonical encoding minus signature).
    ///
    /// Hashing signing-bytes rather than full canonical bytes means two peers
    /// derive the same id even when the signature is absent/placeholder —
    /// needed once the broker has rewritten a redacted op's payload to a
    /// marker (signature no longer verifies, but the pre-redaction id must
    /// still match the audit-log record).
    pub fn from_op(op: &SignedOp) -> Self {
        Self::from_signing_bytes(op.author, op.seq, &op.signing_bytes().unwrap_or_default())
    }

    /// Variant of [`OpId::from_op`] for callers that already have the
    /// signing-bytes (the audit log, which snapshots them at apply-time).
    /// Cuts one canonical-encode pass.
    pub fn from_signing_bytes(author: Ed25519PublicKey, seq: u64, signing_bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"tilo-redact-op-id-v1\0");
        hasher.update(author.as_bytes());
        hasher.update(seq.to_le_bytes());
        hasher.update(signing_bytes);
        let digest = hasher.finalize();
        // SHA-256 always yields 32 bytes, so the 16-byte prefix always
        // exists; the fallback is unreachable in practice.
        let bytes = digest.first_chunk::<16>().copied().unwrap_or([0u8; 16]);
        Self(bytes)
    }

    /// Borrow the raw 16-byte payload — for audit-log readers rendering a
    /// short hex preview.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// A signed redaction request payload, carried by a [`SignedOp`] whose
/// `kind` is [`OpKind::Redaction`]. `target_op_id`/`target_op_hash` are
/// derived from the targeted op at minting time; the audit log
/// cross-references the original envelope with them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactRequest {
    /// The artifact the redacted op lived on — same id as the enclosing
    /// `SignedOp::artifact`, so recipients can short-circuit "which log to
    /// scan" without parsing the payload. Kept for self-description across
    /// artifact-id renames (not a current feature; defensively recorded).
    pub artifact: ArtifactId,

    /// Stable id of the op being redacted, derived via [`OpId::from_op`].
    pub target_op_id: OpId,

    /// SHA-256 of the target op's canonical bytes (full envelope, signature
    /// included). Lets a peer verify "this redaction targets the op I have"
    /// without re-deriving the `OpId` — and, for an MLS-encrypted op, without
    /// decrypting (we hash the ciphertext, not the cleartext).
    pub target_op_hash: [u8; 32],

    /// Free-text justification, capped at [`MAX_REDACTION_REASON_BYTES`].
    /// Recorded verbatim in the audit log and the redaction tooltip; longer
    /// rationale belongs in the org's compliance system.
    pub reason: String,

    /// Public key of the redactor — must equal the enclosing envelope's
    /// `SignedOp::author`, validated by the validator. Deliberate duplication:
    /// lets the validator refuse a Redact whose payload claims a different
    /// identity without trusting the envelope's fields.
    pub redacted_by: Ed25519PublicKey,

    /// Wall-clock time the redaction was issued — independent of the
    /// envelope's `timestamp` (the redactor's own clock at minting-time).
    pub redacted_at: SystemTime,
}

/// Fixed sentinel payload a peer's op-log carries in place of a redacted
/// op's original payload. Round-trips through postcard so downstream
/// readers can decode it instead of seeing opaque bytes.
///
/// Deliberately tiny — a single `kind` discriminator. The audit log holds
/// the full structured `Redaction` record; the marker only needs to say
/// "this seq is redacted, do not render."
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactedMarker {
    /// Version tag for future format evolution; v1 is the only value.
    pub kind: u8,
}

impl RedactedMarker {
    /// Construct the canonical v1 marker.
    pub const fn v1() -> Self {
        Self { kind: 1 }
    }

    /// Encode to canonical bytes — the form an op's payload field
    /// carries after redaction.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if canonical serialisation fails.
    pub fn canonical(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(self)
    }
}

/// Errors produced by the redaction pipeline.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RedactionError {
    /// The op claimed `kind == Redact` but the payload didn't decode as a
    /// `RedactRequest` — distinct from `WrongOpKind` so audit logs can tell
    /// "valid postcard, wrong type" from "garbage bytes".
    #[error("malformed redaction payload: {0}")]
    Malformed(#[source] CanonicalError),

    /// The op claimed `kind != Redact` but a caller tried to parse its
    /// payload as one. The validator catches this earlier; this variant
    /// should only surface from the lower-level parser.
    #[error("redaction op had the wrong kind: {actual:?}")]
    WrongOpKind {
        /// The `kind` the envelope actually carried.
        actual: OpKind,
    },

    /// `reason` exceeded `MAX_REDACTION_REASON_BYTES`. Rejected at parse time
    /// rather than truncated, so a peer can't sneak an oversized justification past the bound.
    #[error("redaction reason exceeds {MAX_REDACTION_REASON_BYTES} byte cap ({0} bytes)")]
    ReasonTooLong(usize),

    /// `redacted_by` disagrees with the enclosing envelope's `author` —
    /// surfaced so a peer can't mint an envelope signed by Bob whose payload
    /// claims Alice did the redacting.
    #[error("redactor pubkey in payload does not match envelope author")]
    RedactorMismatch,
}

/// Encode a `RedactRequest` to canonical bytes — the form an op's payload
/// carries when `kind == Redact`.
///
/// # Errors
///
/// Returns [`CanonicalError`] if canonical serialisation fails.
pub fn encode_redact_payload(req: &RedactRequest) -> Result<Vec<u8>, CanonicalError> {
    canonical_bytes(req)
}

/// Decode a [`SignedOp::payload`] as a [`RedactRequest`].
///
/// Verifies: `op.kind == Redact`; the payload decodes; `reason.len()
/// <= MAX_REDACTION_REASON_BYTES`; `payload.redacted_by == op.author`
/// (catching the identity mismatch before the validator sees it).
///
/// The returned struct is owned, so callers can drop the original envelope.
///
/// # Errors
///
/// Returns [`RedactionError::WrongOpKind`] if `op.kind != Redact`,
/// [`RedactionError::Malformed`] if the payload doesn't decode as a
/// `RedactRequest`, [`RedactionError::ReasonTooLong`] if `reason` exceeds
/// [`MAX_REDACTION_REASON_BYTES`], or [`RedactionError::RedactorMismatch`]
/// if `redacted_by` disagrees with `op.author`.
pub fn parse_redact_payload(op: &SignedOp) -> Result<RedactRequest, RedactionError> {
    if op.kind != OpKind::Redaction {
        return Err(RedactionError::WrongOpKind { actual: op.kind });
    }
    let req: RedactRequest =
        from_canonical_bytes(&op.payload).map_err(RedactionError::Malformed)?;
    if req.reason.len() > MAX_REDACTION_REASON_BYTES {
        return Err(RedactionError::ReasonTooLong(req.reason.len()));
    }
    if req.redacted_by != op.author {
        return Err(RedactionError::RedactorMismatch);
    }
    Ok(req)
}

/// SHA-256 over the canonical bytes of a [`SignedOp`] — the value
/// `RedactRequest::target_op_hash` carries. Shared rather than
/// re-implemented at each call site, keeping the audit log's verification
/// in lock-step with the minting path.
///
/// # Errors
///
/// Returns [`CanonicalError`] if canonicalising `op` fails.
pub fn hash_target_op(op: &SignedOp) -> Result<[u8; 32], CanonicalError> {
    let bytes = canonical_bytes(op)?;
    let digest = Sha256::digest(&bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::canonical_bytes;
    use crate::channel::ArtifactId;
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

    /// Op ids are deterministic, change with signature, seq or author, and match their byte view;
    /// the target-op hash is deterministic and changes with the payload.
    #[test]
    fn op_ids_and_target_hashes_identify_exactly_one_op() {
        // Phase: op_id_is_deterministic_for_the_same_op.
        {
            let op = dummy_op(7);
            let a = OpId::from_op(&op);
            let b = OpId::from_op(&op);
            assert_eq!(a, b);
        }

        // Phase: op_id_changes_on_signature_change.
        {
            // OpId is derived from signing-bytes (envelope minus signature), so
            // changing JUST the signature does not change the id — the audit
            // log's id stays stable through a post-hoc signature refresh.
            let mut op = dummy_op(7);
            let id_before = OpId::from_op(&op);
            op.signature = crate::crypto::Signature::from_bytes([0xff; 64]);
            let id_after = OpId::from_op(&op);
            assert_eq!(id_before, id_after);
        }

        // Phase: op_id_changes_on_seq_or_author_change.
        {
            let a = dummy_op(7);
            let mut b = a.clone();
            b.seq = 8;
            assert_ne!(OpId::from_op(&a), OpId::from_op(&b));

            let (_, other_pk) = generate_keypair();
            let mut c = a.clone();
            c.author = other_pk;
            assert_ne!(OpId::from_op(&a), OpId::from_op(&c));
        }

        // Phase: op_id_byte_view_matches_constructor.
        {
            let id = OpId::from_bytes([0x77; 16]);
            assert_eq!(id.as_bytes(), &[0x77; 16]);
        }

        // Phase: hash_target_op_is_deterministic_and_changes_with_payload.
        {
            let mut op = dummy_op(3);
            let h1 = hash_target_op(&op).expect("hash");
            let h2 = hash_target_op(&op).expect("hash");
            assert_eq!(h1, h2);
            // Mutating the payload (the redaction-rewrite case) produces a
            // different hash — `target_op_hash` is sensitive to mint-time bytes.
            op.payload = vec![0xff; 16];
            let h3 = hash_target_op(&op).expect("hash");
            assert_ne!(h1, h3);
        }
    }

    /// Redact payloads round-trip and reject a wrong op class, an over-long reason and a redactor
    /// mismatch; the redacted marker and redact request round-trip canonically.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn redaction_payloads_round_trip_and_reject_what_they_cannot_honour() {
        // Phase: parse_redact_payload_round_trip.
        {
            let (_, pk) = generate_keypair();
            let req = RedactRequest {
                artifact: ArtifactId::new(),
                target_op_id: OpId::from_bytes([0xab; 16]),
                target_op_hash: [0xcd; 32],
                reason: "GDPR request".into(),
                redacted_by: pk,
                redacted_at: SystemTime::UNIX_EPOCH,
            };
            let bytes = encode_redact_payload(&req).expect("encode");

            let op = SignedOp::new_unsigned(
                bytes,
                req.artifact,
                pk,
                AttestationId::new(),
                OpKind::Redaction,
                SystemTime::UNIX_EPOCH,
                42,
            );
            let parsed = parse_redact_payload(&op).expect("parse");
            assert_eq!(parsed, req);
        }

        // Phase: parse_redact_payload_rejects_wrong_op_kind.
        {
            let (_, pk) = generate_keypair();
            let req = RedactRequest {
                artifact: ArtifactId::new(),
                target_op_id: OpId::from_bytes([0; 16]),
                target_op_hash: [0; 32],
                reason: String::new(),
                redacted_by: pk,
                redacted_at: SystemTime::UNIX_EPOCH,
            };
            let bytes = encode_redact_payload(&req).expect("encode");
            let op = SignedOp::new_unsigned(
                bytes,
                req.artifact,
                pk,
                AttestationId::new(),
                OpKind::Change, // wrong on purpose
                SystemTime::UNIX_EPOCH,
                1,
            );
            match parse_redact_payload(&op) {
                Err(RedactionError::WrongOpKind { actual }) => {
                    assert_eq!(actual, OpKind::Change);
                }
                other => panic!("expected WrongOpKind, got {other:?}"),
            }
        }

        // Phase: parse_redact_payload_rejects_long_reason.
        {
            let (_, pk) = generate_keypair();
            let req = RedactRequest {
                artifact: ArtifactId::new(),
                target_op_id: OpId::from_bytes([0; 16]),
                target_op_hash: [0; 32],
                reason: "x".repeat(MAX_REDACTION_REASON_BYTES + 1),
                redacted_by: pk,
                redacted_at: SystemTime::UNIX_EPOCH,
            };
            let bytes = encode_redact_payload(&req).expect("encode");
            let op = SignedOp::new_unsigned(
                bytes,
                req.artifact,
                pk,
                AttestationId::new(),
                OpKind::Redaction,
                SystemTime::UNIX_EPOCH,
                1,
            );
            match parse_redact_payload(&op) {
                Err(RedactionError::ReasonTooLong(n)) => {
                    assert_eq!(n, MAX_REDACTION_REASON_BYTES + 1);
                }
                other => panic!("expected ReasonTooLong, got {other:?}"),
            }
        }

        // Phase: parse_redact_payload_rejects_redactor_mismatch.
        {
            // Envelope says "signed by Bob", payload claims "redacted by Alice" —
            // must fail; the validator relies on this to refuse a malicious
            // "redact in someone else's name" envelope.
            let (_, author_pk) = generate_keypair();
            let (_, claimed_pk) = generate_keypair();
            let req = RedactRequest {
                artifact: ArtifactId::new(),
                target_op_id: OpId::from_bytes([0; 16]),
                target_op_hash: [0; 32],
                reason: String::new(),
                redacted_by: claimed_pk, // not the envelope author
                redacted_at: SystemTime::UNIX_EPOCH,
            };
            let bytes = encode_redact_payload(&req).expect("encode");
            let op = SignedOp::new_unsigned(
                bytes,
                req.artifact,
                author_pk,
                AttestationId::new(),
                OpKind::Redaction,
                SystemTime::UNIX_EPOCH,
                1,
            );
            match parse_redact_payload(&op) {
                Err(RedactionError::RedactorMismatch) => {}
                other => panic!("expected RedactorMismatch, got {other:?}"),
            }
        }

        // Phase: redacted_marker_canonical_round_trip.
        {
            let marker = RedactedMarker::v1();
            let bytes = marker.canonical().expect("encode");
            let back: RedactedMarker = from_canonical_bytes(&bytes).expect("decode");
            assert_eq!(marker, back);
        }

        // Phase: redact_request_canonical_round_trip.
        // Belt-and-braces: canonical encoding of `RedactRequest` must
        // round-trip through postcard so the wire layer doesn't silently drop
        // fields when the struct grows.
        {
            let (_, pk) = generate_keypair();
            let req = RedactRequest {
                artifact: ArtifactId::new(),
                target_op_id: OpId::from_bytes([1; 16]),
                target_op_hash: [2; 32],
                reason: "compliance".into(),
                redacted_by: pk,
                redacted_at: SystemTime::UNIX_EPOCH + web_time::Duration::from_secs(123_456),
            };
            let bytes = canonical_bytes(&req).expect("encode");
            let back: RedactRequest = from_canonical_bytes(&bytes).expect("decode");
            assert_eq!(req, back);
        }
    }
}
