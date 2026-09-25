//! Strong ownership handles for accounting shared byte buffers.
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

// Pinned Rust 1.97.1 alloc::sync::ArcInner uses this C layout. The byte
// vector's capacity is a second allocation. Neither is an RSS measurement.
#[repr(C, align(2))]
pub(crate) struct ArcVecLayout {
    _strong: AtomicUsize,
    _weak: AtomicUsize,
    _data: Vec<u8>,
}

/// Concrete column-construction allocations for a caller's checked counts.
/// This is a layout calculation, not an allocation or admission capability.
#[derive(Debug, Clone, Copy)]
pub struct ColumnAllocation {
    /// Writer actions, result vector, slab bytes/Arc headers, tree buffers and
    /// search scratch. Nested owned values and weight maps are excluded.
    pub bytes: usize,
    /// Maximum distinct slab backings retained by this construction.
    pub backings: usize,
    /// Maximum tree nodes; owners use it for nested weight collections.
    pub weight_nodes: usize,
}

/// Reports feature-unified diagnostic storage without constructing a column.
pub const fn diagnostic_exports_enabled() -> bool {
    cfg!(feature = "slow_path_assertions")
}

/// Bound a column load/encoding from explicit event and encoded-output limits.
/// Every slab boundary consumes an event; the smallest regular slab byte
/// threshold is 64. The extra slab covers a final partial slab. Empty columns
/// can retain the tree's default empty Arc-backed slab.
pub fn column_construction_allocation<C: crate::ColumnCursor>(
    events: usize,
    encoded_bytes: usize,
) -> Option<ColumnAllocation> {
    // An unlocked copy can close a short slab before the next copy overflows
    // 64 bytes; each pair of adjacent slabs then spans at least 64 bytes.
    // Locked loaders/manual breaks have a tighter bound. Cover both paths.
    let slabs = events
        .checked_add(1)?
        .min((encoded_bytes / 64).checked_mul(2)?.checked_add(2)?)
        .max(1);
    let mut bytes =
        crate::slab::writer::SlabWriter::<C::Item>::construction_allocation_bound(events, slabs)?;
    // RLE and delta Encoder states retain a literal Vec while SlabWriter owns
    // earlier actions. A literal run has at most one Cow per supplied event.
    // Count its current and overlapping old RawVec allocations explicitly;
    // Boolean/raw states need no such buffer but fit this conservative layout.
    if events != 0 {
        let literals = events.checked_mul(2)?.max(4).checked_add(events)?;
        bytes = bytes.checked_add(
            literals.checked_mul(std::mem::size_of::<std::borrow::Cow<'_, C::Item>>())?,
        )?;
    }
    bytes = bytes.checked_add(encoded_bytes)?;
    bytes = bytes.checked_add(slabs.checked_mul(std::mem::size_of::<ArcVecLayout>())?)?;
    bytes = bytes
        .checked_add(crate::SlabTree::<C::SlabIndex>::construction_allocation_bound(slabs)?)?;
    Some(ColumnAllocation {
        bytes,
        backings: slabs,
        weight_nodes: (slabs / 15).checked_add(2)?,
    })
}

/// Raw columns make one exact copied byte buffer and one slab/tree, without a
/// typed SlabWriter action stream.
pub fn raw_column_construction_allocation(bytes: usize) -> Option<ColumnAllocation> {
    Some(ColumnAllocation {
        bytes: bytes
            .checked_add(std::mem::size_of::<ArcVecLayout>())?
            .checked_add(crate::SlabTree::<crate::SlabWeight>::construction_allocation_bound(1)?)?,
        backings: 1,
        weight_nodes: 2,
    })
}

/// Identity of one live Arc-backed byte allocation group.
/// Keep its SharedBytes handle alive while retaining this identity in a ledger.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SharedBytesId(usize);

/// Keeps the backing alive without copying its byte buffer.
/// This is ownership/accounting metadata, not an authorization capability.
#[derive(Clone)]
pub struct SharedBytes(Arc<Vec<u8>>);

impl std::fmt::Debug for SharedBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedBytes")
            .field("identity", &self.identity())
            .field("allocation_bytes", &self.allocation_bytes())
            .finish()
    }
}

impl SharedBytes {
    /// Takes a strong handle to an existing backing without allocation.
    pub fn from_arc(bytes: Arc<Vec<u8>>) -> Self {
        Self(bytes)
    }

    /// Identity remains valid while a strong handle to this backing is retained.
    pub fn identity(&self) -> SharedBytesId {
        SharedBytesId(Arc::as_ptr(&self.0) as usize)
    }

    /// Requested bytes for both the byte vector and its Arc allocation.
    pub fn allocation_bytes(&self) -> usize {
        self.0.capacity() + std::mem::size_of::<ArcVecLayout>()
    }
}
