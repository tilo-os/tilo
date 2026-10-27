//! The strip's scroll units and the motion it settles with.
//!
//! A wheel reports fractions of a detent and a touchpad reports pixels; both
//! must become the same discrete step. Once the strip is sent somewhere it
//! eases onto a group edge on the shared snap curve. These are the two halves
//! of one conversation between input and a resting pan, kept beside the row
//! rather than inside its gesture and layout code.

use super::MOVE_SETTLED_PX;
use crate::time::{Duration, Instant};
use crate::tokens::MotionSource;
use winit::event::MouseScrollDelta;

/// Touchpad px per wheel detent for the discrete jump gestures; also the
/// scale for hi-res wheel notches reported as pixel deltas.
pub const JUMP_PX_PER_DETENT: f64 = crate::tokens::Layout::BASE.jump_px_per_detent as f64;

/// Turns fractional-detent scroll deltas into whole steps. No quiet-time
/// decay: it would stop slow scrolling from ever accumulating a whole
/// detent, and a stale sub-detent balance is harmless since it clears the
/// next time the user reverses direction.
#[derive(Default)]
pub struct WheelJumpAccum {
    accum: f64,
}

impl WheelJumpAccum {
    pub fn push(&mut self, detents: f64, _now: Instant) -> i32 {
        if detents == 0.0 {
            return 0;
        }
        if self.accum != 0.0 && self.accum.signum() != detents.signum() {
            self.accum = 0.0;
        }
        self.accum += detents;
        let whole = self.accum.trunc();
        self.accum -= whole;
        // Real scroll deltas never approach i32 range; clamping first
        // makes the narrowing cast below a lossless, saturating no-op.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "clamped into i32 range on the line above, so this cast cannot truncate"
        )]
        {
            whole.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
        }
    }
}

/// Scroll delta → fractional jump detents. Winit's y is flipped (positive
/// = up), so wheel-DOWN yields a positive detent = jump right/next,
/// matching Niri's `Mod+Shift+WheelScrollDown → focus-column-right`.
pub fn wheel_jump_detents(delta: &MouseScrollDelta) -> f64 {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => f64::from(-x - y),
        MouseScrollDelta::PixelDelta(pp) => (-pp.x - pp.y) / JUMP_PX_PER_DETENT,
    }
}

/// A pan or a group's drawn offset at rest at `at`, arriving within a
/// quarter pixel once sent somewhere.
pub(super) fn strip_motion(at: f64) -> crate::motion::MotionValue<f64> {
    crate::motion::MotionValue::new(at).with_tolerance(MOVE_SETTLED_PX)
}

/// The pan at rest at `from`, to snap to `to` on a [`crate::motion::Snap`]
/// timing: it arrives within a thousandth of its travel (a quarter pixel at
/// least), as a token spring run once does, so a snap is over within its
/// token however far it goes.
pub(super) fn snap_motion(from: f64, to: f64) -> crate::motion::MotionValue<f64> {
    crate::motion::MotionValue::new(from)
        .with_tolerance(MOVE_SETTLED_PX.max((to - from).abs() * 1e-3))
}

/// How the pan settles on a group's edge, on [`crate::motion::Snap`]: its
/// spring, and how long a run from rest takes (zero when it lands).
#[derive(Clone, Copy, Debug)]
pub(super) struct SnapTiming {
    pub(super) timing: crate::motion::Timing,
    pub(super) settles: Duration,
}

impl SnapTiming {
    pub(super) fn of(motion: crate::tokens::EffectiveMotion, source: MotionSource) -> Self {
        let (spec, enter) = (crate::motion::Snap::SPEC, crate::motion::Direction::Enter);
        let timing = spec.timing(enter, motion, source);
        let settles = spec.settle_time(enter, motion, source);
        Self { timing, settles }
    }
}
