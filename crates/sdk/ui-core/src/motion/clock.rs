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

/// One motion's wish for frames: until when it moves, who asked, and the
/// motion's own key when it can take the wish back ([`retract`]).
#[derive(Clone, Copy)]
struct Wish {
    key: Option<u64>,
    caller: &'static Location<'static>,
    until: Instant,
}

thread_local! {
    /// What the motions on this thread still need frames for. A wish is
    /// met once a frame is drawn at or after its end ([`frame_drawn`]), and
    /// gone at once when its motion lands early ([`retract`]): a motion that
    /// was jumped to its end, or dropped mid-way by its owner, asks for no
    /// frames it will never draw.
    static WISHES: RefCell<Vec<Wish>> = const { RefCell::new(Vec::new()) };
    /// The instant the latest drawn frame on this thread read its motions
    /// at ([`frame_drawn`]): the frame at rest is owed until one is drawn at
    /// or after a wish's end.
    static DRAWN_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    /// The next key [`motion_key`] hands out.
    static NEXT_KEY: Cell<u64> = const { Cell::new(1) };
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
    wish(None, Location::caller(), until);
}

/// A key for one motion's wish ([`ask`], [`retract`]).
pub(crate) fn motion_key() -> u64 {
    NEXT_KEY.with(|next| {
        let key = next.get();
        next.set(key.wrapping_add(1).max(1));
        key
    })
}

/// The motion `key` moves until `until`; it replaces what that motion asked
/// before (a retarget or a deferral moves its end).
#[track_caller]
pub(crate) fn ask(key: u64, until: Instant) {
    wish(Some(key), Location::caller(), until);
}

/// The motion `key` landed (jumped to its end) or was let go: it needs no
/// more frames, not even one at rest, since whoever lands it repaints.
pub(crate) fn retract(key: u64) {
    WISHES.with(|wishes| wishes.borrow_mut().retain(|wish| wish.key != Some(key)));
}

fn wish(key: Option<u64>, caller: &'static Location<'static>, until: Instant) {
    let drawn = DRAWN_AT.with(Cell::get);
    WISHES.with(|wishes| {
        let mut wishes = wishes.borrow_mut();
        wishes.retain(|wish| !met(wish, drawn));
        let same = |wish: &&mut Wish| match key {
            Some(_) => wish.key == key,
            None => wish.key.is_none() && wish.caller == caller,
        };
        match wishes.iter_mut().find(same) {
            Some(wish) if key.is_some() => {
                wish.until = until;
                wish.caller = caller;
            }
            Some(wish) => wish.until = wish.until.max(until),
            None => wishes.push(Wish { key, caller, until }),
        }
    });
}

/// Whether `wish` has had the frame showing its end drawn.
fn met(wish: &Wish, drawn: Option<Instant>) -> bool {
    drawn.is_some_and(|drawn| drawn >= wish.until)
}

/// Where the motions that still need frames at `now` asked for them
/// (`file:line` of each caller), for diagnostics: the ones moving, and the
/// ones whose frame at rest is still owed.
#[must_use]
pub fn frames_wanted_by(now: Instant) -> Vec<String> {
    if testing::is_settled() {
        return Vec::new();
    }
    let drawn = DRAWN_AT.with(Cell::get);
    WISHES.with(|wishes| {
        wishes
            .borrow()
            .iter()
            .filter(|wish| now < wish.until || !met(wish, drawn))
            .map(|wish| format!("{}:{}", wish.caller.file(), wish.caller.line()))
            .collect()
    })
}

/// Whether a motion on this thread still needs a frame at `now`: it is
/// moving, or it has settled since the last frame was drawn and the frame
/// showing it at rest is still owed. A frame read while a motion was a hair
/// from its end leaves that last sliver on screen otherwise.
#[must_use]
pub fn frames_wanted(now: Instant) -> bool {
    if testing::is_settled() {
        return false;
    }
    let drawn = DRAWN_AT.with(Cell::get);
    WISHES.with(|wishes| {
        let mut wishes = wishes.borrow_mut();
        wishes.retain(|wish| now < wish.until || !met(wish, drawn));
        !wishes.is_empty()
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
    WISHES.with(|wishes| wishes.borrow_mut().clear());
    DRAWN_AT.with(|cell| cell.set(None));
}

/// When the motions on this thread that still need frames have all
/// settled; `None` when none does.
#[must_use]
pub fn frames_until() -> Option<Instant> {
    let drawn = DRAWN_AT.with(Cell::get);
    WISHES.with(|wishes| {
        wishes
            .borrow()
            .iter()
            .filter(|wish| !met(wish, drawn))
            .map(|wish| wish.until)
            .max()
    })
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
