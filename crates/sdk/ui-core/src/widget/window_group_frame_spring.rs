//! One window's motion channel, sampled once per drawn frame.
//!
//! A rect, slot width or offset is driven by the same closed-form spring, so
//! every rebuild of a frame reads the value the frame already showed instead
//! of advancing it again. A new target mid-flight keeps the value and its
//! velocity, which is what makes two shortcuts in a row one motion. Kept out
//! of `WindowGroup` because sampling and retargeting a spring is a small
//! self-contained contract that the group only carries.

use crate::time::Instant;

/// One channel of a window's motion on the strip (its rect while it
/// maximizes or restores, its slot's width while it opens, closes or is
/// resized by command), on the window move (`motion::WindowMotion::MOVE`:
/// the heavy, critically damped spring). Sampled once per drawn frame, so
/// every rebuild of one frame reads the same value. Closed form: a stalled
/// frame lands where the motion is, and a new target mid-flight keeps the
/// value and its velocity, so Super+M twice is one motion, not a queue.
#[derive(Debug)]
pub(super) struct FrameSpring<T: crate::motion::Animatable> {
    pub(super) motion: crate::motion::MotionValue<T>,
    drawn: T,
    at: Instant,
}

impl<T: crate::motion::Animatable> Drop for FrameSpring<T> {
    /// A spring let go mid-way (a shortcut landing what it changed, a
    /// close completing) is drawn no more: it asks for no more frames.
    fn drop(&mut self) {
        let target = self.motion.target();
        self.motion.snap(target);
    }
}

impl FrameSpring<f64> {
    /// A width in pixels arrives within a quarter of one.
    pub(super) fn with_tolerance_px(mut self) -> Self {
        self.motion = self.motion.with_tolerance(0.25);
        self
    }
}

impl<T: crate::motion::Animatable> FrameSpring<T> {
    pub(super) fn new(value: T) -> Self {
        Self {
            motion: crate::motion::MotionValue::new(value),
            drawn: value,
            at: crate::time::now(),
        }
    }

    /// Go to `target` from where the motion is now, at the speed it has.
    pub(super) fn set_target(&mut self, target: T) {
        let timing = crate::motion::WindowMotion::MOVE.timing(
            crate::motion::Direction::Enter,
            crate::tokens::EffectiveMotion::default(),
            crate::tokens::MotionSource::Pointer,
        );
        self.motion.set(target, timing, crate::motion::frame_now());
    }

    pub(super) fn snap(&mut self, value: T) {
        self.motion.snap(value);
        self.drawn = value;
    }

    pub(super) fn step(&mut self, now: Instant) {
        self.drawn = self.motion.get(now);
        self.at = now;
    }

    /// The value as of the last frame sampled.
    pub(super) fn value(&self) -> T {
        self.drawn
    }

    pub(super) fn target(&self) -> T {
        self.motion.target()
    }

    #[cfg(test)]
    pub(super) fn velocity(&self) -> T {
        self.motion.velocity(self.at)
    }

    /// Whether the last frame sampled is the motion's rest.
    pub(super) fn is_settled(&self) -> bool {
        !self.motion.is_moving(self.at)
    }
}
