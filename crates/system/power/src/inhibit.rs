//! The **inhibitor registry** — the API the shell and apps use to say "do
//! not suspend right now" while something important is happening (media
//! playing, a long task running).
//!
//! This is a pure, ref-counted registry: [`InhibitRegistry::acquire`] hands
//! back an [`InhibitToken`] whose `Drop` releases the hold, so callers never
//! have to remember to release — the lock lives exactly as long as the value
//! that represents the reason. While any token is held [`InhibitRegistry::is_inhibited`]
//! is `true`, and the compositor's idle loop declines to suspend.
//!
//! The registry is deliberately independent of D-Bus so it is trivially
//! testable and usable in headless contexts. The [`super::service::PowerService`]
//! *mirrors* the registry into a real `systemd-logind` block inhibitor (so
//! logind's own idle/suspend handling is held off too) by watching the
//! registry's held-count cross 0↔1 — see [`InhibitRegistry::is_inhibited`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct RegistryInner {
    /// `id -> human-readable reason`. A `BTreeMap` keeps `active_reasons`
    /// deterministic for tests and logs.
    held: BTreeMap<u64, String>,
}

/// A cloneable handle to the shared set of active suspend inhibitors.
///
/// Clone freely — every clone shares the same underlying state (an `Arc`
/// bump), so the compositor, the media player, and a background task can
/// each hold their own handle and their holds compose.
#[derive(Clone, Debug)]
pub struct InhibitRegistry {
    inner: Arc<Mutex<RegistryInner>>,
    next_id: Arc<AtomicU64>,
}

impl Default for InhibitRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InhibitRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryInner::default())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Take a suspend inhibitor for `reason`. The returned token releases the
    /// hold when dropped. Hold it for as long as suspend must be blocked
    /// (e.g. store it in the media player's "is playing" state).
    pub fn acquire(&self, reason: impl Into<String>) -> InhibitToken {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let reason = reason.into();
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.held.insert(id, reason);
        }
        InhibitToken {
            id,
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Whether any inhibitor is currently held — the signal the idle loop
    /// consults before suspending.
    pub fn is_inhibited(&self) -> bool {
        !self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .held
            .is_empty()
    }

    /// How many inhibitors are held.
    pub fn count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .held
            .len()
    }

    /// The reasons currently holding the machine awake, sorted by acquisition
    /// order — surfaced in logs and the real logind inhibitor's `why` field.
    pub fn active_reasons(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .held
            .values()
            .cloned()
            .collect()
    }
}

/// An outstanding suspend inhibitor. Dropping it releases the hold.
///
/// Holds only a `Weak` back-reference to the registry, so a token outliving
/// its registry is harmless (the drop is a no-op) — nothing keeps the
/// registry alive just because a token exists.
#[derive(Debug)]
pub struct InhibitToken {
    id: u64,
    inner: std::sync::Weak<Mutex<RegistryInner>>,
}

impl InhibitToken {
    /// The registry-internal id of this hold (for diagnostics/tests).
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for InhibitToken {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .held
                .remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_is_not_inhibited() {
        let r = InhibitRegistry::new();
        assert!(!r.is_inhibited());
        assert_eq!(r.count(), 0);
        assert!(r.active_reasons().is_empty());
    }

    #[test]
    fn a_held_token_inhibits_and_dropping_releases() {
        let r = InhibitRegistry::new();
        {
            let _t = r.acquire("media playback");
            assert!(r.is_inhibited());
            assert_eq!(r.count(), 1);
            assert_eq!(r.active_reasons(), vec!["media playback".to_string()]);
        }
        // Token dropped at end of scope → released.
        assert!(!r.is_inhibited());
        assert_eq!(r.count(), 0);
    }

    #[test]
    fn overlapping_holds_compose_and_release_independently() {
        let r = InhibitRegistry::new();
        let a = r.acquire("media");
        let b = r.acquire("backup task");
        assert_eq!(r.count(), 2);
        assert!(r.is_inhibited());
        drop(a);
        // Still inhibited: the backup task holds it.
        assert!(r.is_inhibited());
        assert_eq!(r.active_reasons(), vec!["backup task".to_string()]);
        drop(b);
        assert!(!r.is_inhibited());
    }

    #[test]
    fn clones_share_state() {
        let r = InhibitRegistry::new();
        let r2 = r.clone();
        let _t = r.acquire("x");
        // The hold taken on `r` is visible through the clone `r2`.
        assert!(r2.is_inhibited());
        assert_eq!(r2.count(), 1);
    }

    #[test]
    fn token_outliving_registry_is_harmless() {
        let token;
        {
            let r = InhibitRegistry::new();
            token = r.acquire("x");
            assert_eq!(r.count(), 1);
        }
        // Registry dropped; dropping the token must not panic (Weak upgrade
        // fails and the drop is a no-op).
        drop(token);
    }
}
