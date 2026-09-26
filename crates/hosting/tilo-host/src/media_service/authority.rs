//! Signing input minted only by the authenticated media control.
pub use crate::room_media_policy::RoomMediaPolicy;

/// Only the common, authenticated media control constructs these inputs.
pub struct MediaSigningInput([u8; 32]);
impl MediaSigningInput {
    pub fn digest(&self) -> &[u8; 32] {
        &self.0
    }
    pub(super) fn new(digest: [u8; 32]) -> Self {
        Self(digest)
    }
}
