//! The motion clock's two thread-wide facts: which motions still need
//! frames, and (for captures and tests only) a clock that reads every motion
//! at its end.
//!
//! **Frames.** A surface that starts a motion says until when it moves
//! ([`want_frames_until`]); the window's frame pump asks [`frames_wanted`]
//! besides polling its widget tree. So a popover or menu that an owner draws
//! in its own overlay pass, outside the tree the pump polls, still gets the
//! frames of its entrance and exit, and nothing asks for frames once it has
//! settled (rule 8: a settled motion costs nothing).
//!
//! **Settled clock.** [`testing::settled`] makes every motion on this thread
//! read as arrived: a capture or a single-frame test sees the resting pose of
//! what it opened, deterministically, without sleeping through the motion.
//! Production never sets it; motion is never switched off for a test.

use crate::time::Instant;
use std::cell::Cell;

thread_local! {
    /// The latest instant a started motion on this thread settles at.
    static FRAMES_UNTIL: Cell<Option<Instant>> = const { Cell::new(None) };
    /// How many [`testing::settled`] scopes are open on this thread.
    static SETTLED: Cell<u32> = const { Cell::new(0) };
}

/// A motion on this thread moves until `until`: the frame pump draws frames
/// until then.
pub fn want_frames_until(until: Instant) {
    FRAMES_UNTIL.with(|cell| {
        if cell.get().is_none_or(|current| current < until) {
            cell.set(Some(until));
        }
    });
}

/// Whether a motion started on this thread is still moving at `now`.
#[must_use]
pub fn frames_wanted(now: Instant) -> bool {
    if testing::is_settled() {
        return false;
    }
    FRAMES_UNTIL.with(|cell| match cell.get() {
        Some(until) if now < until => true,
        Some(_) => {
            cell.set(None);
            false
        }
        None => false,
    })
}

/// When the motions started on this thread have all settled; `None` when
/// none is moving.
#[must_use]
pub fn frames_until() -> Option<Instant> {
    FRAMES_UNTIL.with(Cell::get)
}

/// The settled clock, for captures and tests.
pub mod testing {
    use super::SETTLED;

    /// While held, every motion on this thread reads as arrived: a
    /// [`super::super::MotionValue`] returns its target and is not moving, a
    /// [`super::super::Presence`] is fully open or fully closed. Dropping it
    /// restores the running clock. Never for production frames.
    #[must_use = "the clock reads settled only while the guard is held"]
    pub struct Settled(());

    impl Drop for Settled {
        fn drop(&mut self) {
            SETTLED.with(|count| count.set(count.get().saturating_sub(1)));
        }
    }

    /// Read every motion on this thread at its end until the guard drops: a
    /// single-frame test or a golden capture sees what it opened at rest.
    pub fn settled() -> Settled {
        SETTLED.with(|count| count.set(count.get().saturating_add(1)));
        Settled(())
    }

    /// Whether a [`settled`] scope is open on this thread.
    #[must_use]
    pub fn is_settled() -> bool {
        SETTLED.with(|count| count.get() > 0)
    }
}
