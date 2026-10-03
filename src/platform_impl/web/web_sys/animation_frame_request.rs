use std::cell::Cell;

/// Coalesce redraw demand without moving an already scheduled callback later.
/// Another window can request us again while the browser dispatches this frame.
pub(super) fn request(pending: &Cell<Option<i32>>, schedule: impl FnOnce() -> i32) {
    if pending.get().is_none() {
        pending.set(Some(schedule()));
    }
}
