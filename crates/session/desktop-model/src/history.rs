//! A workspace's append-only history, and serialising without it.
//!
//! A workspace keeps every effect receipt, every task's audit trail and every
//! ownership-transfer record. They only ever grow, so whatever serialises a
//! whole tree for a reader that wants the desktop as it is now (the control
//! socket's state dump) pays for the desktop's whole past. [`without_history`]
//! leaves those lists out of every `Serialize` it runs. Persistence and
//! replication never run inside it, so they always write them.

use std::cell::Cell;

thread_local! {
    static OMIT_HISTORY: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` with the history fields left out of whatever it serialises on
/// this thread: [`Workspace::effect_receipts`],
/// [`Workspace::ownership_transfer_audit`] and
/// [`Workspace::ownership_transfer_cancellations`]. The fields are absent, not empty, so a reader
/// cannot mistake the omission for "nothing happened". Nested calls are fine.
///
/// [`Workspace::effect_receipts`]: crate::Workspace::effect_receipts
/// [`Workspace::ownership_transfer_audit`]: crate::Workspace::ownership_transfer_audit
/// [`Workspace::ownership_transfer_cancellations`]: crate::Workspace::ownership_transfer_cancellations
pub fn without_history<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            OMIT_HISTORY.with(|omit| omit.set(self.0));
        }
    }
    let _restore = Restore(OMIT_HISTORY.with(|omit| omit.replace(true)));
    f()
}

/// `skip_serializing_if` for a history field: true only inside
/// [`without_history`].
pub(crate) fn omitted<T: ?Sized>(_: &T) -> bool {
    OMIT_HISTORY.with(Cell::get)
}
