//! Damage-aware encode throttle.
//!
//! A wall-clock frame-rate cap the encode loop threads through:
//! [`StreamingThrottle::should_encode`] returns `true` at most once per
//! configured interval on average, so a flood of captures doesn't burn CPU
//! re-encoding faster than the target rate.
//!
//! Encodes are due on a fixed schedule rather than an interval after the
//! last one. A client that damages at the display rate commits a hair early
//! or late of any interval; measured from the previous encode, an early
//! commit was refused and the next accepted one interval and a half later,
//! so a 30 fps cap paced a 60 Hz window at an uneven 20 fps.

use std::time::{Duration, Instant};

/// Gap between two scheduled encodes on a shared source: a thirtieth of a
/// second, the ceiling the software openh264 path can sustain on a typical
/// laptop without burning the encode thread (hardware encode lifts this).
/// Rounded up, so a second never holds a thirty-first frame.
const ENCODE_INTERVAL: Duration = Duration::from_micros(33_334);

/// Damage-aware encode throttle, one instance per shared source, threaded
/// through the encode loop's per-frame handler. Enforces no frame queue
/// by design — callers either encode synchronously and ship the bytes, or
/// drop the frame.
#[derive(Debug)]
pub struct StreamingThrottle {
    /// When the next encode is due; `None` before the first.
    next_due: Option<Instant>,
    interval: Duration,
}

impl StreamingThrottle {
    /// Construct a throttle gated at 30 fps.
    pub fn new() -> Self {
        Self::with_interval(ENCODE_INTERVAL)
    }

    /// Construct a throttle with a caller-chosen minimum encode interval.
    pub fn with_interval(interval: Duration) -> Self {
        Self {
            next_due: None,
            interval,
        }
    }

    /// Whether the caller should encode now: `true` before the first encode
    /// and once the next one is due. Callers must then call
    /// [`Self::mark_encoded`] only when they actually encode — gating on this
    /// for telemetry without encoding would poison the throttle.
    pub fn should_encode(&self, now: Instant) -> bool {
        self.next_due.is_none_or(|due| now >= due)
    }

    /// When the next encode is due, if one has happened: a caller holding a
    /// frame the throttle refused encodes it then, so the last change of a
    /// window that went still is not left unsent.
    pub fn next_due(&self) -> Option<Instant> {
        self.next_due
    }

    /// Marks a successful encode at `now`. Kept separate from
    /// `should_encode` so a failed encode can skip the stamp without manual state surgery.
    ///
    /// The next encode is due one interval after this one was due. After a
    /// pause longer than an interval the schedule restarts from `now`
    /// instead, so idle time never banks a burst.
    pub fn mark_encoded(&mut self, now: Instant) {
        let scheduled = self
            .next_due
            .filter(|due| now.saturating_duration_since(*due) < self.interval)
            .unwrap_or(now);
        self.next_due = Some(scheduled.checked_add(self.interval).unwrap_or(now));
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

    /// A window damaging at 60 Hz, each commit a little early or late,
    /// streams at an even 30 fps under a 30 fps cap.
    #[test]
    fn a_60hz_window_is_paced_evenly_at_the_cap() {
        let start = Instant::now();
        let mut throttle = StreamingThrottle::with_interval(Duration::from_micros(33_333));
        let mut encoded = Vec::new();
        for frame in 0..120_u32 {
            let jitter: u32 = if frame % 2 == 0 { 0 } else { 900 };
            let now = start + Duration::from_micros(u64::from(frame) * 16_667 - u64::from(jitter));
            if throttle.should_encode(now) {
                throttle.mark_encoded(now);
                encoded.push(now);
            }
        }
        assert!(
            (58..=61).contains(&encoded.len()),
            "two seconds at a 30 fps cap encode about 60 frames, got {}",
            encoded.len()
        );
        let widest = encoded
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .max()
            .unwrap();
        assert!(
            widest < Duration::from_millis(40),
            "no gap stretches to one and a half intervals: {widest:?}"
        );

        // A still window banks nothing: after a pause the next commit goes
        // out at once and the one right after it waits a full interval.
        let later = start + Duration::from_secs(10);
        assert!(throttle.should_encode(later));
        throttle.mark_encoded(later);
        assert_eq!(
            throttle.next_due(),
            Some(later + Duration::from_micros(33_333))
        );
        assert!(!throttle.should_encode(later + Duration::from_millis(20)));
    }
}
