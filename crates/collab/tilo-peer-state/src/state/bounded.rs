//! Peer state restored within explicit limits, separate from authenticated
//! owner authority.
mod domain;
mod revocations;
use super::backend::BoundedNative;
pub use super::backend::member::PeerMember;
use super::{DeviceCert, PeerCore, RevocationList, Route, SystemTime, backend, room};
use crate::resources::{OuterLimits, PeerResourceError};
pub use domain::{
    PeerDeliveryPolicy, PeerDeliveryPolicyError, PeerDocumentCapture, PeerDocumentRead,
    PeerDocumentReadError, PeerMediaCall, PeerPolicyBasis, PeerRead, PeerRoomAccess,
    committed_document_revision,
};
pub use revocations::PeerRevocations;

/// Refusal limits for one restore and the reads made from it.
#[derive(Clone, Copy, Debug)]
pub struct PeerLimits {
    pub normalized_bytes: usize,
    pub durable: OuterLimits,
    pub exact: OuterLimits,
    pub metadata_bytes: usize,
    pub policy: OuterLimits,
    pub governance_json: OuterLimits,
    pub governance_collection_bytes: usize,
}
impl Default for PeerLimits {
    fn default() -> Self {
        Self {
            normalized_bytes: 8 * 1024 * 1024,
            durable: OuterLimits {
                allocation_bytes: 16 * 1024 * 1024,
                collection_entries: 65536,
            },
            exact: OuterLimits {
                allocation_bytes: 4 * 1024 * 1024,
                collection_entries: 65536,
            },
            metadata_bytes: 4 * 1024 * 1024,
            policy: OuterLimits {
                allocation_bytes: 1024 * 1024,
                collection_entries: 65536,
            },
            governance_json: OuterLimits {
                allocation_bytes: 128 * 1024,
                collection_entries: 65536,
            },
            governance_collection_bytes: 4 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundedPeerInvalid {
    Version,
    Envelope,
    Namespace,
    Certificate,
    Revocation,
    Pending,
    Coverage,
    Media,
    Governance,
    AuthorityRevoked,
    ExactFloor,
    Sealed,
    Document,
}
#[derive(Debug, PartialEq, Eq)]
pub enum BoundedPeerError {
    /// An explicit limit in [`PeerLimits`] was reached.
    Capacity,
    Decode(PeerResourceError),
    Invalid(BoundedPeerInvalid),
}
impl std::fmt::Display for BoundedPeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bounded peer refusal: {self:?}")
    }
}
impl std::error::Error for BoundedPeerError {}
impl From<crate::room::read::Capacity> for BoundedPeerError {
    fn from(_: crate::room::read::Capacity) -> Self {
        Self::Capacity
    }
}
impl From<crate::resources::native::LoadError> for BoundedPeerError {
    fn from(error: crate::resources::native::LoadError) -> Self {
        use crate::resources::native::LoadError;
        match error {
            LoadError::Capacity
            | LoadError::Rejected(tilo_automerge_isolate::Rejected::TooLarge) => Self::Capacity,
            LoadError::Rejected(_) => Self::Invalid(BoundedPeerInvalid::Document),
        }
    }
}
/// Restored peer storage. No Clone, raw peer, document extraction, or
/// owner/transport installation credential.
/// ```compile_fail
/// use tilo_peer_state::BoundedStatePeer;
/// fn duplicate(peer: BoundedStatePeer) -> BoundedStatePeer { peer.clone() }
/// ```
pub struct BoundedStatePeer {
    pub(super) core: PeerCore<BoundedNative>,
    media: backend::media::Cache,
    limits: PeerLimits,
}
impl BoundedStatePeer {
    pub(super) fn from_core(
        core: PeerCore<BoundedNative>,
        media: backend::media::Cache,
        limits: PeerLimits,
    ) -> Self {
        Self {
            core,
            media,
            limits,
        }
    }
    /// The embedding owner authenticates the original durable bytes and pins
    /// room/authority/enrollment under its actual barrier. This checks the same
    /// durable admission semantics within `limits`.
    ///
    /// # Errors
    ///
    /// Returns a [`BoundedPeerError`] if the bytes are refused, the room or
    /// authority does not match, or a limit is reached.
    pub fn restore(
        bytes: &[u8],
        room: &str,
        authority: &DeviceCert,
        roster: &[DeviceCert],
        revocations: &RevocationList,
        now: SystemTime,
        limits: &PeerLimits,
    ) -> Result<Self, BoundedPeerError> {
        backend::restore::restore(bytes, room, authority, roster, revocations, now, limits)
    }
}
