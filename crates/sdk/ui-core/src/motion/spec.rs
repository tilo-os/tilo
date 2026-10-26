//! How a named motion times itself: its pace in and out, what it does under
//! reduced motion, and what a shortcut gets.

use super::spring::SpringSpec;
use super::value::Timing;
use crate::tokens::{EffectiveMotion, MotionSource};

/// A pace from the motion tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pace {
    /// `durations.fast`: feedback, exits, small changes.
    Fast,
    /// `durations.base`: panels, pages, reflows.
    Base,
    /// `durations.slow`: whole-view changes.
    Slow,
    /// `springs.heavy`: a window's rect, which should feel like it has mass.
    Heavy,
    /// `gesture.fling_projection_ms`: a released gesture coasting on. Its
    /// velocity dies away exponentially at the rate the coast time gives
    /// (a critically damped spring of natural frequency `1 / coast`), so a
    /// coast aimed at where the release projects ([`super::Snap::rest_for`])
    /// starts at the fingers' speed and never turns back.
    Coast,
    /// No motion: the change shows on the next frame (something leaving
    /// that has nothing to say).
    Instant,
}

/// What a motion becomes under reduced motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reduced {
    /// It lands at once (geometry: a height, a rect, a slide).
    Lands,
    /// Its progress runs at `fast`, and its pose drops everything spatial, so
    /// only opacity changes (a popover fading in place).
    Fades,
}

/// What a motion does when a shortcut or an action asked for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    /// The result shows on the next frame.
    Lands,
    /// Spatial context: it may still move, at `fast` at most.
    Context,
}

/// The timing of one named motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MotionSpec {
    /// Toward the shown / opened / new state.
    pub enter: Pace,
    /// Back toward the hidden / closed / old state. Exits are shorter.
    pub exit: Pace,
    pub reduced: Reduced,
    pub shortcut: Shortcut,
}

/// Which way a motion is going.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Enter,
    Exit,
}

impl Direction {
    /// `Enter` when `opening`.
    #[must_use]
    pub fn of(opening: bool) -> Self {
        if opening { Self::Enter } else { Self::Exit }
    }
}

impl MotionSpec {
    /// How this motion moves in `direction`, for the motion the person allows
    /// and what started it.
    #[must_use]
    pub fn timing(
        self,
        direction: Direction,
        motion: EffectiveMotion,
        source: MotionSource,
    ) -> Timing {
        self.timing_over(direction, motion, source, 1.0)
    }

    /// [`Self::timing`] for a travel `stretch` times the motion's reference
    /// distance: longer travel takes longer, by the square root of the ratio,
    /// never past `slow`. Material's rule: a short hop and a cross-screen move
    /// at one speed read as one too slow or the other too fast.
    #[must_use]
    pub fn timing_over(
        self,
        direction: Direction,
        motion: EffectiveMotion,
        source: MotionSource,
        stretch: f64,
    ) -> Timing {
        let pace = match direction {
            Direction::Enter => self.enter,
            Direction::Exit => self.exit,
        };
        let durations = motion.tokens().durations;
        if pace == Pace::Instant {
            return Timing::Instant;
        }
        if source.lands_at_once() {
            return match self.shortcut {
                Shortcut::Lands => Timing::Instant,
                Shortcut::Context if motion.reduced() => Timing::Instant,
                Shortcut::Context => Timing::Spring(SpringSpec::settling_within(
                    durations.fast().min(pace_duration(pace, motion)),
                )),
            };
        }
        if motion.reduced() {
            return match self.reduced {
                Reduced::Lands => Timing::Instant,
                Reduced::Fades => Timing::Spring(SpringSpec::settling_within(durations.fast())),
            };
        }
        let spring = match pace {
            Pace::Heavy => SpringSpec::from_params(motion.tokens().springs.heavy),
            Pace::Coast => SpringSpec::critical(1.0 / pace_duration(pace, motion).as_secs_f64()),
            _ => SpringSpec::settling_within(pace_duration(pace, motion)),
        };
        let base = pace_duration(pace, motion).as_secs_f64().max(1e-3);
        let cap = (durations.slow().as_secs_f64() / base).max(1.0);
        let factor = stretch.max(1.0).sqrt().min(cap);
        Timing::Spring(spring.slowed(factor))
    }
}

impl MotionSpec {
    /// The progress (`0..=1`) of this motion run once from rest, `elapsed`
    /// after it started: for a motion fixed to a known instant that keeps no
    /// state of its own (a banner arriving from when it first showed).
    #[must_use]
    pub fn progress_after(
        self,
        direction: Direction,
        motion: EffectiveMotion,
        source: MotionSource,
        elapsed: crate::time::Duration,
    ) -> f64 {
        // The settled clock (captures and tests) reads every motion at its
        // end.
        if super::clock::testing::is_settled() {
            return 1.0;
        }
        match self.timing(direction, motion, source) {
            Timing::Instant => 1.0,
            Timing::Spring(spring) => {
                if elapsed >= settle_of(spring) {
                    1.0
                } else {
                    (1.0 - spring.state(1.0, 0.0, elapsed.as_secs_f64()).0).clamp(0.0, 1.0)
                }
            }
        }
    }

    /// This motion's entrance curve at `fraction` (`0..=1`) of its run, for a
    /// driver that keeps its own linear clock (the overview's, which credits
    /// time a full presenter held it): the same shape a run from rest has.
    #[must_use]
    pub fn eased(self, motion: EffectiveMotion, fraction: f64) -> f64 {
        let source = MotionSource::Pointer;
        let run = self.settle_time(Direction::Enter, motion, source);
        self.progress_after(
            Direction::Enter,
            motion,
            source,
            run.mul_f64(fraction.clamp(0.0, 1.0)),
        )
    }

    /// How long this motion run once from rest takes to settle; zero when it
    /// lands at once.
    #[must_use]
    pub fn settle_time(
        self,
        direction: Direction,
        motion: EffectiveMotion,
        source: MotionSource,
    ) -> crate::time::Duration {
        match self.timing(direction, motion, source) {
            Timing::Instant => crate::time::Duration::ZERO,
            Timing::Spring(spring) => settle_of(spring),
        }
    }
}

/// A motion run once settles on a fraction's tolerance.
const ONCE_TOLERANCE: f64 = 1e-3;

/// When a run from rest on `spring` settles, to the millisecond: a token
/// spring then settles after exactly its token (`fast` is 130 ms), so a
/// lifecycle timed by the token and its motion agree.
fn settle_of(spring: SpringSpec) -> crate::time::Duration {
    let ms = (spring.settle_secs(1.0, 0.0, ONCE_TOLERANCE) * 1000.0).round();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a settle time is non-negative and bounded by the solver's ten seconds"
    )]
    let ms = ms.max(0.0) as u64;
    crate::time::Duration::from_millis(ms)
}

fn pace_duration(pace: Pace, motion: EffectiveMotion) -> crate::time::Duration {
    let d = motion.tokens().durations;
    match pace {
        Pace::Fast => d.fast(),
        Pace::Base | Pace::Heavy => d.base(),
        Pace::Instant => crate::time::Duration::ZERO,
        Pace::Slow => d.slow(),
        Pace::Coast => crate::time::Duration::from_millis(u64::from(
            motion.tokens().gesture.fling_projection_ms.max(1),
        )),
    }
}
