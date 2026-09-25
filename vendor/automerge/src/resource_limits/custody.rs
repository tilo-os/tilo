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

mod probe;
#[doc(hidden)]
pub use probe::NativeBoundProbe;

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
        let checked =
            crate::AutoCommit::inspect_bounded(input, limits, &mut self.allowance.clone())?;
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
        f.debug_struct("NativeAllocationSet")
            .finish_non_exhaustive()
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
    /// Custody of one initial reservation (zero-byte credit is not kept).
    pub(crate) fn new(first: NativeReservation) -> Self {
        Self {
            first: (first.bytes() != 0).then_some(first),
            later: Vec::new(),
            metadata: None,
        }
    }
    pub(crate) fn bytes(&self) -> usize {
        self.first.as_ref().map_or(0, NativeReservation::bytes)
            + self
                .later
                .iter()
                .map(NativeReservation::bytes)
                .sum::<usize>()
    }
    /// Slot index (0 = `first`, i + 1 = `later[i]`) of the credit drawn from
    /// the same ledger as `peak`.
    fn matching(&self, peak: &NativeReservation) -> Option<usize> {
        if self
            .first
            .as_ref()
            .is_some_and(|first| Arc::ptr_eq(&first.ledger, &peak.ledger))
        {
            return Some(0);
        }
        self.later
            .iter()
            .position(|credit| Arc::ptr_eq(&credit.ledger, &peak.ledger))
            .map(|index| index + 1)
    }
    /// Re-covers a value whose retained bound is now `measured` from the held
    /// credits plus `peak` (growth), or releases the surplus, newest credit
    /// first (shrink). Growth from a new ledger needs a `later` slot; a full
    /// `later` buffer is replaced by one whose capacity is reserved from
    /// `peak` (as `metadata`) *before* it is allocated. Every fallible step
    /// precedes the first credit move.
    pub(crate) fn settle(&mut self, measured: usize, peak: &mut NativeReservation) -> Result {
        let held = self.bytes();
        if measured > held {
            let additional = measured - held;
            let matching = self.matching(peak);
            let needs_slot = self.first.is_some() && matching.is_none();
            let mut replacement = None;
            if needs_slot && self.later.len() == self.later.capacity() {
                let capacity = (self.later.len() * 2).max(4);
                let meta_bytes = capacity
                    .checked_mul(std::mem::size_of::<NativeReservation>())
                    .ok_or(NativeResourceError::WorkBytes)?;
                if peak.bytes() < additional.saturating_add(meta_bytes) {
                    return Err(NativeResourceError::WorkBytes);
                }
                let metadata = peak.split_off(meta_bytes)?;
                let mut values: Vec<NativeReservation> = Vec::new();
                values
                    .try_reserve_exact(capacity)
                    .map_err(|_| NativeResourceError::WorkBytes)?;
                if values
                    .capacity()
                    .saturating_mul(std::mem::size_of::<NativeReservation>())
                    > metadata.bytes()
                {
                    return Err(NativeResourceError::Unaccounted);
                }
                replacement = Some((values, metadata));
            } else if additional > peak.bytes() {
                return Err(NativeResourceError::WorkBytes);
            }
            let added = peak.split_off(additional)?;
            // No fallible operation follows.
            if let Some((mut values, metadata)) = replacement {
                values.append(&mut self.later);
                // The old buffer is freed before its metadata credit is.
                drop(std::mem::replace(&mut self.later, values));
                drop(self.metadata.replace(metadata));
            }
            match matching {
                Some(0) => absorb(self.first.as_mut(), added),
                Some(index) => absorb(self.later.get_mut(index - 1), added),
                None if self.first.is_none() => self.first = Some(added),
                None => self.later.push(added),
            }
        } else {
            let mut release = held - measured;
            while release != 0 {
                let last = match self.later.last_mut() {
                    Some(last) => last,
                    None => match self.first.as_mut() {
                        Some(first) => first,
                        None => break,
                    },
                };
                if last.bytes() <= release {
                    release -= last.bytes();
                    if self.later.pop().is_none() {
                        self.first = None;
                    }
                } else {
                    drop(last.split_off(release)?);
                    release = 0;
                }
            }
            if self.later.is_empty() {
                // Buffer deallocation precedes returning its credit.
                self.later = Vec::new();
                self.metadata = None;
            }
        }
        Ok(())
    }
}
/// Merges same-ledger credit into an existing slot without a
/// release/reacquire window (the ledger's charged total is unchanged).
fn absorb(slot: Option<&mut NativeReservation>, mut added: NativeReservation) {
    if let Some(slot) = slot {
        slot.bytes += added.bytes;
        added.bytes = 0;
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
        f.debug_struct("NativeCloneCandidate")
            .finish_non_exhaustive()
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
    pub fn clone_bounded(
        &self,
        allocations: &mut NativeAllocationSet,
        turn: &NativeWorkTurn,
    ) -> Result<Self> {
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
    pub fn clone_bounded(
        &self,
        allocations: &mut NativeAllocationSet,
        turn: &NativeWorkTurn,
    ) -> Result<Self> {
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
    /// Upper bound on retained allocations, from measured std capacities
    /// and conservative column bounds. Does not close a transaction or
    /// change caches, and allocates nothing.
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
        let allowance = allocations.check_turn(turn)?;
        let dims = DocDims::of(self);
        let peak_bytes = dims::clone_peak_bound(&dims)?;
        // Both reservations precede the copy; a refusal leaves the ledger
        // unchanged (the domain credit is released on the error path).
        let domain = allocations.domain.copy_in(allowance)?;
        let mut peak = allowance.reserve(peak_bytes)?;
        let value = self.clone();
        let retained = match dims::retained_bound(&DocDims::of(&value)) {
            Ok(retained) if retained <= peak.bytes() => retained,
            Ok(_) => {
                drop(value);
                return Err(NativeResourceError::Unaccounted);
            }
            Err(error) => {
                drop(value);
                return Err(error);
            }
        };
        drop(peak.split_off(peak.bytes() - retained)?);
        Ok(NativeCloneCandidate {
            value,
            domain,
            retention: NativeRetention::new(peak),
        })
    }
}

/// Private handoff: adopt a document produced under `peak` (load or apply)
/// into custody. Fails closed with `Unaccounted` if the document's retained
/// bound exceeds `peak`; the excess of `peak` is released.
pub(crate) fn adopt(
    value: crate::AutoCommit,
    allocations: &mut NativeAllocationSet,
    turn: &NativeWorkTurn,
    mut peak: NativeReservation,
) -> Result<NativeOwned<crate::AutoCommit>> {
    let checked = allocations
        .check_turn(turn)
        .and_then(|allowance| {
            if peak.is_from(allowance) {
                Ok(allowance)
            } else {
                Err(NativeResourceError::Unaccounted)
            }
        })
        .and_then(|allowance| {
            let retained = dims::retained_bound(&DocDims::of(&value))?;
            if retained > peak.bytes() {
                return Err(NativeResourceError::Unaccounted);
            }
            let domain = allocations.domain.copy_in(allowance)?;
            Ok((retained, domain))
        });
    match checked {
        Ok((retained, domain)) => {
            drop(peak.split_off(peak.bytes() - retained)?);
            Ok(NativeOwned {
                value,
                domain,
                retention: NativeRetention::new(peak),
            })
        }
        Err(error) => {
            // The value is freed before the credit that covered it.
            drop(value);
            drop(peak);
            Err(error)
        }
    }
}

/// Private handoff after in-place growth of an owned document (apply): the
/// grown document's retained bound is re-covered from its existing credits
/// plus `peak`; the excess of `peak` is released.
pub(crate) fn readopt(
    mut owned: NativeOwned<crate::AutoCommit>,
    turn: &NativeWorkTurn,
    mut peak: NativeReservation,
) -> Result<NativeOwned<crate::AutoCommit>> {
    let settled = owned.check_turn(turn).and_then(|allowance| {
        if !peak.is_from(allowance) {
            return Err(NativeResourceError::Unaccounted);
        }
        let retained = dims::retained_bound(&DocDims::of(&owned.value))?;
        owned.retention.settle(retained, &mut peak)
    });
    match settled {
        Ok(()) => {
            drop(peak);
            Ok(owned)
        }
        Err(error) => {
            drop(owned);
            drop(peak);
            Err(error)
        }
    }
}
