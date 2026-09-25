//! Requested-layout bounds for the pinned standard-library implementations.
use super::*;
use std::alloc::Layout;

pub(super) fn add(a: usize, b: usize) -> Result<usize, ResourceError> {
    a.checked_add(b).ok_or(ResourceError::Capacity)
}
fn mul(a: usize, b: usize) -> Result<usize, ResourceError> {
    a.checked_mul(b).ok_or(ResourceError::Capacity)
}
/// BTreeMap/BTreeSet B=6, CAPACITY=11, as in the existing pinned native
/// resource_limits/retained.rs model. At most n+1 allocated nodes, each bounded
/// by an internal node. Padding each field to maximum alignment bounds every
/// repr(Rust) field ordering; no private offset is assumed. Fresh empty maps
/// allocate nothing. Counts are concrete typed entries, never encoded bytes.
pub(super) fn tree<K, V>(len: usize) -> Result<usize, ResourceError> {
    if len == 0 {
        return Ok(0);
    }
    let alignment = align_of::<K>()
        .max(align_of::<V>())
        .max(align_of::<usize>());
    let mut node = 0;
    for field in [
        size_of::<usize>(),
        size_of::<u16>(),
        size_of::<u16>(),
        mul(11, size_of::<K>())?,
        mul(11, size_of::<V>())?,
        mul(12, size_of::<usize>())?,
    ] {
        let padded = add(field, alignment - 1)? & !(alignment - 1);
        node = add(node, padded)?;
    }
    mul(add(len, 1)?, node)
}
pub(super) fn arc<T>() -> Result<usize, ResourceError> {
    Layout::new::<[usize; 2]>()
        .extend(Layout::new::<T>())
        .map(|(l, _)| l.pad_to_align().size())
        .map_err(|_| ResourceError::Capacity)
}
#[derive(Clone, Copy, Default)]
pub(super) struct Vectors {
    pub retained: usize,
    pub scratch: usize,
}
impl Vectors {
    pub fn add(self, other: Self) -> Result<Self, ResourceError> {
        Ok(Self {
            retained: add(self.retained, other.retained)?,
            scratch: add(self.scratch, other.scratch)?,
        })
    }
    pub fn pushed<T>(len: usize) -> Result<Self, ResourceError> {
        if len == 0 || size_of::<T>() == 0 {
            return Ok(Self::default());
        }
        // RawVec's min_non_zero_cap and doubling used by the legacy bounded_vec
        // visitor. All concrete elements here are fixed-size and below 1024B.
        let minimum = if size_of::<T>() == 1 {
            8
        } else if size_of::<T>() <= 1024 {
            4
        } else {
            1
        };
        let capacity = len
            .max(minimum)
            .checked_next_power_of_two()
            .ok_or(ResourceError::Capacity)?;
        let retained = mul(capacity, size_of::<T>())?;
        // A realloc can simultaneously hold its old and new backing. Summing
        // this per vector is conservative even though only one grows at once.
        let scratch = if capacity > minimum {
            mul(capacity / 2, size_of::<T>())?
        } else {
            0
        };
        Ok(Self { retained, scratch })
    }
    pub fn cloned<T>(len: usize) -> Result<Self, ResourceError> {
        Ok(Self {
            retained: mul(len, size_of::<T>())?,
            scratch: 0,
        })
    }
}
