//! The curves a theme's painter declares for a part ([`tilo_theme_sdk::motion::Transition`]):
//! the painter says how a change moves and the toolkit runs it on its clock. The math lives
//! here, with the rest of the motion library, so a theme cannot hand-roll its own.

use tilo_theme_sdk::motion::{Easing, Transition};

/// How close to its end a spring counts as settled.
const SETTLED: f32 = 1e-3;

/// The progress `elapsed` seconds after a change, and whether it has settled; `None` for
/// [`Transition::Inherit`], whose curve the toolkit's named motion owns. A spring's progress
/// overshoots 1 when it is underdamped.
#[must_use]
pub fn progress(transition: Transition, elapsed: f32) -> Option<(f32, bool)> {
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

/// Where `easing` puts the part `t` of the way along: a change that answers input is fast out
/// and slow in, so it reads as the person's own speed.
fn eased(easing: Easing, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
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
        assert_eq!(progress(t, 0.0), Some((0.0, false)));
        assert_eq!(progress(t, 0.06), Some((0.25, false)));
        assert_eq!(progress(t, 0.11), Some((0.5, false)));
        assert_eq!(progress(t, 0.2), Some((1.0, true)));
    }

    #[test]
    fn a_spring_starts_at_rest_overshoots_when_underdamped_and_settles() {
        let bouncy = Transition::Spring {
            response: 0.3,
            damping: 0.4,
        };
        assert!(progress(bouncy, 0.0).unwrap().0.abs() < 1e-6);
        let peak = (1..60)
            .map(|i: u16| progress(bouncy, f32::from(i) * 0.01).unwrap().0)
            .fold(0.0_f32, f32::max);
        assert!(peak > 1.05, "{peak}");
        assert_eq!(progress(bouncy, 5.0), Some((1.0, true)));
        let calm = Transition::Spring {
            response: 0.3,
            damping: 1.0,
        };
        assert!((1..200_u16).all(|i| progress(calm, f32::from(i) * 0.01).unwrap().0 <= 1.0));
    }

    #[test]
    fn inherit_leaves_the_curve_to_the_toolkit_and_none_lands_at_once() {
        assert_eq!(progress(Transition::Inherit, 0.1), None);
        assert_eq!(progress(Transition::None, 0.0), Some((1.0, true)));
    }

    #[test]
    fn an_eased_change_follows_its_curve_and_lands_one_at_once() {
        let eased = Transition::Eased {
            duration: 0.2,
            easing: Easing::EaseOut,
        };
        assert_eq!(progress(eased, 0.1), Some((0.875, false)));
        assert_eq!(progress(eased, 0.2), Some((1.0, true)));
    }
}
