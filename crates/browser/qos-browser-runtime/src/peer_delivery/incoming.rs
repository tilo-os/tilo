//! Backing for one incoming canonical item. Callers check the item's declared
//! size against their explicit limit before asking for its backing.

/// An empty buffer with room for exactly `maximum` bytes, or an error when the
/// allocator refuses it.
pub(crate) fn buffer(maximum: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(maximum)
        .map_err(|_| "incoming allocation unavailable")?;
    Ok(bytes)
}
