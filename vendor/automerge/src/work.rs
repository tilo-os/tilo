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
    /// Objects whose id an actor sorting before others renumbered: what taking in a change by
    /// a new actor rewrites of the document's object index besides the change.
    pub renumbered_objects: u64,
    /// Ops of objects read to tell which changes last wrote in them
    /// (`Automerge::latest_changes_within`).
    pub object_ops: u64,
    /// Changes rebuilt from the op set (`Automerge::get_changes` and its kin): each decodes every
    /// op the change made.
    pub rebuilt_changes: u64,
    /// Runs of values the op set's columns took in: a splice of like ops,
    /// such as the letters of a text written at once, gives each column one.
    pub column_runs: u64,
    /// Splices into the op set: each costs every column a seek to its place, however few ops it
    /// carries.
    pub op_set_splices: u64,
    /// Runs of values the columns of a change being built took in: like ops, such as a text's
    /// letters, give each column one.
    pub change_runs: u64,
}

thread_local! {
    static WORK: Cell<Work> = const {
        Cell::new(Work {
            walked_ops: 0,
            searched_slabs: 0,
            visited_changes: 0,
            renumbered_objects: 0,
            object_ops: 0,
            rebuilt_changes: 0,
            column_runs: 0,
            op_set_splices: 0,
            change_runs: 0,
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

#[inline]
pub(crate) fn renumbered_objects(objects: usize) {
    count(|work| {
        work.renumbered_objects = work.renumbered_objects.saturating_add(objects as u64);
    });
}

#[inline]
pub(crate) fn read_object_ops(ops: usize) {
    count(|work| work.object_ops = work.object_ops.saturating_add(ops as u64));
}

#[inline]
pub(crate) fn rebuilt_changes(changes: usize) {
    count(|work| work.rebuilt_changes = work.rebuilt_changes.saturating_add(changes as u64));
}

#[inline]
pub(crate) fn wrote_column_run() {
    count(|work| work.column_runs = work.column_runs.saturating_add(1));
}

#[inline]
pub(crate) fn spliced_op_set() {
    count(|work| work.op_set_splices = work.op_set_splices.saturating_add(1));
}

#[inline]
pub(crate) fn wrote_change_run() {
    count(|work| work.change_runs = work.change_runs.saturating_add(1));
}
