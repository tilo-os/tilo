//! A window's content whose first frame is not drawn yet: the arrival model
//! ([`crate::arriving`]) at the scale of a tile.
//!
//! A first frame from this device may hold the window's first frame up to
//! one frame budget ([`ArrivalClock::holds_frame_until`]); one that lands
//! inside it shows at once, with no placeholder and no motion. Any other
//! reserves the window's place at its final size from its first shown frame:
//! the ground alone, then the framework's placeholder ([`ArrivalFace`] in
//! the shape it declared, else its ground alone), saying where the frame comes from
//! after a while. The first frame cross-fades in over what stood there
//! ([`CrossFade::arrive`]) once it is due. Every step is a deadline: a tile
//! waiting between steps asks for no frames.

use crate::arriving::{ArrivalClock, Progress, Step};
use crate::motion::CrossFade;
use crate::time::Instant;
use crate::tokens::EffectiveMotion;

/// The arrival of one tile's content. See the module docs.
#[derive(Clone)]
pub(super) struct TileArrival {
    clock: ArrivalClock,
    /// `false` while the placeholder stands, `true` once the content shows.
    fade: CrossFade<bool>,
    /// The placeholder step the content replaced: what fades out.
    replaced: Step,
    /// The placeholder step and amount the last frame painted.
    drawn: Option<(Step, Option<Progress>)>,
}

/// What a tile paints of its arrival at an instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum ArrivalPaint {
    /// The placeholder at this step, and no content.
    Placeholder(Step),
    /// The content fading in over the step it replaced.
    Fading {
        replaced: Step,
        outgoing: f32,
        incoming: f32,
    },
}

impl TileArrival {
    pub(super) fn new(clock: ArrivalClock) -> Self {
        Self {
            clock,
            fade: CrossFade::new(false),
            replaced: Step::Ground,
            drawn: None,
        }
    }

    pub(super) fn clock(&self) -> &ArrivalClock {
        &self.clock
    }

    /// The content took the tile over: its arrival goes on with the same
    /// timing, answered from where the new content comes from.
    pub(super) fn answered_from(&self, from: crate::arriving::Source) -> Self {
        Self::new(self.clock.answered_from(from))
    }

    /// Whether the content shows (or is fading in).
    pub(super) fn revealed(&self) -> bool {
        *self.fade.shown()
    }

    /// The content's first frame landed at `at`.
    pub(super) fn arrive(&mut self, at: Instant) {
        self.clock.arrive(at);
    }

    /// The placeholder step standing at `now` before the reveal: a reveal
    /// that is due shows what stood just before it until it is made.
    pub(super) fn placeholder_step(&self, now: Instant) -> Step {
        match self.clock.step(now) {
            Step::Shown => self.clock.replaced_step(),
            step => step,
        }
    }

    /// Show the content once it is due: at once when it came inside its
    /// frame budget, else cross-fading in over what it replaces.
    pub(super) fn reveal_when_due(&mut self, motion: EffectiveMotion, now: Instant) {
        if self.revealed() || self.clock.shows_at().is_none_or(|at| now < at) {
            return;
        }
        self.replaced = self.clock.replaced_step();
        if self.clock.arrives_with_motion() {
            self.fade.arrive(true, motion, now);
        } else {
            self.fade = CrossFade::new(true);
        }
    }

    /// What shows at `now`; `None` once the content alone does.
    pub(super) fn paint_at(&self, now: Instant) -> Option<ArrivalPaint> {
        if !self.revealed() {
            return Some(ArrivalPaint::Placeholder(self.placeholder_step(now)));
        }
        if !self.fade.is_moving(now) {
            return None;
        }
        Some(ArrivalPaint::Fading {
            replaced: self.replaced,
            outgoing: self.fade.outgoing(now).map_or(0.0, |(_, opacity)| opacity),
            incoming: self.fade.incoming(now),
        })
    }

    pub(super) fn note_drawn(&mut self, step: Step, progress: Option<Progress>) {
        self.drawn = Some((step, progress));
    }

    /// Whether what the tile shows of its arrival changed since it was last
    /// painted: a step came due, the content's first frame is due, or the
    /// amount it says has come moved.
    pub(super) fn needs_redraw(&self, now: Instant, progress: Option<Progress>) -> bool {
        if self.revealed() {
            return self.fade.is_moving(now);
        }
        self.drawn != Some((self.clock.step(now), progress))
    }

    /// Whether the cross-fade still moves.
    pub(super) fn is_moving(&self, now: Instant) -> bool {
        self.fade.is_moving(now)
    }

    /// The next instant what shows changes without anything else happening:
    /// the next placeholder step, or the reveal of a first frame that landed.
    pub(super) fn next_step_at(&self, now: Instant) -> Option<Instant> {
        if self.revealed() {
            return None;
        }
        match self.clock.shows_at() {
            Some(at) => Some(at.max(now)),
            None => self.clock.next_step_at(now),
        }
    }
}
