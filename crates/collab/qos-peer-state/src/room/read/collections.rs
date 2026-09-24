//! Private source-specific collection work; never a policy or owner witness.
use automerge::resource_limits::{NativeReservation, NativeResourceError, NativeWorkTurn};
/// Own this budget after every backing it covers. Cumulative debits include
/// intermediates and old/new growth overlap, and are not refunded before drop.
/// Values inserted into a collection must have their nested ownership paid by
/// this same budget (or separately retained custody) before insertion.
pub(crate) struct Collections {
    remaining: usize,
    _retention: Option<NativeReservation>,
}
impl Collections {
    /// Explicitly unqualified adapter for existing owned legacy outputs.
    pub(crate) fn unqualified() -> Self {
        Self {
            remaining: usize::MAX,
            _retention: None,
        }
    }
    pub(crate) fn native(turn: &NativeWorkTurn, limit: usize) -> Result<Self, NativeResourceError> {
        Ok(Self {
            remaining: limit,
            _retention: Some(turn.reserve(limit)?),
        })
    }
    pub(crate) fn debit(&mut self, bytes: Option<usize>) -> Result<(), NativeResourceError> {
        self.remaining = bytes
            .and_then(|n| self.remaining.checked_sub(n))
            .ok_or(NativeResourceError::WorkBytes)?;
        Ok(())
    }
    pub(crate) fn tree_entry<K, V>(&mut self) -> Result<(), NativeResourceError> {
        if align_of::<K>() > 16 || align_of::<V>() > 16 {
            return Err(NativeResourceError::Unaccounted);
        }
        // Unchanged pinned Rust1.97.1 fresh BTreeMap:11 slots,12 edges,
        // header/alignment <=256; <=3 live new nodes per insertion includes
        // split/root peaks. This is allocation work, not a map-length check.
        self.debit(
            size_of::<K>()
                .checked_add(size_of::<V>())
                .and_then(|n| n.checked_mul(33))
                .and_then(|n| n.checked_add(768)),
        )
    }
    pub(crate) fn text(&mut self, value: &str) -> Result<String, NativeResourceError> {
        self.debit(Some(value.len()))?;
        let mut result = String::new();
        result
            .try_reserve_exact(value.len())
            .map_err(|_| NativeResourceError::WorkBytes)?;
        // Pinned Global requests the exact byte layout. This defensive check
        // does not substitute for that preallocation/source proof.
        if result.capacity() > value.len() {
            return Err(NativeResourceError::Unaccounted);
        }
        result.push_str(value);
        Ok(result)
    }
    pub(crate) fn bytes(&mut self, value: &[u8]) -> Result<Vec<u8>, NativeResourceError> {
        let mut result = self.zeroed(value.len())?;
        result.copy_from_slice(value);
        Ok(result)
    }
    pub(crate) fn zeroed(&mut self, len: usize) -> Result<Vec<u8>, NativeResourceError> {
        self.debit(Some(len))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(len)
            .map_err(|_| NativeResourceError::WorkBytes)?;
        if bytes.capacity() > len {
            return Err(NativeResourceError::Unaccounted);
        }
        bytes.resize(len, 0);
        Ok(bytes)
    }
    pub(crate) fn push<T>(
        &mut self,
        values: &mut Vec<T>,
        value: T,
    ) -> Result<(), NativeResourceError> {
        if size_of::<T>() == 0 {
            return Err(NativeResourceError::Unaccounted);
        }
        if values.len() == values.capacity() {
            let capacity = values
                .capacity()
                .checked_mul(2)
                .map(|n| n.max(4))
                .ok_or(NativeResourceError::WorkBytes)?;
            self.debit(capacity.checked_mul(size_of::<T>()))?;
            // Exact pinned Global layout, with every previous capacity still
            // included in the cumulative debit during moving reallocation.
            values
                .try_reserve_exact(capacity - values.len())
                .map_err(|_| NativeResourceError::WorkBytes)?;
            if values.capacity() > capacity {
                return Err(NativeResourceError::Unaccounted);
            }
        }
        values.push(value);
        Ok(())
    }
}
#[cfg(test)]
mod tests;
