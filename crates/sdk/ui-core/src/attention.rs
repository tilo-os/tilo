//! Attention, and the ledger: the two places a thing the system says can go.
//!
//! A person has two questions about what the system is doing — **what wants
//! me?** and **what happened?** — and this module is both answers, kept
//! apart on one rule ([`publish`]):
//!
//! * something that **needs a decision** waits in [`items`], one row each,
//!   with the app that asks (icon *and* name), what it is about, since when,
//!   and **one** thing the person can do about it. It never expires on its
//!   own; it waits, it is seen, it is deferred (and the count of what is
//!   deferred stays on screen), or it is acted on. A row that cannot act is
//!   not a row: [`Action`] is required to make one.
//! * something that **just happened** is written to the ledger
//!   ([`ledger`]): append-only, attributed, never on a timer, never stealing
//!   focus. Activity's History reads it beside the workspace's changes, so
//!   "3 files moved to Trash" is where "the budget note was edited" is.
//!
//! Both are the system's, not a window's: the registries live in the
//! process, so a row raised in one app is read in another and survives the
//! window that raised it.
//!
//! ```ignore
//! // An outcome that needs a person: it waits.
//! ui::attention::publish(
//!     ui::attention::Notice::needs(owner, "Couldn’t reach the printer", action)
//!         .because("The print service stopped answering."),
//! );
//! // An outcome that does not: it is recorded.
//! ui::attention::publish(ui::attention::Notice::did(owner, "Moved 3 files to Trash"));
//! ```
//!
//! This replaces the transient notice as a category. A toast claims
//! something matters right now and will not matter in four seconds; both
//! halves are usually false, and the timer teaches people not to read. What
//! disappears is the timer, not the message.
//!
//! The surface is reachable from anywhere: [`CHORD`], [`open`], and the
//! count ([`unseen`]) a window's bar can show.

mod ledger;
mod pane;
mod record;
#[cfg(test)]
mod tests;

pub use ledger::{Entry, clear as clear_ledger, newest_first, search as search_ledger};
pub use pane::{Press, Presses, Stack, apply, pane, pill};
pub use record::{
    Acted, Action, Item, ItemId, Notice, Owner, Published, SHOW_JOB, State, Target, Urgency, act,
    clear, defer, deferred, dismiss, dismissed, items, publish, raise, revision, seen, take,
    unseen, waiting,
};

use crate::wake::WakeHandle;

/// The chord that opens Needs you, wherever the person is.
pub const CHORD: &str = "ctrl+shift+a";

/// An app whose own identity a raised notice does not carry: one raised by
/// the toolkit itself, for a failure it saw (a job that could not finish).
pub const UNKNOWN_APP: &str = "so.tilo.system";

thread_local! {
    static OPENING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Ask for Needs you: what [`CHORD`], the bar's bell and a record of
/// "3 more" do.
pub fn open() {
    OPENING.with(|opening| opening.set(true));
}

/// Whether the surface was asked for since the last call.
#[must_use]
pub fn take_opening() -> bool {
    OPENING.with(|opening| opening.replace(false))
}

/// Call `wake` whenever an item is raised, seen, deferred or acted on, until
/// the returned guard drops: a surface that shows what is waiting redraws
/// then, rather than on a timer.
#[must_use = "the surface stops hearing about attention when the guard drops"]
pub fn watch(wake: WakeHandle) -> Watch {
    record::watch(wake)
}

/// The ledger's entries, oldest first: every outcome the system recorded.
#[must_use]
pub fn entries() -> Vec<Entry> {
    ledger::entries()
}

/// How many entries the ledger holds.
#[must_use]
pub fn recorded() -> usize {
    ledger::len()
}

/// One test at a time: the stack and the ledger are the process's, as the
/// real ones are, so two tests running at once would see each other's rows.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    use crate::lock::MutexRecover as _;
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock_recover()
}

/// A registered watcher of the waiting stack; dropping it stops the wakes.
pub struct Watch {
    id: u64,
}

impl Watch {
    pub(crate) const fn new(id: u64) -> Self {
        Self { id }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        record::unwatch(self.id);
    }
}
