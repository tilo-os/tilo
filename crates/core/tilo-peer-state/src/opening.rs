//! Opening a sealed document's history a turn at a time.
//!
//! A newcomer's first read of a large sealed document opens and merges every
//! write in its history: seconds for a tracker of thousands of issues. On a
//! page, whose one thread also draws and answers input, a read made within
//! [`in_turns`] stops once its turn is over, keeps what it opened for the
//! next read (`KeptDocument`), and answers that the document is not all here
//! yet. The caller reads it again in a later task, which goes on from there:
//! the document reads only once it is whole, so nothing reads half of it.
//!
//! Outside [`in_turns`] a read opens the whole history at once, as it always
//! did.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

thread_local! {
    static DUE: RefCell<Option<Rc<dyn Fn() -> bool>>> = const { RefCell::new(None) };
    static STOPPED: Cell<bool> = const { Cell::new(false) };
}

/// Call `read`, in which a sealed document's history is opened only until
/// `due` says the caller's turn is over; whether a read in it stopped early,
/// so the document is to be read again in a later turn.
pub fn in_turns<T>(due: impl Fn() -> bool + 'static, read: impl FnOnce() -> T) -> (T, bool) {
    struct Scope(Option<Rc<dyn Fn() -> bool>>, bool);
    impl Drop for Scope {
        fn drop(&mut self) {
            DUE.with_borrow_mut(|due| *due = self.0.take());
            STOPPED.set(self.1);
        }
    }
    let outer = DUE.with_borrow_mut(|held| held.replace(Rc::new(due)));
    let scope = Scope(outer, STOPPED.replace(false));
    let value = read();
    let stopped = STOPPED.get();
    drop(scope);
    (value, stopped)
}

/// Whether reads on this thread open a history a turn at a time.
pub(crate) fn in_a_turn() -> bool {
    DUE.with_borrow(Option::is_some)
}

/// Whether the current turn is over.
pub(crate) fn due() -> bool {
    DUE.with_borrow(Clone::clone).is_some_and(|due| due())
}

/// A read stopped with its history partly opened.
pub(crate) fn stopped() {
    STOPPED.set(true);
}
