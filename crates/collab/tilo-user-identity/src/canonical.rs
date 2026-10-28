//! Canonical byte-encoding of everything that gets signed.
//!
//! All signed types in this crate — `DeviceCert`, `RevocationEntry`,
//! `EnrollmentToken`, and every workspace-directory delta — encode
//! their signing preimage through the same three-step pipeline:
//!
//! 1. Build a view of the struct that omits its `signature` field.
//! 2. Encode the view with postcard directly into the hasher.
//! 3. Prepend a fixed byte-constant "domain tag" (`tilo-devcert-v1\0`
//!    etc.) and hash the concatenation with BLAKE3-256.
//!
//! The Ed25519 signature is then produced over the 32-byte hash, not
//! the raw bytes. Two reasons the design pins this arrangement:
//!
//! - **Domain separation**: the tag stops a signature made under
//!   one tag from being replayed under another (a `DeviceCert`
//!   signature can never be reinterpreted as a `RevocationEntry`
//!   signature, no matter how similar the byte layout).
//! - **Wire-ABI stability**: adding a field forces a new tag
//!   (`tilo-devcert-v2\0`); we never accidentally accept an old
//!   signature over a new struct.
//!
//! This module holds the postcard helpers + the BLAKE3 wrapper.
//! The domain-tag constants themselves live next to the types they
//! sign (`cert::DEVCERT_PREIMAGE_TAG`, etc.) so a reader looking at
//! a type sees its tag in the same file.

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

/// Errors from canonical encoding/decoding of signable types.
///
/// Mirrors `collab-types::CanonicalError` so error handling is
/// familiar to anyone who has worked on the collab stack. Wraps
/// `postcard::Error` behind a `String` so callers do not need to
/// depend on postcard directly.
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum CanonicalError {
    /// The value could not be encoded. Unreachable for the types
    /// this crate defines but propagated for hygiene.
    #[error("postcard serialisation failed: {0}")]
    Serialize(String),

    /// The bytes could not be decoded. Common causes: truncated
    /// input, extra trailing bytes (postcard is exact-length), enum
    /// discriminant out of range, or a `deny_unknown_fields` violation.
    #[error("postcard deserialisation failed: {0}")]
    Deserialize(String),
}

/// Serialise a value to its canonical byte form.
///
/// This uses the same postcard encoding as [`signing_digest`]. Two
/// peers running the same crate version will produce identical bytes
/// for the same input.
///
/// # Errors
///
/// Returns [`CanonicalError::Serialize`] if `value` cannot be
/// postcard-encoded (effectively unreachable for the types this
/// crate defines).
pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CanonicalError> {
    postcard::to_allocvec(value).map_err(|e| CanonicalError::Serialize(e.to_string()))
}

/// Decode a canonical-encoded value. Rejects trailing bytes.
///
/// `postcard::from_bytes` in 1.1.x is *lenient* — it happily decodes
/// a value out of an over-long slice and drops the leftover. That
/// leniency would let a peer append arbitrary junk to a signed
/// blob without invalidating the signature (which was computed
/// over the exact-length preimage). To close that hole we use
/// `take_from_bytes` and reject any non-empty remainder.
///
/// # Errors
///
/// Returns [`CanonicalError::Deserialize`] if `bytes` does not decode
/// as a `T`, or if any bytes remain after decoding one value.
pub fn from_canonical_bytes<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CanonicalError> {
    let (value, rest) =
        postcard::take_from_bytes(bytes).map_err(|e| CanonicalError::Deserialize(e.to_string()))?;
    if !rest.is_empty() {
        return Err(CanonicalError::Deserialize(format!(
            "{} trailing bytes after value",
            rest.len()
        )));
    }
    Ok(value)
}

/// Compute the 32-byte signing preimage hash for `view`, prefixed
/// by `domain_tag`. The Ed25519 signature is then produced over
/// (or verified against) this 32-byte digest.
///
/// The BLAKE3-256 output size is 32 bytes — a small, fixed-length
/// hash that Ed25519's `sign()` and `verify()` accept as message
/// bytes directly. Prefixing the raw postcard bytes with `domain_tag`
/// gives us domain separation: a signature made under
/// `tilo-devcert-v1\0` cannot be reinterpreted as a signature made
/// under any other tag.
///
/// # Errors
///
/// Returns [`CanonicalError::Serialize`] if `view` cannot be
/// postcard-encoded (effectively unreachable for the types this
/// crate defines).
pub fn signing_digest<T: Serialize>(
    domain_tag: &[u8],
    view: &T,
) -> Result<[u8; 32], CanonicalError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain_tag);
    postcard::serialize_with_flavor(view, SigningHash(hasher))
        .map_err(|e| CanonicalError::Serialize(e.to_string()))
}

/// Consume the canonical encoding without retaining a temporary preimage.
struct SigningHash(blake3::Hasher);

impl postcard::ser_flavors::Flavor for SigningHash {
    type Output = [u8; 32];

    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.0.update(&[byte]);
        Ok(())
    }

    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.0.update(bytes);
        Ok(())
    }

    fn finalize(self) -> postcard::Result<Self::Output> {
        Ok(*self.0.finalize().as_bytes())
    }
}

#[cfg(test)]
mod streaming_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Fixture {
        a: u32,
        b: String,
    }

    #[test]
    fn canonical_bytes_round_trip_exactly_and_digests_are_domain_separated() {
        // Phase: round_trip_via_canonical_bytes.
        {
            let f = Fixture {
                a: 42,
                b: "hi".into(),
            };
            let bytes = canonical_bytes(&f).unwrap();
            let back: Fixture = from_canonical_bytes(&bytes).unwrap();
            assert_eq!(f, back);
        }

        // Phase: rejects_trailing_bytes.
        {
            let f = Fixture {
                a: 1,
                b: "x".into(),
            };
            let mut bytes = canonical_bytes(&f).unwrap();
            bytes.push(0xff);
            assert!(from_canonical_bytes::<Fixture>(&bytes).is_err());
        }

        // Phase: domain_tag_changes_digest.
        // Different domain tags produce different digests even for
        // the same body — pinning the domain-separation property.
        {
            let f = Fixture {
                a: 1,
                b: "same".into(),
            };
            let d1 = signing_digest(b"tag-a\0", &f).unwrap();
            let d2 = signing_digest(b"tag-b\0", &f).unwrap();
            assert_ne!(d1, d2);
        }

        // Phase: digest_is_deterministic.
        // Digest is deterministic across independent runs.
        {
            let f = Fixture {
                a: 1,
                b: "same".into(),
            };
            assert_eq!(
                signing_digest(b"tag-a\0", &f).unwrap(),
                signing_digest(b"tag-a\0", &f).unwrap(),
            );
        }
    }
}
