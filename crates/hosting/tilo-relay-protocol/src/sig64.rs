//! Serde adapter for a 64-byte Ed25519 signature carried as one byte string
//! (length-prefixed in postcard) rather than 64 separate `u8` elements.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub(crate) fn serialize<S: Serializer>(bytes: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
    serde_bytes::Bytes::new(bytes).serialize(s)
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
    let bytes = serde_bytes::ByteBuf::deserialize(d)?.into_vec();
    <[u8; 64]>::try_from(bytes).map_err(|bytes| {
        D::Error::custom(format!("expected 64-byte signature, got {}", bytes.len()))
    })
}
