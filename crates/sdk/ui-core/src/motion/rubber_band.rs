//! A scroll view's give past its ends (`motion::RubberBand`): while the hand
//! holds the content it follows one to one inside, and past an end through
//! the shared band ([`crate::tokens::rubber_band`]), stiffening the further
//! it goes; let go, the give springs back to the end, carrying the hand's
//! speed. Under reduced motion there is no give: the content stops at its
//! ends.

use super::spec::{Direction, MotionSpec, Pace, Reduced, Shortcut};
use super::value::MotionValue;
use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};
use vello::kurbo::Vec2;

/// The give of a scroll view past its ends, per axis: negative before the
/// start, positive past the end, in logical pixels.
#[derive(Clone, Debug)]
pub struct RubberBand {
    /// How far past an end the hand has asked the content to go while it
    /// holds it; the give is this through the band.
    pulled: Vec2,
    /// The give as drawn: the hand's, at rest, while it holds the content;
    /// springing back to none once it let go.
    give: MotionValue<Vec2>,
    /// The give last handed out to be drawn ([`Self::give`]).
    given: std::cell::Cell<Vec2>,
}

impl Default for RubberBand {
    fn default() -> Self {
        Self {
            pulled: Vec2::ZERO,
            give: MotionValue::new(Vec2::ZERO).with_tolerance(0.25),
            given: std::cell::Cell::new(Vec2::ZERO),
        }
    }
}

impl RubberBand {
    /// The spring back: `fast`; reduced motion has no give to spring from,
    /// and nothing a shortcut does stretches it.
    pub const SPEC: MotionSpec = MotionSpec {
        enter: Pace::Fast,
        exit: Pace::Fast,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Lands,
    };

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The hand asked the content `excess` past its ends this input (what
    /// the content could not take, per axis; zero where it took it all), in
    /// a view `viewport` large: the give follows through the band. A give
    /// springing back is caught where it is drawn.
    pub fn pull(&mut self, excess: Vec2, viewport: Vec2, motion: EffectiveMotion, now: Instant) {
        if motion.reduced() {
            self.stop();
            return;
        }
        let limit = Self::limits(viewport, motion);
        if self.give.is_moving(now) {
            let drawn = self.give.get(now);
            self.pulled = Vec2::new(unstretch(drawn.x, limit.x), unstretch(drawn.y, limit.y));
        }
        self.pulled += excess;
        self.give.snap(Vec2::new(
            stretch(self.pulled.x, limit.x),
            stretch(self.pulled.y, limit.y),
        ));
    }

    /// The hand moved the content by `delta` while it may hold a give: the
    /// part of it that goes back toward the ends takes the give in first,
    /// through the band. Returns what is left to move the content by.
    pub fn take_in(
        &mut self,
        delta: Vec2,
        viewport: Vec2,
        motion: EffectiveMotion,
        now: Instant,
    ) -> Vec2 {
        if self.give.target() == Vec2::ZERO && !self.give.is_moving(now) {
            return delta;
        }
        let limit = Self::limits(viewport, motion);
        if self.give.is_moving(now) {
            let drawn = self.give.get(now);
            self.pulled = Vec2::new(unstretch(drawn.x, limit.x), unstretch(drawn.y, limit.y));
        }
        let mut left = delta;
        for (pulled, moved) in [
            (&mut self.pulled.x, &mut left.x),
            (&mut self.pulled.y, &mut left.y),
        ] {
            // Back toward the content: the opposite way to the pull.
            if *pulled * *moved < 0.0 {
                let taken = if moved.abs() >= pulled.abs() {
                    -*pulled
                } else {
                    *moved
                };
                *pulled += taken;
                *moved -= taken;
            }
        }
        self.give.snap(Vec2::new(
            stretch(self.pulled.x, limit.x),
            stretch(self.pulled.y, limit.y),
        ));
        left
    }

    /// The hand let go moving the content at `velocity` (logical pixels per
    /// second): the give springs back to the ends, starting at the speed the
    /// band gave it there.
    pub fn release(
        &mut self,
        velocity: Vec2,
        viewport: Vec2,
        motion: EffectiveMotion,
        now: Instant,
    ) {
        if self.give.target() == Vec2::ZERO {
            self.pulled = Vec2::ZERO;
            return;
        }
        let limit = Self::limits(viewport, motion);
        // The band's slope where the hand let go: how far the give moved
        // for each pixel the hand did.
        let slope = |pulled: f64, limit: f64| {
            let reach = pulled.abs() + limit;
            if reach > 0.0 {
                (limit / reach).powi(2)
            } else {
                0.0
            }
        };
        let carried = Vec2::new(
            velocity.x * slope(self.pulled.x, limit.x),
            velocity.y * slope(self.pulled.y, limit.y),
        );
        let timing = Self::SPEC.timing(Direction::Exit, motion, MotionSource::Gesture);
        self.pulled = Vec2::ZERO;
        self.give.release(Vec2::ZERO, carried, timing, now);
    }

    /// No give, at once.
    pub fn stop(&mut self) {
        self.pulled = Vec2::ZERO;
        self.give.snap(Vec2::ZERO);
    }

    /// The give drawn at `now`, in whole device pixels of `device_scale`
    /// per logical pixel, so a host that copies what a scroll keeps copies
    /// it.
    #[must_use]
    pub fn give(&self, now: Instant, device_scale: f64) -> Vec2 {
        let give = self.give_at(now, device_scale);
        self.given.set(give);
        give
    }

    /// Whether the content wants a frame for its give: it springs back, or
    /// was last drawn with another give than it has now.
    #[must_use]
    pub fn needs_frame(&self, now: Instant, device_scale: f64) -> bool {
        self.give.is_moving(now) || self.give_at(now, device_scale) != self.given.get()
    }

    fn give_at(&self, now: Instant, device_scale: f64) -> Vec2 {
        let give = self.give.get(now);
        if device_scale <= 0.0 {
            return give;
        }
        Vec2::new(
            (give.x * device_scale).round() / device_scale,
            (give.y * device_scale).round() / device_scale,
        )
    }

    /// Whether the content stands past an end, held or springing back.
    #[must_use]
    pub fn is_stretched(&self, now: Instant) -> bool {
        self.give.target() != Vec2::ZERO || self.give.is_moving(now)
    }

    /// Whether the give is springing back at `now`.
    #[must_use]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.give.is_moving(now)
    }

    /// When the spring back arrives; `None` at rest.
    #[must_use]
    pub fn settles_at(&self) -> Option<Instant> {
        self.give.settles_at()
    }

    /// The furthest the content gives on each axis, never reached: a
    /// little over one viewport.
    fn limits(viewport: Vec2, motion: EffectiveMotion) -> Vec2 {
        viewport * f64::from(motion.tokens().gesture.overscroll_reach_frac)
    }
}

/// How far the content gives for a pull of `pulled` against `limit`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the shared band takes f32 screen distances; a pull is a few viewports at most"
)]
fn stretch(pulled: f64, limit: f64) -> f64 {
    let give = f64::from(crate::tokens::rubber_band(
        pulled.abs() as f32,
        limit as f32,
    ));
    give.copysign(pulled)
}

/// The pull that gives `give` against `limit`: the band's inverse.
fn unstretch(give: f64, limit: f64) -> f64 {
    let held = give.abs().min(limit * 0.999);
    if held <= 0.0 {
        return 0.0;
    }
    (limit * held / (limit - held)).copysign(give)
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "a give at rest is exactly zero, and whole device pixels compare exactly"
)]
mod tests {
    use super::*;
    use crate::time::Duration;
    use crate::time::testing::with_monotonic_time;
    use crate::tokens::Motion;

    const VIEW: Vec2 = Vec2::new(0.0, 400.0);

    fn motion(reduced: bool) -> EffectiveMotion {
        EffectiveMotion::new(Motion::default(), reduced)
    }

    #[test]
    fn a_pull_past_an_end_gives_less_and_less_and_springs_back_when_let_go() {
        let start = crate::time::now();
        with_monotonic_time(start, || {
            let mut band = RubberBand::new();
            band.pull(Vec2::new(0.0, -40.0), VIEW, motion(false), start);
            let first = band.give(start, 1.0).y;
            assert!(
                first < 0.0 && first > -40.0,
                "the start gives, less than the hand: {first}"
            );
            band.pull(Vec2::new(0.0, -40.0), VIEW, motion(false), start);
            let second = band.give(start, 1.0).y - first;
            assert!(
                second < 0.0 && second > first,
                "it stiffens: {first} then {second}"
            );
            band.release(Vec2::ZERO, VIEW, motion(false), start);
            assert!(band.is_moving(start));
            let later = start + Duration::from_secs(1);
            assert!(!band.is_moving(later));
            assert_eq!(band.give(later, 1.0), Vec2::ZERO);
        });
    }

    #[test]
    fn moving_back_takes_the_give_in_before_the_content_moves() {
        let start = crate::time::now();
        with_monotonic_time(start, || {
            let mut band = RubberBand::new();
            band.pull(Vec2::new(0.0, 30.0), VIEW, motion(false), start);
            let left = band.take_in(Vec2::new(0.0, -20.0), VIEW, motion(false), start);
            assert_eq!(left, Vec2::ZERO, "all of it went into the give");
            assert!(band.give(start, 1.0).y > 0.0);
            let left = band.take_in(Vec2::new(0.0, -20.0), VIEW, motion(false), start);
            assert_eq!(left, Vec2::new(0.0, -10.0), "the rest moves the content");
            assert_eq!(band.give(start, 1.0), Vec2::ZERO);
        });
    }

    #[test]
    fn under_reduced_motion_the_content_stops_at_its_ends() {
        let start = crate::time::now();
        with_monotonic_time(start, || {
            let mut band = RubberBand::new();
            band.pull(Vec2::new(0.0, -80.0), VIEW, motion(true), start);
            assert_eq!(band.give(start, 1.0), Vec2::ZERO);
            assert!(!band.is_stretched(start));
        });
    }

    #[test]
    fn the_give_steps_in_whole_device_pixels() {
        let start = crate::time::now();
        with_monotonic_time(start, || {
            let mut band = RubberBand::new();
            band.pull(Vec2::new(0.0, 13.3), VIEW, motion(false), start);
            let give = band.give(start, 2.0).y * 2.0;
            assert_eq!(give, give.round());
        });
    }
}
