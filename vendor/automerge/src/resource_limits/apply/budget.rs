//! Allocation-size helpers and a running work charge for checked apply.
//!
//! Every helper returns requested allocation bytes for the exact allocation
//! pattern its caller uses: `try_reserve_exact` Vecs and `try_reserve`d std
//! hash tables that are never grown past the reserved capacity.
use crate::resource_limits::{NativeAllowance, NativeReservation, NativeResourceError};

type Result<T = ()> = std::result::Result<T, NativeResourceError>;

pub(crate) fn sum(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b).ok_or(NativeResourceError::WorkBytes)
}
pub(crate) fn product(a: usize, b: usize) -> Result<usize> {
    a.checked_mul(b).ok_or(NativeResourceError::WorkBytes)
}

/// `Vec::try_reserve_exact(n)` on an empty Vec.
pub(crate) fn vec<T>(n: usize) -> Result<usize> {
    product(n, std::mem::size_of::<T>())
}

/// hashbrown `capacity_to_buckets` for a table reserved at `n`.
fn buckets(n: usize) -> Result<usize> {
    if n == 0 {
        return Ok(0);
    }
    if n < 4 {
        return Ok(4);
    }
    if n < 8 {
        return Ok(8);
    }
    let adjusted = product(n, 8)? / 7;
    adjusted
        .checked_next_power_of_two()
        .ok_or(NativeResourceError::WorkBytes)
}

/// A std `HashMap<K, V>`/`HashSet<K>` (entry = `(K, V)`) reserved at `n` with
/// `try_reserve` and never grown past it: buckets of entries (rounded up to
/// the 16-byte group alignment) plus one control byte per bucket and one
/// trailing group.
pub(crate) fn hash<E>(n: usize) -> Result<usize> {
    let buckets = buckets(n)?;
    if buckets == 0 {
        return Ok(0);
    }
    let data = sum(product(buckets, std::mem::size_of::<E>())?, 16)?;
    sum(sum(data, buckets)?, 16)
}

/// Upper bound on a std `BTreeSet<T>` holding `n` small (<= 8 byte) keys:
/// leaf nodes hold at least 5 of 11 keys, so at most `n/5 + 1` leaves of at
/// most 160 bytes each, and internal nodes (with 12 edge pointers) add less
/// than one more leaf's worth per 6 nodes. 64 bytes per key plus two nodes
/// of slack covers both.
pub(crate) fn btree_set(n: usize) -> Result<usize> {
    sum(product(n, 64)?, 512)
}

/// Running work charge: grows its single reservation on `allowance` before
/// each allocation it covers and releases bytes when they are freed. Dropping
/// it releases everything.
pub(crate) struct Charge<'a> {
    allowance: &'a NativeAllowance,
    held: Option<NativeReservation>,
}
impl<'a> Charge<'a> {
    pub(crate) fn new(allowance: &'a NativeAllowance) -> Self {
        Self {
            allowance,
            held: None,
        }
    }
    /// Reserve `bytes` more before the allocation they cover.
    pub(crate) fn grow(&mut self, bytes: usize) -> Result {
        if bytes == 0 {
            return Ok(());
        }
        let reservation = self.allowance.reserve(bytes)?;
        match &mut self.held {
            Some(held) => held.absorb(reservation)?,
            None => self.held = Some(reservation),
        }
        Ok(())
    }
    /// Release `bytes` after the allocation they covered was freed.
    pub(crate) fn shrink(&mut self, bytes: usize) {
        if let Some(held) = &mut self.held {
            let bytes = bytes.min(held.bytes());
            if let Ok(released) = held.split_off(bytes) {
                drop(released);
            }
        }
    }
}
