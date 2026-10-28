//! Retirement synchronization, never Source, caller or disclosure authority.
//!
//! The embedding owner must separately bind its live Source/worker. No
//! constructor here installs a semantic reader or changes its availability.
//! There is no public guard, positive probe, raw Arc, callback or payload slot.
//!
//! Construction initializes the platform mutex before any handle is shared, so
//! retirement and drop never allocate.
mod lifecycle;
pub use lifecycle::MountRetirement;
pub(crate) use lifecycle::{MountConstructionRetirement, RootRetirementPrelude};

use std::sync::{
    Arc, Mutex, TryLockError,
    atomic::{AtomicBool, Ordering},
};
use tilo_lock::MutexRecover as _;

/// Fixed lifecycle failures; none contains application content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetirementError {
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

// There are no Weak refs: every strong ref keeps the backing alive until its
// own drop, never the owner's logical life.
struct DomainRef {
    core: Arc<DomainCore>,
}
#[derive(Clone)]
struct MarkerRef {
    domain: Arc<DomainCore>,
    marker: Arc<MarkerCore>,
}

/// Domain owner. Drop retires every marker, even with observers.
/// This is deliberately non-Clone and has no public positive validation method.
pub struct RetirementDomain {
    value: DomainRef,
}
/// Marker owner. Drop retires this marker, not its siblings.
pub struct RetirementOwner {
    value: MarkerRef,
}
/// A negative reference. It cannot keep its owner logically live.
/// Cloning is permitted even after retirement; explicit retirement is irreversible.
#[derive(Clone)]
pub struct RetirementHandle {
    value: MarkerRef,
}

impl DomainCore {
    fn request_retirement(&self) {
        self.requested.store(true, Ordering::Release);
    }
    fn finish_retirement(&self) {
        self.request_retirement();
        // Poison is permanent. Recover only to complete negative retirement;
        // never clear poison, invoke a callback, or drop custody under the guard.
        let guard = self.gate.lock_recover();
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
    fn request_retirement(&self) {
        self.marker.requested.store(true, Ordering::Release);
    }
    fn finish_retirement(&self) {
        self.request_retirement();
        let guard = self.domain.gate.lock_recover();
        self.marker.retired.store(true, Ordering::Release);
        drop(guard);
    }
}

impl RetirementDomain {
    /// # Errors
    ///
    /// [`RetirementError::Poisoned`] if the fresh gate cannot be initialized.
    pub fn new() -> Result<Self, RetirementError> {
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
            value: DomainRef { core },
        })
    }
    /// Create an inert marker. A successful check orders creation before a
    /// concurrent domain retirement; that retirement still denies this marker.
    ///
    /// # Errors
    ///
    /// [`RetirementError::Retired`], [`RetirementError::Busy`] or [`RetirementError::Poisoned`]
    /// when the domain no longer accepts markers.
    pub fn marker(&self) -> Result<RetirementOwner, RetirementError> {
        self.value.core.accepts_new_marker()?;
        // The gate is released before allocation.
        Ok(RetirementOwner {
            value: MarkerRef {
                domain: Arc::clone(&self.value.core),
                marker: Arc::new(MarkerCore {
                    requested: AtomicBool::new(false),
                    retired: AtomicBool::new(false),
                }),
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
    /// A negative observer. This does not allocate another marker.
    pub fn handle(&self) -> RetirementHandle {
        RetirementHandle {
            value: self.value.clone(),
        }
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
pub(crate) mod tests;

#[cfg(test)]
pub(crate) use tests::allocations::allocations_only as measure_test_allocations;

#[cfg(test)]
pub(crate) use tests::{
    probe as retirement_test_probe, retirement_requested as retirement_test_requested,
};
