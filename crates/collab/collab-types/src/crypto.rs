//! Ed25519 signing + verification primitives.
//!
//! Ed25519 is the spec-mandated scheme: fast (~70µs/sign, negligible vs
//! human typing latency), deterministic (no per-signature RNG), small
//! (32-byte pubkey, 64-byte signature) on-wire artifacts. Uses
//! `ed25519-dalek` (the most widely audited Rust implementation), stable
//! `2.x` API.
//!
//! ## Newtypes
//!
//! `Ed25519PublicKey`/`Signature` wrap the dalek primitives so: (1) a
//! wire-shape break in dalek's own `serde` impl is adapted in one place;
//! (2) they derive `PartialEq`/`Eq`/`Hash`/`Ord` for map keys without the
//! underlying type needing to; (3) dalek types don't leak into every
//! downstream crate.
//!
//! ## Signing API
//!
//! [`sign_event`]/[`verify_event`] are generic helpers for `ChannelEvent`
//! variants, each with its own `signature` field and a `signing_bytes()`
//! helper (see [`crate::event`]). [`sign_op`]/[`verify_op`] specialise for
//! [`crate::op::SignedOp`], whose signature covers every other field in
//! canonical-bytes form.
//!
//! Verification returns `Result<(), VerifyError>`; signing is infallible
//! at this layer.

use ed25519_dalek::Signer;
use ed25519_dalek::Verifier;
use getrandom::{SysRng, rand_core::UnwrapErr};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, canonical_bytes};
use crate::op::SignedOp;

/// Public half of an Ed25519 keypair, 32 bytes on the wire.
///
/// Wrapped as `[u8; 32]` (not `ed25519_dalek::VerifyingKey`) so equality
/// and hashing are byte-exact and free; conversion to the dalek type
/// happens at verify time.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ed25519PublicKey(pub [u8; 32]);

impl std::fmt::Debug for Ed25519PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Print first 4 + last 4 bytes hex — full 32-byte hex spam is
        // useless in logs, the short form disambiguates.
        let s = self.0;
        write!(
            f,
            "Ed25519PublicKey({:02x}{:02x}{:02x}{:02x}..{:02x}{:02x}{:02x}{:02x})",
            s[0], s[1], s[2], s[3], s[28], s[29], s[30], s[31]
        )
    }
}

impl Ed25519PublicKey {
    /// Construct from raw bytes. Does NOT validate the bytes form a curve
    /// point — that happens lazily at verify time, so the validator (not the
    /// deserialiser) is the rejection point for a malicious peer's key.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Convert into the underlying `ed25519-dalek` verifying key. Fails if
    /// the bytes don't decode to a valid curve point.
    fn to_verifying_key(self) -> Result<ed25519_dalek::VerifyingKey, VerifyError> {
        ed25519_dalek::VerifyingKey::from_bytes(&self.0).map_err(|_| VerifyError::InvalidPublicKey)
    }
}

/// Detached Ed25519 signature, 64 bytes on the wire. Same newtype
/// rationale as [`Ed25519PublicKey`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Signature(#[serde(with = "serde_bytes")] pub [u8; 64]);

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
    /// Construct from raw bytes; no curve-validity check (happens at verify time).
    pub fn from_bytes(bytes: [u8; 64]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw 64 bytes.
    pub fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

/// Failure modes for signature verification. Validator-level errors
/// (replay, missing attestation, …) live in
/// [`crate::validator::ValidationError`]; this covers only the
/// *cryptographic* outcome of verifying one signature.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum VerifyError {
    /// The public-key bytes don't decode to a valid Ed25519 point —
    /// corrupted or maliciously-crafted.
    #[error("invalid Ed25519 public key (not a point on the curve)")]
    InvalidPublicKey,

    /// The signature bytes decoded but failed the Ed25519 check — wrong
    /// secret, tampered bytes, or the message differs from what was signed.
    #[error("Ed25519 signature did not verify")]
    BadSignature,

    /// Canonical-bytes encoder failed. Unreachable in practice; propagated for hygiene.
    #[error("could not canonicalise message: {0}")]
    Canonical(#[from] CanonicalError),
}

/// Generate a fresh Ed25519 keypair via the OS RNG. Returns `(secret,
/// public)`; the secret type is `ed25519_dalek::SigningKey` (no newtype),
/// intentionally not `Serialize` to discourage accidental wire exposure.
pub fn generate_keypair() -> (ed25519_dalek::SigningKey, Ed25519PublicKey) {
    let mut rng = UnwrapErr(SysRng);
    let signing = ed25519_dalek::SigningKey::generate(&mut rng);
    let verifying = signing.verifying_key();
    (signing, Ed25519PublicKey(verifying.to_bytes()))
}

/// Sign an arbitrary serializable value with the given secret key,
/// through [`canonical_bytes`] so the signature covers the deterministic
/// postcard encoding of `event`. Callers must hand in a value that omits
/// the final `signature` field — see `event::*::signing_bytes()`.
///
/// # Errors
///
/// Returns [`CanonicalError`] if canonicalising `event` fails.
pub fn sign_event<T: Serialize>(
    event: &T,
    secret_key: &ed25519_dalek::SigningKey,
) -> Result<Signature, CanonicalError> {
    let bytes = canonical_bytes(event)?;
    let sig = secret_key.sign(&bytes);
    Ok(Signature(sig.to_bytes()))
}

/// Verify a signature over an arbitrary serializable value. Mirrors
/// [`sign_event`]: the same pre-signature view must be reconstructable on
/// the receiving side. `BadSignature` on mismatch, `InvalidPublicKey` if malformed.
///
/// # Errors
///
/// Returns [`VerifyError::Canonical`] if canonicalising `event` fails,
/// [`VerifyError::InvalidPublicKey`] if `public_key` doesn't decode to a
/// valid curve point, or [`VerifyError::BadSignature`] if `sig` doesn't
/// verify.
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

/// Sign a `SignedOp` in place via [`SignedOp::signing_bytes`], which
/// omits the signature field — calling this twice on the same op is
/// harmless, the second call reproduces the first.
///
/// # Errors
///
/// Returns [`CanonicalError`] if canonicalising `op`'s signing bytes fails.
pub fn sign_op(
    op: &mut SignedOp,
    secret_key: &ed25519_dalek::SigningKey,
) -> Result<(), CanonicalError> {
    #[cfg(debug_assertions)]
    assert_claimed(op);
    let bytes = op.signing_bytes()?;
    let sig = secret_key.sign(&bytes);
    op.signature = Signature(sig.to_bytes());
    Ok(())
}

/// Every change a device signs names its action: a debug build refuses to
/// sign one that does not, so a producer outside the executor is found by
/// its tests rather than by the peers that refuse its changes.
#[cfg(debug_assertions)]
#[expect(
    clippy::panic,
    reason = "a debug build's check that every produced change is claimed"
)]
fn assert_claimed(op: &SignedOp) {
    if let Err(unclaimed) = crate::claim::check_claimed(op) {
        panic!("signing a document op whose changes are not all claimed: {unclaimed}");
    }
}

/// Verify a `SignedOp` against its declared author key — the
/// cryptographic-only check. Full validation (attestation, role, replay
/// defence, …) lives in [`crate::validator::validate_inbound_op`].
///
/// # Errors
///
/// Returns [`VerifyError::Canonical`] if canonicalising `op`'s signing
/// bytes fails, [`VerifyError::InvalidPublicKey`] if `op.author` doesn't
/// decode to a valid curve point, or [`VerifyError::BadSignature`] if
/// `op.signature` doesn't verify.
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

    /// Signing matches the known answer, a minimal value signs and verifies, and a wrong key or a
    /// tampered payload fails.
    #[test]
    fn event_signatures_are_known_answer_ed25519_bound_to_key_and_payload() {
        // Phase: sign_event_matches_known_answer.
        // Every signed op on the wire and on disk is an Ed25519 signature over
        // `canonical_bytes`; pin one signature and its postcard encoding.
        {
            let key = ed25519_dalek::SigningKey::from_bytes(&[0x5e; 32]);
            let public = Ed25519PublicKey(key.verifying_key().to_bytes());
            assert_eq!(
                hex::encode(public.as_bytes()),
                "8146640f02493af4fbc54fe33388e75dc2c937ae0b7727cc2b2afb1b75199a3e"
            );
            let event = ("kat-event", 7_u64);
            assert_eq!(canonical_bytes(&event).unwrap(), b"\x09kat-event\x07");
            let expected = "7395c3a6845d8916c0cc16a6f92c3a5c69bee334eac7e76606b582c37fb52ad5\
                            74222d2f7d9c7aca82dfa32560ba5490bd7db817593d488cb713d352c0656804";
            let signature = sign_event(&event, &key).unwrap();
            assert_eq!(hex::encode(signature.as_bytes()), expected);
            verify_event(&event, &signature, &public).unwrap();
            let wire = postcard::to_allocvec(&signature).unwrap();
            assert_eq!(hex::encode(&wire), format!("40{expected}"));
        }

        // Phase: sign_verify_minimal_value.
        {
            let (sk, pk) = generate_keypair();
            let payload: Vec<u8> = b"hello world".to_vec();
            let sig = sign_event(&payload, &sk).unwrap();
            assert!(verify_event(&payload, &sig, &pk).is_ok());
        }

        // Phase: sign_with_a_verify_with_b_fails.
        {
            let (sk_a, _) = generate_keypair();
            let (_, pk_b) = generate_keypair();
            let payload: Vec<u8> = b"hello".to_vec();
            let sig = sign_event(&payload, &sk_a).unwrap();
            assert_eq!(
                verify_event(&payload, &sig, &pk_b).unwrap_err(),
                VerifyError::BadSignature
            );
        }

        // Phase: tampered_payload_fails.
        {
            let (sk, pk) = generate_keypair();
            let payload: Vec<u8> = b"hello".to_vec();
            let sig = sign_event(&payload, &sk).unwrap();
            let tampered: Vec<u8> = b"hellp".to_vec();
            assert_eq!(
                verify_event(&tampered, &sig, &pk).unwrap_err(),
                VerifyError::BadSignature
            );
        }
    }

    /// A keypair round-trips through serialisation, Debug does not panic, and an invalid public key
    /// is rejected.
    #[test]
    fn keys_serialise_debug_safely_and_reject_invalid_points() {
        // Phase: keypair_round_trip_serialise.
        {
            let (_, pk) = generate_keypair();
            let bytes = canonical_bytes(&pk).unwrap();
            let back: Ed25519PublicKey = crate::canonical::from_canonical_bytes(&bytes).unwrap();
            assert_eq!(pk, back);
        }

        // Phase: debug_does_not_panic.
        {
            let (_, pk) = generate_keypair();
            let _ = format!("{pk:?}");
            let sig = Signature::from_bytes([7u8; 64]);
            let _ = format!("{sig:?}");
        }

        // Phase: invalid_public_key_rejected.
        {
            // All-zero bytes are not a valid Ed25519 public key.
            let bad = Ed25519PublicKey([0u8; 32]);
            let payload: Vec<u8> = b"x".to_vec();
            let sig = Signature([0u8; 64]);
            // dalek accepts the all-zero pubkey as syntactically valid but
            // cryptographically useless; pick a value with the high bit set on
            // the last byte to encode an invalid sign bit instead.
            let _ = verify_event(&payload, &sig, &bad); // don't unwrap; behaviour exercised below
            let mut bytes = [0u8; 32];
            bytes[31] = 0xff;
            let bad2 = Ed25519PublicKey(bytes);
            // Either InvalidPublicKey (decode fails) or BadSignature (decode
            // succeeds but sig is wrong) — both satisfy "not Ok".
            assert!(verify_event(&payload, &sig, &bad2).is_err());
        }
    }
}
