//! Guarded decoding of the closed join transport response schema.
use super::{Held, NativeAllowance, OuterLimits, PeerResourceError};
use crate::join::JoinTransportResponse;

/// Retains the complete decoded response backing. This is not join authority or
/// native document qualification; the caller still authenticates and admits it.
pub struct BoundedJoinResponse {
    held: Held<JoinTransportResponse>,
}
impl BoundedJoinResponse {
    pub fn decode(
        bytes: &[u8],
        limits: OuterLimits,
        allowance: &NativeAllowance,
    ) -> Result<Self, PeerResourceError> {
        // The only custom allocating diagnostics in this closed schema are
        // join::bytes64 and identity::serde_bytes_64. The longer fixed prefix
        // below plus a decimal usize bounds their format! output. The pinned
        // String/Vec growth bound includes simultaneous old/new backing; this
        // is the same source-based signature bound used by guarded JSON.
        const DIAGNOSTIC: usize =
            3 * ("expected 64-byte signature, got ".len() + usize::BITS as usize);
        let allocation_bytes = limits
            .allocation_bytes
            .checked_sub(DIAGNOSTIC)
            .ok_or(PeerResourceError::Capacity)?;
        let _diagnostic = allowance
            .reserve(DIAGNOSTIC)
            .map_err(|_| PeerResourceError::Capacity)?;
        let held = Held::decode(
            bytes,
            OuterLimits {
                allocation_bytes,
                ..limits
            },
            allowance,
        )?;
        Ok(Self { held })
    }
    pub fn response(&self) -> &JoinTransportResponse {
        &self.held.value
    }
}

// Exactly the seven derived response variants and their audited join/identity
// fields. This does not expose the generic guard to arbitrary Deserialize code.
impl super::outer::EncodeSchema for JoinTransportResponse {}
impl<'de> super::outer::Schema<'de> for JoinTransportResponse {}

#[cfg(test)]
mod tests;
