//! Motion tokens — the one shared timing + spring vocabulary the whole UI (toolkit
//! widgets AND shell chrome) animates against, so same-class gestures share a feel.

use crate::time::Duration;

/// A dragged group that moves less than this per second is settled.
pub const MOVE_SETTLED_PX_PER_S: f64 = 4.0;
/// How deep the band along a viewport's edge is in which a drag scrolls the viewport
/// ([`crate::edge_autoscroll`]).
pub const EDGE_AUTOSCROLL_BAND: f64 = crate::tokens::Spacing::BASE.s8 as f64;
/// How fast a drag pressed right against a viewport's edge scrolls it; it starts from
/// nothing at the band's inner side and grows with depth.
pub const EDGE_AUTOSCROLL_PX_PER_S: f64 = 1600.0;
/// Longest frame step an edge autoscroll integrates, so a stalled frame does not fling.
pub const EDGE_AUTOSCROLL_MAX_STEP: Duration = Duration::from_millis(50);
/// The slowest release that still flings.
pub const FLING_MIN_VELOCITY: f64 = 60.0;
/// How far a group sprite follows a drag before it stops.
pub const GROUP_SPRITE_REACH: f64 = 160.0;
/// The rate a revealed top edge pushes the window down.
pub const PUSH_SPEED: f64 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringParams {
    pub stiffness: f32,
    pub damping: f32,
    pub mass: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringPresets {
    /// Gentle, no overshoot (ζ ≈ 1.0).
    pub subtle: SpringParams,
    /// Quick, barely-perceptible overshoot (ζ ≈ 0.87).
    pub snappy: SpringParams,
    /// Playful overshoot (ζ ≈ 0.45).
    pub bouncy: SpringParams,
    /// Mass you can feel (ζ = 1.0, ω = 32 rad/s).
    pub heavy: SpringParams,
    /// Stiff critical follow (ζ = 1.0, ω = 40 rad/s).
    pub tracking: SpringParams,
}

impl Default for SpringPresets {
    fn default() -> Self {
        Self {
            subtle: SpringParams {
                stiffness: 120.0,
                damping: 22.0,
                mass: 1.0,
            },
            snappy: SpringParams {
                stiffness: 260.0,
                damping: 28.0,
                mass: 1.0,
            },
            bouncy: SpringParams {
                stiffness: 180.0,
                damping: 12.0,
                mass: 1.0,
            },
            heavy: SpringParams {
                stiffness: 2048.0,
                damping: 128.0,
                mass: 2.0,
            },
            tracking: SpringParams {
                stiffness: 1600.0,
                damping: 80.0,
                mass: 1.0,
            },
        }
    }
}

/// Three-slot duration scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Durations {
    pub fast_ms: u32,
    pub base_ms: u32,
    pub slow_ms: u32,
}

impl Default for Durations {
    fn default() -> Self {
        Self {
            fast_ms: 130,
            base_ms: 210,
            slow_ms: 400,
        }
    }
}

impl Durations {
    pub fn fast(self) -> Duration {
        Duration::from_millis(u64::from(self.fast_ms))
    }
    pub fn base(self) -> Duration {
        Duration::from_millis(u64::from(self.base_ms))
    }
    pub fn slow(self) -> Duration {
        Duration::from_millis(u64::from(self.slow_ms))
    }
}

/// Tuning for direct-manipulation gestures — the numbers that decide how a dragged
/// surface resists, catches and coasts. The caller supplies the extent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gesture {
    /// Ceiling on overscroll past an end, as a multiple of the surface's reference extent
    /// — the pull approaches this and never reaches it. Big enough that first contact
    /// reads as give rather than a stop, small enough the surface never leaves its
    /// region: slightly over one extent lets a drag pull an end clear of where it
    /// rests, see nothing beyond it, and be unable to pull further.
    pub overscroll_reach_frac: f32,
    pub detent_magnet_frac: f32,
    pub fling_projection_ms: u32,
}

impl Default for Gesture {
    fn default() -> Self {
        Self {
            overscroll_reach_frac: 1.2,
            // At 0.18 an in-between rest was comfortable (read as continuous, not
            // notched); at 0.45 the pull dominates the last third of a gap and crossing
            // feels like give-then-catch.
            detent_magnet_frac: 0.45,
            fling_projection_ms: 110,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Motion {
    pub springs: SpringPresets,
    pub durations: Durations,
    pub gesture: Gesture,
    /// The theme asks for no motion at all (`MotionCurve::None`): nothing moves and
    /// nothing fades, every change lands at once.
    pub still: bool,
}

impl Motion {
    /// These tokens with every spring critically damped: an eased curve with no overshoot
    /// at each spring's own speed.
    #[must_use]
    pub fn eased(mut self) -> Self {
        let critical = |spring: SpringParams| SpringParams {
            damping: 2.0 * (spring.stiffness * spring.mass).sqrt(),
            ..spring
        };
        let s = &mut self.springs;
        s.subtle = critical(s.subtle);
        s.snappy = critical(s.snappy);
        s.bouncy = critical(s.bouncy);
        s.heavy = critical(s.heavy);
        s.tracking = critical(s.tracking);
        self
    }

    /// These tokens with every timed duration scaled by `factor`.
    #[must_use]
    pub fn with_duration_scale(mut self, factor: f32) -> Self {
        let scale = |ms: u32| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a duration of a few hundred ms times a factor of at most 2, rounded"
            )]
            let scaled = (f64::from(ms) * f64::from(factor)).round().max(0.0) as u32;
            scaled
        };
        self.durations = Durations {
            fast_ms: scale(self.durations.fast_ms),
            base_ms: scale(self.durations.base_ms),
            slow_ms: scale(self.durations.slow_ms),
        };
        self
    }
}

/// What started a transition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MotionSource {
    Keyboard,
    #[default]
    Pointer,
    Gesture,
    /// An agent, a script or the action door. Nobody watches a hand move, and the caller
    /// acts on the result at once, so it follows the keyboard rule.
    Action,
}

impl MotionSource {
    /// Whether the result shows on the next frame.
    #[must_use]
    pub fn lands_at_once(self) -> bool {
        matches!(self, Self::Keyboard | Self::Action)
    }
}

impl Motion {
    /// How long a transition that changes what is shown (a crossfade, a group springing
    /// to its new slot) lasts when `source` started it: `nominal` for a pointer or
    /// gesture, nothing for a shortcut or action.
    #[must_use]
    pub fn result(&self, source: MotionSource, nominal: Duration) -> Duration {
        if source.lands_at_once() {
            Duration::ZERO
        } else {
            nominal
        }
    }

    /// How long a transition that only says where something went (an off-screen target
    /// sliding into view, a direction cue) lasts when `source` started it: `nominal` for
    /// a pointer or gesture, at most `fast` for a shortcut or action.
    #[must_use]
    pub fn context(&self, source: MotionSource, nominal: Duration) -> Duration {
        if source.lands_at_once() {
            nominal.min(self.durations.fast())
        } else {
            nominal
        }
    }
}

/// The motion a widget may play: the [`Motion`] tokens with the person's reduced-motion
/// setting applied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectiveMotion {
    tokens: Motion,
    reduced: bool,
}

impl Default for EffectiveMotion {
    fn default() -> Self {
        Self::new(Motion::default(), false)
    }
}

impl EffectiveMotion {
    #[must_use]
    pub const fn new(tokens: Motion, reduced: bool) -> Self {
        Self { tokens, reduced }
    }

    /// Whether movement is off: the person asked for reduced motion, or the theme holds
    /// still ([`Motion::still`]).
    #[must_use]
    pub const fn reduced(self) -> bool {
        self.reduced || self.tokens.still
    }

    /// Whether the theme holds still: no movement and no fade.
    #[must_use]
    pub const fn still(self) -> bool {
        self.tokens.still
    }

    /// Whether anything may move: false under reduced motion.
    #[must_use]
    pub const fn moves(self) -> bool {
        !self.reduced()
    }

    /// Whether a looping indicator animates; under reduced motion it draws its resting
    /// mark.
    #[must_use]
    pub const fn loops(self) -> bool {
        !self.reduced()
    }

    /// How long a movement lasts: `nominal`, or nothing under reduced motion.
    #[must_use]
    pub fn movement(self, nominal: Duration) -> Duration {
        if self.reduced() {
            Duration::ZERO
        } else {
            nominal
        }
    }

    /// How long a cross-fade lasts: `nominal`, or at most `fast` under reduced motion,
    /// where it stands in for the movement it replaces.
    #[must_use]
    pub fn fade(self, nominal: Duration) -> Duration {
        if self.tokens.still {
            Duration::ZERO
        } else if self.reduced {
            nominal.min(self.tokens.durations.fast())
        } else {
            nominal
        }
    }

    /// The cross-fade that replaces a spatial transition under reduced motion: `fast`, or
    /// nothing when motion is on (the movement itself shows the change).
    #[must_use]
    pub fn replacement_fade(self) -> Duration {
        if self.tokens.still {
            Duration::ZERO
        } else if self.reduced {
            self.tokens.durations.fast()
        } else {
            Duration::ZERO
        }
    }

    /// `fast` as a movement.
    #[must_use]
    pub fn fast(self) -> Duration {
        self.movement(self.tokens.durations.fast())
    }

    /// `base` as a movement.
    #[must_use]
    pub fn base(self) -> Duration {
        self.movement(self.tokens.durations.base())
    }

    /// `slow` as a movement.
    #[must_use]
    pub fn slow(self) -> Duration {
        self.movement(self.tokens.durations.slow())
    }

    /// The tokens themselves, for values reduced motion does not change (springs a
    /// gesture tracks with, gesture tuning).
    #[must_use]
    pub const fn tokens(self) -> Motion {
        self.tokens
    }

    /// These tokens instead, with the person's reduced-motion setting kept: how a
    /// component paces its own motion ([`crate::motion::Feel`]).
    #[must_use]
    pub const fn with_tokens(self, tokens: Motion) -> Self {
        Self {
            tokens,
            reduced: self.reduced,
        }
    }
}

/// iOS-style overscroll resistance. `limit` is the asymptote — the surface approaches it
/// and never arrives.
pub fn rubber_band(distance: f32, limit: f32) -> f32 {
    if distance <= 0.0 || limit <= 0.0 {
        return 0.0;
    }
    limit * distance / (distance + limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduced_motion_lands_movement_at_once_and_keeps_a_short_fade() {
        let tokens = Motion::default();
        let fast = tokens.durations.fast();
        let base = tokens.durations.base();
        let on = EffectiveMotion::new(tokens, false);
        assert!(on.moves() && on.loops() && !on.reduced());
        assert_eq!(on.movement(base), base);
        assert_eq!(on.fade(base), base);
        assert_eq!(on.base(), base);
        assert_eq!(on.replacement_fade(), Duration::ZERO);

        let reduced = EffectiveMotion::new(tokens, true);
        assert!(!reduced.moves() && !reduced.loops() && reduced.reduced());
        assert_eq!(reduced.movement(base), Duration::ZERO);
        assert_eq!(reduced.slow(), Duration::ZERO);
        assert_eq!(reduced.fade(base), fast, "a fade is capped at fast");
        assert_eq!(
            reduced.fade(Duration::from_millis(50)),
            Duration::from_millis(50)
        );
        assert_eq!(reduced.replacement_fade(), fast);
    }

    #[test]
    fn a_still_theme_neither_moves_nor_fades_and_eased_never_overshoots() {
        let still = EffectiveMotion::new(
            Motion {
                still: true,
                ..Motion::default()
            },
            false,
        );
        let base = Motion::default().durations.base();
        assert!(!still.moves() && !still.loops() && still.reduced() && still.still());
        assert_eq!(still.movement(base), Duration::ZERO);
        assert_eq!(still.fade(base), Duration::ZERO);
        assert_eq!(still.replacement_fade(), Duration::ZERO);

        let eased = Motion::default().eased();
        for spring in [eased.springs.snappy, eased.springs.bouncy] {
            let zeta = spring.damping / (2.0 * (spring.stiffness * spring.mass).sqrt());
            assert!((zeta - 1.0).abs() < 1e-4, "ζ = {zeta}");
        }
        let slow = Motion::default().with_duration_scale(2.0);
        assert_eq!(slow.durations.base_ms, 420);
    }

    #[test]
    fn motion_springs_have_their_documented_damping() {
        // phase: durations_expose_std_duration_helpers
        {
            let d = Durations::default();
            assert_eq!(d.fast(), Duration::from_millis(130));
            assert_eq!(d.base(), Duration::from_millis(210));
            assert_eq!(d.slow(), Duration::from_millis(400));
        }
        // phase: snappy_is_lightly_underdamped
        {
            // ζ = c / (2 sqrt(k m)); snappy should sit just under 1.0 so it settles fast
            // with a barely-visible overshoot.
            let p = SpringPresets::default().snappy;
            let zeta = p.damping / (2.0 * (p.stiffness * p.mass).sqrt());
            assert!(zeta > 0.8 && zeta < 1.0, "snappy ζ = {zeta}");
        }
        // phase: heavy_is_critically_damped_and_settles_promptly
        // "Weight without wobble" means ζ must reach exactly 1.0 — anything
        // below and the spring crosses its target, the bouncy reading this
        // preset exists to avoid.
        //
        // The settle bound is measured on the production motion engine
        // (`crate::motion::MotionValue`), so a change to how motion is
        // solved can't quietly lengthen it.
        {
            let p = SpringPresets::default();
            let z = zeta(p.heavy);
            assert!((z - 1.0).abs() < 1e-3, "heavy ζ = {z}, must be 1.0");
            assert!(
                p.heavy.mass > p.snappy.mass,
                "heavy must carry more mass than snappy, not just more stiffness"
            );

            let heavy =
                crate::motion::Timing::Spring(crate::motion::SpringSpec::from_params(p.heavy));
            let start = crate::time::now();
            let mut s = crate::motion::MotionValue::new(0.0_f64);
            s.set(1.0, heavy, start);
            let elapsed = s
                .settles_at()
                .expect("moving")
                .duration_since(start)
                .as_secs_f32();
            assert!(
                (0.10..=0.30).contains(&elapsed),
                "heavy must settle within 0.10-0.30 s, took {elapsed:.3} s"
            );

            // Critical damping is a claim about the path, not just the endpoint: it must
            // approach from one side and never cross.
            for ms in 0..2000_u64 {
                let v = s.get(start + Duration::from_millis(ms));
                assert!(v <= 1.0 + 1e-4, "overshot to {v}");
            }
        }
        // phase: tracking_is_critically_damped_and_stiffer_than_snappy
        // The follow preset trades the other way: also critical, but stiff
        // enough that the lag it introduces stays a cue and never becomes
        // latency.
        {
            let p = SpringPresets::default();
            let z = zeta(p.tracking);
            assert!((z - 1.0).abs() < 1e-3, "tracking ζ = {z}, must be 1.0");
            assert!(
                omega(p.tracking) > 2.0 * omega(p.snappy),
                "tracking must be far stiffer than a settle spring: ω = {}",
                omega(p.tracking)
            );
        }
    }

    #[test]
    fn shortcuts_land_at_once_and_keep_only_a_fast_context_cue() {
        let motion = Motion::default();
        let base = motion.durations.base();
        let slow = motion.durations.slow();
        for source in [MotionSource::Keyboard, MotionSource::Action] {
            assert_eq!(motion.result(source, base), Duration::ZERO, "{source:?}");
            assert_eq!(motion.context(source, slow), motion.durations.fast());
        }
        for source in [MotionSource::Pointer, MotionSource::Gesture] {
            assert_eq!(motion.result(source, base), base, "{source:?}");
            assert_eq!(motion.context(source, slow), slow, "{source:?}");
        }
    }

    fn zeta(p: SpringParams) -> f32 {
        p.damping / (2.0 * (p.stiffness * p.mass).sqrt())
    }

    fn omega(p: SpringParams) -> f32 {
        (p.stiffness / p.mass).sqrt()
    }

    #[test]
    fn rubber_banding_stiffens_and_retargeting_preserves_motion() {
        // phase: rubber_band_starts_soft_and_stiffens_without_bound
        // The resistance curve's two load-bearing properties: it starts at
        // 1:1 with the input (so an end gives before it resists) and it is
        // bounded (so the surface can never be dragged somewhere it does
        // not belong), with resistance growing the whole way between.
        {
            let limit = 18.0;
            assert_eq!(rubber_band(0.0, limit), 0.0);
            assert_eq!(rubber_band(-5.0, limit), 0.0);

            // First contact is nearly free: a small pull is nearly 1:1.
            let tiny = rubber_band(0.2, limit);
            assert!(
                tiny > 0.18 && tiny < 0.2,
                "first contact should barely resist, got {tiny}"
            );

            // Every additional unit of input buys strictly less movement than the one
            // before it, all the way out.
            let mut prev_gain = f32::INFINITY;
            let mut prev = 0.0;
            for step in 1_u16..=400 {
                let d = f32::from(step);
                let v = rubber_band(d, limit);
                let gain = v - prev;
                assert!(
                    gain > 0.0,
                    "resistance must never become a hard stop (d = {d})"
                );
                assert!(
                    gain < prev_gain,
                    "resistance must grow monotonically (d = {d})"
                );
                assert!(v < limit, "overscroll must stay under the limit (d = {d})");
                prev_gain = gain;
                prev = v;
            }
            assert!(rubber_band(1.0e4, limit) > limit * 0.99);
        }
        // phase: a_new_spring_preserves_value_and_velocity
        // A motion mid-flight must be able to change its spring without
        // losing where it is or how fast it is going — that hand-off is
        // what lets a drag's momentum carry into the release settle.
        {
            use crate::motion::{MotionValue, SpringSpec, Timing};
            let presets = SpringPresets::default();
            let start = crate::time::now();
            let mut s = MotionValue::new(0.0_f64);
            s.set(
                40.0,
                Timing::Spring(SpringSpec::from_params(presets.tracking)),
                start,
            );
            let swap = start + Duration::from_millis(160);
            let (v, vel) = (s.get(swap), s.velocity(swap));
            assert!(
                vel.abs() > 1.0,
                "the motion should be moving before the swap"
            );
            s.set(
                41.0,
                Timing::Spring(SpringSpec::from_params(presets.heavy)),
                swap,
            );
            assert!((s.get(swap) - v).abs() < 1e-9);
            assert!((s.velocity(swap) - vel).abs() < 1e-9);
        }
    }
}
