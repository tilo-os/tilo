//! Retained native allocation custody. No mutable or owning native escape.
//!
//! Layout bounds below are for the repository's pinned Rust 1.97.1 standard
//! library (hashbrown 0.17.1). They measure requested allocations, not RSS.
mod history;
pub(crate) mod mutate;
mod owned;
mod read;
pub(crate) mod save;
pub(crate) mod save_columns;
use super::{NativeAllowance, NativeReservation, NativeResourceError};
use crate::marks::MarkSet;
use crate::transaction::Transactable;
use hexane::{ColumnCursor, ColumnData, SharedBytes, SharedBytesId};
pub use history::{NativeChanges, NativeHashes};
pub use mutate::{NativeApplyResult, NativeDependencyPolicy, NativeIncomingOutcome, NativeMutationError};
pub use read::{
    NativeId, NativeListEntry, NativeObject, NativeReadError, NativeReadSession, NativeReadSteps, NativeScalar, NativeSelection,
    NativeValue,
};
pub use save::NativeSaved;
use smol_str::SmolStr;
use owned::OwnedCredits;
use std::alloc::Layout;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::Hash;
use std::mem::{align_of, size_of};
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

type Result<T = ()> = std::result::Result<T, NativeResourceError>;
fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b).ok_or(NativeResourceError::WorkBytes)
}
fn mul(a: usize, b: usize) -> Result<usize> {
    a.checked_mul(b).ok_or(NativeResourceError::WorkBytes)
}
fn padded(bytes: usize, alignment: usize) -> Result<usize> {
    Ok(add(bytes, alignment - 1)? & !(alignment - 1))
}
#[repr(C, align(2))]
struct ArcHeader {
    strong: AtomicUsize,
    weak: AtomicUsize,
}
pub(super) fn arc_bytes<T>() -> Result<usize> {
    arc_layout(Layout::new::<T>())
}
fn arc_layout(value: Layout) -> Result<usize> {
    Layout::new::<ArcHeader>()
        .extend(value)
        .map(|(layout, _)| layout.pad_to_align().size())
        .map_err(|_| NativeResourceError::WorkBytes)
}

/// Pinned hashbrown RawTable keeps deleted buckets. `high_water` must be the
/// maximum reported capacity since the current table's creation, observed
/// after every insertion and before any removal (or a larger proven bound).
/// The scalar current capacity alone is only valid for insert-only tables.
pub(crate) fn table_bytes<T>(high_water: usize) -> Result<usize> {
    if high_water == 0 {
        return Ok(0);
    }
    let buckets = (mul(high_water, 8)? / 7)
        .max(4)
        .checked_next_power_of_two()
        .ok_or(NativeResourceError::WorkBytes)?;
    // Every enabled pinned control group is at most 16 bytes, including x86
    // SSE2; aarch64's NEON group is 8. TableLayout rounds data to ctrl_align,
    // then allocates one byte per bucket plus a trailing control group.
    let ctrl_align = align_of::<T>().max(16);
    add(
        add(padded(mul(size_of::<T>(), buckets)?, ctrl_align)?, buckets)?,
        16,
    )
}
pub(super) fn btree_bytes<K, V>(len: usize) -> Result<usize> {
    // Pinned BTree node B=6, CAPACITY=11. At most len+1 nodes can remain,
    // including a retained empty root. Bound every node by an internal node:
    // parent pointer, two u16s, eleven keys/values and twelve edge pointers.
    // Padding each field to the maximum alignment bounds every Rust field
    // ordering; no private repr(Rust) offset is guessed.
    let alignment = align_of::<K>()
        .max(align_of::<V>())
        .max(align_of::<usize>());
    let fields = [
        size_of::<usize>(),
        size_of::<u16>(),
        size_of::<u16>(),
        mul(11, size_of::<K>())?,
        mul(11, size_of::<V>())?,
        mul(12, size_of::<usize>())?,
    ];
    let mut node = 0;
    for field in fields {
        node = add(node, padded(field, alignment)?)?;
    }
    mul(add(len, 1)?, node)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BackingId {
    Bytes(SharedBytesId),
    Mark(usize),
    String(usize),
}
#[derive(Clone)]
enum Backing {
    Bytes(SharedBytes),
    Mark { value: Arc<MarkSet>, bytes: usize },
    String(SmolStr),
}
impl Backing {
    fn id(&self) -> BackingId {
        match self {
            Self::Bytes(value) => BackingId::Bytes(value.identity()),
            Self::Mark { value, .. } => BackingId::Mark(Arc::as_ptr(value) as usize),
            Self::String(value) => BackingId::String(value.as_ptr() as usize),
        }
    }
    fn bytes(&self) -> Result<usize> {
        match self {
            Self::Bytes(value) => Ok(value.allocation_bytes()),
            Self::Mark { bytes, .. } => Ok(*bytes),
            Self::String(value) => arc_layout(
                Layout::array::<u8>(value.len()).map_err(|_| NativeResourceError::WorkBytes)?,
            ),
        }
    }
}

/// Private visitor of allocation ownership, never serialized size. A first
/// pass counts metadata slots without allocation; the second fills a fixed
/// reserved vector. Strong backing handles prevent address reuse during dedup.
pub(crate) struct Visitor<'a> {
    owned: usize,
    references: usize,
    backing: Option<&'a mut Vec<Backing>>,
}
pub(crate) trait NativeVisit {
    fn native_visit(&self, visitor: &mut Visitor<'_>) -> Result;
}
impl Visitor<'_> {
    pub(crate) fn owned(&mut self, bytes: usize) -> Result {
        self.owned = add(self.owned, bytes)?;
        Ok(())
    }
    fn shared(&mut self, backing: Backing) -> Result {
        self.references = add(self.references, 1)?;
        if let Some(output) = &mut self.backing {
            if output.len() == output.capacity() {
                return Err(NativeResourceError::Unaccounted);
            }
            output.push(backing);
        }
        Ok(())
    }
    pub(crate) fn hash_set<T: Eq + Hash + NativeVisit>(
        &mut self,
        values: &HashSet<T>,
        high_water: usize,
    ) -> Result {
        if values.capacity() > high_water {
            return Err(NativeResourceError::Unaccounted);
        }
        self.owned(table_bytes::<T>(high_water)?)?;
        for value in values {
            value.native_visit(self)?;
        }
        Ok(())
    }
    pub(crate) fn hash_map<K: Eq + Hash + NativeVisit, V: NativeVisit>(
        &mut self,
        values: &HashMap<K, V>,
        high_water: usize,
    ) -> Result {
        if values.capacity() > high_water {
            return Err(NativeResourceError::Unaccounted);
        }
        self.owned(table_bytes::<(K, V)>(high_water)?)?;
        for (key, value) in values {
            key.native_visit(self)?;
            value.native_visit(self)?;
        }
        Ok(())
    }
}
macro_rules! leaf {
    ($($ty:ty),* $(,)?) => { $(impl NativeVisit for $ty { fn native_visit(&self, _: &mut Visitor<'_>) -> Result { Ok(()) } })* };
}
leaf!(
    (),
    u8,
    u16,
    u32,
    u64,
    usize,
    bool,
    char,
    i64,
    std::num::NonZeroU32,
    crate::ChangeHash,
    crate::types::OpId,
    crate::types::ObjId,
    hexane::SlabWeight
);
impl<T: NativeVisit> NativeVisit for Vec<T> {
    fn native_visit(&self, visitor: &mut Visitor<'_>) -> Result {
        visitor.owned(mul(self.capacity(), size_of::<T>())?)?;
        for value in self {
            value.native_visit(visitor)?;
        }
        Ok(())
    }
}
impl<T: NativeVisit> NativeVisit for Option<T> {
    fn native_visit(&self, visitor: &mut Visitor<'_>) -> Result {
        if let Some(value) = self {
            value.native_visit(visitor)?;
        }
        Ok(())
    }
}
impl NativeVisit for String {
    fn native_visit(&self, visitor: &mut Visitor<'_>) -> Result {
        visitor.owned(self.capacity())
    }
}
impl NativeVisit for Cow<'_, str> {
    fn native_visit(&self, visitor: &mut Visitor<'_>) -> Result {
        if let Cow::Owned(value) = self {
            value.native_visit(visitor)?;
        }
        Ok(())
    }
}
impl NativeVisit for Cow<'_, [u8]> {
    fn native_visit(&self, visitor: &mut Visitor<'_>) -> Result {
        if let Cow::Owned(value) = self {
            value.native_visit(visitor)?;
        }
        Ok(())
    }
}
impl<T: NativeVisit + Clone> NativeVisit for Cow<'_, T> {
    fn native_visit(&self, visitor: &mut Visitor<'_>) -> Result {
        if let Cow::Owned(value) = self {
            value.native_visit(visitor)?;
        }
        Ok(())
    }
}
impl<A: NativeVisit, B: NativeVisit> NativeVisit for (A, B) {
    fn native_visit(&self, v: &mut Visitor<'_>) -> Result {
        self.0.native_visit(v)?;
        self.1.native_visit(v)
    }
}
impl<A: NativeVisit, B: NativeVisit, C: NativeVisit> NativeVisit for (A, B, C) {
    fn native_visit(&self, v: &mut Visitor<'_>) -> Result {
        self.0.native_visit(v)?;
        self.1.native_visit(v)?;
        self.2.native_visit(v)
    }
}
impl<A: NativeVisit, B: NativeVisit, C: NativeVisit, D: NativeVisit> NativeVisit for (A, B, C, D) {
    fn native_visit(&self, v: &mut Visitor<'_>) -> Result {
        self.0.native_visit(v)?;
        self.1.native_visit(v)?;
        self.2.native_visit(v)?;
        self.3.native_visit(v)
    }
}
impl<K: NativeVisit, V: NativeVisit> NativeVisit for BTreeMap<K, V> {
    fn native_visit(&self, visitor: &mut Visitor<'_>) -> Result {
        visitor.owned(btree_bytes::<K, V>(self.len())?)?;
        for (key, value) in self {
            key.native_visit(visitor)?;
            value.native_visit(visitor)?;
        }
        Ok(())
    }
}
impl<T: NativeVisit> NativeVisit for BTreeSet<T> {
    fn native_visit(&self, visitor: &mut Visitor<'_>) -> Result {
        visitor.owned(btree_bytes::<T, ()>(self.len())?)?;
        for value in self {
            value.native_visit(visitor)?;
        }
        Ok(())
    }
}
impl NativeVisit for SmolStr {
    fn native_visit(&self, visitor: &mut Visitor<'_>) -> Result {
        // smol_str 0.3.6 heap representation is Arc<str>; inline and static
        // variants allocate nothing. Clone retains this exact strong backing.
        if self.is_heap_allocated() {
            visitor.shared(Backing::String(self.clone()))?;
        }
        Ok(())
    }
}
impl NativeVisit for Arc<MarkSet> {
    fn native_visit(&self, visitor: &mut Visitor<'_>) -> Result {
        let before = visitor.owned;
        self.as_ref().native_visit(visitor)?;
        let bytes = add(arc_bytes::<MarkSet>()?, visitor.owned - before)?;
        visitor.owned = before;
        visitor.shared(Backing::Mark {
            value: Arc::clone(self),
            bytes,
        })
    }
}
impl<C: ColumnCursor> NativeVisit for ColumnData<C>
where
    C::SlabIndex: NativeVisit,
{
    fn native_visit(&self, visitor: &mut Visitor<'_>) -> Result {
        // This feature adds retained debug exports with a separate ownership
        // representation. It is deliberately unqualified until visited too.
        if self.has_diagnostic_exports() {
            return Err(NativeResourceError::Unaccounted);
        }
        self.slabs.visit_allocations(|allocation| match allocation {
            hexane::tree::TreeAllocation::OwnedBytes(bytes) => visitor.owned(bytes),
            hexane::tree::TreeAllocation::Element(slab) => {
                visitor.shared(Backing::Bytes(slab.shared_backing()))
            }
            hexane::tree::TreeAllocation::Weight(weight) => weight.native_visit(visitor),
        })
    }
}

struct Plan {
    owned: usize,
    backing: Vec<Backing>,
    _metadata: NativeReservation,
}
impl Plan {
    fn inspect(value: &impl NativeVisit, allowance: &NativeAllowance) -> Result<Self> {
        Self::inspect_with(value, |bytes| allowance.reserve(bytes))
    }
    fn inspect_with(
        value: &impl NativeVisit,
        reserve: impl FnOnce(usize) -> Result<NativeReservation>,
    ) -> Result<Self> {
        let mut count = Visitor {
            owned: 0,
            references: 0,
            backing: None,
        };
        value.native_visit(&mut count)?;
        let metadata = reserve(mul(count.references, size_of::<Backing>())?)?;
        let mut backing = Vec::new();
        backing
            .try_reserve_exact(count.references)
            .map_err(|_| NativeResourceError::WorkBytes)?;
        if mul(backing.capacity(), size_of::<Backing>())? > metadata.bytes() {
            return Err(NativeResourceError::Unaccounted);
        }
        let mut collect = Visitor {
            owned: 0,
            references: 0,
            backing: Some(&mut backing),
        };
        value.native_visit(&mut collect)?;
        if collect.owned != count.owned || collect.references != count.references {
            return Err(NativeResourceError::Unaccounted);
        }
        backing.sort_unstable_by_key(Backing::id);
        backing.dedup_by_key(|value| value.id());
        Ok(Self {
            owned: count.owned,
            backing,
            _metadata: metadata,
        })
    }
    fn shared_bytes(&self) -> Result<usize> {
        self.backing
            .iter()
            .try_fold(0, |total, item| add(total, item.bytes()?))
    }
}

/// Read-only retained allocation bounds. Producing this measurement is not
/// qualification and does not reserve the existing document's storage.
#[derive(Debug)]
pub struct NativeFootprint {
    owned: usize,
    shared: usize,
}
impl NativeFootprint {
    /// Owned allocation bound, excluding shared backing allocations.
    pub fn owned_bytes(&self) -> usize {
        self.owned
    }
    /// Unique shared backing allocation bytes.
    pub fn shared_bytes(&self) -> usize {
        self.shared
    }
    /// Total retained allocation bound.
    pub fn total_bytes(&self) -> usize {
        self.owned + self.shared
    }
}
struct SharedLease {
    backing: Backing,
    _reservation: NativeReservation,
}
// Every strong domain handle has an external metadata reservation. Its Arc
// field drops before that reservation, covering the control block through its
// actual last deallocation; no reservation lives inside its own allocation.
struct DomainRef {
    identity: Arc<()>,
    _metadata: NativeReservation,
}
impl DomainRef {
    fn new(allowance: &NativeAllowance) -> Result<Self> {
        let metadata = allowance.reserve(arc_bytes::<()>()?)?;
        Ok(Self {
            identity: Arc::new(()),
            _metadata: metadata,
        })
    }
    fn copy_in(&self, allowance: &NativeAllowance) -> Result<Self> {
        let metadata = allowance.reserve(arc_bytes::<()>()?)?;
        Ok(Self {
            identity: Arc::clone(&self.identity),
            _metadata: metadata,
        })
    }
    fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
    }
}
/// Additional native work for one turn, branded to a private owner registry.
/// The embedding owner must validate the actual HostBytes pool and preserve
/// its credit; this native budget token is not authorization.
pub struct NativeWorkTurn {
    domain: DomainRef,
    allowance: NativeAllowance,
}
impl NativeWorkTurn {
    /// Inspect and normalize complete storage under this exact current ledger.
    /// This creates checked input, not native owner or installation authority.
    pub fn normalize_input<'a>(
        &self,
        input: &'a [u8],
        limits: &crate::resource_limits::NativeLimits,
    ) -> Result<crate::resource_limits::NormalizedStorage<'a>> {
        let checked = crate::AutoCommit::inspect_bounded(
            input,
            limits,
            &mut self.allowance.clone(),
        )?;
        checked.normalize(&self.allowance)
    }

    pub(super) fn accepts_reservation(&self, reservation: &NativeReservation) -> bool {
        Arc::ptr_eq(&self.allowance.ledger, &reservation.ledger)
    }
    /// Reserves bounded embedding output or metadata in this exact work turn.
    /// The caller must retain the reservation until that allocation is dropped.
    /// This is budget custody only, not authorization or native qualification.
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
/// Reconciliation retires only registry entries with no private custody lease.
/// It never interprets the backing's own Arc count as authority.
pub struct NativeAllocationSet {
    domain: DomainRef,
    entries: Vec<Arc<SharedLease>>,
    metadata: Option<NativeReservation>,
}
impl NativeAllocationSet {
    /// Reserves this registry's private domain identity in the initial turn.
    pub fn new(initial: NativeAllowance) -> Result<Self> {
        Ok(Self {
            domain: DomainRef::new(&initial)?,
            entries: Vec::new(),
            metadata: None,
        })
    }
    /// Brands an embedding-owner work allowance to this registry. Old resident
    /// leases retain their original credit; new work uses this turn's ledger.
    pub fn work_turn(&self, allowance: NativeAllowance) -> Result<NativeWorkTurn> {
        Ok(NativeWorkTurn {
            domain: self.domain.copy_in(&allowance)?,
            allowance,
        })
    }
    pub(super) fn check_turn<'a>(&self, turn: &'a NativeWorkTurn) -> Result<&'a NativeAllowance> {
        if !self.domain.matches(&turn.domain) {
            return Err(NativeResourceError::Unaccounted);
        }
        Ok(&turn.allowance)
    }
    /// Releases backing entries after every guarded value using them was
    /// dropped. Lease Arcs are private, have no Weak handles, and can only be
    /// cloned while this registry is exclusively borrowed; concurrent drops
    /// can only reduce their count. The backing's Arc count is never consulted.
    pub fn reconcile(&mut self) {
        self.entries.retain(|entry| Arc::strong_count(entry) > 1);
        if self.entries.is_empty() {
            self.entries = Vec::new();
            self.metadata = None;
        }
    }
}
/// Credit retained alongside a native value; it is not independently extractable.
pub struct NativeRetention {
    _leases: Vec<Arc<SharedLease>>,
    _owned: OwnedCredits,
    _metadata: NativeReservation,
}
/// A native value and inseparable retained credit. No raw native reference,
/// owning extraction, Clone, or unchecked mutable operation is exposed.
pub struct NativeOwned<T> {
    value: T,
    domain: DomainRef,
    _retention: NativeRetention,
}
/// A measured clone of an unqualified source. Shared backing may still be
/// held by unqualified source clones, so this is only copy-work custody and
/// is never native admission, readiness, or a conversion to `NativeOwned`.
pub struct NativeCloneCandidate<T> {
    value: T,
    domain: DomainRef,
    _retention: NativeRetention,
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
    fn check_turn<'a>(&self, turn: &'a NativeWorkTurn) -> Result<&'a NativeAllowance> {
        if !self.domain.matches(&turn.domain) {
            return Err(NativeResourceError::Unaccounted);
        }
        Ok(&turn.allowance)
    }
}
impl NativeOwned<crate::AutoCommit> {
    /// Number of operations in the still-open transaction (no allocation).
    pub fn pending_ops(&self) -> usize {
        self.value.pending_ops()
    }
    /// Clone through the owner's shared allocation registry.
    pub fn clone_bounded(
        &self,
        allocations: &mut NativeAllocationSet,
        turn: &NativeWorkTurn,
    ) -> Result<Self> {
        self.check_turn(turn)?;
        let cloned = self.value.clone_bounded(allocations, turn)?;
        // This source is already private qualified custody. No native value,
        // shared backing or unchecked clone can escape the NativeOwned API.
        Ok(Self {
            value: cloned.value,
            domain: cloned.domain,
            _retention: cloned._retention,
        })
    }
    fn check_turn<'a>(&self, turn: &'a NativeWorkTurn) -> Result<&'a NativeAllowance> {
        if !self.domain.matches(&turn.domain) {
            return Err(NativeResourceError::Unaccounted);
        }
        Ok(&turn.allowance)
    }
}
fn split(reservation: &mut NativeReservation, bytes: usize) -> NativeReservation {
    assert!(bytes <= reservation.bytes);
    reservation.bytes -= bytes;
    NativeReservation {
        ledger: Arc::clone(&reservation.ledger),
        bytes,
        host: reservation.host.clone(),
    }
}

/// Private reconstruction planning only: the caller proves this upper bound
/// on all shared references from checked input before starting native work.
pub(super) fn adoption_extra_bytes(
    references: usize,
    allocations: &NativeAllocationSet,
) -> Result<usize> {
    let lease_slot = add(size_of::<Arc<SharedLease>>(), arc_bytes::<SharedLease>()?)?;
    let slots = add(add(allocations.entries.len(), references)?, references)?;
    add(
        add(
            mul(references, size_of::<Backing>())?,
            mul(slots, lease_slot)?,
        )?,
        arc_bytes::<()>()?,
    )
}

/// Reserved private handoff for detached checked reconstruction. No public
/// unchecked native source can obtain this construction authority.
pub(super) fn adopt_loaded(
    value: crate::AutoCommit,
    allocations: &mut NativeAllocationSet,
    turn: &NativeWorkTurn,
    peak: NativeReservation,
) -> Result<NativeOwned<crate::AutoCommit>> {
    struct Pending {
        value: Option<crate::AutoCommit>,
        peak: NativeReservation,
    }
    let mut pending = Pending {
        value: Some(value),
        peak,
    };
    let allowance = allocations.check_turn(turn)?;
    if !Arc::ptr_eq(&pending.peak.ledger, &allowance.ledger) {
        return Err(NativeResourceError::Unaccounted);
    }
    let plan = Plan::inspect_with(pending.value.as_ref().unwrap(), |bytes| {
        if bytes > pending.peak.bytes() {
            return Err(NativeResourceError::WorkBytes);
        }
        Ok(split(&mut pending.peak, bytes))
    })?;
    let layout = retention_layout(&plan, allocations)?;
    let (domain, retention) = retain_plan(&plan, allocations, turn, &mut pending.peak, layout)?;
    // Every real allocation now has transferred custody. Planning backing
    // handles and their Vec die before excess reconstruction credit is freed.
    drop(plan);
    let value = pending.value.take().unwrap();
    Ok(NativeOwned {
        value,
        domain,
        _retention: retention,
    })
}

struct RetentionLayout {
    registry_capacity: usize,
    registry_bytes: usize,
    retention_bytes: usize,
    total: usize,
}
fn retention_layout(plan: &Plan, allocations: &NativeAllocationSet) -> Result<RetentionLayout> {
    retention_layout_with_owned_credit(plan, allocations, 0)
}

// `owned_credit` is available only from the same consuming private value's
// actual OwnedCredits. It never describes credit held by another native clone.
fn retention_layout_with_owned_credit(
    plan: &Plan,
    allocations: &NativeAllocationSet,
    owned_credit: usize,
) -> Result<RetentionLayout> {
    let mut new_count = 0;
    let mut new_bytes = 0;
    for backing in &plan.backing {
        if !allocations
            .entries
            .iter()
            .any(|entry| entry.backing.id() == backing.id())
        {
            new_count = add(new_count, 1)?;
            new_bytes = add(new_bytes, backing.bytes()?)?;
        }
    }
    let registry_capacity = add(allocations.entries.len(), new_count)?;
    let lease_slot = add(size_of::<Arc<SharedLease>>(), arc_bytes::<SharedLease>()?)?;
    let registry_bytes = if registry_capacity > allocations.entries.capacity() {
        mul(registry_capacity, lease_slot)?
    } else {
        0
    };
    let retention_bytes = mul(plan.backing.len(), lease_slot)?;
    let total = add(
        add(plan.owned.saturating_sub(owned_credit), new_bytes)?,
        add(add(registry_bytes, retention_bytes)?, arc_bytes::<()>()?)?,
    )?;
    Ok(RetentionLayout {
        registry_capacity,
        registry_bytes,
        retention_bytes,
        total,
    })
}

fn retain_plan(
    plan: &Plan,
    allocations: &mut NativeAllocationSet,
    turn: &NativeWorkTurn,
    reservation: &mut NativeReservation,
    layout: RetentionLayout,
) -> Result<(DomainRef, NativeRetention)> {
    let (domain, shared) = retain_shared_plan(plan, allocations, turn, reservation, layout)?;
    let owned = split(reservation, plan.owned);
    Ok((
        domain,
        NativeRetention {
            _leases: shared.leases,
            _owned: OwnedCredits::new(owned),
            _metadata: shared.metadata,
        },
    ))
}

// Shared lease metadata is kept separate from unique owned credit. This
// private factor grants no native construction or transformation authority.
struct SharedRetention {
    leases: Vec<Arc<SharedLease>>,
    metadata: NativeReservation,
}

fn retain_shared_plan(
    plan: &Plan,
    allocations: &mut NativeAllocationSet,
    turn: &NativeWorkTurn,
    reservation: &mut NativeReservation,
    layout: RetentionLayout,
) -> Result<(DomainRef, SharedRetention)> {
    if reservation.bytes() < layout.total {
        return Err(NativeResourceError::WorkBytes);
    }
    let domain = DomainRef {
        identity: Arc::clone(&turn.domain.identity),
        _metadata: split(reservation, arc_bytes::<()>()?),
    };
    let retention_metadata = split(reservation, layout.retention_bytes);
    let mut leases = Vec::new();
    leases
        .try_reserve_exact(plan.backing.len())
        .map_err(|_| NativeResourceError::WorkBytes)?;
    if mul(leases.capacity(), size_of::<Arc<SharedLease>>())? > retention_metadata.bytes() {
        return Err(NativeResourceError::Unaccounted);
    }
    if layout.registry_bytes != 0 {
        let metadata = split(reservation, layout.registry_bytes);
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(layout.registry_capacity)
            .map_err(|_| NativeResourceError::WorkBytes)?;
        if mul(entries.capacity(), size_of::<Arc<SharedLease>>())? > metadata.bytes() {
            return Err(NativeResourceError::Unaccounted);
        }
        entries.append(&mut allocations.entries);
        allocations.entries = entries;
        allocations.metadata = Some(metadata);
    }
    // All fallible allocation and capacity checks are complete. Resident
    // bytes remained in the caller's peak guard through those error paths.
    // Backing sizes were validated in retention_layout and cannot change:
    // this plan holds strong immutable handles for every identity.
    for backing in &plan.backing {
        let lease = if let Some(existing) = allocations
            .entries
            .iter()
            .find(|entry| entry.backing.id() == backing.id())
        {
            Arc::clone(existing)
        } else {
            let bytes = backing
                .bytes()
                .expect("immutable backing size was validated before retention allocation");
            let charge = split(reservation, bytes);
            let lease = Arc::new(SharedLease {
                backing: backing.clone(),
                _reservation: charge,
            });
            allocations.entries.push(Arc::clone(&lease));
            lease
        };
        leases.push(lease);
    }
    Ok((
        domain,
        SharedRetention {
            leases,
            metadata: retention_metadata,
        },
    ))
}
impl crate::AutoCommit {
    /// Measures retained allocations without closing a transaction or changing caches.
    pub fn native_footprint(&self, allowance: &NativeAllowance) -> Result<NativeFootprint> {
        let plan = Plan::inspect(self, allowance)?;
        let shared = plan.shared_bytes()?;
        add(plan.owned, shared)?;
        Ok(NativeFootprint {
            owned: plan.owned,
            shared,
        })
    }
    /// Clones after reserving all deep-copy allocations, unique shared backing,
    /// registry growth, and retention metadata. The legacy source remains
    /// unqualified; the result is also explicitly unqualified and exposes no
    /// raw shared-backed values. Detached checked load is a separate boundary.
    pub fn clone_bounded(
        &self,
        allocations: &mut NativeAllocationSet,
        turn: &NativeWorkTurn,
    ) -> Result<NativeCloneCandidate<Self>> {
        let allowance = allocations.check_turn(turn)?;
        let plan = Plan::inspect(self, allowance)?;
        let layout = retention_layout(&plan, allocations)?;
        let mut reservation = allowance.reserve(layout.total)?;
        let (domain, retention) = retain_plan(&plan, allocations, turn, &mut reservation, layout)?;
        // Derived Clone copies each owned vector/string no larger than its
        // source capacity. Pinned RawTable Clone retains its bucket count;
        // BTree Clone allocates nodes covered by the structural node bound.
        let value = self.clone();
        Ok(NativeCloneCandidate {
            value,
            domain,
            _retention: retention,
        })
    }
}

impl std::fmt::Debug for NativeAllocationSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeAllocationSet")
            .field("shared_backings", &self.entries.len())
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for NativeRetention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeRetention")
            .field("shared_backings", &self._leases.len())
            .finish_non_exhaustive()
    }
}
impl<T> std::fmt::Debug for NativeOwned<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeOwned")
            .field("retention", &self._retention)
            .finish_non_exhaustive()
    }
}
impl<T> std::fmt::Debug for NativeCloneCandidate<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeCloneCandidate")
            .field("retention", &self._retention)
            .finish_non_exhaustive()
    }
}
