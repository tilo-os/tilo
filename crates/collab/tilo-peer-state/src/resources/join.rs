//! Guarded decoding of the closed join transport response schema.
use super::{OuterLimits, PeerResourceError};
use crate::join::JoinTransportResponse;

/// A join response decoded within [`OuterLimits`]. This is not join authority
/// or document admission; the caller still authenticates and admits it.
pub struct BoundedJoinResponse {
    response: JoinTransportResponse,
}
impl BoundedJoinResponse {
    /// # Errors
    ///
    /// [`PeerResourceError::Capacity`] if the limits are exceeded and
    /// [`PeerResourceError::Malformed`] if the bytes do not decode.
    pub fn decode(bytes: &[u8], limits: OuterLimits) -> Result<Self, PeerResourceError> {
        Ok(Self {
            response: super::decode(bytes, limits)?,
        })
    }
    pub fn response(&self) -> &JoinTransportResponse {
        &self.response
    }
}

// Exactly the seven derived response variants and their audited join/identity
// fields. This does not expose the generic guard to arbitrary Deserialize code.
impl super::outer::EncodeSchema for JoinTransportResponse {}
impl super::outer::Schema<'_> for JoinTransportResponse {}

#[cfg(test)]
mod tests;
