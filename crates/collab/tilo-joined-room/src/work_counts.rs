//! Attribution lives across a serialized owner turn, including its store's
//! preflights, digest encodes and state-file write.
pub use tilo_peer_state::work_counts::WorkSnapshot;
use tilo_peer_state::work_counts::{WorkCounts, WorkScope};

#[derive(Default)]
pub(super) struct Mutation {
    turn: parking_lot::Mutex<()>,
    pub(super) work: WorkCounts,
}
pub(super) struct Turn<'a> {
    _turn: parking_lot::MutexGuard<'a, ()>,
    _work: WorkScope,
}
impl Mutation {
    pub(super) fn lock(&self) -> Turn<'_> {
        Turn {
            _turn: self.turn.lock(),
            _work: self.work.enter(),
        }
    }
}
