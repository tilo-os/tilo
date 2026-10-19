//! [`Appearing`]: one surface's [`Appear`] and dismiss, as the toolkit's
//! popovers, menus, dropdowns, tooltips, sheets and dialogs keep it.

use super::named::Appear;
use super::pose::Pose;
use super::presence::Presence;
use crate::tokens::{EffectiveMotion, MotionSource};
use vello::kurbo::{Point, Rect};

/// The coming and going of one surface on the standard [`Appear`] motion.
///
/// The owner keeps its own open state, which input acts on at once; this
/// only says how the surface is drawn. [`Self::follow`] (once a frame, from
/// the draw, and wherever the owner opens or closes it) picks up a change
/// and starts the motion from where it is drawn: a reopen mid-exit reverses,
/// repeated toggles never queue. What asked for the change is the input the
/// toolkit handled last ([`super::input_source`]): a key lands it at once, a
/// click moves it. A closing surface keeps being drawn until it has gone,
/// but takes no input (a click goes to whatever is under it).
///
/// Every motion it starts asks the frame pump for frames until it settles
/// ([`super::want_frames_until`]), so a menu an owner draws outside the
/// polled tree still moves, and nothing asks once it rests.
#[derive(Clone, Copy, Debug, Default)]
pub struct Appearing {
    presence: Presence,
    motion: EffectiveMotion,
}

impl Appearing {
    /// Not shown, at rest.
    #[must_use]
    pub fn closed() -> Self {
        Self {
            presence: Presence::closed(),
            motion: EffectiveMotion::default(),
        }
    }

    /// Shown, at rest: a surface that is open from its first paint (first
    /// paint shows the resting state; motion explains changes).
    #[must_use]
    pub fn opened() -> Self {
        Self {
            presence: Presence::opened(),
            motion: EffectiveMotion::default(),
        }
    }

    /// Follow the owner's `open` state with the motion the person allows
    /// (`theme.motion()`), as the last input asked for it.
    pub fn follow(&mut self, open: bool, motion: EffectiveMotion) {
        self.follow_from(open, motion, super::source::input_source());
    }

    /// Open or close it with the motion it last drew with, as the last input
    /// asked for it: what an owner's `open` and `close` call, so a surface
    /// whose owner stops drawing it once closed still arrives the next time.
    pub fn show(&mut self, open: bool) {
        let motion = self.motion;
        self.follow(open, motion);
    }

    /// [`Self::follow`] for a change `source` asked for (a hover dwell is the
    /// pointer's, whatever was pressed last).
    pub fn follow_from(&mut self, open: bool, motion: EffectiveMotion, source: MotionSource) {
        self.motion = motion;
        if open == self.presence.is_open() {
            return;
        }
        self.presence
            .show(open, Appear::SPEC, self.motion, source, crate::time::now());
        if let Some(until) = self.presence.settles_at() {
            super::clock::want_frames_until(until);
        }
    }

    /// Jump to `open`, at rest (a surface whose owner was rebuilt open).
    pub fn snap(&mut self, open: bool) {
        self.presence.snap(open);
    }

    /// The state it is going to.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.presence.is_open()
    }

    /// Whether anything of it is drawn now: open, opening or still leaving.
    #[must_use]
    pub fn is_drawn(&self) -> bool {
        self.presence.is_visible(crate::time::now())
    }

    /// Whether it is still moving now (its owner asks for frames).
    #[must_use]
    pub fn is_moving(&self) -> bool {
        self.presence.is_moving(crate::motion::frame_now())
    }

    /// How open it is drawn now, `0..=1`.
    #[must_use]
    pub fn progress(&self) -> f64 {
        self.presence.progress(crate::motion::frame_now())
    }

    /// The pose of `surface` now, coming from `anchor`: the summoning
    /// control's centre, the caret, the pointer of a context menu, or the
    /// surface's own centre for a centred dialog. It scales about the point
    /// of the surface nearest the anchor ([`Appear::origin`]).
    #[must_use]
    pub fn pose(&self, surface: Rect, anchor: Point) -> Pose {
        Appear::pose(
            self.progress(),
            Appear::origin(surface, anchor),
            self.motion,
        )
    }
}
