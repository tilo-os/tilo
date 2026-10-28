//! TILO(work-count): what applying changes, finding ops by id and walking
//! the change graph did on this thread, counted so that tests pin how that
//! work grows with a document's history instead of timing it
//! (vendor/automerge/TILO-PATCH.md).

use std::cell::Cell;

/// Work this thread has done since it started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    /// Document ops an apply stepped over one at a time rather than seeked
    /// past.
    pub walked_ops: u64,
    /// Slabs of the op id column that searches for an op by its id may
    /// have decoded.
    pub searched_slabs: u64,
    /// Changes visited walking the change graph's ancestry.
    pub visited_changes: u64,
}

thread_local! {
    static WORK: Cell<Work> = const {
        Cell::new(Work {
            walked_ops: 0,
            searched_slabs: 0,
            visited_changes: 0,
        })
    };
}

/// The work this thread has done so far.
pub fn work() -> Work {
    WORK.with(Cell::get)
}

fn count(add: impl FnOnce(&mut Work)) {
    WORK.with(|work| {
        let mut now = work.get();
        add(&mut now);
        work.set(now);
    });
}

#[inline]
pub(crate) fn walked_op() {
    count(|work| work.walked_ops = work.walked_ops.saturating_add(1));
}

#[inline]
pub(crate) fn searched_slabs(slabs: usize) {
    count(|work| work.searched_slabs = work.searched_slabs.saturating_add(slabs as u64));
}

#[inline]
pub(crate) fn visited_change() {
    count(|work| work.visited_changes = work.visited_changes.saturating_add(1));
}
