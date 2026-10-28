//! When direct GPU scanout is tried again after it failed.
//!
//! Direct scanout is the normal presenter: the renderer draws into the
//! buffers KMS flips, and no frame is read back. A failure there — one slow
//! frame on a cold shader cache, a flip that did not retire while the panel
//! was blanking, a busy GPU — demotes the output to the readback presenter
//! only for a back-off interval. The next attempt after it rebuilds the
//! direct targets; a long enough healthy run forgets earlier failures.

use std::time::{Duration, Instant};

/// Back-off after the first failure; doubled for every further one.
const FIRST_RETRY: Duration = Duration::from_millis(500);
/// Ceiling for the back-off: a device that keeps failing is retried this often.
const MAX_RETRY: Duration = Duration::from_secs(30);
/// Direct presentation this long without a failure clears the failure count.
const STABLE_AFTER: Duration = Duration::from_secs(10);

/// One output's direct-scanout failure record.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DirectDegradation {
    /// Why the output is on the readback presenter now; `None` while direct
    /// scanout is healthy or not tried yet.
    reason: Option<String>,
    /// Failures since direct scanout last ran stably.
    failures: u32,
    /// The earliest time direct targets may be built again.
    retry_at: Option<Instant>,
    /// When the current direct targets started presenting.
    enabled_at: Option<Instant>,
}

impl DirectDegradation {
    /// Whether direct targets must not be built at `now`.
    pub fn blocked(&self, now: Instant) -> bool {
        self.reason.is_some() && self.retry_at.is_some_and(|retry_at| now < retry_at)
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// Failures since direct scanout last ran stably.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Record a failure at `now` and return how long the readback presenter
    /// stays in charge.
    pub fn record_failure(&mut self, reason: String, now: Instant) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let backoff = retry_delay(self.failures);
        self.reason = Some(reason);
        self.retry_at = Some(now.checked_add(backoff).unwrap_or(now));
        self.enabled_at = None;
        backoff
    }

    /// Direct targets were built and present from `now` on.
    pub fn record_enabled(&mut self, now: Instant) {
        self.reason = None;
        self.retry_at = None;
        self.enabled_at = Some(now);
    }

    /// A direct frame reached KMS at `now`.
    pub fn record_presented(&mut self, now: Instant) {
        if self.failures != 0
            && self
                .enabled_at
                .is_some_and(|enabled_at| now.saturating_duration_since(enabled_at) >= STABLE_AFTER)
        {
            self.failures = 0;
        }
    }

    /// Forget every failure: a new output generation or a changed request
    /// makes the next attempt meaningful at once.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Back-off before attempt `failures + 1`.
fn retry_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    FIRST_RETRY
        .checked_mul(1_u32.checked_shl(doublings).unwrap_or(u32::MAX))
        .unwrap_or(MAX_RETRY)
        .min(MAX_RETRY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_scanout_failures_back_off_and_are_retried() {
        {
            let start = Instant::now();
            let mut degradation = DirectDegradation::default();
            assert!(
                !degradation.blocked(start),
                "a fresh output tries direct scanout"
            );

            let backoff =
                degradation.record_failure("GPU-only absolute deadline exceeded".to_owned(), start);
            assert_eq!(backoff, FIRST_RETRY);
            assert!(degradation.blocked(start));
            assert!(
                degradation.blocked(
                    (start + FIRST_RETRY)
                        .checked_sub(Duration::from_millis(1))
                        .unwrap()
                )
            );
            assert!(
                !degradation.blocked(start + FIRST_RETRY),
                "the readback presenter is a back-off, not a permanent demotion"
            );
            assert_eq!(
                degradation.reason(),
                Some("GPU-only absolute deadline exceeded")
            );

            let retried = start + FIRST_RETRY;
            degradation.record_enabled(retried);
            assert_eq!(degradation.reason(), None);
            assert!(!degradation.blocked(retried));
        }
        {
            let now = Instant::now();
            let mut degradation = DirectDegradation::default();
            let delays: Vec<Duration> = (0..10)
                .map(|_| degradation.record_failure("flip".to_owned(), now))
                .collect();
            assert_eq!(delays[0], Duration::from_millis(500));
            assert_eq!(delays[1], Duration::from_secs(1));
            assert_eq!(delays[2], Duration::from_secs(2));
            assert!(delays.windows(2).all(|pair| pair[0] <= pair[1]));
            assert_eq!(delays[9], MAX_RETRY);
            assert!(
                !degradation.blocked(now + MAX_RETRY),
                "even a device that keeps failing is retried"
            );
            assert_eq!(degradation.failures(), 10);
            assert_eq!(retry_delay(u32::MAX), MAX_RETRY);
        }
        {
            let start = Instant::now();
            let mut degradation = DirectDegradation::default();
            degradation.record_failure("deadline".to_owned(), start);
            degradation.record_failure("deadline".to_owned(), start);
            degradation.record_enabled(start);

            degradation.record_presented(
                (start + STABLE_AFTER)
                    .checked_sub(Duration::from_millis(1))
                    .unwrap(),
            );
            assert_eq!(
                degradation.failures(),
                2,
                "a short run keeps the back-off growing"
            );
            degradation.record_presented(start + STABLE_AFTER);
            assert_eq!(degradation.failures(), 0);
            assert_eq!(
                degradation.record_failure("deadline".to_owned(), start + STABLE_AFTER),
                FIRST_RETRY
            );
        }
    }
}
