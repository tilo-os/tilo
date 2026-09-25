//! Explicit bounds for inspecting native storage before reconstruction.
mod inspect;
mod normalize;
pub(crate) mod reconstruct;
mod structure;
pub(crate) mod retained;
pub use retained::{NativeAllocationSet, NativeApplyResult, NativeIncomingOutcome, NativeChanges, NativeCloneCandidate, NativeDependencyPolicy, NativeFootprint, NativeHashes, NativeMutationError, NativeOwned, NativeRetention, NativeSaved, NativeWorkTurn, NativeId, NativeListEntry, NativeObject, NativeReadError, NativeReadSession, NativeReadSteps, NativeScalar, NativeSelection, NativeValue};
pub use hexane::{SharedBytes, SharedBytesId};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Policy ceilings for a bounded native operation.
#[derive(Clone, Debug)]
pub struct NativeLimits {
    /// Sum of normalized native bytes across chunks and columns.
    pub max_normalized_bytes: usize,
    /// Sum of logical operations across the complete input.
    pub max_operations: usize,
    /// Largest absolute native operation counter, independent of batch rows.
    pub max_operation_counter: u64,
    /// Total changes represented by the complete input.
    pub max_changes: usize,
    /// Total predecessor, successor and change-dependency references.
    pub max_edges: usize,
    /// Total actor entries in storage prefixes, including repeated entries.
    pub max_actors: usize,
    /// Total stored heads/dependency hashes in storage prefixes.
    pub max_heads: usize,
    /// Maximum column descriptors in a column group (at most 128).
    pub max_columns: usize,
    /// Maximum number of concatenated chunks.
    pub max_chunks: usize,
    /// Total expanded strings and value bytes represented by columns.
    pub max_value_bytes: usize,
}
impl Default for NativeLimits {
    fn default() -> Self {
        Self {
            max_normalized_bytes: 8 * 1024 * 1024,
            max_operations: 65_536,
            max_operation_counter: 65_536,
            max_changes: 65_536,
            max_edges: 262_144,
            max_actors: 1_024,
            max_heads: 4_096,
            max_columns: 128,
            max_chunks: 1_024,
            max_value_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Closed native inspection/resource failures.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NativeResourceError {
    /// The native state contains allocation ownership that cannot be measured.
    #[error("native allocation state cannot be accounted")]
    Unaccounted,
    /// Normalized native input exceeds its ceiling.
    #[error("normalized native input exceeds its ceiling")]
    NormalizedBytes,
    /// Logical operation count exceeds its ceiling.
    #[error("native operation count exceeds its ceiling")]
    Operations,
    /// A bounded native collection cannot hold the represented cardinality.
    #[error("native collection cardinality exceeds its ceiling")]
    Collection,
    /// Expanded values exceed their explicit ceiling.
    #[error("native value bytes exceed their ceiling")]
    ValueBytes,
    /// Work cannot fit the caller's already reserved allowance.
    #[error("native work allowance exhausted")]
    WorkBytes,
    /// The complete storage input is malformed.
    #[error("invalid native storage")]
    Invalid,
}

/// A subdivision of memory already reserved by the embedding owner.
/// Construction does not reserve host memory or establish authorization.
/// Host-backed construction retains the embedding owner's actual credit.
#[derive(Clone, Debug)]
pub struct NativeAllowance {
    ledger: Arc<Ledger>,
    // Drops after the ledger Arc, so final native metadata stays host-funded.
    host: Option<HostKeeper>,
}
#[derive(Debug)]
struct Ledger {
    limit: usize,
    used: AtomicUsize,
}
// All supported allowance limits fit below this bit. Sealing and reservation
// use one atomic word, so a racing reservation cannot start after a turn ends.
const TURN_FINISHED: usize = 1usize << (usize::BITS - 1);
const RETAINED_MASK: usize = TURN_FINISHED - 1;

/// Opaque custody of memory already reserved by the embedding owner.
///
/// This is a budget mechanism, not proof of host identity or authorization.
/// The embedding owner must validate its pool and reserve this concrete boxed
/// keeper before constructing it. Calls must not allocate, panic or reenter
/// native accounting. The native layer never exposes a retained keeper.
pub trait NativeHostCredit: Send + Sync {
    /// Called only after the native ledger is sealed. Retain at least these
    /// native bytes plus this keeper's concrete Box allocation. Concurrent
    /// calls may arrive out of order; shrink monotonically, never grow.
    fn shrink_to(&self, native_retained_bytes: usize);

    /// Deallocate this concrete Box before releasing its final host credit.
    /// For example, take the credit out, drop the now-empty Box, then drop the
    /// credit. The containing native keeper Arc has already been deallocated.
    fn release(self: Box<Self>);
}

#[derive(Clone)]
struct HostKeeper {
    // No Weak handles or raw Arc escapes. Every strong holder uses into_inner
    // on drop, so the final caller receives the Box after this Arc is freed.
    inner: Option<Arc<Box<dyn NativeHostCredit>>>,
}
impl std::fmt::Debug for HostKeeper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostKeeper").finish_non_exhaustive()
    }
}
impl HostKeeper {
    fn shrink_to(&self, retained: usize) {
        if let Some(inner) = &self.inner {
            inner.shrink_to(retained);
        }
    }
}
impl Drop for HostKeeper {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            if let Some(credit) = Arc::into_inner(inner) {
                credit.release();
            }
        }
    }
}

/// Retains a subdivision of its allowance until dropped.
/// A hosted allowance's actual host credit remains alive with each reservation.
/// Standalone allowances are budget mechanisms only.
#[derive(Debug)]
pub struct NativeReservation {
    ledger: Arc<Ledger>,
    bytes: usize,
    host: Option<HostKeeper>,
}
impl NativeReservation {
    /// The requested allocation bytes covered by this reservation.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}
impl Drop for NativeReservation {
    fn drop(&mut self) {
        let previous = self.ledger.used.fetch_sub(self.bytes, Ordering::AcqRel);
        if previous & TURN_FINISHED != 0 {
            if let Some(host) = &self.host {
                host.shrink_to((previous & RETAINED_MASK) - self.bytes);
            }
        }
    }
}
impl NativeAllowance {
    /// Binds the owner's actual reserved credit to all derived reservations.
    /// `reserved_bytes` excludes the concrete host keeper Box, which the host
    /// must fund separately. Native ledger and keeper Arc metadata are included.
    pub fn with_host_credit(
        reserved_bytes: usize,
        credit: Box<dyn NativeHostCredit>,
    ) -> Result<Self, NativeResourceError> {
        Self::with_optional_credit(reserved_bytes, Some(credit))
    }

    /// Creates an allowance within the initial native work policy ceiling.
    pub fn new(reserved_bytes: usize) -> Result<Self, NativeResourceError> {
        Self::with_optional_credit(reserved_bytes, None)
    }

    fn with_optional_credit(
        reserved_bytes: usize,
        credit: Option<Box<dyn NativeHostCredit>>,
    ) -> Result<Self, NativeResourceError> {
        // Arc's two atomic reference counts and the ledger itself remain live
        // for the allowance and every retained reservation derived from it.
        let mut ledger_bytes = std::mem::size_of::<Ledger>() + 2 * std::mem::size_of::<usize>();
        if credit.is_some() {
            // Pinned ArcInner repr(C, align(2)): two usize atomics followed by
            // the sized fat-pointer Box. All fields are usize-aligned here.
            ledger_bytes +=
                std::mem::size_of::<Box<dyn NativeHostCredit>>() + 2 * std::mem::size_of::<usize>();
        }
        if reserved_bytes < ledger_bytes || reserved_bytes > 64 * 1024 * 1024 {
            if let Some(credit) = credit {
                credit.release();
            }
            return Err(NativeResourceError::WorkBytes);
        }
        // Both allocations are covered before allocation by the caller's
        // genuine host credit (when present), including the recorded metadata.
        let host = credit.map(|credit| HostKeeper {
            inner: Some(Arc::new(credit)),
        });
        Ok(Self {
            ledger: Arc::new(Ledger {
                limit: reserved_bytes,
                used: AtomicUsize::new(ledger_bytes),
            }),
            host,
        })
    }
    /// Reserves work from the ledger shared by all clones of this allowance.
    pub fn reserve(&self, bytes: usize) -> Result<NativeReservation, NativeResourceError> {
        self.ledger
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                if used & TURN_FINISHED != 0 {
                    None
                } else {
                    used.checked_add(bytes)
                        .filter(|sum| *sum <= self.ledger.limit)
                }
            })
            .map_err(|_| NativeResourceError::WorkBytes)?;
        Ok(NativeReservation {
            ledger: Arc::clone(&self.ledger),
            bytes,
            host: self.host.clone(),
        })
    }
    /// Current unreserved bytes; a subsequent reservation still checks atomically.
    pub fn available_bytes(&self) -> usize {
        let used = self.ledger.used.load(Ordering::Acquire);
        if used & TURN_FINISHED != 0 {
            0
        } else {
            self.ledger.limit - used
        }
    }

    /// Ends additional work on this allowance; existing reservations remain live.
    pub fn finish_turn(&self) {
        let previous = self.ledger.used.fetch_or(TURN_FINISHED, Ordering::AcqRel);
        if let Some(host) = &self.host {
            host.shrink_to(previous & RETAINED_MASK);
        }
    }

    /// Bytes still retained by native reservations and ledger metadata.
    pub fn retained_bytes(&self) -> usize {
        self.ledger.used.load(Ordering::Acquire) & RETAINED_MASK
    }
}

/// Structural measurements from native storage inspection.
#[derive(Debug)]
pub struct NativeShape {
    operations: usize,
    normalized_bytes: usize,
    changes: usize,
    edges: usize,
    actors: usize,
    heads: usize,
    value_bytes: usize,
    dense_operations: usize,
    maximum_operation_counter: u64,
}
impl NativeShape {
    fn empty() -> Self {
        Self { operations: 0, normalized_bytes: 0, changes: 0, edges: 0,
            actors: 0, heads: 0, value_bytes: 0, dense_operations: 0,
            maximum_operation_counter: 0 }
    }
    /// Total logical operations represented by the inspected chunks.
    pub fn operations(&self) -> usize {
        self.operations
    }
    /// Size of normalized storage, including native framing and metadata.
    pub fn normalized_bytes(&self) -> usize {
        self.normalized_bytes
    }
    /// Aggregate dense operation slots required by native change collectors.
    pub fn dense_operations(&self) -> usize { self.dense_operations }
    /// Largest absolute operation counter seen in native change metadata.
    pub fn maximum_operation_counter(&self) -> u64 { self.maximum_operation_counter }
}

/// Inspection evidence bound to the unchanged original bytes.
/// This is not admission, authority, or durability evidence.
#[derive(Debug)]
pub struct CheckedStorage<'a> {
    input: &'a [u8],
    shape: NativeShape,
}
impl<'a> CheckedStorage<'a> {
    /// The exact original storage bytes, including their original compression.
    pub fn original_bytes(&self) -> &[u8] {
        self.input
    }
    /// The measurements validated by inspection.
    pub fn shape(&self) -> &NativeShape {
        &self.shape
    }

    /// Normalizes validated storage under retained caller credit. This does not
    /// reconstruct a native document or establish semantic validity.
    pub fn normalize(
        self,
        allowance: &NativeAllowance,
    ) -> Result<NormalizedStorage<'a>, NativeResourceError> {
        let reservation = allowance.reserve(self.shape.normalized_bytes)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(self.shape.normalized_bytes)
            .map_err(|_| NativeResourceError::WorkBytes)?;
        normalize::normalize(
            self.input,
            &mut bytes,
            self.shape.normalized_bytes,
            allowance,
        )?;
        Ok(NormalizedStorage {
            original: self.input,
            bytes,
            shape: self.shape,
            _reservation: reservation,
        })
    }
}

/// Owned normalized storage, original-byte identity and retained work credit.
/// It is not authority, durability or native reconstruction evidence.
#[derive(Debug)]
pub struct NormalizedStorage<'a> {
    original: &'a [u8],
    bytes: Vec<u8>,
    shape: NativeShape,
    _reservation: NativeReservation,
}
impl NormalizedStorage<'_> {
    /// The immutable normalized native representation.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// The exact original input; normalization never rewrites its identity.
    pub fn original_bytes(&self) -> &[u8] {
        self.original
    }
    /// The validated structural measurements.
    pub fn shape(&self) -> &NativeShape {
        &self.shape
    }
}

impl crate::AutoCommit {
    /// Inspect complete native input under explicit resource ceilings.
    pub fn inspect_bounded<'a>(
        input: &'a [u8],
        limits: &NativeLimits,
        allowance: &mut NativeAllowance,
    ) -> Result<CheckedStorage<'a>, NativeResourceError> {
        let shape = inspect::inspect(input, limits, allowance)?;
        Ok(CheckedStorage { input, shape })
    }
}
