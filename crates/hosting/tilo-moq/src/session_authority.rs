//! The one session-authority lifecycle shared by every media carrier: the
//! voice relay ([`crate::relay`]), the live relay ([`crate::live`]) and its
//! HTTPS fallback ([`crate::live::fallback`]).
//!
//! Admission is the host authority accepting the grant *and* a first
//! observation that reports [`AuthorizationStatus::Current`]: only then does an
//! [`AuthorityDriver`] exist for the session. The driver then decides when the
//! session's authority is observed again ([`AuthorityDriver::next`]) and what
//! an observation means ([`AuthorityDriver::apply`]).
//!
//! The policy is deliberately two-valued. `Current` keeps the session;
//! `Denied` ends it on that observation. There is no third "cannot tell right
//! now" state: an authority that cannot observe current permission must answer
//! `Denied`, so a session never outlives the check that would revoke it.

use std::time::Duration;

use crate::relay::AuthorizationStatus;

/// How often an admitted session's authority is re-observed.
pub(crate) const AUTHORITY_INTERVAL: Duration = Duration::from_millis(100);

/// What one observation means for an admitted session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub(crate) enum Verdict {
    /// Authority is current: the session continues.
    Continue,
    /// Authority was denied: the session ends now.
    Revoked,
}

/// The re-observation schedule and policy of one admitted session.
pub(crate) struct AuthorityDriver {
    schedule: tokio::time::Interval,
}

impl AuthorityDriver {
    /// Admit a session whose host authority accepted the grant, given its
    /// first observation. Only `Current` admits. Must be called inside Tokio.
    pub(crate) fn admit(first: AuthorizationStatus) -> Option<Self> {
        match first {
            AuthorizationStatus::Current => {}
            AuthorizationStatus::Denied => return None,
        }
        let mut schedule = tokio::time::interval(AUTHORITY_INTERVAL);
        // A stalled scheduler observes once when it resumes, never in a burst.
        schedule.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Some(Self { schedule })
    }

    /// The single policy: `Current` continues, `Denied` revokes.
    pub(crate) const fn apply(observed: AuthorizationStatus) -> Verdict {
        match observed {
            AuthorizationStatus::Current => Verdict::Continue,
            AuthorizationStatus::Denied => Verdict::Revoked,
        }
    }

    /// Wait until the session's authority is due to be observed, then observe
    /// it once with `observe` and apply the policy. The first call observes
    /// at once. Cancel-safe: dropping the future before it resolves never
    /// calls `observe` and loses no schedule.
    ///
    /// This is every carrier's only wait point for re-authorization, so a
    /// change-driven schedule replaces the interval here alone.
    pub(crate) async fn next(&mut self, observe: impl FnOnce() -> AuthorizationStatus) -> Verdict {
        self.schedule.tick().await;
        Self::apply(observe())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn denied_ends_session() {
        assert_eq!(
            AuthorityDriver::apply(AuthorizationStatus::Denied),
            Verdict::Revoked
        );
    }

    #[test]
    fn current_keeps_session() {
        assert_eq!(
            AuthorityDriver::apply(AuthorizationStatus::Current),
            Verdict::Continue
        );
    }

    #[tokio::test]
    async fn only_a_current_first_observation_admits() {
        assert!(AuthorityDriver::admit(AuthorizationStatus::Denied).is_none());
        assert!(AuthorityDriver::admit(AuthorizationStatus::Current).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn observes_at_once_then_once_per_interval_until_denied() {
        let mut driver = AuthorityDriver::admit(AuthorizationStatus::Current).unwrap();
        let observed = Cell::new(0_u32);
        let started = tokio::time::Instant::now();
        let observe = || {
            observed.set(observed.get() + 1);
            AuthorizationStatus::Current
        };
        assert_eq!(driver.next(observe).await, Verdict::Continue);
        assert_eq!(started.elapsed(), Duration::ZERO);
        assert_eq!(driver.next(observe).await, Verdict::Continue);
        assert_eq!(started.elapsed(), AUTHORITY_INTERVAL);
        assert_eq!(observed.get(), 2);
        let verdict = driver.next(|| AuthorizationStatus::Denied).await;
        assert_eq!(verdict, Verdict::Revoked);
        assert_eq!(started.elapsed(), AUTHORITY_INTERVAL * 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_wait_never_observes() {
        let mut driver = AuthorityDriver::admit(AuthorizationStatus::Current).unwrap();
        assert_eq!(
            driver.next(|| AuthorizationStatus::Current).await,
            Verdict::Continue
        );
        let observed = Cell::new(false);
        let cancelled = tokio::time::timeout(
            AUTHORITY_INTERVAL / 2,
            driver.next(|| {
                observed.set(true);
                AuthorizationStatus::Denied
            }),
        )
        .await;
        assert!(cancelled.is_err());
        assert!(!observed.get(), "a cancelled wait must not observe");
    }
}
