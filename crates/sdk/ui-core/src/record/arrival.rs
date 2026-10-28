//! Waking the windows that read a store when another replica's change
//! arrives in it, and what the room said of records it asked for.

use super::{RecordId, RecordStore};

impl RecordStore {
    /// Wake the window `reader` belongs to whenever another replica's change
    /// arrives in this store: merged into an open record
    /// ([`Self::merge_replica`]), or kept for a record it follows
    /// ([`Self::note_kept`], [`Self::announce`]). Nothing else tells a window
    /// that its records moved, so without it what arrived waits for the
    /// window's next input or timer. A closed window's handle is dropped.
    pub fn wake_on_arrival(&self, reader: crate::WakeHandle) {
        let set = self.0.borrow();
        let mut readers = set.readers.borrow_mut();
        readers.retain(crate::WakeHandle::is_active);
        if !readers.iter().any(|known| known.same_open_scope(&reader)) {
            readers.push(reader);
        }
    }

    /// Wake every window that reads this store ([`Self::wake_on_arrival`]).
    /// Callable while a frame reads the store.
    pub(super) fn wake_readers(&self) {
        let readers = match self.0.try_borrow() {
            Ok(set) => set.readers.borrow().clone(),
            Err(_) => return,
        };
        for reader in readers {
            reader.wake();
        }
    }

    /// The room's hosts sent everything they hold of `id`, a record this
    /// store asked them for ([`RecordId::arrives_on_demand`]), or no host is
    /// connected to ask: what of it is here is all there is for now. What
    /// reads it and still lacks it (a part held from its genesis that
    /// nothing arrived in) shows it unavailable rather than loading, until
    /// something of it arrives, which clears this. Wakes the store's
    /// readers. Callable while a frame reads the store.
    pub fn note_room_answered(&self, id: RecordId) {
        if let Ok(set) = self.0.try_borrow() {
            set.answered.borrow_mut().insert(id);
            set.kept.set(set.kept.get().wrapping_add(1));
        }
        self.wake_readers();
    }

    /// Whether the room's hosts sent everything they hold of `id`
    /// ([`Self::note_room_answered`]) and nothing of it arrived since.
    #[must_use]
    pub fn room_answered(&self, id: &RecordId) -> bool {
        self.0
            .try_borrow()
            .is_ok_and(|set| set.answered.borrow().contains(id))
    }

    /// Something of `id` arrived: more of it may follow.
    pub(super) fn arrived(&self, id: &RecordId) {
        if let Ok(set) = self.0.try_borrow() {
            set.answered.borrow_mut().remove(id);
        }
    }
}
