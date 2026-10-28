//! Seat reads that smithay answers under a lock the caller may already hold.
//!
//! Smithay keeps a seat's pointer and keyboard state behind a mutex that is not
//! re-entrant, and holds it while it delivers an event: a motion that moves the
//! focus calls [`SeatHandler::focus_changed`] and
//! [`SeatHandler::cursor_image`] with the lock held. A handler that asks the
//! seat for the pointer's location, focus or grab from there waits for a lock
//! its own thread holds, and the compositor stops answering without a
//! message. That shipped once: the cursor's output scale was read from the
//! seat inside `cursor_image`, so a pointer leaving a client froze the desktop.
//!
//! Every such callback opens a [`Callback`] scope, and every read of the seat's
//! locked state goes through the functions here. In a debug build a read inside
//! a scope panics at the call, naming it, so the first occurrence on any code
//! path is a backtrace rather than a hang. Release builds read straight
//! through; the check costs them nothing.
//!
//! [`SeatHandler::focus_changed`]: smithay::input::SeatHandler::focus_changed
//! [`SeatHandler::cursor_image`]: smithay::input::SeatHandler::cursor_image

use std::cell::Cell;

use smithay::input::keyboard::KeyboardHandle;
use smithay::input::pointer::PointerHandle;
use smithay::utils::{Logical, Point};

use super::state::State;

thread_local! {
    /// How many seat callbacks this thread is inside.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// A scope inside a [`SeatHandler`](smithay::input::SeatHandler) callback.
/// Held for the callback's whole body.
#[must_use = "the scope ends when the guard is dropped"]
pub struct Callback(());

impl Callback {
    /// Enter a seat callback on this thread.
    pub fn enter() -> Self {
        DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self(())
    }
}

impl Drop for Callback {
    fn drop(&mut self) {
        DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Whether this thread is inside a seat callback, and so holds the seat's lock.
#[must_use]
pub fn in_callback() -> bool {
    DEPTH.with(|depth| depth.get() > 0)
}

/// Fail loudly, in a debug build, when `read` is attempted under the seat's
/// lock: it would wait on itself.
#[track_caller]
fn assert_unlocked(read: &str) {
    assert!(
        !(cfg!(debug_assertions) && in_callback()),
        "{read} asked the seat while a seat callback holds its lock, which deadlocks the \
         compositor; keep what the callback needs in State (see State::pointer_at) and read that"
    );
}

#[track_caller]
pub fn pointer_location(pointer: &PointerHandle<State>) -> Point<f64, Logical> {
    assert_unlocked("the pointer's location");
    pointer.current_location()
}

/// Whether a grab is active on the seat's pointer.
#[track_caller]
pub fn pointer_is_grabbed(pointer: &PointerHandle<State>) -> bool {
    assert_unlocked("the pointer's grab");
    pointer.is_grabbed()
}

#[track_caller]
pub fn keyboard_focus(
    keyboard: &KeyboardHandle<State>,
) -> Option<<State as smithay::input::SeatHandler>::KeyboardFocus> {
    assert_unlocked("the keyboard's focus");
    keyboard.current_focus()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_callback_scope_is_counted_and_released() {
        assert!(!in_callback());
        {
            let _outer = Callback::enter();
            let _inner = Callback::enter();
            assert!(in_callback());
        }
        assert!(!in_callback());
    }

    #[test]
    #[should_panic(expected = "deadlocks the compositor")]
    fn reading_the_seat_inside_a_callback_panics_in_a_debug_build() {
        let _scope = Callback::enter();
        assert_unlocked("the pointer's location");
    }
}
