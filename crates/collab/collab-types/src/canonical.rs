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

    /// Canonical bytes round-trip, are deterministic, and a truncated
    /// encoding fails to decode.
    #[test]
    fn canonical_bytes_round_trip_deterministically_and_reject_truncation() {
        // Case: simple round trip.
        let s = Sample {
            a: 42,
            b: "hello".into(),
            c: vec![1, 2, 3],
        };
        let bytes = canonical_bytes(&s).unwrap();
        let back: Sample = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(s, back);

        // Case: truncated bytes fail.
        let s = Sample {
            a: 1,
            b: "x".into(),
            c: vec![],
        };
        let bytes = canonical_bytes(&s).unwrap();
        let truncated = &bytes[..bytes.len().saturating_sub(1)];
        assert!(from_canonical_bytes::<Sample>(truncated).is_err());

        // Case: determinism — the whole point of canonical bytes: two
        // encoders must agree.
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
