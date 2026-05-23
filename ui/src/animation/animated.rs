//! Unified animation primitives. Replaces ad-hoc Instant-based fields
//! with a single `Animated<T: Lerp>` driver consumable by widgets.
//!
//! Foundation spec item #17. Existing widgets (which currently track their
//! own `spawned_at`/`closing_at`/`wheel_anim_state`/`snap_anim` fields) are
//! migrated in a follow-up — this module just ships the primitives + tests.

use crate::color_compat::ColorCompat;
use std::time::{Duration, Instant};

/// Linear interpolation between two values.
pub trait Lerp {
    fn lerp(self, other: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(self, b: f32, t: f32) -> f32 {
        self + (b - self) * t
    }
}

impl Lerp for f64 {
    fn lerp(self, b: f64, t: f32) -> f64 {
        self + (b - self) * t as f64
    }
}

impl Lerp for vello::peniko::Color {
    fn lerp(self, other: Self, t: f32) -> Self {
        let lerp_u8 = |a: u8, b: u8, t: f32| -> u8 {
            let a = a as f32;
            let b = b as f32;
            (a + (b - a) * t).round().clamp(0.0, 255.0) as u8
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
    /// `1 - (1 - t)^3` — matches the existing widget pattern (e.g. tile_host).
    EaseOut,
    /// Smoothstep: `t * t * (3 - 2t)`.
    EaseInOut,
    /// Damped-oscillator unit-step response. The curve approaches `1.0` as
    /// `t -> 1`; under-damped configurations briefly overshoot `1.0`.
    Spring {
        stiffness: f32,
        damping: f32,
        mass: f32,
    },
}

impl Curve {
    /// Apply the curve to a normalized `0..1` input.
    ///
    /// `Linear`/`EaseOut`/`EaseInOut` clamp to `0..1`. `Spring` uses the
    /// closed-form damped-oscillator solution for the unit step, which can
    /// briefly exceed `1.0` for under-damped parameters.
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

/// Closed-form unit-step response of a damped harmonic oscillator at time `t`.
///
/// Solves `m * x'' + c * x' + k * x = k` with `x(0) = 0`, `x'(0) = 0`, so the
/// position approaches `1.0` as `t -> infinity`. Time is in normalized
/// progress units (matching the `0..1` duration ratio passed in).
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
    /// Create an animation that runs from `from` to `to` over `duration`,
    /// starting now.
    pub fn new(from: T, to: T, duration: Duration, curve: Curve) -> Self {
        Self {
            from,
            to,
            started: Instant::now(),
            duration,
            curve,
        }
    }

    /// Like `new`, but with an explicit start instant — useful for tests
    /// and for scheduling animations to begin in the future.
    pub fn starting_at(
        from: T,
        to: T,
        started: Instant,
        duration: Duration,
        curve: Curve,
    ) -> Self {
        Self {
            from,
            to,
            started,
            duration,
            curve,
        }
    }

    /// Interpolated value at `now`. Clamps before/after the active window
    /// to `from`/`to` respectively.
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

    /// `true` once `duration` has fully elapsed since `started`.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_curve_is_identity() {
        assert_eq!(Curve::Linear.apply(0.0), 0.0);
        assert_eq!(Curve::Linear.apply(0.5), 0.5);
        assert_eq!(Curve::Linear.apply(1.0), 1.0);
    }

    #[test]
    fn linear_curve_clamps() {
        assert_eq!(Curve::Linear.apply(-0.5), 0.0);
        assert_eq!(Curve::Linear.apply(2.0), 1.0);
    }

    #[test]
    fn ease_out_starts_fast_ends_slow() {
        let half = Curve::EaseOut.apply(0.5);
        assert!(half > 0.5, "ease-out at midpoint should be past linear: {half}");
        assert!(Curve::EaseOut.apply(0.0).abs() < 1e-6);
        assert!((Curve::EaseOut.apply(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ease_in_out_is_smoothstep() {
        let a = Curve::EaseInOut.apply(0.25);
        let b = Curve::EaseInOut.apply(0.75);
        assert!((a + b - 1.0).abs() < 1e-5);
        assert!((Curve::EaseInOut.apply(0.5) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn spring_curve_settles_near_one() {
        // Critically damped spring: settles by the end of the unit window.
        let c = Curve::Spring {
            stiffness: 200.0,
            damping: 2.0 * (200.0_f32).sqrt(),
            mass: 1.0,
        };
        let v = c.apply(1.0);
        assert!(v > 0.9 && v <= 1.05, "spring at t=1 should be near 1: {v}");
    }

    #[test]
    fn animated_progresses_through_curve_over_duration() {
        let now = Instant::now();
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

    #[test]
    fn animated_clamps_at_endpoints() {
        let now = Instant::now();
        let a = Animated::starting_at(
            0.0_f32,
            100.0,
            now,
            Duration::from_millis(100),
            Curve::Linear,
        );
        assert_eq!(a.current(now - Duration::from_millis(10)), 0.0);
        assert_eq!(a.current(now + Duration::from_millis(200)), 100.0);
    }

    #[test]
    fn done_flips_after_duration_elapsed() {
        let now = Instant::now();
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

    #[test]
    fn raw_t_is_unclamped() {
        let now = Instant::now();
        let a = Animated::starting_at(
            0.0_f32,
            1.0,
            now,
            Duration::from_millis(100),
            Curve::Linear,
        );
        assert!((a.raw_t(now + Duration::from_millis(50)) - 0.5).abs() < 0.01);
        let over = a.raw_t(now + Duration::from_millis(300));
        assert!(over > 2.5 && over < 3.5, "raw_t should overshoot: {over}");
        let under = a.raw_t(now - Duration::from_millis(50));
        assert!(under < -0.4 && under > -0.6, "raw_t should be negative: {under}");
    }

    #[test]
    fn zero_duration_is_immediate() {
        let now = Instant::now();
        let a = Animated::starting_at(0.0_f32, 1.0, now, Duration::ZERO, Curve::Linear);
        assert!(a.done(now));
        assert_eq!(a.current(now), 1.0);
        assert_eq!(a.current(now - Duration::from_millis(1)), 0.0);
    }

    #[test]
    fn color_lerp_interpolates_per_channel() {
        use vello::peniko::Color;
        // UFCS: peniko::Color (AlphaColor<Srgb>) inherits its own
        // 3-arg `lerp(other, t, HueDirection)` from the color crate.
        // Disambiguate to our 2-arg `Lerp::lerp` so the test exercises
        // the per-channel u8 interpolation, not the color-space lerp.
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

    #[test]
    fn animated_color_runs() {
        use vello::peniko::Color;
        let now = Instant::now();
        let a = Animated::starting_at(
            Color::from_rgba8(0, 0, 0, 255),
            Color::from_rgba8(200, 100, 50, 255),
            now,
            Duration::from_millis(100),
            Curve::Linear,
        );
        let mid = a.current(now + Duration::from_millis(50));
        assert!(mid.r() > 90 && mid.r() < 110, "mid R should be ~100: {}", mid.r());
        assert!(mid.g() > 40 && mid.g() < 60);
        assert!(mid.b() > 20 && mid.b() < 30);
    }

    #[test]
    fn f64_lerp_works() {
        let mid = (0.0_f64).lerp(10.0, 0.5);
        assert!((mid - 5.0).abs() < 1e-9);
    }
}
