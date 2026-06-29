//! Deterministic byte serialisation for everything that gets signed.
//!
//! Signatures verify byte-for-byte, so two peers must agree on the
//! exact bytes that hash into a signature. The spec specifies
//! `postcard` as the canonical format because:
//!
//! - postcard is little-endian and varint-encoded, with no padding or
//!   field-reorder leeway — the byte sequence is a deterministic
//!   function of the struct layout and the values.
//! - it supports `no_std` (we may want to verify on embedded devices
//!   eventually).
//! - it round-trips serde types without any custom derive bookkeeping.
//!
//! Every wire type in this crate derives `Serialize + Deserialize`
//! with `#[serde(deny_unknown_fields)]` so that any inbound trailing
//! garbage or extra field gets rejected at decode time instead of
//! silently absorbed.
//!
//! ## Signing protocol
//!
//! When a struct contains a `signature` field, the signed bytes are
//! produced by serialising a *helper view* of the struct that omits
//! the signature field itself. The convention used throughout this
//! crate is: each signed type provides a `signing_bytes()` method
//! that builds the helper and feeds it to [`canonical_bytes`]. This
//! means the same canonical encoder is used everywhere; only the
//! "what fields are covered by the signature" decision lives in the
//! per-type helper.

use serde::Serialize;
use thiserror::Error;

/// Errors that can occur during canonical (de)serialisation.
///
/// Wraps `postcard::Error` so callers don't need to import postcard
/// themselves. Downstream code typically converts this into a
/// per-domain error type (`VerifyError`, `ValidationError`, etc.).
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum CanonicalError {
    /// The value could not be encoded. This is essentially
    /// unreachable for our types (all `Vec`-shaped fields are
    /// length-prefixed by postcard) but exposed so callers don't have
    /// to assume.
    #[error("postcard serialisation failed: {0}")]
    Serialize(String),

    /// The bytes could not be decoded. Common causes:
    /// - truncated input;
    /// - extra trailing bytes (postcard is exact-length);
    /// - an enum discriminant out of range;
    /// - a `deny_unknown_fields` violation (extra struct field).
    #[error("postcard deserialisation failed: {0}")]
    Deserialize(String),
}

/// Serialise a value to its canonical byte form.
///
/// This is the single function every signature in the crate flows
/// through. Two peers running the same Rust toolchain version *and*
/// the same crate version will produce identical bytes for the same
/// input. (Across Rust versions, postcard guarantees stability for a
/// fixed type definition; the wire risk is a struct field reorder in
/// this crate, not a compiler update.)
///
/// Returns `Err(CanonicalError::Serialize)` only if the value's
/// `Serialize` impl returns an error — for all types in this crate
/// the result is effectively infallible, but we don't unwrap so that
/// callers can propagate the error if they later add a fallible
/// serde impl.
pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CanonicalError> {
    postcard::to_allocvec(value).map_err(|e| CanonicalError::Serialize(e.to_string()))
}

/// Decode bytes produced by [`canonical_bytes`] back into a value,
/// requiring exact-length input.
///
/// Bound by `for<'de> Deserialize<'de>` rather than a lifetime
/// parameter so that owned types are the common case. Returns
/// `CanonicalError::Deserialize` on any decode failure, including
/// trailing-bytes leftover after a successful struct decode.
///
/// The exact-length check is critical for signature integrity: a
/// signed envelope is "the bytes I sign over". If the receiver
/// accepts an envelope plus N trailing bytes, the trailing bytes
/// are unsigned-but-present data the validator never inspects.
/// `postcard::take_from_bytes` returns both the decoded value and
/// the remaining slice; we treat any non-empty remainder as an
/// error.
pub fn from_canonical_bytes<T>(bytes: &[u8]) -> Result<T, CanonicalError>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let (value, rest) = postcard::take_from_bytes(bytes)
        .map_err(|e| CanonicalError::Deserialize(e.to_string()))?;
    if !rest.is_empty() {
        return Err(CanonicalError::Deserialize(format!(
            "{} trailing bytes after value",
            rest.len()
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Sample {
        a: u32,
        b: String,
        c: Vec<u8>,
    }

    #[test]
    fn round_trip_simple() {
        let s = Sample {
            a: 42,
            b: "hello".into(),
            c: vec![1, 2, 3],
        };
        let bytes = canonical_bytes(&s).unwrap();
        let back: Sample = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn truncated_bytes_fail() {
        let s = Sample {
            a: 1,
            b: "x".into(),
            c: vec![],
        };
        let bytes = canonical_bytes(&s).unwrap();
        let truncated = &bytes[..bytes.len().saturating_sub(1)];
        assert!(from_canonical_bytes::<Sample>(truncated).is_err());
    }

    #[test]
    fn determinism_same_value_same_bytes() {
        // The whole point of canonical-bytes: two encoders must agree.
        let s = Sample {
            a: 7,
            b: "abc".into(),
            c: vec![9, 9, 9],
        };
        let a = canonical_bytes(&s).unwrap();
        let b = canonical_bytes(&s).unwrap();
        assert_eq!(a, b);
    }
}
