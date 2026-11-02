//! The one session-authority lifecycle shared by every carrier: the relay
//! ([`crate::relay`]).
//!
//! Admission is the host authority accepting the grant *and* a first
//! observation that reports [`AuthorizationStatus::Current`]: only then does an
//! [`AuthorityDriver`] exist for the session. After that the session is
//! observed when the authority's own source reports a change — a membership or
//! roster change, a new channel epoch — and the observation's meaning is fixed:
//! `Current` keeps the session, `Denied` ends it. There is no periodic
//! observation and no "cannot tell right now": an authority that cannot observe
//! current permission answers `Denied`, so a session never outlives the check
//! that would revoke it.

use tokio::sync::watch;

use crate::relay::AuthorizationStatus;

/// What one observation means for an admitted session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub(crate) enum Verdict {
    /// Authority is current: the session continues.
    Continue,
    /// Authority was denied: the session ends now.
    Revoked,
}

/// The observation trigger and policy of one admitted session.
pub(crate) struct AuthorityDriver {
    /// Fires when the authority's source (membership, roster or epoch) has
    /// changed. `None` for an authority that cannot change while the
    /// connection lives.
    changed: Option<watch::Receiver<u64>>,
}

impl AuthorityDriver {
    /// Admit a session whose host authority accepted the grant, given its
    /// first observation and, when it has one, the change signal of the state
    /// that observation read. Only `Current` admits.
    pub(crate) fn admit(
        first: AuthorizationStatus,
        changed: Option<watch::Receiver<u64>>,
    ) -> Option<Self> {
        match first {
            AuthorizationStatus::Current => Some(Self { changed }),
            AuthorizationStatus::Denied => None,
        }
    }

    /// The single policy: `Current` continues, `Denied` revokes.
    pub(crate) const fn apply(observed: AuthorizationStatus) -> Verdict {
        match observed {
            AuthorizationStatus::Current => Verdict::Continue,
            AuthorizationStatus::Denied => Verdict::Revoked,
        }
    }

    /// Wait for the authority's source to report a change, then observe it once
    /// with `observe` and apply the policy. Cancel-safe: dropping the future
    /// before it resolves calls no `observe` and loses no change.
    ///
    /// This is every carrier's only wait point for re-authorization, and it has
    /// no timer: a removal cuts when its event arrives, not up to an interval
    /// later. A session whose authority has no change signal is never observed
    /// again; a signal whose sender is gone revokes, because the state it stood
    /// for can no longer be observed.
    pub(crate) async fn next(&mut self, observe: impl FnOnce() -> AuthorizationStatus) -> Verdict {
        let Some(changed) = &mut self.changed else {
            return std::future::pending().await;
        };
        if changed.changed().await.is_err() {
            return Verdict::Revoked;
        }
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

    #[test]
    fn only_a_current_first_observation_admits() {
        assert!(AuthorityDriver::admit(AuthorizationStatus::Denied, None).is_none());
        assert!(AuthorityDriver::admit(AuthorizationStatus::Current, None).is_some());
    }

    #[tokio::test]
    async fn a_change_observes_once() {
        let (sender, receiver) = watch::channel(0_u64);
        let mut driver =
            AuthorityDriver::admit(AuthorizationStatus::Current, Some(receiver)).unwrap();
        let observed = Cell::new(0_u32);
        let wait = driver.next(|| {
            observed.set(observed.get() + 1);
            AuthorizationStatus::Current
        });
        tokio::pin!(wait);
        // Nothing happens until the source reports a change.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut wait)
                .await
                .is_err()
        );
        assert_eq!(observed.get(), 0, "no observation without a change");
        sender.send_modify(|generation| *generation += 1);
        assert_eq!(wait.await, Verdict::Continue);
        assert_eq!(observed.get(), 1);
    }

    #[tokio::test]
    async fn a_change_that_denies_revokes() {
        let (sender, receiver) = watch::channel(0_u64);
        let mut driver =
            AuthorityDriver::admit(AuthorizationStatus::Current, Some(receiver)).unwrap();
        sender.send_modify(|generation| *generation += 1);
        let verdict = driver.next(|| AuthorizationStatus::Denied).await;
        assert_eq!(verdict, Verdict::Revoked);
    }

    #[tokio::test]
    async fn a_cancelled_wait_never_observes() {
        let (sender, receiver) = watch::channel(0_u64);
        let mut driver =
            AuthorityDriver::admit(AuthorizationStatus::Current, Some(receiver)).unwrap();
        let observed = Cell::new(false);
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            driver.next(|| {
                observed.set(true);
                AuthorizationStatus::Denied
            }),
        )
        .await;
        assert!(cancelled.is_err());
        assert!(!observed.get(), "a cancelled wait must not observe");
        sender.send_modify(|generation| *generation += 1);
    }

    #[tokio::test]
    async fn a_gone_source_revokes() {
        let (sender, receiver) = watch::channel(0_u64);
        let mut driver =
            AuthorityDriver::admit(AuthorizationStatus::Current, Some(receiver)).unwrap();
        drop(sender);
        assert_eq!(
            driver.next(|| AuthorizationStatus::Current).await,
            Verdict::Revoked
        );
    }
}
