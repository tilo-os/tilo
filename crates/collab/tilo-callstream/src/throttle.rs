//! Damage-aware encode throttle.
//!
//! A wall-clock frame-rate cap the encode loop threads through:
//! [`StreamingThrottle::should_encode`] returns `true` at most once per
//! configured interval, so a flood of captures doesn't burn CPU
//! re-encoding faster than the target rate.

use std::time::{Duration, Instant};

/// Minimum gap between two consecutive encodes on a shared source: 33 ms
/// ≈ 30 fps, the ceiling the software openh264 path can sustain on a
/// typical laptop without burning the encode thread (hardware encode lifts this).
const ENCODE_INTERVAL_MS: u64 = 33;

/// Damage-aware encode throttle, one instance per shared source, threaded
/// through the encode loop's per-frame handler. Enforces no frame queue
/// by design — callers either encode synchronously and ship the bytes, or
/// drop the frame.
#[derive(Debug)]
pub struct StreamingThrottle {
    last_encode: Option<Instant>,
    interval: Duration,
}

impl StreamingThrottle {
    /// Construct a throttle gated at 30 fps.
    pub fn new() -> Self {
        Self::with_interval(Duration::from_millis(ENCODE_INTERVAL_MS))
    }

    /// Construct a throttle with a caller-chosen minimum encode interval.
    pub fn with_interval(interval: Duration) -> Self {
        Self {
            last_encode: None,
            interval,
        }
    }

    /// Whether the caller should encode now: `true` if unset or the last
    /// accepted encode is older than `interval`. Callers must then call
    /// [`Self::mark_encoded`] only when they actually encode — gating on this
    /// for telemetry without encoding would poison the throttle.
    pub fn should_encode(&self, now: Instant) -> bool {
        match self.last_encode {
            None => true,
            Some(prev) => now.duration_since(prev) >= self.interval,
        }
    }

    /// Marks a successful encode at `now`. Kept separate from
    /// `should_encode` so a failed encode can skip the stamp without manual state surgery.
    pub fn mark_encoded(&mut self, now: Instant) {
        self.last_encode = Some(now);
    }
}

impl Default for StreamingThrottle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first call always accepts, back-to-back calls are rejected, and a damage loop
    /// throttles to 30 fps.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn the_throttle_accepts_the_first_frame_and_holds_to_30fps() {
        throttle_first_call_always_accepts();
        throttle_rejects_back_to_back();
        damage_loop_throttles_to_30fps();
    }

    /// Drives 100 simulated captures across one wall-clock second; asserts at
    /// most 30 are accepted.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "test phase: arithmetic on small fixed test values cannot overflow"
    )]
    fn damage_loop_throttles_to_30fps() {
        let start = Instant::now();
        let mut throttle = StreamingThrottle::new();
        let mut encoded = 0u32;
        for i in 0..100 {
            let now = start + Duration::from_millis(i * 10);
            if throttle.should_encode(now) {
                throttle.mark_encoded(now);
                encoded += 1;
            }
        }
        assert!(
            encoded <= 30,
            "throttle accepted {encoded} encodes — must cap at 30 fps"
        );
        assert!(encoded > 0, "throttle muted every encode — interval bug");
    }

    fn throttle_first_call_always_accepts() {
        let now = Instant::now();
        let throttle = StreamingThrottle::new();
        assert!(throttle.should_encode(now));
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "test phase: arithmetic on small fixed test values cannot overflow"
    )]
    fn throttle_rejects_back_to_back() {
        let now = Instant::now();
        let mut throttle = StreamingThrottle::new();
        assert!(throttle.should_encode(now));
        throttle.mark_encoded(now);
        assert!(!throttle.should_encode(now + Duration::from_millis(1)));
        assert!(throttle.should_encode(now + Duration::from_millis(40)));
    }
}
