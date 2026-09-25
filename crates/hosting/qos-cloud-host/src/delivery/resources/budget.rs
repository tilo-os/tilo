//! Reserve-only borrow of the embedding owner's exact current peer work.
use super::ResourceError;
use automerge::resource_limits::NativeReservation;
#[cfg(test)]
use automerge::resource_limits::NativeWorkTurn;
use qos_peer_state::PeerWork;
use qos_relay::delivery_v3::commit::{
    DeliveryLedgerV1, PublicationPool,
    resources::{
        HeldDeliveryLedger, HeldPublicationPool, LedgerCustody, PoolMapCustody, ResourceBudget,
        ResourceError as RelayResourceError,
    },
};

pub(crate) struct CodecBudget<'borrow, 'turn> {
    source: Source<'borrow, 'turn>,
}
enum Source<'borrow, 'turn> {
    Peer(&'borrow PeerWork<'turn>),
    #[cfg(test)]
    Native(&'borrow NativeWorkTurn),
}
impl<'borrow, 'turn> CodecBudget<'borrow, 'turn> {
    pub(crate) fn peer(work: &'borrow PeerWork<'turn>) -> Self {
        Self {
            source: Source::Peer(work),
        }
    }
    #[cfg(test)]
    pub(super) fn native(work: &'borrow NativeWorkTurn) -> Self {
        Self {
            source: Source::Native(work),
        }
    }
    pub(super) fn reserve(&self, bytes: usize) -> Result<NativeReservation, ResourceError> {
        match self.source {
            Source::Peer(work) => work
                .reserve_output(bytes)
                .map_err(|_| ResourceError::Capacity),
            #[cfg(test)]
            Source::Native(work) => work.reserve(bytes).map_err(|_| ResourceError::Capacity),
        }
    }
    pub(super) fn decode_ledger(
        &self,
        bytes: &[u8],
    ) -> Result<HeldDeliveryLedger, RelayResourceError> {
        match self.source {
            Source::Peer(work) => {
                HeldDeliveryLedger::decode_in(bytes, &ResourceBudget::from_peer(work))
            }
            #[cfg(test)]
            Source::Native(work) => HeldDeliveryLedger::decode(bytes, work),
        }
    }
    pub(super) fn decode_pool(
        &self,
        bytes: &[u8],
        limit: usize,
    ) -> Result<HeldPublicationPool, RelayResourceError> {
        match self.source {
            Source::Peer(work) => {
                HeldPublicationPool::decode_in(bytes, limit, &ResourceBudget::from_peer(work))
            }
            #[cfg(test)]
            Source::Native(work) => HeldPublicationPool::decode(bytes, limit, work),
        }
    }
    pub(super) fn fork_ledger(
        &self,
        custody: &LedgerCustody,
        ledger: &DeliveryLedgerV1,
    ) -> Result<HeldDeliveryLedger, RelayResourceError> {
        match self.source {
            Source::Peer(work) => custody.fork_in(ledger, &ResourceBudget::from_peer(work)),
            #[cfg(test)]
            Source::Native(work) => custody.fork(ledger, work),
        }
    }
    pub(super) fn fork_pool(
        &self,
        custody: &PoolMapCustody,
        pool: &PublicationPool,
    ) -> Result<HeldPublicationPool, RelayResourceError> {
        match self.source {
            Source::Peer(work) => custody.fork_in(pool, &ResourceBudget::from_peer(work)),
            #[cfg(test)]
            Source::Native(work) => custody.fork(pool, work),
        }
    }
}
#[cfg(test)]
mod tests;
