//! Closed borrowing adapter for the embedding operation's original reservation.
use super::*;
use qos_peer_state::PeerWork;

/// Borrows an existing operation's memory budget without exposing native work,
/// normalization, a fresh allowance, or any owner/admission authority.
pub struct ResourceBudget<'borrow, 'turn> {
    source: Source<'borrow, 'turn>,
}
enum Source<'borrow, 'turn> {
    Native(&'borrow NativeWorkTurn),
    Peer(&'borrow PeerWork<'turn>),
}
impl<'borrow, 'turn> ResourceBudget<'borrow, 'turn> {
    /// Borrow the exact current peer operation. All retained reservations keep
    /// its original ledger/host custody after this short immutable borrow ends.
    pub fn from_peer(work: &'borrow PeerWork<'turn>) -> Self {
        Self {
            source: Source::Peer(work),
        }
    }
    pub(super) fn native(turn: &'borrow NativeWorkTurn) -> Self {
        Self {
            source: Source::Native(turn),
        }
    }
    pub(super) fn reserve(&self, bytes: usize) -> Result<NativeReservation, ResourceError> {
        match self.source {
            Source::Native(turn) => turn.reserve(bytes).map_err(|_| ResourceError::Capacity),
            Source::Peer(work) => work
                .reserve_output(bytes)
                .map_err(|_| ResourceError::Capacity),
        }
    }
}
