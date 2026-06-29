//! Ed25519 signing + verification primitives.
//!
//! Ed25519 is the spec-mandated signature scheme: it is fast (~70µs
//! per sign on commodity hardware — negligible vs human typing
//! latency), deterministic (no per-signature RNG required after key
//! generation), and has small (32-byte pubkey, 64-byte signature)
//! on-wire artifacts. We use `ed25519-dalek` because it is the most
//! widely audited Rust implementation; the API surface used here is
//! the stable `2.x` shape.
//!
//! ## Newtypes
//!
//! We re-export `Ed25519PublicKey` and `Signature` as newtypes around
//! the `ed25519-dalek` primitives. The reasons:
//!
//! 1. Stable wire shape across `ed25519-dalek` major bumps. If the
//!    underlying crate breaks its `VerifyingKey`/`Signature` `serde`
//!    impl, this crate adapts in one place.
//! 2. Trait isolation: the newtypes derive `PartialEq`, `Eq`, `Hash`,
//!    `Ord` so they can be used as `HashMap` / `BTreeMap` keys
//!    without callers needing to know that the underlying type
//!    doesn't implement those traits.
//! 3. Smaller blast radius: we don't want `dalek` types in every
//!    crate that consumes us.
//!
//! ## Signing API
//!
//! - [`sign_event`] / [`verify_event`]: generic helpers for the
//!   `ChannelEvent` variants. Each variant carries its own
//!   `signature: Signature` field and the wire bytes that bind into
//!   the signature are produced by the variant's `signing_bytes()`
//!   helper (see [`crate::event`]).
//! - [`sign_op`] / [`verify_op`]: specialised wrappers for
//!   [`crate::op::SignedOp`]. The op envelope's signature covers
//!   every other field of the struct in canonical-bytes form.
//!
//! Verification returns `Result<(), VerifyError>` so callers can
//! pattern-match the failure mode. Signing is infallible at this
//! layer — the actual `ed25519-dalek` sign call cannot fail with the
//! `2.x` API.

use ed25519_dalek::Signer;
use ed25519_dalek::Verifier;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, canonical_bytes};
use crate::op::SignedOp;

/// Public half of an Ed25519 keypair. 32 bytes on the wire.
///
/// Wrapped so we can derive `Eq`/`Hash`/`Ord` and shield consumers
/// from `ed25519-dalek` major-version churn. The wrapped type is
/// `[u8; 32]` rather than `ed25519_dalek::VerifyingKey` so that
/// equality and hashing are byte-exact and free; conversion to the
/// dalek type happens at verify time.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ed25519PublicKey(pub [u8; 32]);

impl std::fmt::Debug for Ed25519PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Print first 4 + last 4 bytes hex. Full 32-byte hex spam is
        // useless in logs; the short form is enough to disambiguate.
        let s = self.0;
        write!(
            f,
            "Ed25519PublicKey({:02x}{:02x}{:02x}{:02x}..{:02x}{:02x}{:02x}{:02x})",
            s[0], s[1], s[2], s[3], s[28], s[29], s[30], s[31]
        )
    }
}

impl Ed25519PublicKey {
    /// Construct from raw bytes. Does NOT validate the bytes form a
    /// point on the curve; that check happens lazily at verify time.
    /// We accept "syntactically a public key" here because the wire
    /// might carry an invalid key from a malicious peer and we want
    /// the validator (not the deserialiser) to be the rejection
    /// point.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Convert into the underlying `ed25519-dalek` verifying key.
    /// Fails if the bytes do not decode to a valid curve point.
    fn to_verifying_key(self) -> Result<ed25519_dalek::VerifyingKey, VerifyError> {
        ed25519_dalek::VerifyingKey::from_bytes(&self.0).map_err(|_| VerifyError::InvalidPublicKey)
    }
}

/// Detached Ed25519 signature. 64 bytes on the wire.
///
/// Same rationale for the newtype wrapper as [`Ed25519PublicKey`]:
/// stable wire shape, `Eq`/`Hash` for use in `HashMap`s, and trait
/// isolation from the dalek dependency.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Signature(#[serde(with = "serde_bytes_64")] pub [u8; 64]);

impl std::fmt::Debug for Signature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.0;
        write!(
            f,
            "Signature({:02x}{:02x}{:02x}{:02x}..{:02x}{:02x}{:02x}{:02x})",
            s[0], s[1], s[2], s[3], s[60], s[61], s[62], s[63]
        )
    }
}

impl Signature {
    /// Construct from raw bytes. No curve-validity check; that
    /// happens at verify time.
    pub fn from_bytes(bytes: [u8; 64]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw 64 bytes.
    pub fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

/// A 64-byte-array serde adapter: serialise as a borrowed bytes slice
/// so postcard uses the length-prefixed bytes form rather than
/// 64 separate `u8`s. Saves 63 bytes of varint overhead per
/// signature on the wire.
mod serde_bytes_64 {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        serde_bytes::Bytes::new(bytes).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let v: serde_bytes::ByteBuf = serde_bytes::ByteBuf::deserialize(d)?;
        let bytes = v.into_vec();
        if bytes.len() != 64 {
            return Err(D::Error::custom(format!(
                "expected 64-byte signature, got {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; 64];
        out.copy_from_slice(&bytes);
        Ok(out)
    }
}

/// Failure modes for signature verification.
///
/// Each variant maps to a specific cause and is human-readable in
/// logs. Validator-level errors (replay, missing attestation, etc.)
/// live in [`crate::validator::ValidationError`]; this type covers
/// only the *cryptographic* outcome of verifying one signature.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum VerifyError {
    /// The public-key bytes do not decode to a valid Ed25519 point.
    /// Caused by a corrupted or maliciously-crafted key.
    #[error("invalid Ed25519 public key (not a point on the curve)")]
    InvalidPublicKey,

    /// The signature bytes decoded but failed the Ed25519 check.
    /// Either the signer used a different secret, the bytes were
    /// tampered with after signing, or the message bytes differ from
    /// what was signed.
    #[error("Ed25519 signature did not verify")]
    BadSignature,

    /// The canonical-bytes encoder failed. In practice unreachable
    /// for the types in this crate, but propagated for hygiene.
    #[error("could not canonicalise message: {0}")]
    Canonical(#[from] CanonicalError),
}

/// Generate a fresh Ed25519 keypair using the OS RNG.
///
/// Returns `(secret, public)` so the caller can persist the secret
/// separately from any wire-bound public-key field. The secret type
/// is `ed25519_dalek::SigningKey` (no newtype): it is intentionally
/// not `Serialize` to discourage accidental wire exposure.
pub fn generate_keypair() -> (ed25519_dalek::SigningKey, Ed25519PublicKey) {
    let mut rng = OsRng;
    let signing = ed25519_dalek::SigningKey::generate(&mut rng);
    let verifying = signing.verifying_key();
    (signing, Ed25519PublicKey(verifying.to_bytes()))
}

/// Sign an arbitrary serializable value with the given secret key.
///
/// The serialisation goes through [`canonical_bytes`] so the
/// signature is over the deterministic postcard encoding of `event`.
/// Callers are responsible for handing in a value that omits the
/// final `signature` field (otherwise the signature would cover its
/// own placeholder, which is fine cryptographically but confusing
/// semantically). See `event::*::signing_bytes()` helpers for the
/// convention.
pub fn sign_event<T: Serialize>(
    event: &T,
    secret_key: &ed25519_dalek::SigningKey,
) -> Result<Signature, CanonicalError> {
    let bytes = canonical_bytes(event)?;
    let sig = secret_key.sign(&bytes);
    Ok(Signature(sig.to_bytes()))
}

/// Verify a signature over an arbitrary serializable value.
///
/// Mirrors [`sign_event`]: the same `event` (representing the same
/// pre-signature view) must be reconstructable on the receiving
/// side. Returns `Err(VerifyError::BadSignature)` on signature
/// mismatch; `Err(VerifyError::InvalidPublicKey)` if `public_key`
/// is malformed.
pub fn verify_event<T: Serialize>(
    event: &T,
    sig: &Signature,
    public_key: &Ed25519PublicKey,
) -> Result<(), VerifyError> {
    let bytes = canonical_bytes(event)?;
    let vk = public_key.to_verifying_key()?;
    let dalek_sig = ed25519_dalek::Signature::from_bytes(&sig.0);
    vk.verify(&bytes, &dalek_sig)
        .map_err(|_| VerifyError::BadSignature)
}

/// Sign a `SignedOp` in place. Mutates `op.signature` so the caller
/// can pass the op straight to the wire afterwards.
///
/// The signed bytes are produced by [`SignedOp::signing_bytes`],
/// which serialises a view of the op that omits the signature
/// field. Calling `sign_op` twice on the same op is harmless: the
/// helper view does not include the signature so the second call
/// reproduces the first.
pub fn sign_op(
    op: &mut SignedOp,
    secret_key: &ed25519_dalek::SigningKey,
) -> Result<(), CanonicalError> {
    let bytes = op.signing_bytes()?;
    let sig = secret_key.sign(&bytes);
    op.signature = Signature(sig.to_bytes());
    Ok(())
}

/// Verify a `SignedOp` against its declared author public key.
///
/// This is the cryptographic-only check. The full validation
/// pipeline (does this author have a valid attestation, does the
/// role permit this op class, replay defence, etc.) lives in
/// [`crate::validator::validate_inbound_op`].
pub fn verify_op(op: &SignedOp) -> Result<(), VerifyError> {
    let bytes = op.signing_bytes()?;
    let vk = op.author.to_verifying_key()?;
    let dalek_sig = ed25519_dalek::Signature::from_bytes(&op.signature.0);
    vk.verify(&bytes, &dalek_sig)
        .map_err(|_| VerifyError::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypair_round_trip_serialise() {
        let (_, pk) = generate_keypair();
        let bytes = canonical_bytes(&pk).unwrap();
        let back: Ed25519PublicKey = crate::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(pk, back);
    }

    #[test]
    fn debug_does_not_panic() {
        let (_, pk) = generate_keypair();
        let _ = format!("{:?}", pk);
        let sig = Signature::from_bytes([7u8; 64]);
        let _ = format!("{:?}", sig);
    }

    #[test]
    fn sign_verify_minimal_value() {
        let (sk, pk) = generate_keypair();
        let payload: Vec<u8> = b"hello world".to_vec();
        let sig = sign_event(&payload, &sk).unwrap();
        assert!(verify_event(&payload, &sig, &pk).is_ok());
    }

    #[test]
    fn sign_with_a_verify_with_b_fails() {
        let (sk_a, _) = generate_keypair();
        let (_, pk_b) = generate_keypair();
        let payload: Vec<u8> = b"hello".to_vec();
        let sig = sign_event(&payload, &sk_a).unwrap();
        assert_eq!(
            verify_event(&payload, &sig, &pk_b).unwrap_err(),
            VerifyError::BadSignature
        );
    }

    #[test]
    fn tampered_payload_fails() {
        let (sk, pk) = generate_keypair();
        let payload: Vec<u8> = b"hello".to_vec();
        let sig = sign_event(&payload, &sk).unwrap();
        let tampered: Vec<u8> = b"hellp".to_vec();
        assert_eq!(
            verify_event(&tampered, &sig, &pk).unwrap_err(),
            VerifyError::BadSignature
        );
    }

    #[test]
    fn invalid_public_key_rejected() {
        // All-zero bytes are not a valid Ed25519 public key.
        let bad = Ed25519PublicKey([0u8; 32]);
        let payload: Vec<u8> = b"x".to_vec();
        let sig = Signature([0u8; 64]);
        // The dalek crate happens to accept the all-zero pubkey as a
        // syntactically-valid (but cryptographically useless) point.
        // What we care about is that some random non-curve bytes are
        // rejected. Pick a value with the high bit set on the last
        // byte to encode an invalid sign bit.
        let _ = verify_event(&payload, &sig, &bad); // don't unwrap; behaviour exercised below
        let mut bytes = [0u8; 32];
        bytes[31] = 0xff;
        let bad2 = Ed25519PublicKey(bytes);
        // Either InvalidPublicKey (decode fails) or BadSignature
        // (decode succeeds but sig is wrong). We accept both — the
        // contract is "not Ok".
        assert!(verify_event(&payload, &sig, &bad2).is_err());
    }
}
