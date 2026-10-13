//! Reads of a restored peer.
mod access;
pub use access::PeerRoomAccess;
mod admissions;
mod document;
pub use document::{
    PeerDocumentCapture, PeerDocumentRead, PeerDocumentReadError, committed_document_revision,
};
mod media;
pub use media::PeerMediaCall;
mod delivery_policy;
pub use delivery_policy::{PeerDeliveryPolicy, PeerDeliveryPolicyError};
mod private_policy;
use super::{
    BoundedPeerError, BoundedStatePeer, DeviceCert, PeerLimits, PeerMember, Route, SystemTime,
    backend, room,
};
pub use private_policy::PeerPolicyBasis;

/// Facts read from a restored peer, within the limits it was restored with.
/// No `StatePeer` or raw document can be recovered from this view.
pub struct PeerRead<'peer> {
    peer: &'peer BoundedStatePeer,
    limits: PeerLimits,
}
impl BoundedStatePeer {
    #[must_use]
    pub fn read(&self) -> PeerRead<'_> {
        PeerRead {
            peer: self,
            limits: self.limits,
        }
    }
}
impl PeerRead<'_> {
    /// The current governed member for the device `source`.
    ///
    /// # Errors
    ///
    /// Returns a [`BoundedPeerError`] if `source` is not a current member or a
    /// limit is reached.
    pub fn member(
        &self,
        source: [u8; 32],
        now: SystemTime,
    ) -> Result<PeerMember<'_>, BoundedPeerError> {
        backend::member::bounded(&self.peer.core, source, now, self.limits)
    }
    pub fn room(&self) -> &str {
        &self.peer.core.room
    }
    pub fn authority_device(&self) -> [u8; 32] {
        self.peer.core.authority.device_public_key
    }
    pub fn document_count(&self) -> usize {
        self.peer.core.documents.len()
    }
}
#[cfg(test)]
mod tests;
