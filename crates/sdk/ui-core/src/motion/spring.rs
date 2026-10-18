//! The one motion engine: a damped spring solved in closed form.
//!
//! A spring is described the way Apple's springs are: `response` (the period
//! of the undamped spring, in seconds: how quick it is) and `damping` (the
//! damping fraction: 1 is critical, below 1 overshoots). Both are positive.
//!
//! A segment starts at a displacement from its target and a velocity; its
//! state at any later time is computed exactly, so sampling at 30, 60 or
//! 144 Hz, or once after a stall, lands on the same curve. The instant the
//! segment is within tolerance of rest is solved once, when it starts, so a
//! settled motion computes nothing.

use crate::time::Duration;

/// How far a critically damped spring has to go before it counts as having
/// arrived when a motion token is turned into a spring: within 0.1 % of the
/// distance, where a fraction (an openness, an opacity) settles.
/// `(1 + x) e^-x = 0.001` at `x ≈ 9.2335`.
const TOKEN_SETTLE_OMEGA_T: f64 = 9.2335;

/// The longest a segment may take to settle, whatever its parameters: a
/// spring this slow is a mistake, and a bounded solve keeps it harmless.
const MAX_SETTLE_SECS: f64 = 10.0;

/// A spring's shape: how quick (`response`) and how bouncy (`damping`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringSpec {
    /// Period of the undamped spring, in seconds.
    pub response: f64,
    /// Damping fraction: 1.0 is critical (no overshoot); below overshoots.
    pub damping: f64,
}

impl SpringSpec {
    /// A critically damped spring that is within 0.1 % of where it is going
    /// after `duration`: what a motion token (`fast`, `base`, `slow`) means
    /// as a spring.
    #[must_use]
    pub fn settling_within(duration: Duration) -> Self {
        let secs = duration.as_secs_f64().max(1e-4);
        Self {
            response: std::f64::consts::TAU * secs / TOKEN_SETTLE_OMEGA_T,
            damping: 1.0,
        }
    }

    /// A critically damped spring of natural frequency `omega` (rad/s).
    #[must_use]
    pub fn critical(omega: f64) -> Self {
        Self {
            response: std::f64::consts::TAU / omega.max(1e-4),
            damping: 1.0,
        }
    }

    /// A spring from the token presets' stiffness, damping and mass.
    #[must_use]
    pub fn from_params(params: crate::tokens::SpringParams) -> Self {
        let mass = f64::from(params.mass).max(1e-4);
        let stiffness = f64::from(params.stiffness).max(1e-4);
        let omega = (stiffness / mass).sqrt();
        Self {
            response: std::f64::consts::TAU / omega,
            damping: f64::from(params.damping) / (2.0 * (stiffness * mass).sqrt()),
        }
    }

    /// Angular frequency of the undamped spring.
    #[must_use]
    pub fn omega(self) -> f64 {
        std::f64::consts::TAU / self.response.max(1e-4)
    }

    /// The same spring, `factor` times slower (a longer travel).
    #[must_use]
    pub fn slowed(self, factor: f64) -> Self {
        Self {
            response: self.response * factor.max(1e-3),
            damping: self.damping,
        }
    }

    /// Displacement and velocity `t` seconds into a segment that started at
    /// displacement `x0` with velocity `v0` (target at 0).
    #[must_use]
    pub fn state(self, x0: f64, v0: f64, t: f64) -> (f64, f64) {
        let t = t.max(0.0);
        let w = self.omega();
        let z = self.damping.max(0.0);
        if (z - 1.0).abs() < 1e-6 {
            let b = v0 + w * x0;
            let e = (-w * t).exp();
            ((x0 + b * t) * e, (v0 - w * b * t) * e)
        } else if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            let a = x0;
            let b = (v0 + z * w * x0) / wd;
            let e = (-z * w * t).exp();
            let (sin, cos) = (wd * t).sin_cos();
            let x = e * (a * cos + b * sin);
            let v = e * ((wd * b - z * w * a) * cos - (z * w * b + wd * a) * sin);
            (x, v)
        } else {
            let s = (z * z - 1.0).sqrt();
            let r1 = -w * (z - s);
            let r2 = -w * (z + s);
            let c2 = (v0 - r1 * x0) / (r2 - r1);
            let c1 = x0 - c2;
            let (e1, e2) = ((r1 * t).exp(), (r2 * t).exp());
            (c1 * e1 + c2 * e2, c1 * r1 * e1 + c2 * r2 * e2)
        }
    }

    /// An upper bound on `|displacement|` at `t`.
    fn envelope(self, x0: f64, v0: f64, t: f64) -> f64 {
        let w = self.omega();
        let z = self.damping.max(0.0);
        if (z - 1.0).abs() < 1e-6 {
            (x0.abs() + (v0 + w * x0).abs() * t) * (-w * t).exp()
        } else if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            let b = (v0 + z * w * x0) / wd;
            x0.hypot(b) * (-z * w * t).exp()
        } else {
            let s = (z * z - 1.0).sqrt();
            let r1 = -w * (z - s);
            let r2 = -w * (z + s);
            let c2 = (v0 - r1 * x0) / (r2 - r1);
            let c1 = x0 - c2;
            c1.abs() * (r1 * t).exp() + c2.abs() * (r2 * t).exp()
        }
    }

    /// Where the envelope peaks: from there on it only falls.
    fn envelope_peak(self, x0: f64, v0: f64) -> f64 {
        let w = self.omega();
        if (self.damping - 1.0).abs() < 1e-6 {
            let b = (v0 + w * x0).abs();
            if b <= f64::EPSILON {
                0.0
            } else {
                (1.0 / w - x0.abs() / b).max(0.0)
            }
        } else {
            // Both other envelopes are sums of decaying exponentials.
            0.0
        }
    }

    /// Seconds until a segment starting at `(x0, v0)` stays within
    /// `tolerance` of its target for good.
    #[must_use]
    pub fn settle_secs(self, x0: f64, v0: f64, tolerance: f64) -> f64 {
        let tolerance = tolerance.max(1e-9);
        let peak = self.envelope_peak(x0, v0);
        if self.envelope(x0, v0, peak) < tolerance {
            // Already there, and it never gets further away.
            return 0.0;
        }
        // March from the peak in quarter periods, then bisect.
        let step = 0.25 / self.omega();
        let mut lo = peak;
        let mut hi = peak + step;
        loop {
            if self.envelope(x0, v0, hi) < tolerance {
                break;
            }
            if hi >= MAX_SETTLE_SECS {
                return MAX_SETTLE_SECS;
            }
            lo = hi;
            hi += step;
        }
        for _ in 0..24 {
            let mid = 0.5 * (lo + hi);
            if self.envelope(x0, v0, mid) >= tolerance {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        hi
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn springs() -> [SpringSpec; 4] {
        [
            SpringSpec::settling_within(Duration::from_millis(210)),
            SpringSpec {
                response: 0.3,
                damping: 0.4,
            },
            SpringSpec {
                response: 0.3,
                damping: 1.6,
            },
            SpringSpec::from_params(crate::tokens::SpringPresets::default().heavy),
        ]
    }

    #[test]
    fn velocity_is_the_derivative_of_position() {
        for spring in springs() {
            for (x0, v0) in [(1.0, 0.0), (-40.0, 300.0), (0.0, -12.0)] {
                for t in [0.0, 0.013, 0.05, 0.2] {
                    let h = 1e-6;
                    let (a, _) = spring.state(x0, v0, t);
                    let (b, _) = spring.state(x0, v0, t + h);
                    let (_, v) = spring.state(x0, v0, t);
                    let numeric = (b - a) / h;
                    assert!(
                        (numeric - v).abs() < 1e-2 * (1.0 + v.abs()),
                        "{spring:?} at {t}: {numeric} vs {v}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_token_spring_arrives_within_its_token() {
        let d = Duration::from_millis(210);
        let spring = SpringSpec::settling_within(d);
        let (x, _) = spring.state(1.0, 0.0, d.as_secs_f64());
        assert!(x.abs() <= 0.00101, "{x}");
        let (x, _) = spring.state(1.0, 0.0, d.as_secs_f64() * 0.5);
        assert!(x.abs() > 0.05, "not there before its time: {x}");
    }

    #[test]
    fn the_settle_time_is_the_last_time_it_is_out_of_tolerance() {
        for spring in springs() {
            for (x0, v0) in [(100.0, 0.0), (0.0, 800.0), (-3.0, 40.0)] {
                let settle = spring.settle_secs(x0, v0, 0.1);
                for ms in 0..2000_u32 {
                    let t = settle + f64::from(ms) / 1000.0;
                    let (x, _) = spring.state(x0, v0, t);
                    assert!(x.abs() < 0.1, "{spring:?} {x0} {v0}: {x} at {t}");
                }
                assert!(settle < 2.0, "{spring:?} took {settle}");
            }
        }
    }

    #[test]
    fn critical_damping_never_overshoots_from_rest() {
        let spring = SpringSpec::settling_within(Duration::from_millis(400));
        for step in 0..500_u32 {
            let t = f64::from(step) * 0.002;
            let (x, _) = spring.state(1.0, 0.0, t);
            assert!(x >= 0.0, "crossed the target at {t}: {x}");
        }
    }

    #[test]
    fn preset_params_keep_their_damping() {
        let presets = crate::tokens::SpringPresets::default();
        assert!((SpringSpec::from_params(presets.heavy).damping - 1.0).abs() < 1e-3);
        let snappy = SpringSpec::from_params(presets.snappy);
        assert!(snappy.damping > 0.8 && snappy.damping < 1.0);
    }
}
