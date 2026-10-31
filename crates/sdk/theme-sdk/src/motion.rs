//! How a part moves between states. The painter declares it; the toolkit
//! runs it on its own clock.

/// A curve for [`Transition::Eased`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Easing {
    Linear,
    /// Fast out, slow in: the stock ease for a change that answers input.
    EaseOut,
    EaseInOut,
}

impl Easing {
    fn at(self, t: f32) -> f32 {
        match self {
            Self::Linear => t,
            Self::EaseOut => 1.0 - (1.0 - t).powi(3),
            Self::EaseInOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
        }
    }
}

/// How a part moves from one set of states to another.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transition {
    /// The toolkit's own motion for this change.
    Inherit,
    /// The end state at once.
    None,
    /// A spring from 0 to 1: `response` seconds per oscillation, `damping`
    /// the damping ratio (1 critically damped, below 1 it overshoots).
    Spring { response: f32, damping: f32 },
    /// `steps` equal jumps over `duration` seconds: frame-by-frame, pixel
    /// motion.
    Stepped { steps: u16, duration: f32 },
    /// A curve over `duration` seconds.
    Eased { duration: f32, easing: Easing },
}

/// How close to its end a spring counts as settled.
const SETTLED: f32 = 1e-3;

impl Transition {
    /// The progress `elapsed` seconds after the change, and whether it has
    /// settled; `None` for [`Self::Inherit`], whose curve the toolkit owns.
    /// A spring's progress overshoots 1 when it is underdamped.
    #[must_use]
    pub fn at(self, elapsed: f32) -> Option<(f32, bool)> {
        let t = elapsed.max(0.0);
        Some(match self {
            Self::Inherit => return None,
            Self::None => (1.0, true),
            Self::Stepped { steps, duration } => {
                if duration <= 0.0 || steps == 0 || t >= duration {
                    (1.0, true)
                } else {
                    let steps = f32::from(steps);
                    (((t / duration) * steps).floor() / steps, false)
                }
            }
            Self::Eased { duration, easing } => {
                if duration <= 0.0 || t >= duration {
                    (1.0, true)
                } else {
                    (easing.at(t / duration), false)
                }
            }
            Self::Spring { response, damping } => spring(response, damping, t),
        })
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
        assert_eq!(t.at(0.0), Some((0.0, false)));
        assert_eq!(t.at(0.06), Some((0.25, false)));
        assert_eq!(t.at(0.11), Some((0.5, false)));
        assert_eq!(t.at(0.2), Some((1.0, true)));
    }

    #[test]
    fn a_spring_starts_at_rest_overshoots_when_underdamped_and_settles() {
        let bouncy = Transition::Spring {
            response: 0.3,
            damping: 0.4,
        };
        assert!(bouncy.at(0.0).unwrap().0.abs() < 1e-6);
        let peak = (1..60)
            .map(|i: u16| bouncy.at(f32::from(i) * 0.01).unwrap().0)
            .fold(0.0_f32, f32::max);
        assert!(peak > 1.05, "{peak}");
        assert_eq!(bouncy.at(5.0), Some((1.0, true)));
        let calm = Transition::Spring {
            response: 0.3,
            damping: 1.0,
        };
        assert!((1..200_u16).all(|i| calm.at(f32::from(i) * 0.01).unwrap().0 <= 1.0));
    }

    #[test]
    fn inherit_leaves_the_curve_to_the_toolkit_and_none_lands_at_once() {
        assert_eq!(Transition::Inherit.at(0.1), None);
        assert_eq!(Transition::None.at(0.0), Some((1.0, true)));
    }
}
