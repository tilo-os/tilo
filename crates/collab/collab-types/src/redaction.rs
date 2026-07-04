//! Phase 9: cooperative cryptographic redaction.
//!
//! A `RedactRequest` is the payload carried by a [`crate::op::SignedOp`]
//! whose `op_class` is [`crate::role::OpClass::Redact`]. The channel
//! owner (and only the channel owner — see the role table in
//! [`crate::role::role_permits`]) mints one to declare "this earlier op
//! is now compliance-required to be removed; peers, please honour it."
//!
//! Redaction is **cooperative**, not adversarial: an honest peer
//! redacts in-memory state, rewrites its op-log copy of the targeted
//! op to a fixed marker, and records the request in its audit log; a
//! rogue peer who refuses cannot be forced. See the companion design
//! doc `docs/superpowers/specs/2026-06-30-cryptographic-redaction-design.md`
//! for the full threat model and rationale.
//!
//! ## Op-id derivation
//!
//! [`OpId`] is a stable 16-byte identifier derived from a
//! [`SignedOp`]'s (author, seq, canonical-bytes-prefix) so peers can
//! address an op without the original envelope carrying an explicit id
//! field — important for forward-compat with Phase 3+6 ops minted
//! before the Redact op-class existed.
//!
//! ## Wire stability
//!
//! Like every other wire type in this crate the struct uses
//! `#[serde(deny_unknown_fields)]` so a forgotten field on an old peer
//! is a hard decode error rather than a silently-dropped value.
//! Postcard is the canonical encoder
//! ([`crate::canonical::canonical_bytes`]).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::SystemTime;
use thiserror::Error;

use crate::canonical::{CanonicalError, canonical_bytes, from_canonical_bytes};
use crate::channel::ArtifactId;
use crate::crypto::Ed25519PublicKey;
use crate::op::SignedOp;
use crate::role::OpClass;

/// Hard upper bound on the `reason` field of a [`RedactRequest`].
///
/// Long enough for a typed-out compliance-system reference plus a
/// sentence of context ("GDPR request from data subject Alice
/// <alice@acme.example> — case ACME-2026-04-17, see compliance/case-1234
/// for the full record"). Larger justifications belong in the
/// compliance system; the audit log only needs enough to be navigable.
pub const MAX_REDACTION_REASON_BYTES: usize = 512;

/// Stable opaque identifier for one signed op.
///
/// The bytes are a SHA-256-derived prefix of `(author pubkey, seq, op
/// canonical-bytes excluding the signature)`. Two peers running the
/// same code derive the same id for the same op envelope.
///
/// Used by:
/// - `RedactRequest::target_op_id` — the address of the op being
///   redacted.
/// - `ChannelState::redacted_ops` — the set of redacted-op addresses
///   the validator filters subsequent applies against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpId(pub [u8; 16]);

impl OpId {
    /// Construct directly from a byte array. Mostly for tests and
    /// audit-log readers that ingest an id off the wire.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Derive an `OpId` from a [`SignedOp`].
    ///
    /// The derivation hashes:
    ///   - the author's public key,
    ///   - the seq counter,
    ///   - the op's signing-bytes (i.e. the canonical encoding of every
    ///     field *except* the signature).
    ///
    /// Hashing signing-bytes (rather than full canonical bytes) means
    /// two peers can derive the same id for an op whose signature is
    /// not present or is the placeholder — useful when the broker has
    /// rewritten a redacted op's payload to a marker (the signature no
    /// longer verifies, but the id derived from the pre-redaction bytes
    /// is what we want to match the audit-log record).
    pub fn from_op(op: &SignedOp) -> Self {
        Self::from_signing_bytes(op.author, op.seq, &op.signing_bytes().unwrap_or_default())
    }

    /// Variant of [`OpId::from_op`] used by code paths that already
    /// have the signing-bytes in hand (typically the audit log, which
    /// snapshots them at apply-time). Cuts one canonical-encode pass.
    pub fn from_signing_bytes(
        author: Ed25519PublicKey,
        seq: u64,
        signing_bytes: &[u8],
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"qos-redact-op-id-v1\0");
        hasher.update(author.as_bytes());
        hasher.update(seq.to_le_bytes());
        hasher.update(signing_bytes);
        let digest = hasher.finalize();
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        Self(bytes)
    }

    /// Borrow the raw 16-byte payload — handy for audit-log readers
    /// that want to render a short hex preview.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// A signed redaction request payload.
///
/// Carried as the payload of a [`SignedOp`] whose `op_class` is
/// [`OpClass::Redact`]. The targeting fields (`target_op_id` and
/// `target_op_hash`) are derived from the op being redacted at the
/// moment the owner mints the request; the audit log uses them to
/// cross-reference the original envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactRequest {
    /// The artifact the redacted op lived on. Same id as the enclosing
    /// `SignedOp::artifact` — recipients can short-circuit "which log
    /// to scan" without parsing the payload, but the field stays
    /// inside so the request is self-describing for audit replays
    /// across artifact-id renames (not a current feature; defensively
    /// recorded).
    pub artifact: ArtifactId,

    /// Stable id of the op being redacted. Derived via
    /// [`OpId::from_op`] from the target op's envelope.
    pub target_op_id: OpId,

    /// SHA-256 of the **target op's canonical bytes** (the full
    /// envelope, signature included). Lets a peer verify "this
    /// redaction targets the op I have a copy of" without re-deriving
    /// the OpId from scratch — and, for an MLS-encrypted op, without
    /// ever decrypting (we hash the ciphertext, not the cleartext;
    /// see §5 of the design doc).
    pub target_op_hash: [u8; 32],

    /// Free-text justification, capped at [`MAX_REDACTION_REASON_BYTES`].
    /// Recorded verbatim in the audit log + rendered in the chrome's
    /// redaction-tooltip; longer rationale belongs in the org's
    /// compliance system, not on the wire.
    pub reason: String,

    /// Public key of the redactor. Must equal the enclosing envelope's
    /// `SignedOp::author` — validated by the redaction-aware step of
    /// the validator. The duplication is deliberate: the validator
    /// can refuse a Redact whose payload claims a different identity
    /// without trusting the enclosing envelope's fields.
    pub redacted_by: Ed25519PublicKey,

    /// Wall-clock time the redaction was issued. Independent of the
    /// envelope's `timestamp` for audit-log clarity (the redactor's
    /// own clock at minting-time).
    pub redacted_at: SystemTime,
}

/// Fixed sentinel payload that a peer's op-log carries in place of a
/// redacted op's original payload. Round-trips through postcard so
/// downstream readers can decode it instead of seeing opaque bytes.
///
/// The marker is deliberately tiny: a single `kind` discriminator. The
/// audit log holds the structured `Redaction` record (with reason,
/// redactor, original envelope) — the marker on the log entry only
/// needs to say "this seq is redacted, do not render."
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactedMarker {
    /// Version tag for future format evolution. v1 is the only value;
    /// a v2 might add a "soft" vs "hard" distinction.
    pub kind: u8,
}

impl RedactedMarker {
    /// Construct the canonical v1 marker.
    pub const fn v1() -> Self {
        Self { kind: 1 }
    }

    /// Encode to canonical bytes — the form an op's payload field
    /// carries after redaction.
    pub fn canonical(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(self)
    }
}

/// Errors produced by the redaction pipeline.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RedactionError {
    /// The op claimed `op_class == Redact` but the payload didn't
    /// decode as a `RedactRequest`. Kept distinct from `WrongOpClass`
    /// so audit logs can distinguish "valid postcard, wrong type" from
    /// "garbage bytes".
    #[error("malformed redaction payload: {0}")]
    Malformed(#[source] CanonicalError),

    /// The op claimed `op_class != Redact` but a caller tried to
    /// parse its payload as a `RedactRequest`. The validator catches
    /// this earlier (via role-table check + class match), so this
    /// variant should only ever surface from the lower-level parser.
    #[error("redaction op had the wrong op_class: {actual:?}")]
    WrongOpClass {
        /// The op_class the envelope actually carried.
        actual: OpClass,
    },

    /// The `reason` field exceeded `MAX_REDACTION_REASON_BYTES`. We
    /// reject at parse time rather than truncate so a peer can't sneak
    /// a 100KB justification past the bound.
    #[error("redaction reason exceeds {MAX_REDACTION_REASON_BYTES} byte cap ({0} bytes)")]
    ReasonTooLong(usize),

    /// The payload's `redacted_by` field disagrees with the enclosing
    /// envelope's `author`. The validator surfaces this so a peer
    /// cannot mint an envelope signed by Bob whose payload claims
    /// Alice did the redacting.
    #[error("redactor pubkey in payload does not match envelope author")]
    RedactorMismatch,
}

/// Encode a `RedactRequest` to canonical bytes — the form an op's
/// payload field should carry when `op_class == Redact`.
pub fn encode_redact_payload(req: &RedactRequest) -> Result<Vec<u8>, CanonicalError> {
    canonical_bytes(req)
}

/// Decode a [`SignedOp::payload`] as a [`RedactRequest`].
///
/// Verifies:
/// - `op.op_class == Redact` (or returns [`RedactionError::WrongOpClass`]);
/// - the payload bytes decode as a `RedactRequest`;
/// - `reason.len() <= MAX_REDACTION_REASON_BYTES`;
/// - `payload.redacted_by == op.author` (catches the envelope/payload
///   identity mismatch before the validator sees it).
///
/// The returned struct is owned, so callers can drop the original
/// envelope and keep the request around.
pub fn parse_redact_payload(op: &SignedOp) -> Result<RedactRequest, RedactionError> {
    if op.op_class != OpClass::Redact {
        return Err(RedactionError::WrongOpClass { actual: op.op_class });
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

/// Compute the SHA-256 over the canonical bytes of a [`SignedOp`].
///
/// This is the value a `RedactRequest::target_op_hash` carries. Sharing
/// the function (rather than re-implementing it at each call site)
/// keeps the audit-log's verification path in lock-step with the
/// owner's minting path.
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
    fn op_id_is_deterministic_for_the_same_op() {
        let op = dummy_op(7);
        let a = OpId::from_op(&op);
        let b = OpId::from_op(&op);
        assert_eq!(a, b);
    }

    #[test]
    fn op_id_changes_on_signature_change() {
        // OpId is derived from signing-bytes (envelope minus signature),
        // so changing JUST the signature does NOT change the id. This
        // is desirable: the audit log's id stays stable through a
        // post-hoc signature refresh (e.g. after a redaction rewrites
        // the payload to a marker — the original id is still
        // recoverable via the signing-bytes the audit log captured).
        let mut op = dummy_op(7);
        let id_before = OpId::from_op(&op);
        op.signature = crate::crypto::Signature::from_bytes([0xff; 64]);
        let id_after = OpId::from_op(&op);
        assert_eq!(id_before, id_after);
    }

    #[test]
    fn op_id_changes_on_seq_or_author_change() {
        let a = dummy_op(7);
        let mut b = a.clone();
        b.seq = 8;
        assert_ne!(OpId::from_op(&a), OpId::from_op(&b));

        let (_, other_pk) = generate_keypair();
        let mut c = a.clone();
        c.author = other_pk;
        assert_ne!(OpId::from_op(&a), OpId::from_op(&c));
    }

    #[test]
    fn parse_redact_payload_round_trip() {
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
            OpClass::Redact,
            SystemTime::UNIX_EPOCH,
            42,
        );
        let parsed = parse_redact_payload(&op).expect("parse");
        assert_eq!(parsed, req);
    }

    #[test]
    fn parse_redact_payload_rejects_wrong_op_class() {
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
            OpClass::Splice, // wrong on purpose
            SystemTime::UNIX_EPOCH,
            1,
        );
        match parse_redact_payload(&op) {
            Err(RedactionError::WrongOpClass { actual }) => {
                assert_eq!(actual, OpClass::Splice)
            }
            other => panic!("expected WrongOpClass, got {other:?}"),
        }
    }

    #[test]
    fn parse_redact_payload_rejects_long_reason() {
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
            OpClass::Redact,
            SystemTime::UNIX_EPOCH,
            1,
        );
        match parse_redact_payload(&op) {
            Err(RedactionError::ReasonTooLong(n)) => {
                assert_eq!(n, MAX_REDACTION_REASON_BYTES + 1)
            }
            other => panic!("expected ReasonTooLong, got {other:?}"),
        }
    }

    #[test]
    fn parse_redact_payload_rejects_redactor_mismatch() {
        // Envelope says "signed by Bob", payload claims "redacted by
        // Alice" — must fail. The validator relies on this check to
        // refuse a malicious "redact in someone else's name" envelope.
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
            OpClass::Redact,
            SystemTime::UNIX_EPOCH,
            1,
        );
        match parse_redact_payload(&op) {
            Err(RedactionError::RedactorMismatch) => {}
            other => panic!("expected RedactorMismatch, got {other:?}"),
        }
    }

    #[test]
    fn redacted_marker_canonical_round_trip() {
        let marker = RedactedMarker::v1();
        let bytes = marker.canonical().expect("encode");
        let back: RedactedMarker = from_canonical_bytes(&bytes).expect("decode");
        assert_eq!(marker, back);
    }

    #[test]
    fn hash_target_op_is_deterministic_and_changes_with_payload() {
        let mut op = dummy_op(3);
        let h1 = hash_target_op(&op).expect("hash");
        let h2 = hash_target_op(&op).expect("hash");
        assert_eq!(h1, h2);
        // Mutating the payload — the redaction-rewrite case — produces
        // a different hash, so the audit log's `target_op_hash` is
        // sensitive to the bytes it captured at mint time.
        op.payload = vec![0xff; 16];
        let h3 = hash_target_op(&op).expect("hash");
        assert_ne!(h1, h3);
    }

    #[test]
    fn op_id_byte_view_matches_constructor() {
        let id = OpId::from_bytes([0x77; 16]);
        assert_eq!(id.as_bytes(), &[0x77; 16]);
    }

    // Belt-and-braces: canonical encoding of `RedactRequest` itself
    // must round-trip through postcard, so the wire layer doesn't
    // silently drop fields when the struct grows.
    #[test]
    fn redact_request_canonical_round_trip() {
        let (_, pk) = generate_keypair();
        let req = RedactRequest {
            artifact: ArtifactId::new(),
            target_op_id: OpId::from_bytes([1; 16]),
            target_op_hash: [2; 32],
            reason: "compliance".into(),
            redacted_by: pk,
            redacted_at: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(123_456),
        };
        let bytes = canonical_bytes(&req).expect("encode");
        let back: RedactRequest = from_canonical_bytes(&bytes).expect("decode");
        assert_eq!(req, back);
    }
}
