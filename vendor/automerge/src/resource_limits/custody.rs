//! Retained native allocation custody on automerge 0.12.
//!
//! OWNER: lane L1 (dims + custody). Skeleton by the coordinator; the domain
//! branding below is carried from the 0.8 patch. hexane 1.0 slabs are owned
//! `Vec`s, so there is no shared backing registry: every retained byte
//! belongs to exactly one document and a clone costs its full footprint.
use super::dims::{self, DocDims};
use super::{NativeAllowance, NativeReservation, NativeResourceError};
use crate::transaction::Transactable;
use std::sync::Arc;

pub(crate) type Result<T = ()> = std::result::Result<T, NativeResourceError>;

/// Read-only retained allocation bound. Producing this measurement is not
/// qualification and does not reserve the existing document's storage.
#[derive(Debug)]
pub struct NativeFootprint {
    pub(crate) owned: usize,
    pub(crate) shared: usize,
}
impl NativeFootprint {
    /// Owned allocation bound, excluding shared backing allocations.
    pub fn owned_bytes(&self) -> usize {
        self.owned
    }
    /// Unique shared backing allocation bytes. Always 0 on automerge 0.12:
    /// hexane 1.0 slabs are uniquely owned.
    pub fn shared_bytes(&self) -> usize {
        self.shared
    }
    /// Total retained allocation bound.
    pub fn total_bytes(&self) -> usize {
        self.owned + self.shared
    }
}

/// Private owner-domain identity. Every strong handle has an external
/// metadata reservation covering the shared control block.
pub(crate) struct DomainRef {
    identity: Arc<()>,
    _metadata: NativeReservation,
}
pub(crate) fn arc_unit_bytes() -> usize {
    // ArcInner<()>: strong + weak counts.
    2 * std::mem::size_of::<usize>()
}
impl DomainRef {
    fn new(allowance: &NativeAllowance) -> Result<Self> {
        let metadata = allowance.reserve(arc_unit_bytes())?;
        Ok(Self {
            identity: Arc::new(()),
            _metadata: metadata,
        })
    }
    pub(crate) fn copy_in(&self, allowance: &NativeAllowance) -> Result<Self> {
        let metadata = allowance.reserve(arc_unit_bytes())?;
        Ok(Self {
            identity: Arc::clone(&self.identity),
            _metadata: metadata,
        })
    }
    pub(crate) fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
    }
}

/// Additional native work for one turn, branded to a private owner registry.
/// The embedding owner must validate the actual HostBytes pool and preserve
/// its credit; this native budget token is not authorization.
pub struct NativeWorkTurn {
    pub(crate) domain: DomainRef,
    pub(crate) allowance: NativeAllowance,
}
impl NativeWorkTurn {
    /// Inspect and normalize complete storage under this exact current ledger.
    /// This creates checked input, not native owner or installation authority.
    pub fn normalize_input<'a>(
        &self,
        input: &'a [u8],
        limits: &super::NativeLimits,
    ) -> Result<super::NormalizedStorage<'a>> {
        let checked = crate::AutoCommit::inspect_bounded(input, limits, &mut self.allowance.clone())?;
        checked.normalize(&self.allowance)
    }
    pub(crate) fn accepts_reservation(&self, reservation: &NativeReservation) -> bool {
        reservation.is_from(&self.allowance)
    }
    /// Reserves bounded embedding output or metadata in this exact work turn.
    /// The caller must retain the reservation until that allocation is dropped.
    pub fn reserve(&self, bytes: usize) -> Result<NativeReservation> {
        self.allowance.reserve(bytes)
    }
}
impl std::fmt::Debug for NativeWorkTurn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeWorkTurn").finish_non_exhaustive()
    }
}

/// The owner's allocation registry, shared across all native document phases.
pub struct NativeAllocationSet {
    pub(crate) domain: DomainRef,
}
impl NativeAllocationSet {
    /// Reserves this registry's private domain identity in the initial turn.
    pub fn new(initial: NativeAllowance) -> Result<Self> {
        Ok(Self {
            domain: DomainRef::new(&initial)?,
        })
    }
    /// Brands an embedding-owner work allowance to this registry.
    pub fn work_turn(&self, allowance: NativeAllowance) -> Result<NativeWorkTurn> {
        Ok(NativeWorkTurn {
            domain: self.domain.copy_in(&allowance)?,
            allowance,
        })
    }
    pub(crate) fn check_turn<'a>(&self, turn: &'a NativeWorkTurn) -> Result<&'a NativeAllowance> {
        if !self.domain.matches(&turn.domain) {
            return Err(NativeResourceError::Unaccounted);
        }
        Ok(&turn.allowance)
    }
    /// Kept for API compatibility. With uniquely owned slabs there are no
    /// shared backing leases to retire; each document releases its own credit
    /// on drop.
    pub fn reconcile(&mut self) {}
}
impl std::fmt::Debug for NativeAllocationSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeAllocationSet").finish_non_exhaustive()
    }
}

/// Credit retained alongside a native value; it is not independently
/// extractable. Credits from successive turns may come from different
/// ledgers; L1 owns the bookkeeping-metadata accounting for `later`.
pub struct NativeRetention {
    pub(crate) first: Option<NativeReservation>,
    pub(crate) later: Vec<NativeReservation>,
    pub(crate) metadata: Option<NativeReservation>,
}
impl NativeRetention {
    pub(crate) fn bytes(&self) -> usize {
        self.first.as_ref().map_or(0, NativeReservation::bytes)
            + self.later.iter().map(NativeReservation::bytes).sum::<usize>()
    }
}
impl std::fmt::Debug for NativeRetention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeRetention").finish_non_exhaustive()
    }
}

/// A native value and inseparable retained credit. No raw native reference,
/// owning extraction, Clone, or unchecked mutable operation is exposed.
pub struct NativeOwned<T> {
    pub(crate) value: T,
    pub(crate) domain: DomainRef,
    pub(crate) retention: NativeRetention,
}
/// A measured clone of an unqualified source: copy-work custody only, never
/// native admission, readiness, or a conversion to `NativeOwned`.
pub struct NativeCloneCandidate<T> {
    pub(crate) value: T,
    pub(crate) domain: DomainRef,
    pub(crate) retention: NativeRetention,
}
impl<T> std::fmt::Debug for NativeOwned<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeOwned").finish_non_exhaustive()
    }
}
impl<T> std::fmt::Debug for NativeCloneCandidate<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeCloneCandidate").finish_non_exhaustive()
    }
}

fn check_domain<'a>(domain: &DomainRef, turn: &'a NativeWorkTurn) -> Result<&'a NativeAllowance> {
    if !domain.matches(&turn.domain) {
        return Err(NativeResourceError::Unaccounted);
    }
    Ok(&turn.allowance)
}

impl NativeCloneCandidate<crate::AutoCommit> {
    /// Number of operations in the unchanged open transaction (no allocation).
    pub fn pending_ops(&self) -> usize {
        self.value.pending_ops()
    }
    /// Another measured, still unqualified clone under the same owner registry.
    pub fn clone_bounded(&self, allocations: &mut NativeAllocationSet, turn: &NativeWorkTurn) -> Result<Self> {
        self.check_turn(turn)?;
        self.value.clone_bounded(allocations, turn)
    }
    pub(crate) fn check_turn<'a>(&self, turn: &'a NativeWorkTurn) -> Result<&'a NativeAllowance> {
        check_domain(&self.domain, turn)
    }
}
impl NativeOwned<crate::AutoCommit> {
    /// Number of operations in the still-open transaction (no allocation).
    pub fn pending_ops(&self) -> usize {
        self.value.pending_ops()
    }
    /// Clone through the owner's allocation registry.
    pub fn clone_bounded(&self, allocations: &mut NativeAllocationSet, turn: &NativeWorkTurn) -> Result<Self> {
        self.check_turn(turn)?;
        let cloned = self.value.clone_bounded(allocations, turn)?;
        Ok(Self {
            value: cloned.value,
            domain: cloned.domain,
            retention: cloned.retention,
        })
    }
    pub(crate) fn check_turn<'a>(&self, turn: &'a NativeWorkTurn) -> Result<&'a NativeAllowance> {
        check_domain(&self.domain, turn)
    }
}

impl crate::AutoCommit {
    /// Upper bound on retained allocations, from conservative dimension
    /// bounds. Does not close a transaction or change caches.
    pub fn native_footprint(&self, allowance: &NativeAllowance) -> Result<NativeFootprint> {
        let _ = allowance;
        let owned = dims::retained_bound(&DocDims::of(self))?;
        Ok(NativeFootprint { owned, shared: 0 })
    }
    /// Clones after reserving the full deep copy (retained bound of the
    /// source plus clone transients) and retention metadata. The result is
    /// explicitly unqualified.
    pub fn clone_bounded(
        &self,
        allocations: &mut NativeAllocationSet,
        turn: &NativeWorkTurn,
    ) -> Result<NativeCloneCandidate<Self>> {
        let _ = (allocations, turn);
        todo!("L1: reserve dims::clone_peak_bound, clone, retain dims::retained_bound")
    }
}

/// Private handoff: adopt a document produced under `peak` (load or apply)
/// into custody. Fails closed with `Unaccounted` if the document's retained
/// bound exceeds `peak`; the excess of `peak` is released.
pub(crate) fn adopt(
    value: crate::AutoCommit,
    allocations: &mut NativeAllocationSet,
    turn: &NativeWorkTurn,
    peak: NativeReservation,
) -> Result<NativeOwned<crate::AutoCommit>> {
    let _ = (value, allocations, turn, peak);
    todo!("L1")
}

/// Private handoff after in-place growth of an owned document (apply): the
/// grown document's retained bound is re-covered from its existing credits
/// plus `peak`; the excess of `peak` is released.
pub(crate) fn readopt(
    owned: NativeOwned<crate::AutoCommit>,
    turn: &NativeWorkTurn,
    peak: NativeReservation,
) -> Result<NativeOwned<crate::AutoCommit>> {
    let _ = (owned, turn, peak);
    todo!("L1")
}
