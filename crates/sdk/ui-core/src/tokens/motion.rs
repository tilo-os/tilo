//! Motion tokens — the one shared timing + spring vocabulary the whole UI
//! (toolkit widgets AND shell chrome) animates against, so same-class
//! gestures share a feel.

use crate::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringParams {
    pub stiffness: f32,
    pub damping: f32,
    pub mass: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringPresets {
    /// Gentle, no overshoot (ζ ≈ 1.0). Ambient settles.
    pub subtle: SpringParams,
    /// Quick, barely-perceptible overshoot (ζ ≈ 0.87). Default for UI.
    pub snappy: SpringParams,
    /// Playful overshoot (ζ ≈ 0.45). Reserved for deliberate bounce.
    pub bouncy: SpringParams,
    /// Mass you can feel (ζ = 1.0, ω = 32 rad/s). For a control that is
    /// meant to read as a physical object rather than a state change.
    ///
    /// Weight and bounce are opposites: overshoot reads *light* (a thing
    /// with very little inertia springs past and wobbles back), so this is
    /// critically damped — ζ = exactly 1.0, the boundary where overshoot
    /// disappears.
    ///
    /// Weight does **not** come from a long settle: an earlier ω = 14
    /// (~0.5s) tuning read as slow, not heavy — the control had visually
    /// arrived and was still creeping. Inertia is legible while you're
    /// *pushing* (follow-lag, detent pull); once released it should land
    /// promptly, so the settle sits at ~0.26s.
    pub heavy: SpringParams,
    /// Stiff critical follow (ζ = 1.0, ω = 40 rad/s). For a surface that
    /// trails a dragging pointer instead of being welded to it.
    ///
    /// A surface pinned exactly under the cursor reads weightless — mass
    /// can't change velocity instantly, and routing the drag through a
    /// spring puts a lag between hand and surface proportional to speed,
    /// which is how inertia presents itself.
    ///
    /// Tracking error of a critically damped spring at constant speed `v`
    /// is `2v/ω`, so ω sets the whole feel: too soft reads as *late*
    /// rather than heavy. At ω = 40 an unhurried drag trails by a couple
    /// of degrees, a hard yank by about half a detent.
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

/// Three-slot duration scale. Everything time-parametric picks one of
/// these instead of a bespoke literal:
/// - `fast`  — micro-feedback: hover, drop-zone hints, chip toggles.
/// - `base`  — the default: opens, closes, reflows, tab in/out.
/// - `slow`  — large spatial moves: overview zoom, window spawn.
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

/// Tuning for direct-manipulation gestures — the numbers that decide how
/// a dragged surface resists, catches and coasts. Shared for the same
/// reason the springs are: two controls that resist differently read as
/// two unrelated machines.
///
/// Each value is a *fraction of the surface's own reference extent*
/// rather than an absolute distance, so one number covers a scroll
/// viewport and a detented wheel alike. The caller supplies the extent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gesture {
    /// Ceiling on overscroll past an end, as a multiple of the surface's
    /// reference extent — the pull approaches this and never reaches it.
    ///
    /// Big enough that first contact reads as give rather than a stop,
    /// small enough the surface never leaves its region: slightly over
    /// one extent means you can pull an end clear of where it rests, see
    /// nothing beyond it, and be unable to pull further.
    pub overscroll_reach_frac: f32,
    /// Peak strength of the corrective pull toward the nearest detent
    /// while dragging, as a fraction of one detent.
    ///
    /// What makes notches *felt* rather than merely landed on: crossing
    /// between two is a small acceleration then resistance. Zero would be
    /// a smooth slide; too large and the surface fights the hand and
    /// can't be held between notches.
    pub detent_magnet_frac: f32,
    /// How long a release's residual velocity is projected forward when
    /// choosing where a flick lands — the coast time the projection
    /// assumes; the settle spring supplies the actual deceleration.
    /// A flick should carry: distance travelled after release is what
    /// separates throwing something from pushing it.
    pub fling_projection_ms: u32,
}

impl Default for Gesture {
    fn default() -> Self {
        Self {
            overscroll_reach_frac: 1.2,
            // At 0.18 an in-between rest was comfortable (read as
            // continuous, not notched); at 0.45 the pull dominates the
            // last third of a gap and crossing feels like give-then-catch.
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
}

/// What started a transition. Motion follows the input: a shortcut is a
/// frequent expert action with a known result, so animating it adds latency
/// to every repetition, while pointer and finger motion explains where
/// things went.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MotionSource {
    /// A key chord.
    Keyboard,
    /// A click or tap on what is shown.
    #[default]
    Pointer,
    /// Direct manipulation: a drag, swipe or pinch. The surface follows the
    /// hand and the release hands its velocity to the settle.
    Gesture,
    /// An agent, a script or the action door. Nobody watches a hand move,
    /// and the caller acts on the result at once, so it follows the
    /// keyboard rule.
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
    /// How long a transition that changes what is shown (a crossfade, a
    /// group springing to its new slot) lasts when `source` started it:
    /// `nominal` for a pointer or gesture, nothing for a shortcut or action.
    #[must_use]
    pub fn result(&self, source: MotionSource, nominal: Duration) -> Duration {
        if source.lands_at_once() {
            Duration::ZERO
        } else {
            nominal
        }
    }

    /// How long a transition that only says where something went (an
    /// off-screen target sliding into view, a direction cue) lasts when
    /// `source` started it: `nominal` for a pointer or gesture, at most
    /// `fast` for a shortcut or action.
    #[must_use]
    pub fn context(&self, source: MotionSource, nominal: Duration) -> Duration {
        if source.lands_at_once() {
            nominal.min(self.durations.fast())
        } else {
            nominal
        }
    }
}

/// The motion a widget may play: the [`Motion`] tokens with the person's
/// reduced-motion setting applied. Read through [`crate::Theme::motion`];
/// it is the one place reduced motion is decided, so every widget and the
/// shell's chrome follow the same rule (HIG §9.7, design-language "Motion"):
///
/// - Movement — a slide, a scale, a spring, a snap, an overshoot — lands at
///   once ([`Self::movement`] is zero, [`Self::moves`] is false).
/// - A change of what is shown may still cross-fade, at most `fast`
///   ([`Self::fade`]): opacity carries the change without anything moving.
/// - A looping indicator (spinner, busy arc, sweeping bar) stands still
///   ([`Self::loops`] is false); progress and status keep updating.
///
/// Reduced motion never stops the frame pump: a widget that settles asks
/// for no frames, and one that still has a fade or new content to show
/// gets them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectiveMotion {
    tokens: Motion,
    reduced: bool,
}

impl EffectiveMotion {
    #[must_use]
    pub const fn new(tokens: Motion, reduced: bool) -> Self {
        Self { tokens, reduced }
    }

    /// Whether the person asked for reduced motion.
    #[must_use]
    pub const fn reduced(self) -> bool {
        self.reduced
    }

    /// Whether anything may move: false under reduced motion.
    #[must_use]
    pub const fn moves(self) -> bool {
        !self.reduced
    }

    /// Whether a looping indicator animates; under reduced motion it draws
    /// its resting mark.
    #[must_use]
    pub const fn loops(self) -> bool {
        !self.reduced
    }

    /// How long a movement lasts: `nominal`, or nothing under reduced motion.
    #[must_use]
    pub fn movement(self, nominal: Duration) -> Duration {
        if self.reduced {
            Duration::ZERO
        } else {
            nominal
        }
    }

    /// How long a cross-fade lasts: `nominal`, or at most `fast` under
    /// reduced motion, where it stands in for the movement it replaces.
    #[must_use]
    pub fn fade(self, nominal: Duration) -> Duration {
        if self.reduced {
            nominal.min(self.tokens.durations.fast())
        } else {
            nominal
        }
    }

    /// The cross-fade that replaces a spatial transition under reduced
    /// motion: `fast`, or nothing when motion is on (the movement itself
    /// shows the change).
    #[must_use]
    pub fn replacement_fade(self) -> Duration {
        if self.reduced {
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

    /// The tokens themselves, for values reduced motion does not change
    /// (springs a gesture tracks with, gesture tuning).
    #[must_use]
    pub const fn tokens(self) -> Motion {
        self.tokens
    }
}

/// iOS-style overscroll resistance.
///
/// `distance` is how far past the end the input asks to go; the return is
/// how far the surface is allowed to move. `limit` is the asymptote — the
/// surface approaches it and never arrives.
///
/// `L·d / (d + L)` is the algebraic simplification of the form usually
/// quoted for `UIScrollView`, `(1 − 1/(d·c/D + 1))·D/c` with `L = D/c`. Two
/// properties matter, both visible here: slope at `d = 0` is exactly 1 (the
/// first pixel past the end tracks the finger, the edge is never a wall),
/// and slope decays monotonically toward zero (resistance builds the whole
/// way, always a little more give and never much).
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
            // ζ = c / (2 sqrt(k m)); snappy should sit just under 1.0 so it
            // settles fast with a barely-visible overshoot.
            let p = SpringPresets::default().snappy;
            let zeta = p.damping / (2.0 * (p.stiffness * p.mass).sqrt());
            assert!(zeta > 0.8 && zeta < 1.0, "snappy ζ = {zeta}");
        }
        // phase: heavy_is_critically_damped_and_settles_promptly
        // "Weight without wobble" means ζ must reach exactly 1.0 — anything
        // below and the spring crosses its target, the bouncy reading this
        // preset exists to avoid.
        //
        // The settle bound is measured by stepping the real integrator rather
        // than derived from ω, so a change to how `Spring` integrates can't
        // quietly lengthen it.
        {
            let p = SpringPresets::default();
            let z = zeta(p.heavy);
            assert!((z - 1.0).abs() < 1e-3, "heavy ζ = {z}, must be 1.0");
            assert!(
                p.heavy.mass > p.snappy.mass,
                "heavy must carry more mass than snappy, not just more stiffness"
            );

            // Full-scale step through the production integrator at 120 Hz.
            let mut s = crate::animation::Spring::new(0.0, p.heavy);
            s.set_target(1.0);
            let dt = 1.0 / 120.0;
            let mut elapsed = 0.0_f32;
            while !s.is_settled() && elapsed < 2.0 {
                s.step(dt);
                elapsed += dt;
            }
            assert!(
                (0.10..=0.30).contains(&elapsed),
                "heavy must settle within 0.10-0.30 s, took {elapsed:.3} s"
            );

            // Critical damping is a claim about the path, not just the
            // endpoint: it must approach from one side and never cross.
            let mut s = crate::animation::Spring::new(0.0, p.heavy);
            s.set_target(1.0);
            for _ in 0..240 {
                s.step(dt);
                assert!(s.value() <= 1.0 + 1e-4, "overshot to {}", s.value());
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

            // Every additional unit of input buys strictly less movement
            // than the one before it, all the way out.
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
            // Far out, the surface has all but stopped moving.
            assert!(rubber_band(1.0e4, limit) > limit * 0.99);
        }
        // phase: set_params_preserves_value_and_velocity
        // A spring mid-flight must be able to change its constants without
        // losing where it is or how fast it is going — that hand-off is
        // what lets a drag's momentum carry into the release settle.
        {
            use crate::animation::Spring;
            let presets = SpringPresets::default();
            let mut s = Spring::new(0.0, presets.tracking);
            s.set_target(40.0);
            for _ in 0..10 {
                s.step(1.0 / 60.0);
            }
            let (v, vel) = (s.value(), s.velocity());
            assert!(vel.abs() > 1.0, "spring should be moving before the swap");
            s.set_params(presets.heavy);
            assert_eq!(s.value(), v);
            assert_eq!(s.velocity(), vel);
            assert_eq!(s.params().mass, presets.heavy.mass);
        }
    }
}
