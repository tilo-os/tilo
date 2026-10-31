//! A painter's declared transition, run on the toolkit's clock: the spring, the
//! step and the easing curve a theme names, evaluated here where the other
//! motions live. The theme SDK carries the declaration (`tilo_theme_sdk::
//! Transition`); this is how a change moves once the toolkit runs it.

use tilo_theme_sdk::{Easing, Transition};

/// How close to its end a declared spring counts as settled.
const SETTLED: f32 = 1e-3;

/// The progress a painter's [`Transition`] declares, `elapsed` seconds after
/// the change, and whether it has settled; `None` for [`Transition::Inherit`],
/// whose curve the toolkit's own motion for the change decides. A spring's
/// progress overshoots 1 when it is underdamped.
#[must_use]
pub fn painter_progress(transition: Transition, elapsed: f32) -> Option<(f32, bool)> {
    let t = elapsed.max(0.0);
    Some(match transition {
        Transition::Inherit => return None,
        Transition::None => (1.0, true),
        Transition::Stepped { steps, duration } => {
            if duration <= 0.0 || steps == 0 || t >= duration {
                (1.0, true)
            } else {
                let steps = f32::from(steps);
                (((t / duration) * steps).floor() / steps, false)
            }
        }
        Transition::Eased { duration, easing } => {
            if duration <= 0.0 || t >= duration {
                (1.0, true)
            } else {
                (eased(easing, t / duration), false)
            }
        }
        Transition::Spring { response, damping } => spring(response, damping, t),
    })
}

/// A declared easing curve at `t` (`0..=1`).
fn eased(easing: Easing, t: f32) -> f32 {
    match easing {
        Easing::Linear => t,
        Easing::EaseOut => 1.0 - (1.0 - t).powi(3),
        Easing::EaseInOut => {
            if t < 0.5 {
                4.0 * t * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
            }
        }
    }
}

fn spring(response: f32, damping: f32, t: f32) -> (f32, bool) {
    if response <= 0.0 {
        return (1.0, true);
    }
    let omega = std::f32::consts::TAU / response;
    let zeta = damping.max(0.05);
    let envelope = (-zeta * omega * t).exp();
    let x = if zeta >= 1.0 {
        1.0 - (1.0 + omega * t) * envelope
    } else {
        let omega_d = omega * (1.0 - zeta * zeta).sqrt();
        1.0 - envelope * ((omega_d * t).cos() + zeta * omega / omega_d * (omega_d * t).sin())
    };
    // The envelope bounds the distance from the end; (1 + ωt) e^(-ωt) for
    // a critically damped one.
    let bound = if zeta >= 1.0 {
        (1.0 + omega * t) * envelope
    } else {
        envelope / (1.0 - zeta * zeta).sqrt()
    };
    if bound < SETTLED {
        (1.0, true)
    } else {
        (x, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepped_motion_jumps_in_equal_steps_and_lands() {
        let t = Transition::Stepped {
            steps: 4,
            duration: 0.2,
        };
        assert_eq!(painter_progress(t, 0.0), Some((0.0, false)));
        assert_eq!(painter_progress(t, 0.06), Some((0.25, false)));
        assert_eq!(painter_progress(t, 0.11), Some((0.5, false)));
        assert_eq!(painter_progress(t, 0.2), Some((1.0, true)));
    }

    #[test]
    fn a_spring_starts_at_rest_overshoots_when_underdamped_and_settles() {
        let bouncy = Transition::Spring {
            response: 0.3,
            damping: 0.4,
        };
        assert!(painter_progress(bouncy, 0.0).unwrap().0.abs() < 1e-6);
        let peak = (1..60)
            .map(|i: u16| painter_progress(bouncy, f32::from(i) * 0.01).unwrap().0)
            .fold(0.0_f32, f32::max);
        assert!(peak > 1.05, "{peak}");
        assert_eq!(painter_progress(bouncy, 5.0), Some((1.0, true)));
        let calm = Transition::Spring {
            response: 0.3,
            damping: 1.0,
        };
        assert!(
            (1..200_u16).all(|i| painter_progress(calm, f32::from(i) * 0.01).unwrap().0 <= 1.0)
        );
    }

    #[test]
    fn inherit_leaves_the_curve_to_the_toolkit_and_none_lands_at_once() {
        assert_eq!(painter_progress(Transition::Inherit, 0.1), None);
        assert_eq!(painter_progress(Transition::None, 0.0), Some((1.0, true)));
    }
}

/// A blend between two values as a layout's size crosses a range, eased so it
/// is flat at both ends: `0` at or below `from`, `1` at or above `to`, and a
/// smoothstep between. A responsive change driven by width or height (a stage
/// cross-fading between breakpoints) moves at the same shape a motion does,
/// rather than snapping at the breakpoint.
#[must_use]
pub fn blend_across(value: f64, from: f64, to: f64) -> f64 {
    if to <= from {
        return if value >= to { 1.0 } else { 0.0 };
    }
    let t = ((value - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
