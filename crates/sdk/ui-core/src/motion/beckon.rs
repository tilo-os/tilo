//! A control hint calling for attention, showing its gesture, and
//! acknowledging that it was done.
//!
//! The one motion in the library that is meant to be noticed: a hint comes
//! once, at the moment its control is useful, so it arrives with a spring
//! that overshoots a little and a single glow pulse, where routine motion
//! never bounces. Its picture loops the gesture it teaches while it shows.
//! When the person does it, a check is drawn, the card folds into a dot and
//! the dot drifts to where hints are kept (the person's own menu).
//!
//! Every phase is a pure function of the instant it began, so the owner
//! keeps no motion state. Under reduced motion nothing moves: the card and
//! its check fade at `fast`, the picture holds its resting pose, and the
//! dot does not travel.

use super::named::{stage, unit_opacity};
use super::spring::SpringSpec;
use crate::time::{Duration, Instant};
use crate::tokens::EffectiveMotion;

/// A control hint's motion ([`crate::control_hints`]).
pub struct Beckon;

/// How the card is drawn while it arrives and rests.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arrival {
    /// Scale about the point of the card nearest its anchor.
    pub scale: f64,
    /// Opacity of the whole card.
    pub opacity: f32,
    /// How far the card still stands back from its resting place, toward
    /// its anchor, in px.
    pub approach: f64,
    /// The single glow pulse: how far it has spread (`0..=1`) and how
    /// strong it is.
    pub pulse: Option<(f64, f32)>,
}

/// How the card is drawn once its gesture was done.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Completion {
    /// How much of the check mark is drawn, `0..=1`.
    pub check: f64,
    /// How far the card has folded into its dot, `0..=1`.
    pub fold: f64,
    /// How far the dot has travelled toward where hints are kept, `0..=1`.
    pub drift: f64,
    /// The ring where the dot landed: how far it spread and how strong.
    pub landing: Option<(f64, f32)>,
    /// Opacity of what is still drawn.
    pub opacity: f32,
    /// The card's scale as the check lands: a small lift and back.
    pub pop: f64,
}

impl Beckon {
    /// The arrival: quick, with a small overshoot (about 6 %).
    const ARRIVE: SpringSpec = SpringSpec {
        response: 0.42,
        damping: 0.62,
    };
    /// How small the card starts, about its anchor side.
    const ARRIVE_FROM_SCALE: f64 = 0.72;
    /// How far back from its anchor it starts, in px.
    const ARRIVE_FROM_PX: f64 = 14.0;
    /// The glow pulse spreading out from the card, once.
    const PULSE: SpringSpec = SpringSpec {
        response: 0.7,
        damping: 1.0,
    };
    /// When the pulse leaves, after the card began to arrive.
    const PULSE_DELAY: Duration = Duration::from_millis(140);
    /// The pulse is gone once it has spread this far.
    const PULSE_END: f64 = 0.97;
    /// One loop of the picture's gesture.
    pub const GESTURE_PERIOD: Duration = Duration::from_millis(2600);
    /// Where in its loop the picture rests when motion may not loop: the
    /// gesture well under way, so the still picture says what to do.
    pub const RESTING_PHASE: f64 = 0.7;
    /// The check being drawn.
    const CHECK: SpringSpec = SpringSpec {
        response: 0.34,
        damping: 1.0,
    };
    /// How much the card lifts as its check lands.
    const POP: f64 = 0.05;
    /// When the card starts folding, after the gesture was done: the check
    /// has been seen.
    const FOLD_AT: Duration = Duration::from_millis(620);
    const FOLD: SpringSpec = SpringSpec {
        response: 0.3,
        damping: 1.0,
    };
    /// When the dot leaves for where hints are kept.
    const DRIFT_AT: Duration = Duration::from_millis(820);
    const DRIFT: SpringSpec = SpringSpec {
        response: 0.62,
        damping: 1.0,
    };
    /// When the dot lands and rings once.
    const LAND_AT: Duration = Duration::from_millis(1260);
    const LAND: SpringSpec = SpringSpec {
        response: 0.6,
        damping: 1.0,
    };
    /// The whole acknowledgement, after which nothing is drawn.
    pub const COMPLETION: Duration = Duration::from_millis(1800);
    /// Under reduced motion: the check holds this long, then the card fades.
    const REDUCED_HOLD: Duration = Duration::from_millis(700);

    /// The card `age` after it began to arrive.
    #[must_use]
    pub fn arrival(age: Duration, motion: EffectiveMotion) -> Arrival {
        if super::clock::testing::is_settled() {
            return Arrival::rest();
        }
        if motion.reduced() {
            let fade = fraction(age, motion.tokens().durations.fast());
            return Arrival {
                scale: 1.0,
                opacity: unit_opacity(fade),
                approach: 0.0,
                pulse: None,
            };
        }
        let secs = age.as_secs_f64();
        let travel = progress(Self::ARRIVE, secs);
        // Opacity leads: whole by the time the card first reaches its size.
        let opacity = unit_opacity(stage(progress(Self::CHECK, secs), 0.0, 0.6));
        let pulse = age.checked_sub(Self::PULSE_DELAY).and_then(|since| {
            let spread = progress(Self::PULSE, since.as_secs_f64());
            (spread < Self::PULSE_END).then(|| (spread, unit_opacity(0.7 * (1.0 - spread))))
        });
        Arrival {
            scale: Self::ARRIVE_FROM_SCALE + (1.0 - Self::ARRIVE_FROM_SCALE) * travel,
            opacity,
            approach: Self::ARRIVE_FROM_PX * (1.0 - travel),
            pulse,
        }
    }

    /// Where the picture is in its gesture's loop, `0..1`, `age` after the
    /// card began to arrive; [`Self::RESTING_PHASE`] where motion may not
    /// loop.
    #[must_use]
    pub fn gesture_phase(age: Duration, motion: EffectiveMotion) -> f64 {
        if !motion.loops() || super::clock::testing::is_settled() {
            return Self::RESTING_PHASE;
        }
        (age.as_secs_f64() / Self::GESTURE_PERIOD.as_secs_f64()).fract()
    }

    /// How far one stroke of the picture's gesture, which runs over
    /// `from..to` of its loop, has got at `phase`: `0..=1`, starting and
    /// stopping gently as a hand does.
    #[must_use]
    pub fn stroke(phase: f64, from: f64, to: f64) -> f64 {
        let t = stage(phase, from, to);
        t * t * (3.0 - 2.0 * t)
    }

    /// The card going away unanswered, `age` after it began to: it fades
    /// where it is, at `base` (`fast` under reduced motion).
    #[must_use]
    pub fn leaving(age: Duration, motion: EffectiveMotion) -> f32 {
        if super::clock::testing::is_settled() {
            return 0.0;
        }
        let durations = motion.tokens().durations;
        let pace = if motion.reduced() {
            durations.fast()
        } else {
            durations.base()
        };
        unit_opacity(1.0 - fraction(age, pace))
    }

    /// How long [`Self::leaving`] takes.
    #[must_use]
    pub fn leaving_time(motion: EffectiveMotion) -> Duration {
        let durations = motion.tokens().durations;
        if motion.reduced() {
            durations.fast()
        } else {
            durations.base()
        }
    }

    /// The acknowledgement, `age` after the gesture was done; `None` once it
    /// is over.
    #[must_use]
    pub fn completion(age: Duration, motion: EffectiveMotion) -> Option<Completion> {
        if age >= Self::COMPLETION || super::clock::testing::is_settled() {
            return None;
        }
        if motion.reduced() {
            let fast = motion.tokens().durations.fast();
            let check = fraction(age, fast);
            let gone = age
                .checked_sub(Self::REDUCED_HOLD)
                .map_or(0.0, |since| fraction(since, fast));
            if gone >= 1.0 {
                return None;
            }
            return Some(Completion {
                check,
                fold: 0.0,
                drift: 0.0,
                landing: None,
                opacity: unit_opacity(1.0 - gone),
                pop: 1.0,
            });
        }
        let after = |at: Duration, spring: SpringSpec| {
            age.checked_sub(at)
                .map_or(0.0, |since| progress(spring, since.as_secs_f64()))
        };
        let landing = age.checked_sub(Self::LAND_AT).and_then(|since| {
            let spread = progress(Self::LAND, since.as_secs_f64());
            (spread < Self::PULSE_END).then(|| (spread, unit_opacity(0.8 * (1.0 - spread))))
        });
        let drift = after(Self::DRIFT_AT, Self::DRIFT);
        let check = progress(Self::CHECK, age.as_secs_f64());
        Some(Completion {
            check,
            fold: after(Self::FOLD_AT, Self::FOLD),
            drift,
            landing,
            // The dot is absorbed as it lands; only its ring stays.
            opacity: unit_opacity(1.0 - stage(drift, 0.92, 1.0)),
            pop: 1.0 + Self::POP * (std::f64::consts::PI * check.clamp(0.0, 1.0)).sin(),
        })
    }

    /// When a hint shown since `shown` next needs a frame: soon while it
    /// arrives or its picture loops, never once it rests still.
    #[must_use]
    pub fn next_frame(shown: Instant, now: Instant, motion: EffectiveMotion) -> Option<Instant> {
        let arriving = now.saturating_duration_since(shown) < Self::GESTURE_PERIOD;
        (motion.loops() || arriving).then(|| now + Duration::from_millis(16))
    }
}

impl Arrival {
    /// The card at rest.
    #[must_use]
    pub fn rest() -> Self {
        Self {
            scale: 1.0,
            opacity: 1.0,
            approach: 0.0,
            pulse: None,
        }
    }
}

/// A run from rest on `spring`, `secs` in: `0` to `1`, past `1` where the
/// spring overshoots.
fn progress(spring: SpringSpec, secs: f64) -> f64 {
    1.0 - spring.state(1.0, 0.0, secs).0
}

/// How far through `span` `age` is, `0..=1`, eased as a critically damped
/// spring settling over `span`.
fn fraction(age: Duration, span: Duration) -> f64 {
    if span.is_zero() {
        return 1.0;
    }
    let spring = SpringSpec::settling_within(span);
    if age >= span {
        1.0
    } else {
        progress(spring, age.as_secs_f64()).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> EffectiveMotion {
        EffectiveMotion::default()
    }

    fn reduced() -> EffectiveMotion {
        EffectiveMotion::new(crate::tokens::Motion::default(), true)
    }

    #[test]
    fn the_card_arrives_with_a_small_overshoot_and_settles() {
        let peak = (0..800)
            .map(|ms| Beckon::arrival(Duration::from_millis(ms), on()).scale)
            .fold(0.0_f64, f64::max);
        assert!(peak > 1.02 && peak < 1.1, "overshoot {peak}");
        let late = Beckon::arrival(Duration::from_millis(1500), on());
        assert!((late.scale - 1.0).abs() < 0.01);
        assert!((late.opacity - 1.0).abs() < 0.01);
        assert!(late.pulse.is_none(), "the pulse happens once");
        assert!(
            Beckon::arrival(Duration::from_millis(300), on())
                .pulse
                .is_some()
        );
    }

    #[test]
    fn reduced_motion_only_fades() {
        for ms in [0, 40, 90, 400] {
            let a = Beckon::arrival(Duration::from_millis(ms), reduced());
            assert!((a.scale - 1.0).abs() < f64::EPSILON);
            assert!(a.approach.abs() < f64::EPSILON);
            assert!(a.pulse.is_none());
        }
        assert!(
            (Beckon::gesture_phase(Duration::from_millis(1234), reduced()) - Beckon::RESTING_PHASE)
                .abs()
                < f64::EPSILON
        );
        let c = Beckon::completion(Duration::from_millis(500), reduced()).unwrap();
        assert!(c.drift.abs() < f64::EPSILON && c.fold.abs() < f64::EPSILON);
    }

    #[test]
    fn completion_checks_then_folds_then_drifts_then_ends() {
        let at = |ms| Beckon::completion(Duration::from_millis(ms), on()).unwrap();
        assert!(at(300).check > 0.8 && at(300).fold < f64::EPSILON);
        assert!(at(800).fold > 0.5 && at(800).drift < f64::EPSILON);
        assert!(at(1250).drift > 0.6);
        assert!(at(1500).landing.is_some());
        assert!(Beckon::completion(Beckon::COMPLETION, on()).is_none());
    }
}
