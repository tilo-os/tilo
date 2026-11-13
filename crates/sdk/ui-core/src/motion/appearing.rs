//! [`Appearing`] — one surface's [`Appear`] and dismiss, as the toolkit's popovers, menus, dropdowns,
//! tooltips, sheets and dialogs keep it.

use super::named::Appear;
use super::pose::Pose;
use super::presence::Presence;
use crate::tokens::{EffectiveMotion, MotionSource};
use std::cell::Cell;
use std::rc::Rc;
use vello::kurbo::{Point, Rect};

/// The coming and going of one surface on the standard [`Appear`] motion. [`Self::follow`] (once a
/// frame, from the draw, and wherever the owner opens or closes it) picks up a change and starts the
/// motion from where it is drawn: a reopen mid-exit reverses, repeated toggles never queue.
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

    /// Shown, at rest: a surface that is open from its first paint (first paint shows the resting
    /// state; motion explains changes).
    #[must_use]
    pub fn opened() -> Self {
        Self {
            presence: Presence::opened(),
            motion: EffectiveMotion::default(),
        }
    }

    /// Follow the owner's `open` state with the motion the person allows (`theme.motion()`), as the
    /// last input asked for it.
    #[track_caller]
    pub fn follow(&mut self, open: bool, motion: EffectiveMotion) {
        self.follow_from(open, motion, super::source::input_source());
    }

    /// Open or close it with the motion it last drew with, as the last input asked for it: what an
    /// owner's `open` and `close` call, so a surface whose owner stops drawing it once closed still
    /// arrives the next time.
    #[track_caller]
    pub fn show(&mut self, open: bool) {
        let motion = self.motion;
        self.follow(open, motion);
    }

    /// [`Self::show`] for a change `source` asked for: a host that opens and closes a surface of its
    /// own says what asked, since no input of the app did.
    #[track_caller]
    pub fn show_from(&mut self, open: bool, source: MotionSource) {
        let motion = self.motion;
        self.follow_from(open, motion, source);
    }

    /// [`Self::follow`] for a change `source` asked for (a hover dwell is the pointer's, whatever was
    /// pressed last).
    #[track_caller]
    pub fn follow_from(&mut self, open: bool, motion: EffectiveMotion, source: MotionSource) {
        self.motion = motion;
        if open == self.presence.is_open() {
            return;
        }
        self.presence.show(
            open,
            Appear::SPEC,
            self.motion,
            source,
            crate::motion::frame_now(),
        );
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
        self.presence.is_visible(crate::motion::frame_now())
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

    /// The pose of `surface` now, coming from `anchor`: the summoning control's centre, the caret, the
    /// pointer of a context menu, or the surface's own centre for a centred dialog. It comes from — or
    /// goes back to — the point of the surface nearest the anchor, at 92 % and 8 px toward it arriving,
    /// 97 % and 8 px leaving ([`Appearing`] says which of the two it is doing).
    #[must_use]
    pub fn pose(&self, surface: Rect, anchor: Point) -> Pose {
        Appear::pose(
            self.progress(),
            super::Direction::of(self.is_open()),
            surface,
            anchor,
            self.motion,
        )
    }
}

/// One [`Appearing`] a surface and the host that draws it share. A dialog belongs to the tree its app
/// builds, and an app builds a new tree whenever its state changes: a dialog built open would arrive
/// again on every rebuild, so it is built at rest instead — and then it never arrives at all.
#[derive(Clone, Debug)]
pub enum Appearance {
    /// The surface keeps its own: built open, it is drawn at rest from its first paint.
    Owned(Appearing),
    /// Kept by the host that rebuilds the surface's tree.
    Shared(Rc<Cell<Appearing>>),
}

impl Default for Appearance {
    fn default() -> Self {
        Self::Owned(Appearing::closed())
    }
}

impl Appearance {
    /// An appearance a host shares with every rebuild of a surface it draws, starting closed: the first
    /// paint of a surface built open arrives.
    #[must_use]
    pub fn shared() -> Self {
        Self::Shared(Rc::new(Cell::new(Appearing::closed())))
    }

    /// Whether the surface keeps this appearance itself: one built open is drawn at rest, since a
    /// rebuild would otherwise replay its arrival.
    #[must_use]
    pub const fn is_owned(&self) -> bool {
        matches!(self, Self::Owned(_))
    }

    fn get(&self) -> Appearing {
        match self {
            Self::Owned(appearing) => *appearing,
            Self::Shared(shared) => shared.get(),
        }
    }

    fn with(&mut self, change: impl FnOnce(&mut Appearing)) {
        match self {
            Self::Owned(appearing) => change(appearing),
            Self::Shared(shared) => {
                let mut appearing = shared.get();
                change(&mut appearing);
                shared.set(appearing);
            }
        }
    }

    /// [`Appearing::follow`].
    #[track_caller]
    pub fn follow(&mut self, open: bool, motion: EffectiveMotion) {
        self.with(|appearing| appearing.follow(open, motion));
    }

    /// [`Appearing::follow_from`].
    #[track_caller]
    pub fn follow_from(&mut self, open: bool, motion: EffectiveMotion, source: MotionSource) {
        self.with(|appearing| appearing.follow_from(open, motion, source));
    }

    /// [`Appearing::show`].
    #[track_caller]
    pub fn show(&mut self, open: bool) {
        self.with(|appearing| appearing.show(open));
    }

    /// [`Appearing::show_from`].
    #[track_caller]
    pub fn show_from(&mut self, open: bool, source: MotionSource) {
        self.with(|appearing| appearing.show_from(open, source));
    }

    /// [`Appearing::snap`].
    pub fn snap(&mut self, open: bool) {
        self.with(|appearing| appearing.snap(open));
    }

    /// [`Appearing::is_open`].
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.get().is_open()
    }

    /// [`Appearing::is_drawn`].
    #[must_use]
    pub fn is_drawn(&self) -> bool {
        self.get().is_drawn()
    }

    /// [`Appearing::is_moving`].
    #[must_use]
    pub fn is_moving(&self) -> bool {
        self.get().is_moving()
    }

    /// [`Appearing::progress`].
    #[must_use]
    pub fn progress(&self) -> f64 {
        self.get().progress()
    }

    /// [`Appearing::pose`].
    #[must_use]
    pub fn pose(&self, surface: Rect, anchor: Point) -> Pose {
        self.get().pose(surface, anchor)
    }
}
