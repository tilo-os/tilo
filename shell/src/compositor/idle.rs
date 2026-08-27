//! Idle detection + DPMS blanking + session-lock state machine.
//!
//! This module is the **pure-logic** core of the compositor's
//! power-management and lock behaviour. It owns no hardware and performs
//! no I/O: it only tracks "time since the last input event" and the
//! `blanked` / `locked` booleans, and emits [`SessionTransition`]s that
//! the calloop tick in [`crate::compositor::native::run`] turns into real
//! effects (`Backend::set_dpms`, painting the lock overlay, swallowing
//! input).
//!
//! Keeping the decision logic here — independent of `winit`, `smithay`,
//! and DRM — means the state machine is exhaustively unit-testable
//! without a seat, a GPU, or a display. The hardware glue
//! (`DrmScanout::set_dpms`) is reasoned-correct and gated behind the DRM
//! backend so the winit / headless backends are unaffected.
//!
//! # Model
//!
//! Two orthogonal booleans:
//!
//! * `blanked` — the display(s) are powered down (DPMS off / scanout
//!   stopped). Any input wakes them back up.
//! * `locked` — the session is locked: app content is hidden behind a
//!   compositor-drawn overlay and input is captured until an explicit
//!   unlock action arrives.
//!
//! These are independent: an idle session can blank *and* lock (idle
//! while locked still blanks to save power; waking from blank while
//! locked re-shows the lock overlay, it does **not** unlock).
//!
//! # Protocol trigger + compositor-owned overlay
//!
//! `ext-session-lock-v1` is a *client*-driven protocol: a client binds the
//! global and supplies the lock surface(s) the compositor shows. The
//! compositor serves that global (see
//! [`crate::compositor::state::SessionLockHooks`]) so a conforming lock
//! client — or `loginctl lock-session` routed through one — can drive
//! the session lock through the standard protocol. But qOS renders its
//! *own* lock UI as chrome via [`crate::compositor::lock_screen`] rather
//! than compositing a client lock surface, so a protocol `lock` request
//! just folds into the same authoritative locked state this machine
//! tracks. Authentication is the missing piece the overlay adds: unlock
//! now requires a verified password (see
//! [`crate::compositor::lock_controller`]), not merely a keypress.

use std::time::{Duration, Instant};

/// Default idle duration before the display blanks (5 minutes).
pub const DEFAULT_BLANK_SECS: u64 = 300;

/// Env var overriding the idle-to-blank timeout, in seconds. `0`
/// disables blanking. Unset → [`DEFAULT_BLANK_SECS`].
pub const BLANK_SECS_ENV: &str = "QOS_IDLE_BLANK_SECS";

/// Env var enabling idle-to-lock, in seconds. Unset (or `0`) → no
/// automatic lock (the session can still be locked by the Super+L chord
/// on keyboard-capable backends).
pub const LOCK_SECS_ENV: &str = "QOS_IDLE_LOCK_SECS";

/// Env var enabling idle-to-suspend, in seconds. Unset (or `0`) → no
/// automatic suspend from this env source. In production the timeout comes
/// from the Settings power policy ([`IdleConfig::with_suspend`]); this env
/// var is the headless / testing override.
pub const SUSPEND_SECS_ENV: &str = "QOS_IDLE_SUSPEND_SECS";

/// An effect the calloop tick must apply after calling into the
/// [`SessionManager`]. Pure value — the manager never touches hardware
/// itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTransition {
    /// The display(s) should be powered off (DPMS off / stop scanout).
    Blanked,
    /// The display(s) should be powered back on.
    Woke,
    /// The session became locked — paint the lock overlay, capture input.
    Locked,
    /// The session unlocked — resume normal presentation + input.
    Unlocked,
    /// The session has been idle past the suspend threshold — the machine
    /// should suspend (`logind.Suspend`). Unlike blank/lock this leaves the
    /// compositor untouched; the actuator (the power service) and the
    /// inhibitor gate live outside this pure state machine. See
    /// [`SessionManager::cancel_suspend`] for the inhibited case.
    Suspend,
}

/// Timeout configuration for the idle/lock state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleConfig {
    /// Idle duration before the display blanks. `None` disables blanking.
    pub blank_timeout: Option<Duration>,
    /// Idle duration before the session auto-locks. `None` disables
    /// idle-to-lock (manual lock via chord/command still works).
    pub lock_timeout: Option<Duration>,
    /// Idle duration before the session suspends. `None` disables
    /// idle-to-suspend. Sourced from the Settings power policy in
    /// production ([`Self::with_suspend`]); the [`SUSPEND_SECS_ENV`] env var
    /// is the headless override.
    pub suspend_timeout: Option<Duration>,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            blank_timeout: Some(Duration::from_secs(DEFAULT_BLANK_SECS)),
            lock_timeout: None,
            suspend_timeout: None,
        }
    }
}

impl IdleConfig {
    /// Resolve the config from the process environment
    /// ([`BLANK_SECS_ENV`] / [`LOCK_SECS_ENV`] / [`SUSPEND_SECS_ENV`]).
    pub fn from_env() -> Self {
        Self::from_env_values(
            std::env::var(BLANK_SECS_ENV).ok().as_deref(),
            std::env::var(LOCK_SECS_ENV).ok().as_deref(),
            std::env::var(SUSPEND_SECS_ENV).ok().as_deref(),
        )
    }

    /// Override the suspend timeout (the Settings power policy drives this in
    /// production; env/`from_env` only seeds the headless default). Chainable.
    pub fn with_suspend(mut self, suspend: Option<Duration>) -> Self {
        self.suspend_timeout = suspend;
        self
    }

    /// Pure helper for [`Self::from_env`] so tests can drive the parser
    /// with fixture strings without poking the real environment.
    ///
    /// * `blank`: unset → [`DEFAULT_BLANK_SECS`]; `"0"` → disabled
    ///   (`None`); a valid positive integer → that many seconds; garbage
    ///   → the default (never refuse to start over a typo).
    /// * `lock`: unset → disabled (`None`); `"0"` → disabled; a valid
    ///   positive integer → that many seconds; garbage → disabled.
    /// * `suspend`: same rules as `lock` (unset/`"0"`/garbage → disabled).
    pub fn from_env_values(blank: Option<&str>, lock: Option<&str>, suspend: Option<&str>) -> Self {
        let blank_timeout = match blank {
            None => Some(Duration::from_secs(DEFAULT_BLANK_SECS)),
            Some(s) => match s.trim().parse::<u64>() {
                Ok(0) => None,
                Ok(n) => Some(Duration::from_secs(n)),
                Err(_) => Some(Duration::from_secs(DEFAULT_BLANK_SECS)),
            },
        };
        // Both lock and suspend are "off unless explicitly set to a positive
        // number" — the same opt-in parse.
        let opt_in = |v: Option<&str>| -> Option<Duration> {
            match v {
                None => None,
                Some(s) => match s.trim().parse::<u64>() {
                    Ok(0) => None,
                    Ok(n) => Some(Duration::from_secs(n)),
                    Err(_) => None,
                },
            }
        };
        Self {
            blank_timeout,
            lock_timeout: opt_in(lock),
            suspend_timeout: opt_in(suspend),
        }
    }
}

/// The compositor's idle + session-lock state machine.
///
/// All time enters through `now: Instant` parameters so tests can drive
/// the machine deterministically (no sleeping, no wall-clock reads).
#[derive(Debug)]
pub struct SessionManager {
    config: IdleConfig,
    last_activity: Instant,
    blanked: bool,
    locked: bool,
    /// Latched once the idle-suspend threshold fires, so `tick` emits
    /// [`SessionTransition::Suspend`] at most once per idle episode. Cleared
    /// by [`Self::note_activity`] (input after resume) and by
    /// [`Self::cancel_suspend`] (a suspend request the inhibitor gate vetoed).
    suspended: bool,
}

impl SessionManager {
    /// Construct a fresh, active (un-blanked, un-locked) manager whose
    /// idle timer starts at `now`.
    pub fn new(config: IdleConfig, now: Instant) -> Self {
        Self {
            config,
            last_activity: now,
            blanked: false,
            locked: false,
            suspended: false,
        }
    }

    /// Whether the display is currently blanked (DPMS off).
    pub fn is_blanked(&self) -> bool {
        self.blanked
    }

    /// Whether the session is currently locked.
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// How long the session has been idle as of `now`.
    pub fn idle_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_activity)
    }

    /// Record an input event at `now`.
    ///
    /// Resets the idle timer and, if the display was blanked, wakes it
    /// (emitting [`SessionTransition::Woke`]). This does **not** unlock a
    /// locked session — input while locked wakes the display so the user
    /// sees the lock overlay, but the session stays locked until an
    /// explicit unlock action ([`Self::request_unlock`]).
    pub fn note_activity(&mut self, now: Instant) -> Vec<SessionTransition> {
        self.last_activity = now;
        // Input after a resume clears the suspend latch so a later idle
        // episode can suspend again. (No transition — resume itself is
        // signalled out-of-band by logind's `PrepareForSleep`.)
        self.suspended = false;
        let mut out = Vec::new();
        if self.blanked {
            self.blanked = false;
            out.push(SessionTransition::Woke);
        }
        out
    }

    /// Advance the timers to `now` and emit any timeout-driven
    /// transitions (blank, auto-lock). Idempotent once a threshold has
    /// fired: a blanked display stays blanked, a locked session stays
    /// locked, until the relevant boolean is cleared.
    pub fn tick(&mut self, now: Instant) -> Vec<SessionTransition> {
        let idle = self.idle_for(now);
        let mut out = Vec::new();
        if !self.blanked {
            if let Some(t) = self.config.blank_timeout {
                if idle >= t {
                    self.blanked = true;
                    out.push(SessionTransition::Blanked);
                }
            }
        }
        if !self.locked {
            if let Some(t) = self.config.lock_timeout {
                if idle >= t {
                    self.locked = true;
                    out.push(SessionTransition::Locked);
                }
            }
        }
        if !self.suspended {
            if let Some(t) = self.config.suspend_timeout {
                if idle >= t {
                    self.suspended = true;
                    out.push(SessionTransition::Suspend);
                }
            }
        }
        out
    }

    /// Whether the idle-suspend latch is currently set.
    pub fn is_suspend_pending(&self) -> bool {
        self.suspended
    }

    /// Un-latch a pending suspend the caller decided **not** to act on
    /// (typically because a [`crate` inhibitor] is held). Leaves the idle
    /// timer untouched, so `tick` re-emits [`SessionTransition::Suspend`] on
    /// the next tick — the machine suspends as soon as the inhibitor releases,
    /// without waiting for fresh activity. No-op if no suspend was latched.
    pub fn cancel_suspend(&mut self) {
        self.suspended = false;
    }

    /// Manually lock the session (Super+L chord / command). Resets the
    /// idle timer to `now` so the blank countdown restarts from the lock
    /// moment. No-op (empty transitions) if already locked.
    pub fn request_lock(&mut self, now: Instant) -> Vec<SessionTransition> {
        self.last_activity = now;
        if self.locked {
            return Vec::new();
        }
        self.locked = true;
        vec![SessionTransition::Locked]
    }

    /// Apply the unlock transition. Clears the lock, wakes the display if it
    /// was blanked, and resets the idle timer. No-op (empty transitions) if
    /// not locked.
    ///
    /// This is the *post-authentication* transition: the caller only reaches
    /// it once a credential has been verified — either the compositor's own
    /// lock screen accepting a password
    /// ([`crate::compositor::lock_controller::LockController::submit`]) or an
    /// `ext-session-lock-v1` client tearing down its lock after authenticating
    /// on its side. This machine stays credential-agnostic; the gate lives one
    /// layer up so it can be driven with an injected verifier in tests.
    pub fn request_unlock(&mut self, now: Instant) -> Vec<SessionTransition> {
        self.last_activity = now;
        if !self.locked {
            return Vec::new();
        }
        let mut out = Vec::new();
        if self.blanked {
            self.blanked = false;
            out.push(SessionTransition::Woke);
        }
        self.locked = false;
        out.push(SessionTransition::Unlocked);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(blank: Option<u64>, lock: Option<u64>) -> IdleConfig {
        IdleConfig {
            blank_timeout: blank.map(Duration::from_secs),
            lock_timeout: lock.map(Duration::from_secs),
            suspend_timeout: None,
        }
    }

    /// Config helper that also sets the suspend timeout.
    fn cfg_s(blank: Option<u64>, lock: Option<u64>, suspend: Option<u64>) -> IdleConfig {
        IdleConfig {
            blank_timeout: blank.map(Duration::from_secs),
            lock_timeout: lock.map(Duration::from_secs),
            suspend_timeout: suspend.map(Duration::from_secs),
        }
    }

    // ---- IdleConfig::from_env_values ---------------------------------

    #[test]
    fn config_defaults_blank_five_minutes_no_lock() {
        let c = IdleConfig::from_env_values(None, None, None);
        assert_eq!(c.blank_timeout, Some(Duration::from_secs(300)));
        assert_eq!(c.lock_timeout, None);
        assert_eq!(c.suspend_timeout, None);
    }

    #[test]
    fn config_zero_disables() {
        let c = IdleConfig::from_env_values(Some("0"), Some("0"), Some("0"));
        assert_eq!(c.blank_timeout, None);
        assert_eq!(c.lock_timeout, None);
        assert_eq!(c.suspend_timeout, None);
    }

    #[test]
    fn config_parses_positive_seconds() {
        let c = IdleConfig::from_env_values(Some("60"), Some("120"), Some("900"));
        assert_eq!(c.blank_timeout, Some(Duration::from_secs(60)));
        assert_eq!(c.lock_timeout, Some(Duration::from_secs(120)));
        assert_eq!(c.suspend_timeout, Some(Duration::from_secs(900)));
    }

    #[test]
    fn config_garbage_blank_falls_back_to_default_garbage_lock_disables() {
        let c = IdleConfig::from_env_values(Some("nonsense"), Some("nonsense"), Some("nonsense"));
        assert_eq!(c.blank_timeout, Some(Duration::from_secs(300)));
        assert_eq!(c.lock_timeout, None);
        assert_eq!(c.suspend_timeout, None);
    }

    #[test]
    fn with_suspend_overrides_the_timeout() {
        // The Settings power policy drives suspend, overriding env.
        let c = IdleConfig::from_env_values(Some("300"), None, None)
            .with_suspend(Some(Duration::from_secs(1200)));
        assert_eq!(c.suspend_timeout, Some(Duration::from_secs(1200)));
        assert_eq!(c.blank_timeout, Some(Duration::from_secs(300)));
    }

    // ---- blank / wake cycle ------------------------------------------

    #[test]
    fn idle_past_threshold_blanks_then_input_wakes() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(Some(300), None), t0);
        assert!(!s.is_blanked());

        // Just under the threshold: no transition.
        assert!(s.tick(t0 + Duration::from_secs(299)).is_empty());
        assert!(!s.is_blanked());

        // At/after the threshold: blanks once.
        let trans = s.tick(t0 + Duration::from_secs(300));
        assert_eq!(trans, vec![SessionTransition::Blanked]);
        assert!(s.is_blanked());

        // Subsequent ticks while blanked emit nothing (idempotent).
        assert!(s.tick(t0 + Duration::from_secs(600)).is_empty());
        assert!(s.is_blanked());

        // Any input wakes the display and resets the timer.
        let trans = s.note_activity(t0 + Duration::from_secs(601));
        assert_eq!(trans, vec![SessionTransition::Woke]);
        assert!(!s.is_blanked());
        assert_eq!(s.idle_for(t0 + Duration::from_secs(601)), Duration::ZERO);
    }

    #[test]
    fn blank_disabled_never_blanks() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(None, None), t0);
        assert!(s.tick(t0 + Duration::from_secs(100_000)).is_empty());
        assert!(!s.is_blanked());
    }

    #[test]
    fn note_activity_without_blank_emits_nothing() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(Some(300), None), t0);
        assert!(s.note_activity(t0 + Duration::from_secs(1)).is_empty());
        assert!(!s.is_blanked());
    }

    // ---- auto-lock ---------------------------------------------------

    #[test]
    fn idle_past_lock_threshold_locks() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(Some(300), Some(600)), t0);

        // Blank fires first.
        let trans = s.tick(t0 + Duration::from_secs(300));
        assert_eq!(trans, vec![SessionTransition::Blanked]);
        assert!(!s.is_locked());

        // Lock fires at its own threshold.
        let trans = s.tick(t0 + Duration::from_secs(600));
        assert_eq!(trans, vec![SessionTransition::Locked]);
        assert!(s.is_locked());
    }

    #[test]
    fn equal_timeouts_emit_both_blank_and_lock_same_tick() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(Some(300), Some(300)), t0);
        let trans = s.tick(t0 + Duration::from_secs(300));
        assert_eq!(
            trans,
            vec![SessionTransition::Blanked, SessionTransition::Locked]
        );
        assert!(s.is_blanked());
        assert!(s.is_locked());
    }

    // ---- idle-to-suspend ---------------------------------------------

    #[test]
    fn idle_past_suspend_threshold_emits_suspend_once() {
        let t0 = Instant::now();
        // Blank at 300, suspend at 900, no lock.
        let mut s = SessionManager::new(cfg_s(Some(300), None, Some(900)), t0);

        // Blank fires first, no suspend yet.
        assert_eq!(
            s.tick(t0 + Duration::from_secs(300)),
            vec![SessionTransition::Blanked]
        );
        assert!(!s.is_suspend_pending());

        // Just under the suspend threshold: nothing new.
        assert!(s.tick(t0 + Duration::from_secs(899)).is_empty());
        assert!(!s.is_suspend_pending());

        // At the threshold: suspend once.
        assert_eq!(
            s.tick(t0 + Duration::from_secs(900)),
            vec![SessionTransition::Suspend]
        );
        assert!(s.is_suspend_pending());

        // Latched: subsequent ticks emit nothing.
        assert!(s.tick(t0 + Duration::from_secs(1200)).is_empty());
    }

    #[test]
    fn suspend_disabled_never_suspends() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg_s(Some(300), None, None), t0);
        // Way past any threshold, but suspend is disabled.
        let trans = s.tick(t0 + Duration::from_secs(100_000));
        assert_eq!(trans, vec![SessionTransition::Blanked]);
        assert!(!s.is_suspend_pending());
    }

    #[test]
    fn activity_after_suspend_rearms_for_the_next_episode() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg_s(None, None, Some(600)), t0);
        assert_eq!(
            s.tick(t0 + Duration::from_secs(600)),
            vec![SessionTransition::Suspend]
        );
        assert!(s.is_suspend_pending());
        // Resume: input arrives, clears the latch and resets the timer.
        let _ = s.note_activity(t0 + Duration::from_secs(605));
        assert!(!s.is_suspend_pending());
        // Idle again for the full period → suspends afresh.
        assert!(s.tick(t0 + Duration::from_secs(1200)).is_empty());
        assert_eq!(
            s.tick(t0 + Duration::from_secs(1205)),
            vec![SessionTransition::Suspend]
        );
    }

    #[test]
    fn cancel_suspend_reemits_next_tick_without_resetting_timer() {
        // The inhibited case: a suspend was emitted, the caller vetoed it
        // (inhibitor held), so it un-latches — and re-emits on the very next
        // tick, so as soon as the inhibitor releases the machine suspends,
        // without waiting for fresh activity.
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg_s(None, None, Some(600)), t0);
        assert_eq!(
            s.tick(t0 + Duration::from_secs(600)),
            vec![SessionTransition::Suspend]
        );
        // Caller decided not to suspend (inhibited).
        s.cancel_suspend();
        assert!(!s.is_suspend_pending());
        // Still idle → re-emits immediately (timer was NOT reset).
        assert_eq!(
            s.tick(t0 + Duration::from_secs(601)),
            vec![SessionTransition::Suspend]
        );
    }

    // ---- manual lock / unlock ----------------------------------------

    #[test]
    fn request_lock_locks_and_is_idempotent() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(Some(300), None), t0);
        let trans = s.request_lock(t0 + Duration::from_secs(5));
        assert_eq!(trans, vec![SessionTransition::Locked]);
        assert!(s.is_locked());
        // Locking again is a no-op.
        assert!(s.request_lock(t0 + Duration::from_secs(6)).is_empty());
    }

    #[test]
    fn request_unlock_clears_lock_and_wakes_if_blanked() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(Some(300), Some(300)), t0);
        // Drive into locked + blanked.
        let _ = s.tick(t0 + Duration::from_secs(300));
        assert!(s.is_locked() && s.is_blanked());

        let trans = s.request_unlock(t0 + Duration::from_secs(305));
        // Wake comes before Unlocked so the display is on before the
        // chrome resumes painting.
        assert_eq!(
            trans,
            vec![SessionTransition::Woke, SessionTransition::Unlocked]
        );
        assert!(!s.is_locked());
        assert!(!s.is_blanked());
    }

    #[test]
    fn request_unlock_when_unlocked_is_noop() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(Some(300), None), t0);
        assert!(s.request_unlock(t0).is_empty());
    }

    #[test]
    fn input_while_locked_wakes_but_does_not_unlock() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(Some(300), None), t0);
        let _ = s.request_lock(t0);
        // Idle past blank while locked → still blanks.
        let trans = s.tick(t0 + Duration::from_secs(300));
        assert_eq!(trans, vec![SessionTransition::Blanked]);
        assert!(s.is_locked() && s.is_blanked());

        // Input wakes the display (to show the lock overlay) but keeps
        // the session locked.
        let trans = s.note_activity(t0 + Duration::from_secs(305));
        assert_eq!(trans, vec![SessionTransition::Woke]);
        assert!(s.is_locked());
        assert!(!s.is_blanked());
    }

    #[test]
    fn locked_session_can_still_blank_after_wake() {
        // Lock, blank, wake, then go idle again — must re-blank.
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(Some(300), None), t0);
        let _ = s.request_lock(t0);
        let _ = s.tick(t0 + Duration::from_secs(300)); // blanked
        let _ = s.note_activity(t0 + Duration::from_secs(301)); // woke, timer reset
        // Idle 300s from the wake → blanks again.
        let trans = s.tick(t0 + Duration::from_secs(601));
        assert_eq!(trans, vec![SessionTransition::Blanked]);
        assert!(s.is_blanked());
        assert!(s.is_locked());
    }
}
