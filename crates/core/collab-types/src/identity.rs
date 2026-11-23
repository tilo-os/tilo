//! Ids and bytes an identity carries: the tag on a signed op's route, and an
//! avatar image.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifies the route a signed op belongs to — a UUID v4 derived from the
/// app, workspace and record the op's document lives at, carried as
/// `SignedOp::attestation_id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttestationId(pub Uuid);

impl AttestationId {
    /// Mint a fresh id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for AttestationId {
    fn default() -> Self {
        Self::new()
    }
}

/// Avatar image bytes, wrapped in a newtype with a `serde_bytes` adapter
/// so postcard encodes it length-prefixed (compact) rather than one
/// varint per byte.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AvatarBytes(#[serde(with = "serde_bytes")] pub Vec<u8>);
