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
}

impl Default for SpringPresets {
    fn default() -> Self {
        Self {
            subtle: SpringParams { stiffness: 120.0, damping: 22.0, mass: 1.0 },
            snappy: SpringParams { stiffness: 260.0, damping: 28.0, mass: 1.0 },
            bouncy: SpringParams { stiffness: 180.0, damping: 12.0, mass: 1.0 },
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

#[derive(Clone, Copy, Debug)]
pub struct Motion {
    pub springs: SpringPresets,
    pub durations: Durations,
}

impl Default for Motion {
    fn default() -> Self {
        Self { springs: SpringPresets::default(), durations: Durations::default() }
    }
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
}
