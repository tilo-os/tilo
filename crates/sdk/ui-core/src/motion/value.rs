//! [`MotionValue`]: a value that follows its target on a spring, and can be
//! sent somewhere else at any moment without a jump.

use super::spring::SpringSpec;
use crate::time::{Duration, Instant};
use vello::kurbo::{Point, Rect, Size, Vec2};
use vello::peniko::Color;

/// Up to four channels moving together on one spring.
pub type Channels = [f64; 4];

/// A value a motion can carry: a point, a rect, a colour, an amount.
pub trait Animatable: Copy {
    /// How close each channel must be to its target to count as arrived.
    const TOLERANCE: f64;
    fn to_channels(self) -> Channels;
    fn from_channels(channels: Channels) -> Self;
}

impl Animatable for f64 {
    /// Fractions (an openness, an opacity) and other unit amounts.
    const TOLERANCE: f64 = 1e-3;
    fn to_channels(self) -> Channels {
        [self, 0.0, 0.0, 0.0]
    }
    fn from_channels(c: Channels) -> Self {
        c[0]
    }
}

impl Animatable for f32 {
    const TOLERANCE: f64 = 1e-3;
    fn to_channels(self) -> Channels {
        [f64::from(self), 0.0, 0.0, 0.0]
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "an f32 motion value went in as f32; narrowing its own channel back loses nothing it had"
    )]
    fn from_channels(c: Channels) -> Self {
        c[0] as f32
    }
}

/// Positions and extents in logical pixels arrive within a tenth of one.
const PIXEL_TOLERANCE: f64 = 0.1;

impl Animatable for Point {
    const TOLERANCE: f64 = PIXEL_TOLERANCE;
    fn to_channels(self) -> Channels {
        [self.x, self.y, 0.0, 0.0]
    }
    fn from_channels(c: Channels) -> Self {
        Point::new(c[0], c[1])
    }
}

impl Animatable for Vec2 {
    const TOLERANCE: f64 = PIXEL_TOLERANCE;
    fn to_channels(self) -> Channels {
        [self.x, self.y, 0.0, 0.0]
    }
    fn from_channels(c: Channels) -> Self {
        Vec2::new(c[0], c[1])
    }
}

impl Animatable for Size {
    const TOLERANCE: f64 = PIXEL_TOLERANCE;
    fn to_channels(self) -> Channels {
        [self.width, self.height, 0.0, 0.0]
    }
    fn from_channels(c: Channels) -> Self {
        Size::new(c[0], c[1])
    }
}

impl Animatable for Rect {
    const TOLERANCE: f64 = PIXEL_TOLERANCE;
    fn to_channels(self) -> Channels {
        [self.x0, self.y0, self.x1, self.y1]
    }
    fn from_channels(c: Channels) -> Self {
        Rect::new(c[0], c[1], c[2], c[3])
    }
}

impl Animatable for Color {
    /// Half a step of an 8-bit channel.
    const TOLERANCE: f64 = 0.5 / 255.0;
    fn to_channels(self) -> Channels {
        let [r, g, b, a] = self.components;
        [f64::from(r), f64::from(g), f64::from(b), f64::from(a)]
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "colour channels are stored as f32; the motion ran in f64 on the same values"
    )]
    fn from_channels(c: Channels) -> Self {
        Color::new([
            c[0].clamp(0.0, 1.0) as f32,
            c[1].clamp(0.0, 1.0) as f32,
            c[2].clamp(0.0, 1.0) as f32,
            c[3].clamp(0.0, 1.0) as f32,
        ])
    }
}

/// How a value goes to a new target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Timing {
    /// It is there on the next frame.
    Instant,
    /// It moves on this spring.
    Spring(SpringSpec),
}

impl Timing {
    /// Whether the change lands at once.
    #[must_use]
    pub fn is_instant(self) -> bool {
        matches!(self, Self::Instant)
    }
}

/// A value following its target on a spring, in closed form.
///
/// - [`Self::set`] starts a new segment from the value *and velocity* the
///   current one has at `now`, so a reversal or a new target mid-flight never
///   jumps and never stalls; repeated changes are one motion, never a queue.
/// - [`Self::get`] is exact at any `now`: the curve does not depend on how
///   often, or whether, frames were drawn.
/// - Once within tolerance the value *is* the target, and [`Self::is_moving`]
///   is false: a settled value asks for no frames.
#[derive(Clone, Copy, Debug)]
pub struct MotionValue<T: Animatable> {
    target: T,
    /// Displacement from the target and velocity when the segment started.
    from: Channels,
    velocity: Channels,
    started: Instant,
    spring: SpringSpec,
    /// When the segment is within tolerance for good; `None` at rest.
    settles_at: Option<Instant>,
    tolerance: f64,
    /// This motion's wish for frames on the motion clock, while it has one:
    /// a landed motion takes it back ([`Self::snap`]).
    wish: Option<u64>,
}

impl<T: Animatable> MotionValue<T> {
    /// A value at rest.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self {
            target: value,
            from: [0.0; 4],
            velocity: [0.0; 4],
            started: crate::time::now(),
            spring: SpringSpec::settling_within(Duration::from_millis(210)),
            settles_at: None,
            tolerance: T::TOLERANCE,
            wish: None,
        }
    }

    /// The same value, arriving within `tolerance` per channel instead of the
    /// type's default (a scalar carrying pixels rather than a fraction).
    #[must_use]
    pub fn with_tolerance(mut self, tolerance: f64) -> Self {
        self.tolerance = tolerance.max(1e-9);
        self
    }

    /// Where the value is going (or is, at rest).
    pub fn target(&self) -> T {
        self.target
    }

    #[track_caller]
    pub fn get(&self, now: Instant) -> T {
        T::from_channels(self.channels_at(now).0)
    }

    /// The velocity at `now`, per second, in the value's own units.
    #[track_caller]
    pub fn velocity(&self, now: Instant) -> T {
        T::from_channels(self.channels_at(now).1)
    }

    /// Whether the value is still on its way at `now`.
    #[track_caller]
    pub fn is_moving(&self, now: Instant) -> bool {
        // The settled clock (captures and tests) reads every motion at its
        // end.
        let moving =
            !super::clock::testing::is_settled() && self.settles_at.is_some_and(|at| now < at);
        if moving {
            super::clock::debug_assert_frame_instant(now);
        }
        moving
    }

    /// When the current motion arrives; `None` at rest. A host that sleeps
    /// until then instead of drawing frames has nothing to draw anyway.
    pub fn settles_at(&self) -> Option<Instant> {
        self.settles_at
    }

    /// Hold the motion for `by`: time spent waiting (on a full presenter)
    /// is not progress, so the curve resumes where it stood.
    #[track_caller]
    pub fn defer(&mut self, by: Duration) {
        if self.settles_at.is_none() {
            return;
        }
        self.started = self.started.checked_add(by).unwrap_or(self.started);
        self.settles_at = self.settles_at.and_then(|at| at.checked_add(by));
        if let Some(until) = self.settles_at {
            self.ask_frames_until(until);
        }
    }

    /// Jump to `value` and rest there. A motion that was on its way takes
    /// back its wish for frames: whoever landed it draws the change.
    pub fn snap(&mut self, value: T) {
        self.target = value;
        self.from = [0.0; 4];
        self.velocity = [0.0; 4];
        self.settles_at = None;
        if let Some(key) = self.wish.take() {
            super::clock::retract(key);
        }
    }

    /// Ask the motion clock for frames until `until`, as this motion: a
    /// later ask replaces it, and landing takes it back.
    #[track_caller]
    fn ask_frames_until(&mut self, until: Instant) {
        let key = *self.wish.get_or_insert_with(super::clock::motion_key);
        super::clock::ask(key, until);
    }

    /// Send the value to `target` with `timing`, starting at `now` from where
    /// it is drawn and how fast it is moving there. Setting the target it is
    /// already going to changes nothing (a repeated request never restarts
    /// the curve).
    #[track_caller]
    pub fn set(&mut self, target: T, timing: Timing, now: Instant) {
        let spring = match timing {
            Timing::Instant => {
                self.snap(target);
                return;
            }
            Timing::Spring(spring) => spring,
        };
        let same_target = channels_eq(self.target.to_channels(), target.to_channels());
        if same_target && (spring == self.spring || !self.is_moving(now)) {
            return;
        }
        let (position, velocity) = self.channels_at(now);
        self.start(target, position, velocity, spring, now);
    }

    /// Send the value to `target` carrying `velocity` (a gesture's release,
    /// a kick), whatever it was doing.
    #[track_caller]
    pub fn release(&mut self, target: T, velocity: T, timing: Timing, now: Instant) {
        let Timing::Spring(spring) = timing else {
            self.snap(target);
            return;
        };
        let (position, _) = self.channels_at(now);
        self.start(target, position, velocity.to_channels(), spring, now);
    }

    #[track_caller]
    fn start(
        &mut self,
        target: T,
        position: Channels,
        velocity: Channels,
        spring: SpringSpec,
        now: Instant,
    ) {
        let goal = target.to_channels();
        let mut from = [0.0; 4];
        let mut settle: f64 = 0.0;
        for ((d, (p, g)), v) in from
            .iter_mut()
            .zip(position.iter().zip(goal.iter()))
            .zip(velocity.iter())
        {
            *d = p - g;
            settle = settle.max(spring.settle_secs(*d, *v, self.tolerance));
        }
        self.target = target;
        self.from = from;
        self.velocity = velocity;
        self.started = now;
        self.spring = spring;
        self.settles_at = if settle <= 0.0 {
            None
        } else {
            now.checked_add(Duration::from_secs_f64(settle))
        };
        match self.settles_at {
            // Every motion asks the frame pump for its frames, wherever it
            // was started and whoever draws it: a settled one asks nothing.
            Some(until) => self.ask_frames_until(until),
            None => self.snap(target),
        }
    }

    #[track_caller]
    fn channels_at(&self, now: Instant) -> (Channels, Channels) {
        let goal = self.target.to_channels();
        if !self.is_moving(now) {
            return (goal, [0.0; 4]);
        }
        let t = now.saturating_duration_since(self.started).as_secs_f64();
        let mut position = goal;
        let mut velocity = [0.0; 4];
        for ((p, v), (x0, v0)) in position
            .iter_mut()
            .zip(velocity.iter_mut())
            .zip(self.from.iter().zip(self.velocity.iter()))
        {
            let (x, dx) = self.spring.state(*x0, *v0, t);
            *p += x;
            *v = dx;
        }
        (position, velocity)
    }
}

impl<T: Animatable + Default> Default for MotionValue<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

fn channels_eq(a: Channels, b: Channels) -> bool {
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| (x - y).abs() <= f64::EPSILON)
}
