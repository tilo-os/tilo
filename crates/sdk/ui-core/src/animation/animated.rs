//! Unified animation primitives. Replaces ad-hoc Instant-based fields
//! with a single `Animated<T: Lerp>` driver consumable by widgets.

use crate::color_compat::ColorCompat;
use crate::time::{Duration, Instant};

pub trait Lerp {
    #[must_use]
    fn lerp(self, other: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(self, b: f32, t: f32) -> f32 {
        self + (b - self) * t
    }
}

impl Lerp for f64 {
    fn lerp(self, b: f64, t: f32) -> f64 {
        self + (b - self) * f64::from(t)
    }
}

impl Lerp for vello::peniko::Color {
    fn lerp(self, other: Self, t: f32) -> Self {
        let lerp_u8 = |a: u8, b: u8, t: f32| -> u8 {
            let a = f32::from(a);
            let b = f32::from(b);
            let v = (a + (b - a) * t).round().clamp(0.0, 255.0);
            // Clamped to `0.0..=255.0` just above, so this always fits `u8` exactly.
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "v is clamped to 0.0..=255.0 just above, so it always fits u8 exactly"
            )]
            let result = v as u8;
            result
        };
        vello::peniko::Color::from_rgba8(
            lerp_u8(self.r(), other.r(), t),
            lerp_u8(self.g(), other.g(), t),
            lerp_u8(self.b(), other.b(), t),
            lerp_u8(self.a(), other.a(), t),
        )
    }
}

/// Animation easing / dynamics curve. Applied to a normalized `0..1`
/// duration ratio to produce an interpolation factor (which may itself
/// over/undershoot for `Spring`).
#[derive(Clone, Copy, Debug)]
pub enum Curve {
    Linear,
    /// `1 - (1 - t)^3`.
    EaseOut,
    /// Smoothstep: `t * t * (3 - 2t)`.
    EaseInOut,
    /// Damped-oscillator unit-step response. Approaches `1.0` as `t -> 1`;
    /// under-damped configurations briefly overshoot `1.0`.
    Spring {
        stiffness: f32,
        damping: f32,
        mass: f32,
    },
}

impl Curve {
    pub fn apply(&self, t: f32) -> f32 {
        match *self {
            Curve::Linear => t.clamp(0.0, 1.0),
            Curve::EaseOut => {
                let t = t.clamp(0.0, 1.0);
                1.0 - (1.0 - t).powi(3)
            }
            Curve::EaseInOut => {
                let t = t.clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            }
            Curve::Spring {
                stiffness,
                damping,
                mass,
            } => spring_unit_step(stiffness, damping, mass, t.max(0.0)),
        }
    }
}

/// Solves `m * x'' + c * x' + k * x = k` with `x(0) = 0`, `x'(0) = 0`, so
/// the position approaches `1.0` as `t -> infinity`. `t` is the `0..1`
/// duration ratio.
#[allow(
    clippy::many_single_char_names,
    reason = "m/c/k/t/s mirror the differential equation spelled out in the doc \
              comment above; naming them mass/damping/stiffness/time/aux would \
              just make the formula below harder to check against a textbook"
)]
fn spring_unit_step(stiffness: f32, damping: f32, mass: f32, t: f32) -> f32 {
    let m = mass.max(1e-4);
    let k = stiffness.max(0.0);
    let c = damping.max(0.0);

    if k == 0.0 {
        // No spring — never returns. Treat as immediate.
        return 1.0;
    }

    let omega_n = (k / m).sqrt();
    let zeta = if omega_n > 0.0 {
        c / (2.0 * m * omega_n)
    } else {
        0.0
    };

    if zeta < 1.0 {
        // Under-damped.
        let omega_d = omega_n * (1.0 - zeta * zeta).sqrt();
        let exp = (-zeta * omega_n * t).exp();
        let cos = (omega_d * t).cos();
        let sin = (omega_d * t).sin();
        1.0 - exp * (cos + (zeta * omega_n / omega_d) * sin)
    } else if (zeta - 1.0).abs() < 1e-4 {
        // Critically damped.
        let exp = (-omega_n * t).exp();
        1.0 - exp * (1.0 + omega_n * t)
    } else {
        // Over-damped.
        let s = (zeta * zeta - 1.0).sqrt();
        let r1 = -omega_n * (zeta - s);
        let r2 = -omega_n * (zeta + s);
        let denom = r1 - r2;
        if denom.abs() < 1e-6 {
            let exp = (-omega_n * t).exp();
            1.0 - exp * (1.0 + omega_n * t)
        } else {
            1.0 - (r1 * (r2 * t).exp() - r2 * (r1 * t).exp()) / denom
        }
    }
}

/// Delegates to [`Curve::EaseOut`] so there is exactly one home for the
/// formula — retune it there and every fade/slide/spawn/snap follows.
pub fn ease_out(t: f64) -> f64 {
    // `Curve` does all its arithmetic at f32 precision; a normalized 0..1
    // duration ratio loses nothing meaningful narrowing to that precision.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "Curve operates at f32 precision throughout; a 0..1 ratio has nothing to lose narrowing to it"
    )]
    let t32 = t as f32;
    f64::from(Curve::EaseOut.apply(t32))
}

/// A driver that interpolates `from -> to` over `duration` using `curve`,
/// driven entirely off a wall-clock `Instant`. No per-frame state — just
/// query `current(now)` whenever you need the value.
#[derive(Clone, Copy, Debug)]
pub struct Animated<T: Lerp + Copy> {
    from: T,
    to: T,
    started: Instant,
    duration: Duration,
    curve: Curve,
}

impl<T: Lerp + Copy> Animated<T> {
    pub fn new(from: T, to: T, duration: Duration, curve: Curve) -> Self {
        Self {
            from,
            to,
            started: crate::time::now(),
            duration,
            curve,
        }
    }

    /// Like `new`, but with an explicit start instant — useful for tests
    /// and for scheduling animations to begin in the future.
    pub fn starting_at(from: T, to: T, started: Instant, duration: Duration, curve: Curve) -> Self {
        Self {
            from,
            to,
            started,
            duration,
            curve,
        }
    }

    /// Clamps before/after the active window to `from`/`to` respectively.
    pub fn current(&self, now: Instant) -> T {
        let raw = self.raw_t(now);
        if raw <= 0.0 {
            return self.from;
        }
        if raw >= 1.0 {
            return self.to;
        }
        let eased = self.curve.apply(raw);
        self.from.lerp(self.to, eased)
    }

    pub fn done(&self, now: Instant) -> bool {
        self.raw_t(now) >= 1.0
    }

    /// Unclamped `0..1` duration ratio — negative if `now` is before
    /// `started`, `>1.0` after the animation has ended.
    pub fn raw_t(&self, now: Instant) -> f32 {
        let dur = self.duration.as_secs_f32();
        if dur <= 0.0 {
            return if now >= self.started { 1.0 } else { 0.0 };
        }
        if now >= self.started {
            now.duration_since(self.started).as_secs_f32() / dur
        } else {
            -(self.started.duration_since(now).as_secs_f32() / dur)
        }
    }

    pub fn from(&self) -> T {
        self.from
    }
    pub fn to(&self) -> T {
        self.to
    }
    pub fn duration(&self) -> Duration {
        self.duration
    }
    pub fn curve(&self) -> Curve {
        self.curve
    }
    pub fn started(&self) -> Instant {
        self.started
    }

    /// Move the start later by `by` so time already elapsed does not count.
    /// A presenter that cannot take the frame holds the curve this way.
    fn defer(&mut self, by: Duration) {
        self.started = self.started.checked_add(by).unwrap_or(self.started);
    }
}

/// An interruptible open/close transition tracking a `0.0..=1.0` "openness"
/// so a surface animates in **and out**, and a mid-flight reversal
/// retargets from the current value instead of snapping to 0 first.
#[derive(Clone, Copy, Debug)]
pub struct Transition {
    anim: Animated<f32>,
    open: bool,
}

impl Transition {
    pub fn closed() -> Self {
        Self::at(0.0, false)
    }

    /// For surfaces that boot already shown.
    pub fn open() -> Self {
        Self::at(1.0, true)
    }

    /// Builds a settled, visibly-open transition at a fixed fractional
    /// openness for deterministic visual capture.
    ///
    /// Normal UI code must continue to use [`Self::set_open`]. This only
    /// exists for capture diagnostics that need to replay an in-flight pose
    /// without consulting the wall clock.
    #[doc(hidden)]
    pub fn capture_open_at(permille: u16) -> Self {
        Self::at(f32::from(permille.min(1_000)) / 1_000.0, true)
    }

    fn at(value: f32, open: bool) -> Self {
        let now = crate::time::now();
        Self {
            anim: Animated::starting_at(value, value, now, Duration::ZERO, Curve::Linear),
            open,
        }
    }

    /// Openness in `0.0..=1.0` at `now`.
    pub fn value(&self, now: Instant) -> f32 {
        self.anim.current(now).clamp(0.0, 1.0)
    }

    /// `(alpha, dy, scale)` for a modal this transition drives, covering
    /// both the enter (openness 0→1) and the exit (openness 1→0). The panel
    /// starts 20 px low at `start_scale` and rises to rest on the enter; the
    /// exit plays the same curve in reverse instead of the panel vanishing.
    pub fn modal_curve(&self, now: Instant, start_scale: f64) -> (f32, f64, f64) {
        let o = self.value(now);
        let scale = start_scale + (1.0 - start_scale) * f64::from(o);
        (o, 20.0 * f64::from(1.0 - o), scale)
    }

    /// True while opening, open, or closing (openness `> 0`) — draw code
    /// gates on this, not the raw intent bool, so the exit curve paints
    /// instead of the panel vanishing.
    pub fn is_visible(&self, now: Instant) -> bool {
        self.open || self.value(now) > f32::EPSILON
    }

    /// The logical open/closed intent (the last `set_open`), independent
    /// of whether the animation has settled.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// True while the value is still moving — feeds `needs_animation_frame`.
    pub fn is_animating(&self, now: Instant) -> bool {
        !self.anim.done(now)
    }

    /// No-op when the intent is unchanged AND the animation has settled, so
    /// repeated calls each frame don't restart the curve.
    pub fn set_open(&mut self, open: bool, duration: Duration, curve: Curve) {
        self.set_open_at(open, duration, curve, crate::time::now());
    }

    /// [`Self::set_open`] against a caller-supplied clock.
    ///
    /// Every other reader here — [`Self::value`], [`Self::is_visible`],
    /// [`Self::is_animating`] — already takes `now`; `set_open` was the one
    /// that reached for the clock itself. That asymmetry is untestable: a
    /// test could start a transition and ask whether it was still running,
    /// but the two calls read the clock independently, so a scheduler stall
    /// longer than the animation between them let it finish and the
    /// assertion flip. Pinning both ends to one instant makes such a test a
    /// statement about the animation rather than about the machine's load.
    pub fn set_open_at(&mut self, open: bool, duration: Duration, curve: Curve, now: Instant) {
        if open == self.open && self.anim.done(now) {
            return;
        }
        let current = self.value(now);
        let target = if open { 1.0 } else { 0.0 };
        if (current - target).abs() < f32::EPSILON {
            self.open = open;
            return;
        }
        // Scale duration by remaining distance so a reversal at 80%
        // openness closes in ~80% of the exit time — velocity stays
        // roughly continuous across the flip.
        let remaining = (target - current).abs();
        let scaled = duration.mul_f32(remaining.clamp(0.05, 1.0));
        self.anim = Animated::starting_at(current, target, now, scaled, curve);
        self.open = open;
    }

    /// Hold the curve for `by`. Wall time spent waiting on a full presenter
    /// is not progress.
    pub fn defer(&mut self, by: Duration) {
        self.anim.defer(by);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_curves_have_their_shape_and_clamp() {
        // Case `linear_curve_is_identity`.
        {
            assert_eq!(Curve::Linear.apply(0.0), 0.0);
            assert_eq!(Curve::Linear.apply(0.5), 0.5);
            assert_eq!(Curve::Linear.apply(1.0), 1.0);
        }

        // Case `linear_curve_clamps`.
        {
            assert_eq!(Curve::Linear.apply(-0.5), 0.0);
            assert_eq!(Curve::Linear.apply(2.0), 1.0);
        }

        // Case `ease_out_starts_fast_ends_slow`.
        {
            let half = Curve::EaseOut.apply(0.5);
            assert!(
                half > 0.5,
                "ease-out at midpoint should be past linear: {half}"
            );
            assert!(Curve::EaseOut.apply(0.0).abs() < 1e-6);
            assert!((Curve::EaseOut.apply(1.0) - 1.0).abs() < 1e-6);
        }

        // Case `ease_in_out_is_smoothstep`.
        {
            let a = Curve::EaseInOut.apply(0.25);
            let b = Curve::EaseInOut.apply(0.75);
            assert!((a + b - 1.0).abs() < 1e-5);
            assert!((Curve::EaseInOut.apply(0.5) - 0.5).abs() < 1e-5);
        }

        // Case `spring_curve_settles_near_one`.
        {
            // Critically damped spring: settles by the end of the unit window.
            let c = Curve::Spring {
                stiffness: 200.0,
                damping: 2.0 * (200.0_f32).sqrt(),
                mass: 1.0,
            };
            let v = c.apply(1.0);
            assert!(v > 0.9 && v <= 1.05, "spring at t=1 should be near 1: {v}");
        }
    }

    #[test]
    fn animated_values_progress_clamp_finish_and_lerp() {
        // Case `animated_progresses_through_curve_over_duration`.
        {
            let now = crate::time::now();
            let a = Animated::starting_at(
                0.0_f32,
                100.0,
                now,
                Duration::from_millis(100),
                Curve::Linear,
            );
            assert_eq!(a.current(now), 0.0);
            assert!((a.current(now + Duration::from_millis(50)) - 50.0).abs() < 0.5);
            assert!((a.current(now + Duration::from_millis(100)) - 100.0).abs() < 0.001);
        }

        // Case `animated_clamps_at_endpoints`.
        {
            let now = crate::time::now();
            let a = Animated::starting_at(
                0.0_f32,
                100.0,
                now,
                Duration::from_millis(100),
                Curve::Linear,
            );
            assert_eq!(
                a.current(now.checked_sub(Duration::from_millis(10)).unwrap()),
                0.0
            );
            assert_eq!(a.current(now + Duration::from_millis(200)), 100.0);
        }

        // Case `done_flips_after_duration_elapsed`.
        {
            let now = crate::time::now();
            let a = Animated::starting_at(
                0.0_f32,
                100.0,
                now,
                Duration::from_millis(100),
                Curve::Linear,
            );
            assert!(!a.done(now));
            assert!(!a.done(now + Duration::from_millis(50)));
            assert!(a.done(now + Duration::from_millis(101)));
        }

        // Case `raw_t_is_unclamped`.
        {
            let now = crate::time::now();
            let a =
                Animated::starting_at(0.0_f32, 1.0, now, Duration::from_millis(100), Curve::Linear);
            assert!((a.raw_t(now + Duration::from_millis(50)) - 0.5).abs() < 0.01);
            let over = a.raw_t(now + Duration::from_millis(300));
            assert!(over > 2.5 && over < 3.5, "raw_t should overshoot: {over}");
            let under = a.raw_t(now.checked_sub(Duration::from_millis(50)).unwrap());
            assert!(
                under < -0.4 && under > -0.6,
                "raw_t should be negative: {under}"
            );
        }

        // Case `zero_duration_is_immediate`.
        {
            let now = crate::time::now();
            let a = Animated::starting_at(0.0_f32, 1.0, now, Duration::ZERO, Curve::Linear);
            assert!(a.done(now));
            assert_eq!(a.current(now), 1.0);
            assert_eq!(
                a.current(now.checked_sub(Duration::from_millis(1)).unwrap()),
                0.0
            );
        }

        // Case `color_lerp_interpolates_per_channel`.
        {
            use vello::peniko::Color;
            // UFCS: disambiguates from peniko's own 3-arg `lerp` so this
            // exercises per-channel u8 interpolation, not the color-space lerp.
            let red = Color::from_rgba8(255, 0, 0, 255);
            let blue = Color::from_rgba8(0, 0, 255, 255);
            let mid = <Color as Lerp>::lerp(red, blue, 0.5);
            // Round-to-nearest of 127.5 -> 128.
            assert_eq!(mid.r(), 128);
            assert_eq!(mid.g(), 0);
            assert_eq!(mid.b(), 128);
            assert_eq!(mid.a(), 255);

            assert_eq!(<Color as Lerp>::lerp(red, blue, 0.0).r(), 255);
            assert_eq!(<Color as Lerp>::lerp(red, blue, 1.0).b(), 255);
        }

        // Case `animated_color_runs`.
        {
            use vello::peniko::Color;
            let now = crate::time::now();
            let a = Animated::starting_at(
                Color::from_rgba8(0, 0, 0, 255),
                Color::from_rgba8(200, 100, 50, 255),
                now,
                Duration::from_millis(100),
                Curve::Linear,
            );
            let mid = a.current(now + Duration::from_millis(50));
            assert!(
                mid.r() > 90 && mid.r() < 110,
                "mid R should be ~100: {}",
                mid.r()
            );
            assert!(mid.g() > 40 && mid.g() < 60);
            assert!(mid.b() > 20 && mid.b() < 30);
        }

        // Case `f64_lerp_works`.
        {
            let mid = (0.0_f64).lerp(10.0, 0.5);
            assert!((mid - 5.0).abs() < 1e-9);
        }
    }

    #[test]
    fn transition_visibility_follows_open_close_intent() {
        // Case `transition_closed_is_invisible_open_is_visible`.
        {
            let now = crate::time::now();
            let closed = Transition::closed();
            assert!(!closed.is_visible(now));
            assert!((closed.value(now) - 0.0).abs() < 1e-6);

            let open = Transition::open();
            assert!(open.is_visible(now));
            assert!((open.value(now) - 1.0).abs() < 1e-6);
        }

        // Case `transition_opening_stays_visible_through_the_curve`.
        {
            let mut t = Transition::closed();
            t.set_open(true, Duration::from_millis(100), Curve::EaseOut);
            assert!(t.is_visible(crate::time::now()));
            assert!(t.is_open());
        }

        // Case `transition_closing_stays_visible_until_settled`.
        {
            let mut t = Transition::open();
            t.set_open(false, Duration::from_millis(80), Curve::EaseOut);
            let now = crate::time::now();
            assert!(
                t.is_visible(now),
                "a closing surface must stay visible so its exit animates"
            );
            let later = now + Duration::from_millis(500);
            assert!(!t.is_visible(later));
            assert!((t.value(later) - 0.0).abs() < 1e-6);
        }

        // Case `transition_reversal_retargets_from_current_value`.
        {
            let mut t = Transition::closed();
            t.set_open(true, Duration::from_secs(1), Curve::Linear);
            // Can't advance wall-clock precisely without sleeping, so assert the
            // structural contract: after a reversal it's still visible
            // (mid-flight, not popped to 0).
            t.set_open(false, Duration::from_secs(1), Curve::Linear);
            assert!(!t.is_open());
            assert!(t.is_visible(crate::time::now()));
        }

        // Case `transition_repeated_same_intent_is_noop_once_settled`.
        {
            let mut t = Transition::open();
            let before = t.value(crate::time::now());
            t.set_open(true, Duration::from_millis(100), Curve::EaseOut);
            let after = t.value(crate::time::now());
            assert!((before - after).abs() < 1e-6);
            assert!(t.is_open());
        }
    }

    #[test]
    fn capture_open_at_freezes_clamps_and_closes_from_the_pose() {
        // Case `capture_open_at_keeps_fractional_pose_at_future_times`.
        {
            let now = crate::time::now();
            let t = Transition::capture_open_at(375);
            assert!((t.value(now + Duration::from_secs(1)) - 0.375).abs() < f32::EPSILON);
            assert!(t.is_visible(now + Duration::from_secs(1)));
            assert!(!t.is_animating(now + Duration::from_secs(1)));
        }

        // Case `capture_open_at_same_open_intent_does_not_advance_pose`.
        {
            let now = crate::time::now();
            let mut t = Transition::capture_open_at(625);
            t.set_open_at(
                true,
                Duration::from_millis(180),
                Curve::EaseOut,
                now + Duration::from_secs(1),
            );
            assert!((t.value(now + Duration::from_secs(2)) - 0.625).abs() < f32::EPSILON);
        }

        // Case `capture_open_at_clamps_boundaries`.
        {
            let now = crate::time::now();
            assert_eq!(Transition::capture_open_at(0).value(now), 0.0);
            assert_eq!(Transition::capture_open_at(1_000).value(now), 1.0);
            assert_eq!(Transition::capture_open_at(u16::MAX).value(now), 1.0);
        }

        // Case `capture_open_at_closes_from_frozen_value`.
        {
            let now = crate::time::now();
            let mut t = Transition::capture_open_at(500);
            t.set_open_at(false, Duration::from_millis(100), Curve::Linear, now);
            assert!((t.value(now) - 0.5).abs() < f32::EPSILON);
            assert!((t.value(now + Duration::from_millis(25)) - 0.25).abs() < 0.01);
            assert_eq!(t.value(now + Duration::from_millis(60)), 0.0);
        }
    }
}
