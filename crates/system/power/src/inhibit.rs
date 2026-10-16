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
//!
//! A hold may also be a **lease**: [`InhibitRegistry::acquire_until`] takes one
//! that stops counting at a deadline unless [`InhibitToken::renew_until`]
//! moves it. A remote admin's hold therefore lapses by itself when the
//! requests stop, with no timer and nobody having to remember to drop it.
//!
//! Whoever mirrors the registry elsewhere waits instead of polling: every
//! acquire, renewal and release calls the [`InhibitRegistry::on_change`]
//! listeners, and [`InhibitRegistry::next_lapse_for`] names the one moment a
//! lease stops counting on its own.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tilo_lock::MutexRecover as _;

#[derive(Debug)]
struct Hold {
    reason: String,
    flags: u32,
    /// The moment a lease stops counting; `None` holds until dropped.
    until: Option<Instant>,
}

impl Hold {
    fn counts(&self, flags: u32, now: Instant) -> bool {
        self.flags & flags != 0 && self.until.is_none_or(|until| now < until)
    }
}

type ChangeListener = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct RegistryInner {
    /// `id -> hold`. A `BTreeMap` keeps `active_reasons` deterministic for
    /// tests and logs.
    held: BTreeMap<u64, Hold>,
    /// Called after every change to `held`, outside the lock.
    listeners: Vec<ChangeListener>,
}

impl std::fmt::Debug for RegistryInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryInner")
            .field("held", &self.held)
            .field("listeners", &self.listeners.len())
            .finish()
    }
}

/// Apply `change` to the holds, then tell every listener — after the lock is
/// released, so a listener may read the registry.
fn change_holds<R>(
    inner: &Mutex<RegistryInner>,
    change: impl FnOnce(&mut BTreeMap<u64, Hold>) -> R,
) -> R {
    let (result, listeners) = {
        let mut inner = inner.lock_recover();
        let result = change(&mut inner.held);
        (result, inner.listeners.clone())
    };
    for listener in &listeners {
        listener();
    }
    result
}

pub const INHIBIT_LOGOUT: u32 = 1;
pub const INHIBIT_USER_SWITCH: u32 = 2;
pub const INHIBIT_SUSPEND: u32 = 4;
pub const INHIBIT_IDLE: u32 = 8;
pub const INHIBIT_POWER: u32 = INHIBIT_SUSPEND | INHIBIT_IDLE;

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
        self.acquire_for(reason, INHIBIT_POWER)
    }

    /// Hold only the named desktop operations. This keeps, for example, a
    /// logout-only portal inhibitor from accidentally preventing idle sleep.
    pub fn acquire_for(&self, reason: impl Into<String>, flags: u32) -> InhibitToken {
        self.insert(reason.into(), flags, None)
    }

    /// Hold the named operations until `until`, a lease: past it the hold
    /// stops counting even while the token lives, unless
    /// [`InhibitToken::renew_until`] moved it.
    pub fn acquire_until(
        &self,
        reason: impl Into<String>,
        flags: u32,
        until: Instant,
    ) -> InhibitToken {
        self.insert(reason.into(), flags, Some(until))
    }

    fn insert(&self, reason: String, flags: u32, until: Option<Instant>) -> InhibitToken {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        change_holds(&self.inner, |held| {
            held.insert(
                id,
                Hold {
                    reason,
                    flags,
                    until,
                },
            );
        });
        InhibitToken {
            id,
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Call `listener` after every acquire, renewal and release, on the
    /// thread that made it. Keep it short and non-blocking (wake a task); it
    /// lives as long as the registry.
    pub fn on_change(&self, listener: impl Fn() + Send + Sync + 'static) {
        self.inner.lock_recover().listeners.push(Arc::new(listener));
    }

    /// The earliest moment after `now` at which a lease holding any of
    /// `flags` stops counting by itself — the only change to
    /// [`Self::is_inhibited_for`] that no acquire, renewal or release
    /// announces. `None` when no such lease is pending.
    pub fn next_lapse_for(&self, flags: u32, now: Instant) -> Option<Instant> {
        self.inner
            .lock_recover()
            .held
            .values()
            .filter(|hold| hold.counts(flags, now))
            .filter_map(|hold| hold.until)
            .min()
    }

    /// Whether any inhibitor is currently held — the signal the idle loop
    /// consults before suspending.
    pub fn is_inhibited(&self) -> bool {
        self.is_inhibited_for(INHIBIT_POWER)
    }

    pub fn is_inhibited_for(&self, flags: u32) -> bool {
        self.is_inhibited_for_at(flags, Instant::now())
    }

    /// [`Self::is_inhibited_for`] as of `now`: a lease past its deadline no
    /// longer counts.
    pub fn is_inhibited_for_at(&self, flags: u32, now: Instant) -> bool {
        self.inner
            .lock_recover()
            .held
            .values()
            .any(|hold| hold.counts(flags, now))
    }

    /// How many inhibitors are held, lapsed leases excluded.
    pub fn count(&self) -> usize {
        let now = Instant::now();
        self.inner
            .lock_recover()
            .held
            .values()
            .filter(|hold| hold.until.is_none_or(|until| now < until))
            .count()
    }

    /// The reasons currently holding the machine awake, sorted by acquisition
    /// order — surfaced in logs and the real logind inhibitor's `why` field.
    pub fn active_reasons(&self) -> Vec<String> {
        self.active_reasons_for(INHIBIT_POWER)
    }

    pub fn active_reasons_for(&self, flags: u32) -> Vec<String> {
        let now = Instant::now();
        self.inner
            .lock_recover()
            .held
            .values()
            .filter(|hold| hold.counts(flags, now))
            .map(|hold| hold.reason.clone())
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

    /// Move this hold's deadline to `until` and name it `reason`, making it
    /// a lease if it was not one. Returns `false` when the registry is gone.
    pub fn renew_until(&self, reason: impl Into<String>, until: Instant) -> bool {
        let Some(inner) = self.inner.upgrade() else {
            return false;
        };
        let reason = reason.into();
        change_holds(&inner, |held| {
            held.get_mut(&self.id)
                .map(|hold| {
                    hold.reason = reason;
                    hold.until = Some(until);
                })
                .is_some()
        })
    }
}

impl Drop for InhibitToken {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            change_holds(&inner, |held| held.remove(&self.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inhibitor_tokens_compose_share_state_and_release_on_drop() {
        // Phase `empty_registry_is_not_inhibited`.
        {
            let r = InhibitRegistry::new();
            assert!(!r.is_inhibited());
            assert_eq!(r.count(), 0);
            assert!(r.active_reasons().is_empty());
        }
        // Phase `a_held_token_inhibits_and_dropping_releases`.
        {
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
        // Phase `overlapping_holds_compose_and_release_independently`.
        {
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
        // Phase `clones_share_state`.
        {
            let r = InhibitRegistry::new();
            let r2 = r.clone();
            let _t = r.acquire("x");
            // The hold taken on `r` is visible through the clone `r2`.
            assert!(r2.is_inhibited());
            assert_eq!(r2.count(), 1);
        }
        // Phase `operation_scopes_do_not_cross_inhibit`.
        {
            let registry = InhibitRegistry::new();
            let _logout = registry.acquire_for("unsaved work", INHIBIT_LOGOUT);
            assert!(registry.is_inhibited_for(INHIBIT_LOGOUT));
            assert!(!registry.is_inhibited_for(INHIBIT_POWER));
            assert!(registry.active_reasons().is_empty());
        }
        // Phase `token_outliving_registry_is_harmless`.
        {
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

    #[test]
    fn every_change_is_announced_and_the_next_lapse_is_named() {
        let registry = InhibitRegistry::new();
        let changes = Arc::new(AtomicU64::new(0));
        registry.on_change({
            let changes = Arc::clone(&changes);
            let registry = registry.clone();
            move || {
                // A listener may read the registry: it runs outside the lock.
                let _ = registry.count();
                changes.fetch_add(1, Ordering::SeqCst);
            }
        });
        let start = Instant::now();
        let minute = std::time::Duration::from_secs(60);
        assert_eq!(registry.next_lapse_for(INHIBIT_POWER, start), None);

        let hold = registry.acquire("media");
        assert_eq!(changes.load(Ordering::SeqCst), 1, "acquire is announced");
        assert_eq!(
            registry.next_lapse_for(INHIBIT_POWER, start),
            None,
            "a plain hold never lapses by itself"
        );
        let lease = registry.acquire_until("remote use", INHIBIT_SUSPEND, start + minute);
        assert_eq!(changes.load(Ordering::SeqCst), 2);
        assert_eq!(
            registry.next_lapse_for(INHIBIT_POWER, start),
            Some(start + minute)
        );
        assert_eq!(
            registry.next_lapse_for(INHIBIT_IDLE, start),
            None,
            "a suspend lease does not lapse an idle hold"
        );
        assert!(lease.renew_until("remote use", start + 2 * minute));
        assert_eq!(changes.load(Ordering::SeqCst), 3, "renewal is announced");
        assert_eq!(
            registry.next_lapse_for(INHIBIT_POWER, start),
            Some(start + 2 * minute)
        );
        assert_eq!(
            registry.next_lapse_for(INHIBIT_POWER, start + 2 * minute),
            None,
            "a lapsed lease has nothing left to announce"
        );
        drop(lease);
        drop(hold);
        assert_eq!(changes.load(Ordering::SeqCst), 5, "releases are announced");
    }

    #[test]
    fn a_lease_counts_until_its_deadline_and_renewal_moves_it() {
        let registry = InhibitRegistry::new();
        let start = Instant::now();
        let minute = std::time::Duration::from_secs(60);
        let lease = registry.acquire_until("remote use", INHIBIT_SUSPEND, start + minute);
        assert!(registry.is_inhibited_for_at(INHIBIT_SUSPEND, start));
        assert!(
            !registry.is_inhibited_for_at(INHIBIT_IDLE, start),
            "a suspend lease leaves the screen to blank and lock"
        );
        assert!(
            !registry.is_inhibited_for_at(INHIBIT_SUSPEND, start + minute),
            "the lease lapses at its deadline though the token lives"
        );
        assert!(lease.renew_until("remote use again", start + 2 * minute));
        assert!(registry.is_inhibited_for_at(INHIBIT_SUSPEND, start + minute));
        assert!(!registry.is_inhibited_for_at(INHIBIT_SUSPEND, start + 2 * minute));
        let _forever = registry.acquire_for("download", INHIBIT_SUSPEND);
        assert!(
            registry.is_inhibited_for_at(INHIBIT_SUSPEND, start + 10 * minute),
            "a plain hold has no deadline"
        );
        drop(lease);
        assert_eq!(registry.count(), 1);
        let gone;
        {
            let other = InhibitRegistry::new();
            gone = other.acquire_until("x", INHIBIT_SUSPEND, start + minute);
        }
        assert!(
            !gone.renew_until("x", start + minute),
            "its registry is gone"
        );
    }
}
