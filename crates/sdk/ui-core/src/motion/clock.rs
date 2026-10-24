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
use std::cell::{Cell, RefCell};
use std::panic::Location;

thread_local! {
    /// The latest instant a started motion on this thread settles at.
    static FRAMES_UNTIL: Cell<Option<Instant>> = const { Cell::new(None) };
    /// The instant the latest drawn frame on this thread read its motions
    /// at ([`frame_drawn`]): the frame at rest is owed until one is drawn at
    /// or after [`FRAMES_UNTIL`].
    static DRAWN_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Where each motion still moving was started, and until when: who
    /// [`frames_wanted`] asks for, for diagnostics ([`frames_wanted_by`]).
    /// Pushed only when a motion starts, never per frame.
    static FRAMES_ASKED_BY: RefCell<Vec<(&'static Location<'static>, Instant)>> =
        const { RefCell::new(Vec::new()) };
    /// How many [`testing::settled`] scopes are open on this thread.
    static SETTLED: Cell<u32> = const { Cell::new(0) };
    /// The instant this thread's frame reads its motions at, while a frame's
    /// layout is pinned ([`pin_frame`]).
    static FRAME_AT: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// The instant a motion is read at: the frame's, while its layout is pinned
/// (`pin_frame`), else now. A widget that sizes itself by a motion (a
/// disclosure opening) reads it here, so every layout pass of one frame sees
/// the same pose and the frame's composition can settle; read at the clock,
/// each pass saw the motion a little further on and the passes never agreed.
#[must_use]
pub fn frame_now() -> Instant {
    FRAME_AT.with(Cell::get).unwrap_or_else(crate::time::now)
}

/// While the guard lives, motions on this thread read at the instant it was
/// taken ([`frame_now`]). Nested pins keep the outer frame's instant.
pub(crate) fn pin_frame() -> PinnedFrame {
    let at = frame_now();
    PinnedFrame(FRAME_AT.with(|cell| cell.replace(Some(at))))
}

/// In a debug build, fail a motion that is read while a frame's layout is
/// pinned at any instant but the frame's: a widget that sizes or places
/// itself by a motion must read it at [`frame_now`], or the frame's layout
/// passes disagree and its composition cannot settle. Only a moving motion
/// is checked; one at rest reads the same at any instant.
#[track_caller]
pub(crate) fn debug_assert_frame_instant(now: Instant) {
    if cfg!(debug_assertions)
        && let Some(at) = FRAME_AT.with(Cell::get)
    {
        assert!(
            now == at,
            "a motion read during layout at the live clock: read it at motion::frame_now()"
        );
    }
}

/// Restores the frame clock [`pin_frame`] pinned.
pub(crate) struct PinnedFrame(Option<Instant>);

impl Drop for PinnedFrame {
    fn drop(&mut self) {
        FRAME_AT.with(|cell| cell.set(self.0));
        if self.0.is_none() {
            // The frame's layout is done: what a rebuild before it dropped
            // and nothing took up is let go (`motion::keep`).
            super::frame_ended();
        }
    }
}

/// A motion on this thread moves until `until`: the frame pump draws frames
/// until then. The caller is remembered as the owner of those frames
/// ([`frames_wanted_by`]), so a motion asking here is named while it moves.
#[track_caller]
pub fn want_frames_until(until: Instant) {
    FRAMES_UNTIL.with(|cell| {
        if cell.get().is_none_or(|current| current < until) {
            cell.set(Some(until));
        }
    });
    let caller = Location::caller();
    let now = frame_now();
    FRAMES_ASKED_BY.with(|asked| {
        let mut asked = asked.borrow_mut();
        let drawn = DRAWN_AT.with(Cell::get);
        asked.retain(|(_, at)| *at > now || drawn.is_none_or(|drawn| drawn < *at));
        match asked.iter_mut().find(|(asker, _)| *asker == caller) {
            Some((_, latest)) => *latest = (*latest).max(until),
            None => asked.push((caller, until)),
        }
    });
}

/// Where the motions that still need frames at `now` asked for them
/// (`file:line` of each [`want_frames_until`] caller), for diagnostics:
/// the ones moving, and the ones whose frame at rest is still owed.
#[must_use]
pub fn frames_wanted_by(now: Instant) -> Vec<String> {
    if testing::is_settled() {
        return Vec::new();
    }
    let drawn = DRAWN_AT.with(Cell::get);
    FRAMES_ASKED_BY.with(|asked| {
        asked
            .borrow()
            .iter()
            .filter(|(_, until)| now < *until || drawn.is_none_or(|drawn| drawn < *until))
            .map(|(caller, _)| format!("{}:{}", caller.file(), caller.line()))
            .collect()
    })
}

/// Whether a motion started on this thread still needs a frame at `now`:
/// it is moving, or it has settled since the last frame was drawn and the
/// frame showing it at rest is still owed. A frame read while a motion was
/// a hair from its end leaves that last sliver on screen otherwise.
#[must_use]
pub fn frames_wanted(now: Instant) -> bool {
    if testing::is_settled() {
        return false;
    }
    FRAMES_UNTIL.with(|cell| match cell.get() {
        Some(until) if now < until => true,
        Some(until) => {
            if DRAWN_AT.with(Cell::get).is_some_and(|drawn| drawn >= until) {
                cell.set(None);
                false
            } else {
                true
            }
        }
        None => false,
    })
}

/// A frame was drawn reading its motions at `at` (taken before drawing,
/// so every motion the frame read was at `at` or later). Every host that
/// asks [`frames_wanted`] says so for each frame it draws: the frame at
/// rest after a motion ends is owed until one drawn at or after its end.
pub fn frame_drawn(at: Instant) {
    DRAWN_AT.with(|cell| {
        if cell.get().is_none_or(|drawn| drawn < at) {
            cell.set(Some(at));
        }
    });
}

/// Forget every frame wish and drawn frame on this thread: a test starts
/// from a clock nothing has asked.
#[cfg(test)]
pub(crate) fn forget_frames_for_tests() {
    FRAMES_UNTIL.with(|cell| cell.set(None));
    DRAWN_AT.with(|cell| cell.set(None));
    FRAMES_ASKED_BY.with(|asked| asked.borrow_mut().clear());
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
