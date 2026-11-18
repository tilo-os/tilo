//! Shared bytes, encoded as the bytes they are.

use std::sync::Arc;

use serde::{Deserialize as _, Deserializer, Serialize as _, Serializer};

pub(super) fn serialize<S: Serializer>(bytes: &Arc<[u8]>, out: S) -> Result<S::Ok, S::Error> {
    bytes.as_ref().serialize(out)
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(input: D) -> Result<Arc<[u8]>, D::Error> {
    Vec::<u8>::deserialize(input).map(Arc::from)
}
