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

pub use wire_frame::canonical::{CanonicalError, canonical_bytes, from_canonical_bytes};
