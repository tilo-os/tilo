//! Damage-aware encode throttle.
//!
//! A wall-clock frame-rate cap the encode loop threads through: the
//! caller asks [`StreamingThrottle::should_encode`] on every captured
//! frame; the throttle returns `true` at most once per configured
//! interval so a flood of captures doesn't burn CPU re-encoding at more
//! than the target rate.

use std::time::{Duration, Instant};

/// Minimum gap between two consecutive encodes on a shared source.
///
/// 33 ms ≈ 30 fps cap. The software openh264 path can't sustain higher
/// rates on a typical laptop without burning the encode thread; hardware
/// encode lifts this.
const ENCODE_INTERVAL_MS: u64 = 33;

/// Damage-aware encode throttle.
///
/// One instance per shared source; the encode loop threads it through the
/// per-frame handler. `should_encode(now)` returns `true` when the
/// configured minimum interval has elapsed since the previous accepted
/// encode, `false` otherwise. The throttle does not enforce a frame queue
/// — by design, callers either encode synchronously and ship the bytes or
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

    /// Should the caller encode *now*? Returns `true` if the throttle's
    /// last-accepted timestamp is unset or strictly older than `interval`,
    /// `false` otherwise. The caller is responsible for then calling
    /// [`Self::mark_encoded`] when (and only when) it actually performs
    /// the encode — so callers that gate on `should_encode` for telemetry
    /// without actually encoding don't poison the throttle.
    pub fn should_encode(&self, now: Instant) -> bool {
        match self.last_encode {
            None => true,
            Some(prev) => now.duration_since(prev) >= self.interval,
        }
    }

    /// Mark a successful encode at `now`. Cementing this in a separate
    /// call (vs. `should_encode_and_mark`) lets the caller skip the stamp
    /// on encode failure without manual state surgery.
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

    /// Drive 100 simulated captures across one wall-clock second through a
    /// fake clock; assert the throttle accepts at most 30.
    #[test]
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

    #[test]
    fn throttle_first_call_always_accepts() {
        let now = Instant::now();
        let throttle = StreamingThrottle::new();
        assert!(throttle.should_encode(now));
    }

    #[test]
    fn throttle_rejects_back_to_back() {
        let now = Instant::now();
        let mut throttle = StreamingThrottle::new();
        assert!(throttle.should_encode(now));
        throttle.mark_encoded(now);
        assert!(!throttle.should_encode(now + Duration::from_millis(1)));
        assert!(throttle.should_encode(now + Duration::from_millis(40)));
    }
}
