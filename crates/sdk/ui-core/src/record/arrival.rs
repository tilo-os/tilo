//! Waking the windows that read a store when another replica's change
//! arrives in it.

use super::RecordStore;

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
}
