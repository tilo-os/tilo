//! A byte limit on the collections one read builds from a document.
use super::Capacity;

/// Cumulative bytes a read may allocate for the collections it builds,
/// including intermediates and growth overlap; debits are never refunded.
pub(crate) struct Collections {
    remaining: usize,
}
impl Collections {
    /// No limit, for callers whose input is already bounded elsewhere.
    pub(crate) fn unqualified() -> Self {
        Self {
            remaining: usize::MAX,
        }
    }
    pub(crate) fn limited(limit: usize) -> Self {
        Self { remaining: limit }
    }
    pub(crate) fn debit(&mut self, bytes: Option<usize>) -> Result<(), Capacity> {
        self.remaining = bytes
            .and_then(|n| self.remaining.checked_sub(n))
            .ok_or(Capacity)?;
        Ok(())
    }
    pub(crate) fn tree_entry<K, V>(&mut self) -> Result<(), Capacity> {
        if align_of::<K>() > 16 || align_of::<V>() > 16 {
            return Err(Capacity);
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
    pub(crate) fn text(&mut self, value: &str) -> Result<String, Capacity> {
        self.debit(Some(value.len()))?;
        let mut result = String::new();
        result
            .try_reserve_exact(value.len())
            .map_err(|_| Capacity)?;
        result.push_str(value);
        Ok(result)
    }
    pub(crate) fn bytes(&mut self, value: &[u8]) -> Result<Vec<u8>, Capacity> {
        let mut result = self.zeroed(value.len())?;
        result.copy_from_slice(value);
        Ok(result)
    }
    pub(crate) fn zeroed(&mut self, len: usize) -> Result<Vec<u8>, Capacity> {
        self.debit(Some(len))?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(len).map_err(|_| Capacity)?;
        bytes.resize(len, 0);
        Ok(bytes)
    }
    pub(crate) fn push<T>(&mut self, values: &mut Vec<T>, value: T) -> Result<(), Capacity> {
        if size_of::<T>() == 0 {
            return Err(Capacity);
        }
        if values.len() == values.capacity() {
            let capacity = values
                .capacity()
                .checked_mul(2)
                .map(|n| n.max(4))
                .ok_or(Capacity)?;
            self.debit(capacity.checked_mul(size_of::<T>()))?;
            values
                .try_reserve_exact(capacity.saturating_sub(values.len()))
                .map_err(|_| Capacity)?;
        }
        values.push(value);
        Ok(())
    }
}
#[cfg(test)]
mod tests;
