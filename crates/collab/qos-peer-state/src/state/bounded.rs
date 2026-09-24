//! Resource-qualified peer storage, separate from authenticated owner authority.
mod domain;
mod revocations;
use super::backend::BoundedNative;
pub use super::backend::member::PeerMember;
use super::*;
use crate::resources::{OuterLimits, PeerResourceError, native::NativeBatch};
use automerge::resource_limits::{NativeAllocationSet, NativeResourceError, NativeWorkTurn};
pub use domain::{
    PeerDeliveryPolicy, PeerDeliveryPolicyError, PeerDocumentRead, PeerDocumentReadError,
    PeerMediaCall, PeerPolicyBasis, PeerRead, PeerRegistry, PeerRoomAccess, PeerTurn,
};
pub use revocations::PeerRevocations;

/// Refusal ceilings. They do not promise every input beneath them fits native work.
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
/// One resource-only context for an entire embedding owner's operation. The
/// caller holds its actual host credit and original owner scope. Keep this same
/// value across all documents/phases; constructing it confers no authority.
///
/// Work can only be borrowed from the registry's current operation.
/// ```compile_fail
/// use qos_peer_state::PeerWork;
/// let constructor = PeerWork::new;
/// ```
pub struct PeerWork<'turn> {
    pub(super) batch: NativeBatch<'turn>,
    pub(super) turn: &'turn NativeWorkTurn,
    pub(super) limits: PeerLimits,
    domain: &'turn domain::DomainRef,
}
impl PeerWork<'_> {
    /// Pays caller-owned outer data or output before allocation, using this
    /// operation's exact current ledger. Retain the reservation until all
    /// covered backing has been deallocated. This lends no native input path,
    /// registry, admission credential or owner/domain authority. The embedding
    /// owner still validates its actual HostBytes pool and operation scope.
    pub fn reserve_output(
        &self,
        bytes: usize,
    ) -> Result<automerge::resource_limits::NativeReservation, BoundedPeerError> {
        self.turn.reserve(bytes).map_err(BoundedPeerError::Resource)
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
    WireVersion,
    Media,
    Governance,
    AuthorityRevoked,
    ExactFloor,
}
#[derive(Debug, PartialEq, Eq)]
pub enum BoundedPeerError {
    Resource(NativeResourceError),
    Decode(PeerResourceError),
    Invalid(BoundedPeerInvalid),
}
impl std::fmt::Display for BoundedPeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bounded peer refusal: {self:?}")
    }
}
impl std::error::Error for BoundedPeerError {}
/// Opaque actual retained native and metadata storage. No Clone, raw peer,
/// native document extraction, or owner/transport installation credential.
/// ```compile_fail
/// use qos_peer_state::BoundedStatePeer;
/// fn duplicate(peer: BoundedStatePeer) -> BoundedStatePeer { peer.clone() }
/// ```
pub struct BoundedStatePeer {
    pub(super) core: PeerCore<BoundedNative>,
    media: backend::media::Cache,
    domain: domain::DomainRef,
}
impl BoundedStatePeer {
    pub(super) fn from_core(
        core: PeerCore<BoundedNative>,
        media: backend::media::Cache,
        work: &PeerWork<'_>,
    ) -> Self {
        Self {
            core,
            media,
            domain: work.domain.copy(),
        }
    }
    /// The embedding owner authenticates the original durable bytes and pins
    /// room/authority/enrollment under its actual barrier. This checks the same
    /// durable admission semantics while paying native and metadata storage.
    pub fn restore(
        bytes: &[u8],
        room: &str,
        authority: &DeviceCert,
        roster: &[DeviceCert],
        revocations: &RevocationList,
        now: SystemTime,
        work: &mut PeerWork<'_>,
    ) -> Result<Self, BoundedPeerError> {
        backend::restore::restore(bytes, room, authority, roster, revocations, now, work)
    }
}
