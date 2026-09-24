//! Unique owned allocation credit across successive consuming work turns.
//! Shared backings have independent strong leases and are never funded here.
use super::{NativeReservation, NativeResourceError, Result, add, mul, split};
use std::mem::size_of;
use std::sync::Arc;

/// Values die before this holder. Its extra-reservation vector dies before its
/// own buffer credit; dropping individual credits cannot expose an unfunded
/// vector allocation. The initial load/clone requires no extra vector.
pub(super) struct OwnedCredits {
    first: Option<NativeReservation>,
    later: Vec<NativeReservation>,
    metadata: Option<NativeReservation>,
    bytes: usize,
}

/// Prepared before native mutation, including actual vector allocation. It
/// carries no permission to mutate or to manufacture owned allocation bytes.
pub(super) struct OwnedGrowth {
    buffer: Option<PreparedBuffer>,
}

struct PreparedBuffer {
    values: Vec<NativeReservation>,
    metadata: NativeReservation,
}

impl OwnedCredits {
    pub(super) fn empty() -> Self {
        Self {
            first: None,
            later: Vec::new(),
            metadata: None,
            bytes: 0,
        }
    }

    pub(super) fn new(reservation: NativeReservation) -> Self {
        let bytes = reservation.bytes();
        Self {
            first: (bytes != 0).then_some(reservation),
            later: Vec::new(),
            metadata: None,
            bytes,
        }
    }

    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }

    fn matching(&self, reservation: &NativeReservation) -> Option<usize> {
        if self.first.as_ref().is_some_and(|first| {
            Arc::ptr_eq(&first.ledger, &reservation.ledger)
        }) {
            return Some(0);
        }
        self.later.iter().position(|credit| {
            Arc::ptr_eq(&credit.ledger, &reservation.ledger)
        }).map(|index| index + 1)
    }

    /// Additional bookkeeping capacity if this turn adds any owned bytes.
    /// Same-ledger growth merges into its existing slot; no zero-byte slot is
    /// ever retained. An old vector's actual capacity is already funded.
    pub(super) fn growth_metadata_bytes(&self, peak: &NativeReservation) -> Result<usize> {
        if self.first.is_none() || self.matching(peak).is_some()
            || self.later.len() < self.later.capacity()
        {
            return Ok(0);
        }
        mul(add(self.later.len(), 1)?, size_of::<NativeReservation>())
    }

    pub(super) fn prepare_growth(&self, peak: &mut NativeReservation) -> Result<OwnedGrowth> {
        let bytes = self.growth_metadata_bytes(peak)?;
        if bytes == 0 {
            return Ok(OwnedGrowth { buffer: None });
        }
        if peak.bytes() < bytes {
            return Err(NativeResourceError::WorkBytes);
        }
        let metadata = split(peak, bytes);
        let mut values = Vec::new();
        values.try_reserve_exact(add(self.later.len(), 1)?)
            .map_err(|_| NativeResourceError::WorkBytes)?;
        if mul(values.capacity(), size_of::<NativeReservation>())? > metadata.bytes() {
            return Err(NativeResourceError::Unaccounted);
        }
        Ok(OwnedGrowth {
            buffer: Some(PreparedBuffer { values, metadata }),
        })
    }

    /// Reconcile only after measuring the completed same private native value.
    /// The caller still holds its value, shared leases and full transient peak.
    /// Every fallible check occurs before credits move or are released.
    pub(super) fn settle(
        &mut self,
        measured_bytes: usize,
        peak: &mut NativeReservation,
        growth: &mut OwnedGrowth,
    ) -> Result {
        if measured_bytes > self.bytes {
            let additional = measured_bytes - self.bytes;
            if additional > peak.bytes() {
                return Err(NativeResourceError::WorkBytes);
            }
            let matching = self.matching(peak);
            let needs_slot = self.first.is_some() && matching.is_none();
            if needs_slot && self.later.len() == self.later.capacity() {
                let prepared = growth.buffer.as_ref().ok_or(NativeResourceError::Unaccounted)?;
                if prepared.values.capacity() <= self.later.len()
                    || !Arc::ptr_eq(&prepared.metadata.ledger, &peak.ledger)
                {
                    return Err(NativeResourceError::Unaccounted);
                }
            }

            // No fallible operation follows. A prepared replacement keeps old
            // credit entries alive while the old buffer is deallocated, then
            // retires that buffer's old metadata reservation.
            if needs_slot && self.later.len() == self.later.capacity() {
                let PreparedBuffer { mut values, metadata } = growth.buffer.take().unwrap();
                values.append(&mut self.later);
                let previous = std::mem::replace(&mut self.later, values);
                drop(previous);
                self.metadata = Some(metadata);
            }
            let mut added = split(peak, additional);
            if let Some(index) = matching {
                let existing = if index == 0 {
                    self.first.as_mut().unwrap()
                } else {
                    &mut self.later[index - 1]
                };
                // Both private reservations refer to the same atomic ledger
                // and host keeper. Their total live charged bytes do not
                // change; no release/reacquire window occurs during merging.
                existing.bytes += added.bytes;
                added.bytes = 0;
            } else if self.first.is_none() {
                self.first = Some(added);
            } else {
                self.later.push(added);
            }
        } else {
            let mut release = self.bytes - measured_bytes;
            // Retire newest credits first. Removing zero credits prevents
            // repeated tiny grow/shrink cycles accumulating ledger handles.
            while release != 0 {
                if let Some(last) = self.later.last_mut() {
                    if last.bytes() <= release {
                        release -= last.bytes();
                        self.later.pop();
                    } else {
                        drop(split(last, release));
                        release = 0;
                    }
                } else {
                    let first = self.first.as_mut().unwrap();
                    if first.bytes() <= release {
                        release -= first.bytes();
                        self.first = None;
                    } else {
                        drop(split(first, release));
                        release = 0;
                    }
                }
            }
            if self.later.is_empty() {
                // Buffer deallocation precedes returning its actual credit.
                self.later = Vec::new();
                self.metadata = None;
            }
        }
        self.bytes = measured_bytes;
        Ok(())
    }
}
