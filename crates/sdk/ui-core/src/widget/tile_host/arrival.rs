//! A window's content whose first frame is not drawn yet: the arrival model ([`crate::arriving`])
//! at the scale of a tile.
use crate::arriving::{ArrivalClock, Progress, Step};
use crate::motion::CrossFade;
use crate::time::Instant;
use crate::tokens::EffectiveMotion;

#[derive(Clone)]
pub(super) struct TileArrival {
    clock: ArrivalClock,
    fade: CrossFade<bool>,
    replaced: Step,
    drawn: Option<(Step, Option<Progress>)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum ArrivalPaint {
    Placeholder(Step),
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

    pub(super) fn answered_from(&self, from: crate::arriving::Source) -> Self {
        Self::new(self.clock.answered_from(from))
    }

    pub(super) fn revealed(&self) -> bool {
        *self.fade.shown()
    }

    pub(super) fn arrive(&mut self, at: Instant) {
        self.clock.arrive(at);
    }

    pub(super) fn placeholder_step(&self, now: Instant) -> Step {
        match self.clock.step(now) {
            Step::Shown => self.clock.replaced_step(),
            step => step,
        }
    }

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

    pub(super) fn needs_redraw(&self, now: Instant, progress: Option<Progress>) -> bool {
        if self.revealed() {
            return self.fade.is_moving(now);
        }
        // A tile nothing has painted has nothing on screen to correct: the
        // frame that first draws it paints the step of that moment. A desk
        // keeps such tiles for every tab and strip it is not showing, and
        // asking for their sakes kept it repainting every frame for as long
        // as the session ran.
        let Some(drawn) = self.drawn else {
            return false;
        };
        drawn != (self.clock.step(now), progress)
    }

    pub(super) fn is_moving(&self, now: Instant) -> bool {
        self.fade.is_moving(now)
    }

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
