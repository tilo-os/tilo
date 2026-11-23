//! One window's motion channel, sampled once per drawn frame.
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

    pub(super) fn is_settled(&self) -> bool {
        !self.motion.is_moving(self.at)
    }
}
