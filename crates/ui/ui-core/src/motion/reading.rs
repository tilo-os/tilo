//! [`Reading`]: a drawn figure moving to its new reading, and [`ReadingValue`], one number that does.

use super::named::ask_frames_until;
use super::spec::{Direction, MotionSpec, Pace, Reduced, Shortcut};
use super::value::MotionValue;
use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};

/// A drawn figure moving to its new reading: a ring's fill, a bar's height, a chart's line, each
/// number of a drawing's shapes gliding from where it is drawn to where the new reading puts it.
pub struct Reading;

impl Reading {
    pub const CHANGE: MotionSpec = MotionSpec {
        enter: Pace::Base,
        exit: Pace::Base,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Context,
    };
}

/// One number of a reading on [`Reading`]: what a gauge, a bar or a zoom draws from.
#[derive(Clone, Copy, Debug)]
pub struct ReadingValue(MotionValue<f64>);

impl ReadingValue {
    /// At rest at `value`.
    #[must_use]
    pub fn new(value: f64) -> Self {
        Self(MotionValue::new(value))
    }

    /// Take the reading `target` at `now`, under the motion its owner last drew with: `None` before
    /// its first frame, when it is drawn where it is, never grown into. Whoever's change it is (the
    /// person's, another member's, an agent's), it goes on from where it is drawn.
    #[track_caller]
    pub fn take(&mut self, target: f64, motion: Option<EffectiveMotion>, now: Instant) {
        if target.to_bits() == self.0.target().to_bits() {
            return;
        }
        let Some(motion) = motion else {
            return self.0.snap(target);
        };
        let source = super::recent_input(now).unwrap_or(MotionSource::Pointer);
        self.0.set(
            target,
            Reading::CHANGE.timing(Direction::Enter, motion, source),
            now,
        );
        ask_frames_until(self.0.settles_at());
    }

    /// Put it at rest at `value` at once.
    pub fn snap(&mut self, value: f64) {
        self.0.snap(value);
    }

    /// Where it is drawn at `now`.
    #[must_use]
    pub fn get(&self, now: Instant) -> f64 {
        self.0.get(now)
    }

    #[must_use]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.0.is_moving(now)
    }
}
