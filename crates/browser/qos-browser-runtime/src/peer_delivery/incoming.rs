//! One shared per-Peer incoming work ledger. This accounts for retained outer
//! bytes and bounded DTOs, not legacy StatePeer/AutoCommit candidate allocation.
use automerge::resource_limits::{NativeAllowance, NativeReservation};
#[derive(Clone)]
pub(crate) struct IncomingBudget {
    allowance: NativeAllowance,
}
impl IncomingBudget {
    pub fn new(bytes: usize) -> Result<Self, String> {
        NativeAllowance::new(bytes)
            .map(|allowance| Self { allowance })
            .map_err(|_| "incoming work budget unavailable".into())
    }
    pub fn reserve(&self, bytes: usize) -> Result<NativeReservation, String> {
        self.allowance
            .reserve(bytes)
            .map_err(|_| "storage-pressure: shared incoming work budget exhausted".into())
    }
    pub fn allowance(&self) -> &NativeAllowance {
        &self.allowance
    }
    pub fn available(&self) -> usize {
        self.allowance.available_bytes()
    }
    pub fn buffer(&self, maximum: usize) -> Result<IncomingBuffer, String> {
        let credit = self.reserve(
            maximum
                .checked_add(std::mem::size_of::<IncomingBuffer>())
                .ok_or("incoming charge overflow")?,
        )?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(maximum)
            .map_err(|_| "incoming allocation unavailable")?;
        if bytes.capacity() > maximum {
            return Err("incoming backing exceeds reserved capacity".into());
        }
        Ok(IncomingBuffer {
            bytes,
            _credit: credit,
        })
    }
    pub fn copy(&self, source: &[u8]) -> Result<IncomingBuffer, String> {
        let mut held = self.buffer(source.len())?;
        held.bytes.extend_from_slice(source);
        Ok(held)
    }
}
/// Backing drops before its reservation; no unguarded ownership extraction.
pub(crate) struct IncomingBuffer {
    pub(super) bytes: Vec<u8>,
    _credit: NativeReservation,
}
impl IncomingBuffer {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::ops::Deref for IncomingBuffer {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}
