//! Actual ingress credit for native allocation custody. Not installed as readiness.
use crate::host_bytes::{ByteCredit, HostBytes};
use std::sync::Mutex;
use ui::automerge::resource_limits::{
    NativeAllocationSet, NativeAllowance, NativeHostCredit, NativeResourceError, NativeWorkTurn,
};

/// Additional work, independently of allocations retained from earlier turns.
const NATIVE_TURN_BYTES: usize = 64 * 1024 * 1024;

struct HostNativeCredit {
    credit: Mutex<Option<ByteCredit>>,
}
impl HostNativeCredit {
    fn backing_bytes() -> Result<usize, NativeResourceError> {
        // Pinned std uses an out-of-line pthread Box on macOS, and an inline
        // futex on Linux. Do not assume an uninspected platform is allocation-free.
        #[cfg(target_os = "macos")]
        let mutex_backing = size_of::<libc::pthread_mutex_t>();
        #[cfg(not(target_os = "macos"))]
        let mutex_backing = if cfg!(target_os = "linux") {
            0
        } else {
            return Err(NativeResourceError::Unaccounted);
        };
        size_of::<Self>()
            .checked_add(mutex_backing)
            .ok_or(NativeResourceError::Unaccounted)
    }

    fn new(credit: ByteCredit) -> Result<Box<Self>, NativeResourceError> {
        if credit.bytes() < Self::backing_bytes()? {
            return Err(NativeResourceError::WorkBytes);
        }
        // Keep credit outside the Box through fallible initialization, so
        // unwinding also deallocates all keeper backing before its permit.
        let mut keeper = Box::new(Self {
            credit: Mutex::new(None),
        });
        {
            let guard = keeper
                .credit
                .lock()
                .map_err(|_| NativeResourceError::Unaccounted)?;
            drop(guard);
        }
        // Unique initialization prevents OnceBox racing candidate allocations;
        // every later shrink/release callback sees initialized mutex backing.
        *keeper
            .credit
            .get_mut()
            .map_err(|_| NativeResourceError::Unaccounted)? = Some(credit);
        Ok(keeper)
    }
}
impl NativeHostCredit for HostNativeCredit {
    fn shrink_to(&self, native_retained_bytes: usize) {
        // Both the concrete Box and platform mutex backing live outside the
        // native ledger. Overflow/stale larger notices keep current credit.
        let Ok(keeper_bytes) = Self::backing_bytes() else {
            return;
        };
        let Some(retain) = native_retained_bytes.checked_add(keeper_bytes) else {
            return;
        };
        let mut guard = self
            .credit
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(credit) = guard.as_mut() {
            if let Some(surplus) = credit.bytes().checked_sub(retain) {
                drop(credit.split(surplus));
            }
        }
    }

    fn release(self: Box<Self>) {
        // Keep the actual permit on the stack until the now-empty concrete Box
        // has been deallocated. Native has already freed its keeper Arc here.
        let credit = self
            .credit
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        drop(self);
        #[cfg(test)]
        tests::after_keeper_deallocation(credit.as_ref());
        drop(credit);
    }
}

fn host_allowance(pool: &HostBytes) -> Result<NativeAllowance, NativeResourceError> {
    let native_bytes = NATIVE_TURN_BYTES
        .checked_sub(HostNativeCredit::backing_bytes()?)
        .ok_or(NativeResourceError::WorkBytes)?;
    let credit = pool
        .try_acquire_bulk(NATIVE_TURN_BYTES)
        .ok_or(NativeResourceError::WorkBytes)?;
    NativeAllowance::with_host_credit(native_bytes, HostNativeCredit::new(credit)?)
}

impl HostBytes {
    /// Reserve 64 MiB of additional native work from this original bulk pool.
    /// Retained allocations keep their own credit; this is not an owner limit
    /// or an authority/installation witness. The embedding owner must verify
    /// its pool identity and seal an allowance alias on every completion/error,
    /// including a failed registry or turn constructor.
    pub fn native_allowance(&self) -> Result<NativeAllowance, NativeResourceError> {
        host_allowance(self)
    }
}

/// One private registry for a future bounded SessionOwner. This budget alone
/// does not qualify a peer, authorize an operation, or enable a data route.
pub(super) struct OwnerNativeBudget {
    allocations: NativeAllocationSet,
    pool: HostBytes,
}
impl OwnerNativeBudget {
    pub(super) fn new(pool: &HostBytes) -> Result<Self, NativeResourceError> {
        let allowance = host_allowance(pool)?;
        let allocations = NativeAllocationSet::new(allowance.clone());
        // The domain keeps its original metadata custody; unused startup work
        // is released even when registry construction refuses.
        allowance.finish_turn();
        Ok(Self {
            allocations: allocations?,
            pool: pool.clone(),
        })
    }
    pub(super) fn prepare_turn<'a>(
        &'a mut self,
        pool: &HostBytes,
    ) -> Result<OwnerNativeTurn<'a>, NativeResourceError> {
        if !self.pool.same_pool(pool) {
            return Err(NativeResourceError::Unaccounted);
        }
        let allowance = host_allowance(pool)?;
        let work = match self.allocations.work_turn(allowance.clone()) {
            Ok(work) => work,
            Err(error) => {
                allowance.finish_turn();
                return Err(error);
            }
        };
        Ok(OwnerNativeTurn {
            allocations: &mut self.allocations,
            work,
            allowance,
        })
    }
}

/// Exclusive work against the owner's stable registry; never an authority token.
pub(super) struct OwnerNativeTurn<'a> {
    allocations: &'a mut NativeAllocationSet,
    work: NativeWorkTurn,
    allowance: NativeAllowance,
}
impl OwnerNativeTurn<'_> {
    pub(super) fn native(&mut self) -> (&mut NativeAllocationSet, &NativeWorkTurn) {
        (self.allocations, &self.work)
    }
    pub(super) fn allowance(&self) -> &NativeAllowance {
        &self.allowance
    }
}

impl Drop for OwnerNativeTurn<'_> {
    fn drop(&mut self) {
        // Seal all allowance aliases before returning unused real permits.
        // Registry reconciliation only retires entries without guarded users.
        self.allowance.finish_turn();
        self.allocations.reconcile();
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(super) use tests::allocations::snapshot;
#[cfg(test)]
pub(crate) use tests::allocations::{Stats, allocations_only};
