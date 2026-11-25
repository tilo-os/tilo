//! Deterministic byte serialisation for everything that gets signed.
//!
//! Signatures verify byte-for-byte, so two peers must agree exactly.
//! `postcard` is the canonical format: little-endian, varint-encoded, no
//! padding or field-reorder leeway (byte sequence is a deterministic
//! function of struct layout + values); supports `no_std`; round-trips
//! serde types with no custom derive bookkeeping.
//!
//! Every wire type derives `Serialize + Deserialize` with
//! `#[serde(deny_unknown_fields)]`, so inbound trailing garbage or an
//! extra field is rejected at decode time, not silently absorbed.
//!
//! When a struct has a `signature` field, the signed bytes are produced
//! by serialising a *helper view* that omits the signature — each signed
//! type provides a `signing_bytes()` method that builds the view and
//! feeds it to [`canonical_bytes`].

//!
//! ## Signing digests
//!
//! All signed types in this crate — `DeviceCert`, `RevocationEntry`,
//! and every workspace-directory delta — encode
//! their signing preimage through the same three-step pipeline:
//!
//! 1. Build a view of the struct that omits its `signature` field.
//! 2. Encode the view with postcard directly into the hasher.
//! 3. Prepend a fixed byte-constant "domain tag" (`tilo-devcert-v1\0`
//!    etc.) and hash the concatenation with BLAKE3-256.
//!
//! The Ed25519 signature is then produced over the 32-byte hash, not
//! the raw bytes. Two reasons the design pins this ordering:
//!
//! - **Domain separation**: the tag stops a signature made under
//!   one tag from being replayed under another (a `DeviceCert`
//!   signature can never be reinterpreted as a `RevocationEntry`
//!   signature, no matter how similar the byte layout).
//! - **Wire-ABI stability**: the encoded view is what is signed, so a
//!   signature over one layout never verifies over another.
//!
//! The domain-tag constants live next to the types they sign, so a
//! reader looking at a type sees its tag in the same file.

use serde::Serialize;
use thiserror::Error;

/// Errors during canonical (de)serialisation. Wraps `postcard::Error` so
/// callers needn't import postcard directly; downstream code typically
/// converts this into a per-domain error type.
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum CanonicalError {
    /// The value could not be encoded — essentially unreachable for this
    /// crate's types, but exposed so callers don't have to assume.
    #[error("postcard serialisation failed: {0}")]
    Serialize(String),

    /// The bytes could not be decoded: truncated input, extra trailing bytes
    /// (postcard is exact-length), an out-of-range enum discriminant, or a
    /// `deny_unknown_fields` violation.
    #[error("postcard deserialisation failed: {0}")]
    Deserialize(String),
}

/// Serialise a value to its canonical byte form — the single function
/// every signature in the crate flows through. Two peers on the same
/// Rust toolchain and crate version produce identical bytes for the same
/// input (postcard guarantees stability across Rust versions for a fixed
/// type; the wire risk is a field reorder in this crate).
///
/// Returns `Err(CanonicalError::Serialize)` only if the value's
/// `Serialize` impl errors — effectively infallible here, but not
/// unwrapped so callers can propagate a future fallible impl.
///
/// # Errors
///
/// Returns [`CanonicalError::Serialize`] if `value`'s `Serialize` impl fails.
pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CanonicalError> {
    postcard::to_allocvec(value).map_err(|e| CanonicalError::Serialize(e.to_string()))
}

/// Decode bytes from [`canonical_bytes`] back into a value, requiring
/// exact-length input. Bound by `for<'de> Deserialize<'de>` so owned
/// types are the common case; errors on any decode failure including
/// trailing bytes after a successful struct decode.
///
/// The exact-length check is critical for signature integrity: a signed
/// envelope is "the bytes I sign over" — accepting trailing bytes would
/// let unsigned-but-present data slip past the validator unnoticed.
///
/// # Errors
///
/// Returns [`CanonicalError::Deserialize`] if `bytes` fails to decode as
/// `T`, or if any bytes remain after a successful decode.
pub fn from_canonical_bytes<T>(bytes: &[u8]) -> Result<T, CanonicalError>
where
    T: for<'de> serde::Deserialize<'de>,
{
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
