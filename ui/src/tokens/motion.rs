//! Motion tokens — the one shared timing + spring vocabulary the whole
//! UI (toolkit widgets AND shell chrome) animates against.
//!
//! Before this consolidation the shell scattered ~8 near-but-unequal
//! duration constants (140/180/200/220/250/260/400/420) and never read
//! these tokens at all. Every animation now maps onto one of three
//! semantic durations, so same-class gestures share a feel.

use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct SpringParams {
    pub stiffness: f32,
    pub damping: f32,
    pub mass: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct SpringPresets {
    /// Gentle, no overshoot (ζ ≈ 1.0). Ambient settles.
    pub subtle: SpringParams,
    /// Quick, barely-perceptible overshoot (ζ ≈ 0.87). Default for UI.
    pub snappy: SpringParams,
    /// Playful overshoot (ζ ≈ 0.45). Reserved for deliberate bounce.
    pub bouncy: SpringParams,
    /// Mass you can feel (ζ = 1.0, ω = 14 rad/s). For a control that is
    /// meant to read as a physical object rather than a state change:
    /// a long, completely wobble-free settle.
    ///
    /// Weight and bounce are opposites, not the same dial. Overshoot
    /// reads *light* — a thing that springs past its mark and wobbles
    /// back is a thing with very little inertia. What reads heavy is a
    /// settle that takes visible time and never once reverses. So this
    /// is critically damped (ζ = exactly 1.0, the boundary where
    /// overshoot disappears) and the *duration* comes from the mass
    /// instead: `snappy` lands in ~0.36 s, this in ~0.51 s.
    pub heavy: SpringParams,
    /// Stiff critical follow (ζ = 1.0, ω = 40 rad/s). For a surface that
    /// trails a dragging pointer instead of being welded to it.
    ///
    /// Rigid tracking always reads weightless — a thing with mass cannot
    /// change velocity instantly, so a surface pinned exactly under the
    /// cursor is telling you it has none. Routing the drag through a
    /// spring puts a lag between hand and surface that is proportional
    /// to how fast you are moving, which is precisely how inertia
    /// presents itself.
    ///
    /// The tracking error of a critically damped spring following a
    /// target at constant speed `v` is `2v/ω`, so ω sets the whole feel
    /// and it is a narrow window: too soft and the surface is not heavy,
    /// it is *late*, which reads as the machine being slow. At ω = 40
    /// an unhurried drag trails by a couple of degrees and a hard yank
    /// by about half a detent.
    pub tracking: SpringParams,
}

impl Default for SpringPresets {
    fn default() -> Self {
        Self {
            subtle: SpringParams { stiffness: 120.0, damping: 22.0, mass: 1.0 },
            snappy: SpringParams { stiffness: 260.0, damping: 28.0, mass: 1.0 },
            bouncy: SpringParams { stiffness: 180.0, damping: 12.0, mass: 1.0 },
            heavy: SpringParams { stiffness: 392.0, damping: 56.0, mass: 2.0 },
            tracking: SpringParams { stiffness: 1600.0, damping: 80.0, mass: 1.0 },
        }
    }
}

/// Three-slot duration scale. Everything time-parametric picks one of
/// these instead of a bespoke literal:
/// - `fast`  — micro-feedback: hover, drop-zone hints, chip toggles.
/// - `base`  — the default: opens, closes, reflows, tab in/out.
/// - `slow`  — large spatial moves: overview zoom, window spawn.
#[derive(Clone, Copy, Debug)]
pub struct Durations {
    pub fast_ms: u32,
    pub base_ms: u32,
    pub slow_ms: u32,
}

impl Default for Durations {
    fn default() -> Self {
        Self { fast_ms: 130, base_ms: 210, slow_ms: 400 }
    }
}

impl Durations {
    pub fn fast(self) -> Duration {
        Duration::from_millis(self.fast_ms as u64)
    }
    pub fn base(self) -> Duration {
        Duration::from_millis(self.base_ms as u64)
    }
    pub fn slow(self) -> Duration {
        Duration::from_millis(self.slow_ms as u64)
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
#[derive(Clone, Copy, Debug)]
pub struct Gesture {
    /// Ceiling on overscroll past an end, as a multiple of the surface's
    /// reference extent. The pull approaches this and never reaches it.
    ///
    /// It has to be big enough that first contact with the end is
    /// legible as give rather than as a stop, and small enough that the
    /// surface never leaves the region it belongs in. Slightly over one
    /// extent means you can pull an end clear of where it rests, see
    /// that there is nothing beyond it, and be unable to pull it further.
    pub overscroll_reach_frac: f32,
    /// Peak strength of the corrective pull toward the nearest detent
    /// while dragging, as a fraction of one detent.
    ///
    /// This is what makes notches *felt* rather than merely landed on:
    /// under a slow drag the surface is always being tugged toward the
    /// nearer notch, so crossing between two is a small acceleration
    /// followed by a small resistance. Zero would be a smooth slide;
    /// too large and the surface fights the hand and can no longer be
    /// held between notches at all.
    pub detent_magnet_frac: f32,
    /// How long a release's residual velocity is projected forward when
    /// choosing where a flick lands.
    ///
    /// A flick should carry: the distance travelled after the hand
    /// leaves is the entire difference between throwing something and
    /// pushing it. This is the coast time that projection assumes;
    /// the settle spring then supplies the actual deceleration.
    pub fling_projection_ms: u32,
}

impl Default for Gesture {
    fn default() -> Self {
        Self {
            overscroll_reach_frac: 1.2,
            detent_magnet_frac: 0.18,
            fling_projection_ms: 110,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Motion {
    pub springs: SpringPresets,
    pub durations: Durations,
    pub gesture: Gesture,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            springs: SpringPresets::default(),
            durations: Durations::default(),
            gesture: Gesture::default(),
        }
    }
}

/// iOS-style overscroll resistance.
///
/// `distance` is how far past the end the input actually asks to go;
/// the return is how far the surface is allowed to move. `limit` is the
/// asymptote — the surface approaches it and never arrives.
///
/// The curve is `L·d / (d + L)`, which is the algebraic simplification
/// of the form usually quoted for UIScrollView,
/// `(1 − 1/(d·c/D + 1))·D/c`, with `L = D/c`. Two properties are what
/// matter and both are visible in the simplified form: the slope at
/// `d = 0` is exactly 1, so the first pixel past the end tracks the
/// finger perfectly and the edge is never a wall; and the slope decays
/// monotonically toward zero, so resistance builds the whole way and
/// there is always a little more give and never much.
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
    fn durations_expose_std_duration_helpers() {
        let d = Durations::default();
        assert_eq!(d.fast(), Duration::from_millis(130));
        assert_eq!(d.base(), Duration::from_millis(210));
        assert_eq!(d.slow(), Duration::from_millis(400));
    }

    #[test]
    fn snappy_is_lightly_underdamped() {
        // ζ = c / (2 sqrt(k m)); snappy should sit just under 1.0 so it
        // settles fast with a barely-visible overshoot.
        let p = SpringPresets::default().snappy;
        let zeta = p.damping / (2.0 * (p.stiffness * p.mass).sqrt());
        assert!(zeta > 0.8 && zeta < 1.0, "snappy ζ = {zeta}");
    }

    fn zeta(p: SpringParams) -> f32 {
        p.damping / (2.0 * (p.stiffness * p.mass).sqrt())
    }

    fn omega(p: SpringParams) -> f32 {
        (p.stiffness / p.mass).sqrt()
    }

    /// The heavy preset's whole claim is "weight without wobble", and
    /// wobble is not a matter of taste — it is whether ζ reaches 1.0.
    /// Anything below and the spring crosses its target and comes back,
    /// which is the light, bouncy reading this preset exists to avoid.
    #[test]
    fn heavy_is_critically_damped_and_slower_than_snappy() {
        let p = SpringPresets::default();
        let z = zeta(p.heavy);
        assert!((z - 1.0).abs() < 1e-3, "heavy ζ = {z}, must be 1.0");
        assert!(
            omega(p.heavy) < omega(p.snappy),
            "heavy must settle more slowly than snappy: ω {} vs {}",
            omega(p.heavy),
            omega(p.snappy)
        );
        assert!(
            p.heavy.mass > p.snappy.mass,
            "the extra time must come from mass, not from softening the spring"
        );
    }

    /// The follow preset trades the other way: also critical, but stiff
    /// enough that the lag it introduces stays a cue and never becomes
    /// latency.
    #[test]
    fn tracking_is_critically_damped_and_stiffer_than_snappy() {
        let p = SpringPresets::default();
        let z = zeta(p.tracking);
        assert!((z - 1.0).abs() < 1e-3, "tracking ζ = {z}, must be 1.0");
        assert!(
            omega(p.tracking) > 2.0 * omega(p.snappy),
            "tracking must be far stiffer than a settle spring: ω = {}",
            omega(p.tracking)
        );
    }

    /// The resistance curve's two load-bearing properties: it starts at
    /// 1:1 with the input (so an end gives before it resists) and it is
    /// bounded (so the surface can never be dragged somewhere it does
    /// not belong), with resistance growing the whole way between.
    #[test]
    fn rubber_band_starts_soft_and_stiffens_without_bound() {
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
        let mut d = 1.0;
        while d <= 400.0 {
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
            d += 1.0;
        }
        // Far out, the surface has all but stopped moving.
        assert!(rubber_band(1.0e4, limit) > limit * 0.99);
    }

    /// A spring mid-flight must be able to change its constants without
    /// losing where it is or how fast it is going — that hand-off is
    /// what lets a drag's momentum carry into the release settle.
    #[test]
    fn set_params_preserves_value_and_velocity() {
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
