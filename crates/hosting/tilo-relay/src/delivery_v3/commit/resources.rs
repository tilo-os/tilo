//! Closed allocation custody for the concrete delivery persistence codecs.
//! These guards carry memory only, never admission, durability or authority.
use super::*;
use automerge::resource_limits::{NativeReservation, NativeWorkTurn};

/// Codec failures retain their original meaning; resource refusal is separate.
#[derive(Debug, thiserror::Error)]
pub enum ResourceError {
    /// The existing structural codec rejected this value.
    #[error(transparent)]
    Commit(#[from] CommitError),
    /// The original native work allowance cannot fund this allocation.
    #[error("delivery allocation capacity exceeded")]
    Capacity,
    /// A legacy payload has no original native allocation keeper.
    #[error("publication backing has no retained allocation custody")]
    Unaccounted,
}
/// Opaque retained memory custody for one decoded or forked ledger.
pub struct LedgerCustody {
    _retained: NativeReservation,
}
/// Opaque map custody; immutable payloads carry their own shared original credit.
pub struct PoolMapCustody {
    _retained: NativeReservation,
}
impl LedgerCustody {
    /// Fork the actual ledger held with this custody by Cloud HeldCloudState.
    /// The concrete clone and validation scratch are funded before allocation.
    pub fn fork(
        &self,
        ledger: &DeliveryLedgerV1,
        turn: &NativeWorkTurn,
    ) -> Result<HeldDeliveryLedger, ResourceError> {
        self.fork_in(ledger, &ResourceBudget::native(turn))
    }
    /// Fork the actual Cloud-held ledger using its existing operation budget.
    pub fn fork_in(
        &self,
        ledger: &DeliveryLedgerV1,
        budget: &ResourceBudget<'_, '_>,
    ) -> Result<HeldDeliveryLedger, ResourceError> {
        let custody = LedgerCustody {
            _retained: reserve(budget, preflight::ledger_fork(ledger)?)?,
        };
        let _validation = reserve(
            budget,
            layout::tree::<ConsumerId, ()>(ledger.consumers.len())?,
        )?;
        ledger.validate()?;
        Ok(HeldDeliveryLedger {
            ledger: ledger.clone(),
            custody,
        })
    }
}
impl PoolMapCustody {
    /// Fork the actual pool held with this custody by Cloud HeldCloudState.
    /// Every existing backing must retain its own genuine original keeper.
    pub fn fork(
        &self,
        pool: &PublicationPool,
        turn: &NativeWorkTurn,
    ) -> Result<HeldPublicationPool, ResourceError> {
        self.fork_in(pool, &ResourceBudget::native(turn))
    }
    /// Fork the actual Cloud-held pool without replacing original backing credit.
    pub fn fork_in(
        &self,
        pool: &PublicationPool,
        budget: &ResourceBudget<'_, '_>,
    ) -> Result<HeldPublicationPool, ResourceError> {
        if pool
            .wire
            .payloads
            .values()
            .any(|backing| backing.keeper.is_none())
        {
            return Err(ResourceError::Unaccounted);
        }
        let custody = PoolMapCustody {
            _retained: reserve(budget, layout::tree::<Digest, PayloadBacking>(pool.len())?)?,
        };
        Ok(HeldPublicationPool {
            pool: pool.fork(),
            custody,
        })
    }
}
/// A structurally decoded ledger and its inseparable allocation custody.
pub struct HeldDeliveryLedger {
    ledger: DeliveryLedgerV1,
    custody: LedgerCustody,
}
impl HeldDeliveryLedger {
    /// Decode the exact legacy format using the caller's original work allowance.
    pub fn decode(bytes: &[u8], turn: &NativeWorkTurn) -> Result<Self, ResourceError> {
        Self::decode_in(bytes, &ResourceBudget::native(turn))
    }
    /// Decode the concrete ledger using an existing embedding operation budget.
    pub fn decode_in(bytes: &[u8], budget: &ResourceBudget<'_, '_>) -> Result<Self, ResourceError> {
        let (retained, scratch) = preflight::ledger(bytes)?;
        let custody = LedgerCustody {
            _retained: reserve(budget, retained)?,
        };
        let _scratch = reserve(budget, scratch)?;
        let ledger = DeliveryLedgerV1::decode(bytes)?;
        Ok(Self { ledger, custody })
    }
    /// Borrow inert metadata; this is not an owner qualification.
    pub fn ledger(&self) -> &DeliveryLedgerV1 {
        &self.ledger
    }
    /// Pay the actual separate map/vector allocations before cloning.
    pub fn fork(&self, turn: &NativeWorkTurn) -> Result<Self, ResourceError> {
        self.fork_in(&ResourceBudget::native(turn))
    }
    /// Pay a separate concrete ledger clone from the same embedding operation.
    pub fn fork_in(&self, budget: &ResourceBudget<'_, '_>) -> Result<Self, ResourceError> {
        let custody = LedgerCustody {
            _retained: reserve(budget, preflight::ledger_fork(&self.ledger)?)?,
        };
        Ok(Self {
            ledger: self.ledger.clone(),
            custody,
        })
    }
    /// Transfer once into the audited Cloud HeldCloudState holder. That holder
    /// must drop its State before this custody; never discard custody separately.
    pub fn into_parts(self) -> (DeliveryLedgerV1, LedgerCustody) {
        (self.ledger, self.custody)
    }
}
/// A pool whose map and immutable backing allocations retain original credit.
pub struct HeldPublicationPool {
    pool: PublicationPool,
    custody: PoolMapCustody,
}
impl HeldPublicationPool {
    /// Decode exact legacy pool bytes under both payload policy and real credit.
    pub fn decode(
        bytes: &[u8],
        limit: usize,
        turn: &NativeWorkTurn,
    ) -> Result<Self, ResourceError> {
        Self::decode_in(bytes, limit, &ResourceBudget::native(turn))
    }
    /// Decode the concrete pool using an existing embedding operation budget.
    pub fn decode_in(
        bytes: &[u8],
        limit: usize,
        budget: &ResourceBudget<'_, '_>,
    ) -> Result<Self, ResourceError> {
        let len = preflight::pool(bytes, limit)?;
        let _borrowed = reserve(budget, layout::tree::<Digest, &[u8]>(len)?)?;
        let wire: BorrowedPool<'_> = decode_exact(bytes)?;
        if wire.version != 1 {
            return Err(CommitError::Invalid("pool version").into());
        }
        let mut total = 0usize;
        for (id, payload) in &wire.payloads {
            if payload.is_empty() || payload.len() > MAX_ENVELOPE_BYTES {
                return Err(CommitError::Quota.into());
            }
            total = total.checked_add(payload.len()).ok_or(CommitError::Quota)?;
            if total > limit {
                return Err(CommitError::Quota.into());
            }
            if id != blake3::hash(payload).as_bytes() {
                return Err(CommitError::Invalid("payload digest").into());
            }
        }
        let custody = PoolMapCustody {
            _retained: reserve(budget, layout::tree::<Digest, PayloadBacking>(len)?)?,
        };
        let mut pool = PublicationPool::new(limit)?;
        // Use concrete insertion, not FromIterator's temporary sorting Vec.
        // The borrowed map overlaps owned map/payload construction and remains
        // paid until after its iterator is completely dropped on every exit.
        for (id, bytes) in wire.payloads {
            pool.wire
                .payloads
                .insert(id, PayloadBacking::paid(bytes, budget)?);
        }
        pool.bytes = total;
        Ok(Self { pool, custody })
    }
    /// Borrow the inert pool. Escaped pins retain genuine backing custody.
    pub fn pool(&self) -> &PublicationPool {
        &self.pool
    }
    /// Pay new map metadata; preserve each shared backing's original reservation.
    pub fn fork(&self, turn: &NativeWorkTurn) -> Result<Self, ResourceError> {
        self.fork_in(&ResourceBudget::native(turn))
    }
    /// Pay new map metadata while retaining every original backing's keeper.
    pub fn fork_in(&self, budget: &ResourceBudget<'_, '_>) -> Result<Self, ResourceError> {
        self.custody.fork_in(&self.pool, budget)
    }

    /// Transfer once into the audited Cloud HeldCloudState holder. Its State
    /// must drop before map custody; payload credit follows every fork and pin.
    pub fn into_parts(self) -> (PublicationPool, PoolMapCustody) {
        (self.pool, self.custody)
    }
}
fn reserve(
    budget: &ResourceBudget<'_, '_>,
    bytes: usize,
) -> Result<NativeReservation, ResourceError> {
    budget.reserve(bytes)
}

// Every immutable byte root carries both handles. Field order is essential:
// deallocate the Vec/Arc backing before releasing its original reservation.
#[derive(Clone)]
pub(super) struct PayloadBacking {
    bytes: std::sync::Arc<Vec<u8>>,
    keeper: Option<SharedPayloadCredit>,
}
impl PayloadBacking {
    pub(super) fn legacy(bytes: Vec<u8>) -> Self {
        Self {
            bytes: std::sync::Arc::new(bytes),
            keeper: None,
        }
    }
    fn paid(bytes: &[u8], budget: &ResourceBudget<'_, '_>) -> Result<Self, ResourceError> {
        let cost = layout::add(
            layout::add(bytes.len(), layout::arc::<Vec<u8>>()?)?,
            layout::arc::<NativeReservation>()?,
        )?;
        let retained = reserve(budget, cost)?;
        Ok(Self {
            bytes: std::sync::Arc::new(bytes.to_vec()),
            keeper: Some(SharedPayloadCredit(Some(std::sync::Arc::new(retained)))),
        })
    }
    pub(super) fn as_slice(&self) -> &[u8] {
        self.bytes.as_slice()
    }
    pub(super) fn capacity(&self) -> usize {
        self.bytes.capacity()
    }
    pub(super) fn retained_allocation_bytes(&self) -> Result<usize, CommitError> {
        let arc = layout::arc::<Vec<u8>>().map_err(|_| CommitError::Quota)?;
        let keeper = if self.keeper.is_some() {
            layout::arc::<NativeReservation>().map_err(|_| CommitError::Quota)?
        } else {
            0
        };
        self.capacity()
            .checked_add(arc)
            .and_then(|n| n.checked_add(keeper))
            .ok_or(CommitError::Quota)
    }
}
#[derive(Clone)]
struct SharedPayloadCredit(Option<std::sync::Arc<NativeReservation>>);
impl Drop for SharedPayloadCredit {
    fn drop(&mut self) {
        // No Weak/raw Arc escapes, and every strong holder uses into_inner.
        // The final caller receives the reservation only after the keeper Arc
        // allocation is freed, including concurrent destruction of last roots.
        if let Some(inner) = self.0.take() {
            if let Some(reservation) = std::sync::Arc::into_inner(inner) {
                drop(reservation);
            }
        }
    }
}
mod budget;
pub use budget::ResourceBudget;
mod layout;
mod preflight;
#[cfg(test)]
mod tests;
