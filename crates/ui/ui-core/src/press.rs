//! The input serial of the person's press this window is handling, once it handled one.
//!
//! The compositor issues a serial with every input event it delivers, and the window reads it
//! with the event. An invocation a control makes is that press's: it names the serial, and the
//! host admits it as the person's own when the compositor issued that serial to this window. An
//! app cannot mint one, and the serial names no authority of its own: only the compositor's
//! record of the press it delivered, which the session hands the device's daemon, makes it count.
//!
//! The serial is kept per thread: the window's events, and the controls that answer them, are
//! that thread's.

use std::cell::Cell;

thread_local! {
    static NEWEST: Cell<Option<u32>> = const { Cell::new(None) };
}

/// The window handled the input delivered with `serial`: an invocation made from now on, until
/// newer input is handled, names it.
pub(crate) fn note(serial: u32) {
    NEWEST.with(|newest| newest.set(Some(serial)));
}

/// The serial of the newest input this thread's window handled, while it handled one: what an
/// invocation made now names as the press it followed.
#[must_use]
pub fn current() -> Option<u32> {
    NEWEST.with(Cell::get)
}

/// Forget the newest input, so a test's press does not outlive it on a reused thread.
#[cfg(test)]
pub(crate) fn forget() {
    NEWEST.with(|newest| newest.set(None));
}
