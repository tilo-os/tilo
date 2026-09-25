//! One actual native registry and a retained resource-domain identity.
mod access;
pub use access::PeerRoomAccess;
mod admissions;
mod document;
pub use document::{PeerDocumentRead, PeerDocumentReadError};
mod media;
pub use media::PeerMediaCall;
mod delivery_policy;
pub use delivery_policy::{PeerDeliveryPolicy, PeerDeliveryPolicyError};
mod private_policy;
use super::*;
use automerge::resource_limits::{NativeAllowance, NativeReservation};
pub use private_policy::PeerPolicyBasis;
use std::{
    alloc::Layout,
    sync::{Arc, atomic::AtomicUsize},
};

struct Domain {
    _retention: NativeReservation,
}
/// No raw Arc or Weak escape. All strong owners drop through into_inner, so the
/// last owner's allocation is deallocated BEFORE its retained credit is freed.
pub(super) struct DomainRef {
    inner: Option<Arc<Domain>>,
}
impl DomainRef {
    fn new(allowance: &NativeAllowance) -> Result<Self, BoundedPeerError> {
        // Pinned std ArcInner<T>: repr(C), two AtomicUsize counters then T.
        let (layout, _) = Layout::new::<[AtomicUsize; 2]>()
            .extend(Layout::new::<Domain>())
            .map_err(|_| BoundedPeerError::Resource(NativeResourceError::WorkBytes))?;
        let retention = allowance
            .reserve(layout.pad_to_align().size())
            .map_err(BoundedPeerError::Resource)?;
        Ok(Self {
            inner: Some(Arc::new(Domain {
                _retention: retention,
            })),
        })
    }
    pub(super) fn copy(&self) -> Self {
        Self {
            inner: Some(Arc::clone(self.inner.as_ref().expect("live domain"))),
        }
    }
    fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(
            self.inner.as_ref().expect("live domain"),
            other.inner.as_ref().expect("live domain"),
        )
    }
}
impl Drop for DomainRef {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            // Every strong handle uses this operation, including concurrent
            // drops. No final payload/credit drop occurs inside Arc's drop.
            drop(Arc::into_inner(inner));
        }
    }
}

/// Owns the original native allocation set. This is resource custody only; the
/// embedding owner independently validates its actual pool, scope and authority.
/// ```compile_fail
/// use qos_peer_state::PeerRegistry;
/// fn escape(registry: PeerRegistry) { let _ = registry.allocations; }
/// ```
pub struct PeerRegistry {
    allocations: NativeAllocationSet,
    domain: DomainRef,
}
/// One exclusive operation. All work borrows share its normalized input counter.
/// Dropping the operation seals new work; retained values keep original credit.
pub struct PeerTurn<'registry> {
    registry: &'registry mut PeerRegistry,
    native: NativeWorkTurn,
    allowance: NativeAllowance,
    remaining_normalized: usize,
    limits: PeerLimits,
}
/// Borrowed facts from a genuinely restored peer in its original resource domain.
/// No legacy StatePeer or raw native document can be recovered from this view.
/// ```compile_fail
/// use qos_peer_state::{BoundedStatePeer, PeerRead, PeerWork};
/// fn escape<'peer>(peer: &'peer BoundedStatePeer, work: &PeerWork<'_>)
///     -> PeerRead<'peer, 'static> { peer.read(work).unwrap() }
/// ```
pub struct PeerRead<'peer, 'work> {
    peer: &'peer BoundedStatePeer,
    _work: &'work NativeWorkTurn,
    limits: PeerLimits,
}
impl PeerRegistry {
    pub fn new(initial: NativeAllowance) -> Result<Self, BoundedPeerError> {
        let domain = DomainRef::new(&initial)?;
        let allocations = NativeAllocationSet::new(initial).map_err(BoundedPeerError::Resource)?;
        Ok(Self {
            allocations,
            domain,
        })
    }
    pub fn begin_turn(
        &mut self,
        current: NativeAllowance,
        limits: PeerLimits,
    ) -> Result<PeerTurn<'_>, BoundedPeerError> {
        if limits.normalized_bytes > 8 * 1024 * 1024 {
            return Err(BoundedPeerError::Resource(
                NativeResourceError::NormalizedBytes,
            ));
        }
        let native = self
            .allocations
            .work_turn(current.clone())
            .map_err(BoundedPeerError::Resource)?;
        Ok(PeerTurn {
            registry: self,
            native,
            allowance: current,
            remaining_normalized: limits.normalized_bytes,
            limits,
        })
    }
    pub fn reconcile(&mut self) {
        self.allocations.reconcile();
    }
}
impl PeerTurn<'_> {
    pub fn work(&mut self) -> Result<PeerWork<'_>, BoundedPeerError> {
        // Also refuses sealed turns for a legitimate empty peer with no native
        // document operation that would otherwise check the current ledger.
        self.native.reserve(0).map_err(BoundedPeerError::Resource)?;
        Ok(PeerWork {
            batch: NativeBatch::continuing(
                &mut self.registry.allocations,
                &self.native,
                &mut self.remaining_normalized,
            )
            .map_err(BoundedPeerError::Resource)?,
            turn: &self.native,
            limits: self.limits,
            domain: &self.registry.domain,
        })
    }
}
impl Drop for PeerTurn<'_> {
    fn drop(&mut self) {
        self.allowance.finish_turn();
        self.registry.reconcile();
    }
}
impl BoundedStatePeer {
    pub fn read<'peer, 'work>(
        &'peer self,
        work: &'work PeerWork<'_>,
    ) -> Result<PeerRead<'peer, 'work>, BoundedPeerError> {
        if !self.domain.matches(work.domain) {
            return Err(BoundedPeerError::Resource(NativeResourceError::Unaccounted));
        }
        work.turn.reserve(0).map_err(BoundedPeerError::Resource)?;
        Ok(PeerRead {
            peer: self,
            _work: work.turn,
            limits: work.limits,
        })
    }
}
impl PeerRead<'_, '_> {
    pub fn member(
        &self,
        source: [u8; 32],
        now: SystemTime,
    ) -> Result<PeerMember<'_, '_>, BoundedPeerError> {
        backend::member::bounded(&self.peer.core, source, now, self._work, self.limits)
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
