//! Paid retirement synchronization, never Source, caller or disclosure authority.
//!
//! Supplied reservations fund only this primitive. The embedding owner must
//! separately bind its actual original HostBytes pool and live Source/worker.
//! No constructor here installs a semantic reader or changes its availability.
//! There is no public guard, positive probe, raw Arc, callback or payload slot.
//!
//! Allocation layouts follow the pinned Rust 1.97.1 ArcInner and mutex backends.
//! macOS uses a lazy Box<UnsafeCell<pthread_mutex_t>>; construction pays for and
//! initializes it before any handle is shared. Unknown backends are refused.
mod lifecycle;
pub use lifecycle::MountRetirement;
pub(crate) use lifecycle::{MountConstructionRetirement, RootRetirementPrelude};

use automerge::resource_limits::NativeReservation;
use std::{
    alloc::Layout,
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

/// Fixed resource/lifecycle failures; none contains application content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetirementError {
    CreditTooSmall,
    UnsupportedLayout,
    Retired,
    Busy,
    Poisoned,
}

struct DomainCore {
    gate: Mutex<()>,
    requested: AtomicBool,
    retired: AtomicBool,
}
struct MarkerCore {
    requested: AtomicBool,
    retired: AtomicBool,
}

// Every strong ref owns credit OUTSIDE its Arc. Field order keeps all backing,
// the platform mutex and the final Arc control block paid through deallocation.
// There are no Weak refs. Copies explicitly receive independently retained credit.
struct DomainRef {
    core: Arc<DomainCore>,
    _credit: NativeReservation,
}
struct MarkerRef {
    domain: Arc<DomainCore>,
    marker: Arc<MarkerCore>,
    _credit: NativeReservation,
}

/// Resource-only domain owner. Drop retires every marker, even with observers.
/// This is deliberately non-Clone and has no public positive validation method.
pub struct RetirementDomain {
    value: DomainRef,
}
/// Resource-only marker owner. Drop retires this marker, not its siblings.
pub struct RetirementOwner {
    value: MarkerRef,
}
/// A funded negative reference. It cannot keep its owner logically live.
/// Drop releases memory custody only; explicit retirement is irreversible.
pub struct RetirementHandle {
    value: MarkerRef,
}

// alloc/src/sync.rs: ArcInner is repr(C, align(2)), two atomics then data.
#[repr(C, align(2))]
struct ArcHeader {
    strong: AtomicUsize,
    weak: AtomicUsize,
}
fn arc_bytes<T>() -> Result<usize, RetirementError> {
    Layout::new::<ArcHeader>()
        .extend(Layout::new::<T>())
        .map(|(layout, _)| layout.pad_to_align().size())
        .map_err(|_| RetirementError::UnsupportedLayout)
}
fn mutex_backing_bytes() -> Result<usize, RetirementError> {
    // std/sys/sync/mutex/{mod,pthread}.rs and sys/pal/unix/sync/mutex.rs.
    #[cfg(target_os = "macos")]
    {
        return Ok(size_of::<libc::pthread_mutex_t>());
    }
    // Pinned futex (Linux, ordinary Windows, atomics WASM) or inline
    // no_threads (ordinary WASM) mutex; all storage is inside Mutex<()>.
    #[cfg(any(
        target_os = "linux",
        all(target_os = "windows", target_vendor = "pc"),
        all(target_family = "wasm", target_os = "unknown"),
    ))]
    {
        return Ok(0);
    }
    #[allow(
        unreachable_code,
        reason = "test/stub closure keeps its declared return type after a diverging call"
    )]
    Err(RetirementError::UnsupportedLayout)
}
fn require_credit(credit: &NativeReservation, bytes: usize) -> Result<(), RetirementError> {
    if credit.bytes() < bytes {
        Err(RetirementError::CreditTooSmall)
    } else {
        Ok(())
    }
}

impl DomainCore {
    fn request_retirement(&self) {
        self.requested.store(true, Ordering::Release);
    }
    fn finish_retirement(&self) {
        self.request_retirement();
        // Poison is permanent. Recover only to complete negative retirement;
        // never clear poison, invoke a callback, or drop custody under the guard.
        let guard = self
            .gate
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.retired.store(true, Ordering::Release);
        drop(guard);
    }
    fn accepts_new_marker(&self) -> Result<(), RetirementError> {
        let guard = match self.gate.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return Err(RetirementError::Busy),
            Err(TryLockError::Poisoned(_)) => return Err(RetirementError::Poisoned),
        };
        let retired =
            self.requested.load(Ordering::Acquire) || self.retired.load(Ordering::Acquire);
        drop(guard);
        if retired {
            Err(RetirementError::Retired)
        } else {
            Ok(())
        }
    }
}
impl MarkerRef {
    fn copy_with_credit(&self, credit: NativeReservation) -> Result<Self, RetirementError> {
        require_credit(&credit, RetirementOwner::allocation_bytes()?)?;
        Ok(Self {
            domain: Arc::clone(&self.domain),
            marker: Arc::clone(&self.marker),
            _credit: credit,
        })
    }
    fn request_retirement(&self) {
        self.marker.requested.store(true, Ordering::Release);
    }
    fn finish_retirement(&self) {
        self.request_retirement();
        let guard = self
            .domain
            .gate
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.marker.retired.store(true, Ordering::Release);
        drop(guard);
    }
}

impl RetirementDomain {
    /// Required retained bytes for the Arc allocation and platform mutex backing.
    /// Caller container/stack storage is separate, as are its other resource costs.
    pub fn allocation_bytes() -> Result<usize, RetirementError> {
        arc_bytes::<DomainCore>()?
            .checked_add(mutex_backing_bytes()?)
            .ok_or(RetirementError::UnsupportedLayout)
    }
    /// Consume already reserved credit before allocating. Excess is retained.
    /// A standalone NativeAllowance is only a budget, never a host-pool witness.
    pub fn new(credit: NativeReservation) -> Result<Self, RetirementError> {
        require_credit(&credit, Self::allocation_bytes()?)?;
        let core = Arc::new(DomainCore {
            gate: Mutex::new(()),
            requested: AtomicBool::new(false),
            retired: AtomicBool::new(false),
        });
        // Unique, unshared first initialization prevents OnceBox allocation
        // races. No later lock/try_lock/retirement may initialize heap backing.
        {
            let guard = core.gate.lock().map_err(|_| RetirementError::Poisoned)?;
            drop(guard);
        }
        Ok(Self {
            value: DomainRef {
                core,
                _credit: credit,
            },
        })
    }
    /// Create an inert marker. A successful check orders creation before a
    /// concurrent domain retirement; that retirement still denies this marker.
    pub fn marker(&self, credit: NativeReservation) -> Result<RetirementOwner, RetirementError> {
        require_credit(&credit, RetirementOwner::allocation_bytes()?)?;
        self.value.core.accepts_new_marker()?;
        // The gate is released before allocation and before any failed-credit drop.
        Ok(RetirementOwner {
            value: MarkerRef {
                domain: Arc::clone(&self.value.core),
                marker: Arc::new(MarkerCore {
                    requested: AtomicBool::new(false),
                    retired: AtomicBool::new(false),
                }),
                _credit: credit,
            },
        })
    }
    /// Early denial only. Finish outside registry locks before cleanup callbacks.
    pub fn request_retirement(&self) {
        self.value.core.request_retirement();
    }
    /// Request denial and synchronize it, waiting only for an entered critical section.
    /// No callback or allocation runs; do not call while holding this domain's guard.
    pub fn finish_retirement(&self) {
        self.value.core.finish_retirement();
    }
    /// Irreversibly retire this resource domain; idempotent, including on poison.
    pub fn retire(&self) {
        self.finish_retirement();
    }
}
impl Drop for RetirementDomain {
    fn drop(&mut self) {
        self.retire();
    }
}
impl RetirementOwner {
    /// Full domain + marker backing credit for each independently retained ref.
    /// Conservatively charging each ref keeps the final deallocation funded,
    /// irrespective of which owner or observer is dropped last.
    pub fn allocation_bytes() -> Result<usize, RetirementError> {
        RetirementDomain::allocation_bytes()?
            .checked_add(arc_bytes::<MarkerCore>()?)
            .ok_or(RetirementError::UnsupportedLayout)
    }
    /// Explicitly fund a negative observer. This does not allocate another marker.
    pub fn handle(&self, credit: NativeReservation) -> Result<RetirementHandle, RetirementError> {
        Ok(RetirementHandle {
            value: self.value.copy_with_credit(credit)?,
        })
    }
    pub fn request_retirement(&self) {
        self.value.request_retirement();
    }
    pub fn finish_retirement(&self) {
        self.value.finish_retirement();
    }
    pub fn retire(&self) {
        self.finish_retirement();
    }
}
impl Drop for RetirementOwner {
    fn drop(&mut self) {
        self.retire();
    }
}
impl RetirementHandle {
    pub fn allocation_bytes() -> Result<usize, RetirementError> {
        RetirementOwner::allocation_bytes()
    }
    /// Explicit paid reference copy, permitted even after retirement. It only
    /// retains negative custody and cannot undo retirement or acquire authority.
    pub fn copy_with_credit(&self, credit: NativeReservation) -> Result<Self, RetirementError> {
        Ok(Self {
            value: self.value.copy_with_credit(credit)?,
        })
    }
    pub fn request_retirement(&self) {
        self.value.request_retirement();
    }
    pub fn finish_retirement(&self) {
        self.value.finish_retirement();
    }
    pub fn retire(&self) {
        self.finish_retirement();
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use tests::allocations::measure as measure_test_allocations;

#[cfg(test)]
pub(crate) use tests::{
    probe as retirement_test_probe, retirement_requested as retirement_test_requested,
};
