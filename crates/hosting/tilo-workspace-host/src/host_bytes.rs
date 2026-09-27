//! Shared host byte credits. These are resource custody, never authorization.
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Eight maximum control request reservations, within the original host pool.
pub(crate) const CONTROL_INPUT_BYTES: usize = 16 * 1024;
pub(crate) const CONTROL_RESERVE_BYTES: usize =
    8 * (2 * CONTROL_INPUT_BYTES + 1024 * 1024 + 32 * 1024);
#[derive(Clone)]
pub struct HostBytes(Arc<Pool>);
struct Pool {
    semaphore: Arc<Semaphore>,
    admission: Mutex<()>,
}
/// Move or share this credit with the backing it pays for. Drop backing first.
pub struct ByteCredit(OwnedSemaphorePermit);
impl HostBytes {
    /// Called once by ingress construction, wrapping its existing semaphore.
    pub(crate) fn from_ingress(semaphore: Arc<Semaphore>) -> Self {
        Self(Arc::new(Pool {
            semaphore,
            admission: Mutex::new(()),
        }))
    }
    pub fn same_pool(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    /// Diagnostic observation only. Admission always goes through the gate.
    pub fn available_bytes(&self) -> usize {
        self.0.semaphore.available_permits()
    }
    pub fn try_acquire_bulk(&self, bytes: usize) -> Option<ByteCredit> {
        self.acquire(bytes, false)
    }
    pub fn try_acquire_control(&self, bytes: usize) -> Option<ByteCredit> {
        self.acquire(bytes, true)
    }
    fn acquire(&self, bytes: usize, control: bool) -> Option<ByteCredit> {
        let count: u32 = bytes.try_into().ok()?;
        let _gate = self.0.admission.lock().ok()?;
        if !control
            && self.0.semaphore.available_permits().checked_sub(bytes)? < CONTROL_RESERVE_BYTES
        {
            return None;
        }
        self.0
            .semaphore
            .clone()
            .try_acquire_many_owned(count)
            .ok()
            .map(ByteCredit)
    }
}
impl ByteCredit {
    pub fn bytes(&self) -> usize {
        self.0.num_permits()
    }
    pub fn split(&mut self, bytes: usize) -> Option<Self> {
        self.0.split(bytes).map(Self)
    }
    /// Absorb another credit from the same pool (panics on a foreign pool).
    pub(crate) fn merge(&mut self, other: Self) {
        self.0.merge(other.0)
    }
}
#[cfg(test)]
mod tests;
