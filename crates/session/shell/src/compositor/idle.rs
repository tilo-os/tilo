//! Idle detection + DPMS blanking + session-lock state machine.
//!
//! **Pure-logic** core of the compositor's power-management and lock behaviour: owns no
//! hardware, does no I/O — tracks "time since last input" and the `blanked`/`locked` booleans,
//! and emits [`SessionTransition`]s that the calloop tick in [`crate::compositor::native::run`]
//! turns into real effects (`Backend::set_dpms`, painting the lock overlay, swallowing input).
//! Independent of `winit`/`smithay`/DRM, so exhaustively unit-testable without a seat, GPU, or display.
//!
//! Shortly before the display blanks it dims ([`SessionTransition::Dimmed`]), so someone reading
//! without touching anything gets a warning and a single input to keep the screen on
//! ([`SessionTransition::Undimmed`]) instead of a sudden black screen.
//!
//! Two orthogonal booleans: `blanked` (displays powered down; any input wakes them) and `locked`
//! (session locked — content hidden behind a compositor overlay, input captured until an
//! explicit unlock). Independent: an idle locked session still blanks to save power, and waking
//! from blank while locked re-shows the overlay rather than unlocking.
//!
//! `ext-session-lock-v1` is client-driven (a client supplies the lock surface(s)); Tilo instead
//! renders its own lock UI as chrome ([`crate::compositor::lock_screen`]), so a protocol `lock`
//! request just folds into the same locked state this machine tracks. Unlock requires a verified password ([`crate::compositor::lock_controller`]), not merely a keypress.

use std::time::{Duration, Instant};

/// Default idle duration before the display blanks (5 minutes).
pub const DEFAULT_BLANK_SECS: u64 = 300;

/// How long before the blank the display dims. Short blank timeouts dim for at most half of the
/// idle period, so a display that blanks after a few seconds does not dim the instant it goes idle.
pub const DIM_BEFORE_BLANK: Duration = Duration::from_secs(15);

/// Default idle duration before the session locks (5 minutes).
pub const DEFAULT_LOCK_SECS: u64 = 300;

/// Env var seeding the idle-to-blank timeout, in seconds; `0` disables
/// blanking. Unset → [`DEFAULT_BLANK_SECS`]. The Settings power policy
/// replaces this seed in production via [`IdleConfig::with_blank`].
pub const BLANK_SECS_ENV: &str = "TILO_IDLE_BLANK_SECS";

/// Env var overriding idle-to-lock, in seconds. Unset → [`DEFAULT_LOCK_SECS`];
/// `0` explicitly disables automatic locking (Super+L still works).
pub const LOCK_SECS_ENV: &str = "TILO_IDLE_LOCK_SECS";

/// Env var enabling idle-to-suspend, in seconds. Unset (or `0`) → no automatic suspend from this
/// source; production sources the timeout from Settings ([`IdleConfig::with_suspend`]) — this env var is the headless/testing override.
pub const SUSPEND_SECS_ENV: &str = "TILO_IDLE_SUSPEND_SECS";

/// An effect the calloop tick must apply after calling into the [`SessionManager`]; a pure value — the manager never touches hardware itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTransition {
    /// The display is about to blank — lower it (backlight, or a dimming overlay where there is
    /// no backlight) so the user notices and can keep it on with any input.
    Dimmed,
    /// Restore the display's brightness after [`Self::Dimmed`].
    Undimmed,
    /// The display(s) should be powered off (DPMS off / stop scanout).
    Blanked,
    /// The display(s) should be powered back on.
    Woke,
    /// The session became locked — paint the lock overlay, capture input.
    Locked,
    /// The session unlocked — resume normal presentation + input.
    Unlocked,
    /// The session has been idle past the suspend threshold — the machine should suspend
    /// (`logind.Suspend`). Unlike blank/lock this leaves the compositor untouched; the actuator and inhibitor gate live outside this pure state machine. See [`SessionManager::cancel_suspend`].
    Suspend,
}

/// Timeout configuration for the idle/lock state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleConfig {
    /// Idle duration before the display blanks. `None` disables blanking.
    pub blank_timeout: Option<Duration>,
    /// Idle duration before the session auto-locks. `None` explicitly disables idle-to-lock.
    pub lock_timeout: Option<Duration>,
    /// Idle duration before the session suspends. `None` disables idle-to-suspend. Sourced from
    /// the Settings power policy in production ([`Self::with_suspend`]); [`SUSPEND_SECS_ENV`] is the headless override.
    pub suspend_timeout: Option<Duration>,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            blank_timeout: Some(Duration::from_secs(DEFAULT_BLANK_SECS)),
            lock_timeout: Some(Duration::from_secs(DEFAULT_LOCK_SECS)),
            suspend_timeout: None,
        }
    }
}

impl IdleConfig {
    /// Resolve the config from the process environment ([`BLANK_SECS_ENV`] / [`LOCK_SECS_ENV`] / [`SUSPEND_SECS_ENV`]).
    pub fn from_env() -> Self {
        Self::from_env_values(
            std::env::var(BLANK_SECS_ENV).ok().as_deref(),
            std::env::var(LOCK_SECS_ENV).ok().as_deref(),
            std::env::var(SUSPEND_SECS_ENV).ok().as_deref(),
        )
    }

    /// Override the suspend timeout (the Settings power policy drives this in production; env/`from_env` only seeds the headless default). Chainable.
    #[must_use]
    pub fn with_suspend(mut self, suspend: Option<Duration>) -> Self {
        self.suspend_timeout = suspend;
        self
    }

    /// Override the display-blank timeout from the Settings power policy.
    /// `None` is the persisted "Never" choice. Like [`Self::with_suspend`],
    /// this replaces the environment-derived seed in production.
    #[must_use]
    pub fn with_blank(mut self, blank: Option<Duration>) -> Self {
        self.blank_timeout = blank;
        self
    }

    /// Override the automatic-lock timeout from Settings. `None` is the
    /// persisted "Never" choice; explicit locking remains available.
    #[must_use]
    pub fn with_lock(mut self, lock: Option<Duration>) -> Self {
        self.lock_timeout = lock;
        self
    }

    /// Pure helper for [`Self::from_env`] so tests can drive the parser with fixture strings without poking the real environment.
    ///
    /// `blank`/`lock`: unset or garbage → their secure defaults; `"0"` → explicitly disabled;
    /// a valid positive integer → that many seconds. `suspend` remains opt-in: unset, `"0"`, or
    /// garbage disables it.
    pub fn from_env_values(blank: Option<&str>, lock: Option<&str>, suspend: Option<&str>) -> Self {
        let blank_timeout = match blank {
            None => Some(Duration::from_secs(DEFAULT_BLANK_SECS)),
            Some(s) => match s.trim().parse::<u64>() {
                Ok(0) => None,
                Ok(n) => Some(Duration::from_secs(n)),
                Err(_) => Some(Duration::from_secs(DEFAULT_BLANK_SECS)),
            },
        };
        let lock_timeout = match lock {
            None => Some(Duration::from_secs(DEFAULT_LOCK_SECS)),
            Some(s) => match s.trim().parse::<u64>() {
                Ok(0) => None,
                Ok(n) => Some(Duration::from_secs(n)),
                Err(_) => Some(Duration::from_secs(DEFAULT_LOCK_SECS)),
            },
        };
        let opt_in = |v: Option<&str>| -> Option<Duration> {
            match v {
                None => None,
                Some(s) => match s.trim().parse::<u64>() {
                    Ok(0) | Err(_) => None,
                    Ok(n) => Some(Duration::from_secs(n)),
                },
            }
        };
        Self {
            blank_timeout,
            lock_timeout,
            suspend_timeout: opt_in(suspend),
        }
    }
}

/// What the panel is showing, as far as idle power management is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel {
    /// Full brightness.
    On,
    /// Dimmed ahead of blanking.
    Dimmed,
    /// Powered down. `dimmed` survives the blank so waking restores the brightness the user
    /// had, not the dimmed level.
    Off { dimmed: bool },
}

/// The compositor's idle + session-lock state machine. All time enters through `now: Instant` parameters so tests can drive it deterministically (no sleeping, no wall-clock reads).
#[derive(Debug)]
pub struct SessionManager {
    config: IdleConfig,
    last_activity: Instant,
    /// Explicit panel wake is not input: only the display timeout restarts.
    last_display_wake: Option<Instant>,
    panel: Panel,
    locked: bool,
    /// Latched once the idle-suspend threshold fires, so `tick` emits [`SessionTransition::Suspend`]
    /// at most once per idle episode. Cleared by [`Self::note_activity`] (input after resume) and [`Self::cancel_suspend`] (a vetoed suspend request).
    suspended: bool,
}

impl SessionManager {
    /// Construct a fresh, active (un-blanked, un-locked) manager whose idle timer starts at `now`.
    pub fn new(config: IdleConfig, now: Instant) -> Self {
        Self {
            config,
            last_activity: now,
            last_display_wake: None,
            panel: Panel::On,
            locked: false,
            suspended: false,
        }
    }

    /// Whether the display is currently blanked (DPMS off).
    pub fn is_blanked(&self) -> bool {
        matches!(self.panel, Panel::Off { .. })
    }

    /// Whether the display is dimmed ahead of blanking (and not yet restored by input).
    pub fn is_dimmed(&self) -> bool {
        matches!(self.panel, Panel::Dimmed | Panel::Off { dimmed: true })
    }

    /// Idle duration at which the display dims, if blanking is enabled.
    fn dim_timeout(&self) -> Option<Duration> {
        let blank = self.config.blank_timeout?;
        let half = blank.checked_div(2).unwrap_or_default();
        Some(blank.saturating_sub(DIM_BEFORE_BLANK.min(half)))
    }

    /// Clear the dim, reporting [`SessionTransition::Undimmed`] if the display was dimmed.
    fn undim(&mut self, out: &mut Vec<SessionTransition>) {
        let restored = match self.panel {
            Panel::Dimmed => Panel::On,
            Panel::Off { dimmed: true } => Panel::Off { dimmed: false },
            Panel::On | Panel::Off { dimmed: false } => return,
        };
        self.panel = restored;
        out.push(SessionTransition::Undimmed);
    }

    /// Power the panel back on, reporting [`SessionTransition::Woke`] if it was blanked.
    fn wake(&mut self, out: &mut Vec<SessionTransition>) {
        if self.is_blanked() {
            self.panel = Panel::On;
            out.push(SessionTransition::Woke);
        }
    }

    /// Whether the session is currently locked.
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// How long the session has been idle as of `now`.
    pub fn idle_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_activity)
    }

    /// Record an input event at `now`: resets the idle timer, restores a dimmed display
    /// ([`SessionTransition::Undimmed`]) and, if the display was blanked, wakes it
    /// ([`SessionTransition::Woke`]). Brightness comes back before the panel powers on, so waking
    /// never flashes the dimmed level. Does **not** unlock a locked session — input while locked wakes the display but the session stays locked until [`Self::request_unlock`].
    pub fn note_activity(&mut self, now: Instant) -> Vec<SessionTransition> {
        self.last_activity = now;
        // Input after a resume clears the suspend latch so a later idle episode can suspend again (no transition — resume is signalled out-of-band by logind's `PrepareForSleep`).
        self.suspended = false;
        let mut out = Vec::new();
        self.undim(&mut out);
        self.wake(&mut out);
        out
    }

    /// Wake the display without input, authentication, or a new idle-lock episode.
    /// The existing lock and suspend latch remain intact. A fresh display-only
    /// deadline prevents the next tick from immediately blanking it again.
    pub fn request_display_wake(&mut self, now: Instant) -> Vec<SessionTransition> {
        self.last_display_wake = Some(now);
        let mut out = Vec::new();
        self.undim(&mut out);
        self.wake(&mut out);
        out
    }

    /// Advance the timers to `now` and emit any timeout-driven transitions (dim, blank, auto-lock).
    /// A tick that lands past the blank without having seen the dim window blanks directly: the
    /// dim is a warning, and there is nothing left to warn about.
    /// Idempotent once a threshold has fired: a blanked/locked state stays that way until the boolean is cleared.
    pub fn tick(&mut self, now: Instant) -> Vec<SessionTransition> {
        let idle = self.idle_for(now);
        let display_idle = now.saturating_duration_since(
            self.last_activity
                .max(self.last_display_wake.unwrap_or(self.last_activity)),
        );
        let mut out = Vec::new();
        if self.panel == Panel::On
            && let Some(dim) = self.dim_timeout()
            && let Some(blank) = self.config.blank_timeout
            && display_idle >= dim
            && display_idle < blank
        {
            self.panel = Panel::Dimmed;
            out.push(SessionTransition::Dimmed);
        }
        if !self.is_blanked()
            && let Some(t) = self.config.blank_timeout
            && display_idle >= t
        {
            self.panel = Panel::Off {
                dimmed: self.panel == Panel::Dimmed,
            };
            out.push(SessionTransition::Blanked);
        }
        if !self.locked
            && let Some(t) = self.config.lock_timeout
            && idle >= t
        {
            self.locked = true;
            out.push(SessionTransition::Locked);
        }
        if !self.suspended
            && let Some(t) = self.config.suspend_timeout
            && idle >= t
        {
            self.suspended = true;
            out.push(SessionTransition::Suspend);
        }
        out
    }

    /// [`Self::tick`], honouring what the inhibitor registry holds. An idle
    /// inhibitor (a video playing, a presentation, an app's `Inhibit IDLE`)
    /// means the user is there without touching anything, so the idle episode
    /// restarts from `now` and nothing dims, blanks, locks or suspends. A
    /// dim already showing when the inhibitor arrives is lifted (the video
    /// started, so the warning is moot); a blank is not — only input wakes
    /// the panel. A suspend-only hold leaves dim, blank and lock alone.
    pub fn tick_inhibited(
        &mut self,
        now: Instant,
        inhibitors: &tilo_power_mgmt::InhibitRegistry,
    ) -> Vec<SessionTransition> {
        let mut out = Vec::new();
        if inhibitors.is_inhibited_for(tilo_power_mgmt::inhibit::INHIBIT_IDLE) {
            self.defer_idle(now);
            if !self.is_blanked() {
                self.undim(&mut out);
            }
        }
        out.extend(self.tick(now));
        out
    }

    /// Restart the idle episode at `now` without it being input: a blanked
    /// display stays blanked and a latched suspend stays latched, but no
    /// timeout can fire until a full period has passed from here.
    pub fn defer_idle(&mut self, now: Instant) {
        self.last_activity = now;
    }

    /// Whether the idle-suspend latch is currently set.
    pub fn is_suspend_pending(&self) -> bool {
        self.suspended
    }

    /// Replace the live idle-suspend timeout without resetting user activity.
    /// A profile change is policy, not input; preserving the idle timestamp
    /// makes a stricter profile take effect immediately and predictably.
    pub fn set_suspend_timeout(&mut self, timeout: Option<Duration>) {
        self.config.suspend_timeout = timeout;
        self.suspended = false;
    }

    /// Replace the live display-blank timeout without resetting user
    /// activity. A changed policy takes effect on the next tick against the
    /// current idle duration; changing it to `None` ("Never") prevents any
    /// future automatic blanking but never wakes a display behind the user's
    /// back.
    pub fn set_blank_timeout(&mut self, timeout: Option<Duration>) {
        self.config.blank_timeout = timeout;
    }

    /// Replace the live automatic-lock timeout without treating the policy
    /// change as user activity. A shorter timeout can therefore lock on the
    /// next tick when the current idle duration already exceeds it; `None`
    /// (Settings' "Never") prevents future automatic locks while preserving
    /// explicit and pre-sleep locking.
    pub fn set_lock_timeout(&mut self, timeout: Option<Duration>) {
        self.config.lock_timeout = timeout;
    }

    /// Un-latch a pending suspend the caller decided **not** to act on (typically an inhibitor
    /// held). Leaves the idle timer untouched, so `tick` re-emits [`SessionTransition::Suspend`] the moment the inhibitor releases, without waiting for fresh activity. No-op if none was latched.
    pub fn cancel_suspend(&mut self) {
        self.suspended = false;
    }

    /// Manually lock the session (Super+L / command). Resets the idle timer to `now` so the blank countdown restarts from the lock moment, and lifts a dim so the lock screen shows at full brightness. Locking an already locked session changes nothing else.
    pub fn request_lock(&mut self, now: Instant) -> Vec<SessionTransition> {
        self.last_activity = now;
        let mut out = Vec::new();
        if !self.is_blanked() {
            self.undim(&mut out);
        }
        if !self.locked {
            self.locked = true;
            out.push(SessionTransition::Locked);
        }
        out
    }

    /// Enter the secure pre-sleep state. This is deliberately separate from
    /// ordinary activity: suspend always leaves the session locked, even when
    /// automatic idle locking is disabled.
    pub fn prepare_for_sleep(&mut self, now: Instant) -> Vec<SessionTransition> {
        self.last_activity = now;
        self.suspended = true;
        if self.locked {
            Vec::new()
        } else {
            self.locked = true;
            vec![SessionTransition::Locked]
        }
    }

    /// Apply logind's post-sleep edge. Displays wake and timers re-arm, while
    /// the lock state is intentionally preserved until authentication.
    pub fn resume_from_sleep(&mut self, now: Instant) -> Vec<SessionTransition> {
        self.last_activity = now;
        self.suspended = false;
        let mut out = Vec::new();
        self.undim(&mut out);
        self.wake(&mut out);
        out
    }

    /// Apply the unlock transition: clears the lock, wakes the display if blanked, resets the idle timer. No-op if not locked.
    ///
    /// The *post-authentication* transition — reached only once a credential is verified, either the
    /// lock screen accepting a password ([`crate::compositor::lock_controller::LockController::submit`])
    /// or an `ext-session-lock-v1` client tearing down its own lock. This machine stays credential-agnostic; the gate lives one layer up so it can be driven with an injected verifier in tests.
    pub fn request_unlock(&mut self, now: Instant) -> Vec<SessionTransition> {
        self.last_activity = now;
        if !self.locked {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.undim(&mut out);
        self.wake(&mut out);
        self.locked = false;
        out.push(SessionTransition::Unlocked);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_display_wake_preserves_lock_and_input_idle() {
        let start = Instant::now();
        let mut session = SessionManager::new(cfg(Some(10), Some(5)), start);
        session.tick(start + Duration::from_secs(10));
        assert!(session.is_blanked() && session.is_locked());
        let now = start + Duration::from_secs(20);
        let transitions = session.request_display_wake(now);
        assert_eq!(transitions, vec![SessionTransition::Woke]);
        assert!(session.is_locked());
        assert!(!session.is_blanked());
        assert_eq!(session.idle_for(now), Duration::from_secs(20));
        assert!(session.tick(now).is_empty());
        assert!(session.request_display_wake(now).is_empty());
        assert!(
            session
                .tick(now + Duration::from_secs(10))
                .contains(&SessionTransition::Blanked)
        );
        assert!(session.is_locked());
    }

    #[test]
    fn display_wake_does_not_defer_authentication_or_clear_suspend() {
        let start = Instant::now();
        let mut config = cfg(Some(5), Some(10));
        config.suspend_timeout = Some(Duration::from_secs(12));
        let mut session = SessionManager::new(config, start);
        session.tick(start + Duration::from_secs(5));
        session.request_display_wake(start + Duration::from_secs(9));
        assert_eq!(
            session.tick(start + Duration::from_secs(10)),
            vec![SessionTransition::Locked]
        );
        assert_eq!(
            session.tick(start + Duration::from_secs(12)),
            vec![SessionTransition::Dimmed, SessionTransition::Suspend]
        );
        session.request_display_wake(start + Duration::from_secs(13));
        assert!(session.is_locked() && session.is_suspend_pending());
        assert_eq!(
            session.idle_for(start + Duration::from_secs(13)),
            Duration::from_secs(13)
        );
    }

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

    #[test]
    fn an_idle_inhibitor_keeps_the_screen_from_blanking_and_locking() {
        let t0 = Instant::now();
        let registry = tilo_power_mgmt::InhibitRegistry::new();
        let mut session = SessionManager::new(cfg(Some(60), Some(120)), t0);
        let video = registry.acquire_for("Videos", tilo_power_mgmt::inhibit::INHIBIT_IDLE);

        for minute in 1..=10 {
            assert!(
                session
                    .tick_inhibited(t0 + Duration::from_secs(minute * 60), &registry)
                    .is_empty(),
                "minute {minute}: a held idle inhibitor must neither blank nor lock"
            );
        }

        drop(video);
        let released = t0 + Duration::from_secs(600);
        assert!(
            session
                .tick_inhibited(released + Duration::from_secs(44), &registry)
                .is_empty(),
            "the idle episode restarts when the inhibitor lets go, not from the last input"
        );
        assert_eq!(
            session.tick_inhibited(released + Duration::from_secs(45), &registry),
            vec![SessionTransition::Dimmed]
        );
        assert_eq!(
            session.tick_inhibited(released + Duration::from_secs(60), &registry),
            vec![SessionTransition::Blanked]
        );
        assert_eq!(
            session.tick_inhibited(released + Duration::from_secs(120), &registry),
            vec![SessionTransition::Locked]
        );
    }

    #[test]
    fn a_suspend_only_hold_still_blanks_and_locks() {
        let t0 = Instant::now();
        let registry = tilo_power_mgmt::InhibitRegistry::new();
        let mut session = SessionManager::new(cfg(Some(60), Some(60)), t0);
        let _download =
            registry.acquire_for("Downloading", tilo_power_mgmt::inhibit::INHIBIT_SUSPEND);
        assert_eq!(
            session.tick_inhibited(t0 + Duration::from_secs(60), &registry),
            vec![SessionTransition::Blanked, SessionTransition::Locked],
            "keeping the machine awake is not a reason to leave the screen unlocked"
        );
    }

    #[test]
    fn an_idle_inhibitor_taken_while_blanked_does_not_wake_the_display() {
        let t0 = Instant::now();
        let registry = tilo_power_mgmt::InhibitRegistry::new();
        let mut session = SessionManager::new(cfg(Some(60), None), t0);
        assert_eq!(
            session.tick_inhibited(t0 + Duration::from_secs(60), &registry),
            vec![SessionTransition::Blanked]
        );
        let _late = registry.acquire_for("Music", tilo_power_mgmt::inhibit::INHIBIT_IDLE);
        assert!(
            session
                .tick_inhibited(t0 + Duration::from_secs(61), &registry)
                .is_empty()
        );
        assert!(session.is_blanked(), "only input wakes a blanked display");
    }

    // dim before blank

    #[test]
    fn the_display_dims_fifteen_seconds_before_it_blanks() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(Some(300), None), t0);
        assert!(s.tick(t0 + Duration::from_secs(284)).is_empty());
        assert_eq!(
            s.tick(t0 + Duration::from_secs(285)),
            vec![SessionTransition::Dimmed]
        );
        assert!(s.is_dimmed() && !s.is_blanked());
        assert!(
            s.tick(t0 + Duration::from_secs(290)).is_empty(),
            "the dim fires once per idle episode"
        );
        assert_eq!(
            s.tick(t0 + Duration::from_secs(300)),
            vec![SessionTransition::Blanked]
        );
    }

    #[test]
    fn input_while_dimmed_restores_brightness_and_restarts_the_countdown() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(Some(300), None), t0);
        assert_eq!(
            s.tick(t0 + Duration::from_secs(290)),
            vec![SessionTransition::Dimmed]
        );
        assert_eq!(
            s.note_activity(t0 + Duration::from_secs(295)),
            vec![SessionTransition::Undimmed]
        );
        assert!(!s.is_dimmed());
        assert!(
            s.tick(t0 + Duration::from_secs(301)).is_empty(),
            "the input kept the screen on: no blank at the old deadline"
        );
        assert_eq!(
            s.tick(t0 + Duration::from_secs(295 + 285)),
            vec![SessionTransition::Dimmed],
            "a fresh idle episode dims again"
        );
    }

    #[test]
    fn short_blank_timeouts_dim_for_half_the_idle_period() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(Some(10), None), t0);
        assert!(s.tick(t0 + Duration::from_secs(4)).is_empty());
        assert_eq!(
            s.tick(t0 + Duration::from_secs(5)),
            vec![SessionTransition::Dimmed]
        );
        assert_eq!(
            s.tick(t0 + Duration::from_secs(10)),
            vec![SessionTransition::Blanked]
        );
    }

    #[test]
    fn blanking_disabled_never_dims() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(None, Some(300)), t0);
        assert!(s.tick(t0 + Duration::from_secs(290)).is_empty());
        assert!(!s.is_dimmed());
    }

    #[test]
    fn an_idle_inhibitor_blocks_the_dim_and_lifts_one_already_showing() {
        let t0 = Instant::now();
        let registry = tilo_power_mgmt::InhibitRegistry::new();
        let mut session = SessionManager::new(cfg(Some(60), None), t0);
        assert_eq!(
            session.tick_inhibited(t0 + Duration::from_secs(50), &registry),
            vec![SessionTransition::Dimmed]
        );
        let video = registry.acquire_for("Videos", tilo_power_mgmt::inhibit::INHIBIT_IDLE);
        assert_eq!(
            session.tick_inhibited(t0 + Duration::from_secs(51), &registry),
            vec![SessionTransition::Undimmed],
            "playback starting while dimmed restores the screen"
        );
        for second in (60..=600).step_by(5) {
            assert!(
                session
                    .tick_inhibited(t0 + Duration::from_secs(second), &registry)
                    .is_empty(),
                "second {second}: a held idle inhibitor must not dim"
            );
        }
        drop(video);
    }

    #[test]
    fn waking_from_a_dimmed_blank_restores_brightness_before_power() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(Some(60), Some(60)), t0);
        assert_eq!(
            s.tick(t0 + Duration::from_secs(50)),
            vec![SessionTransition::Dimmed]
        );
        assert_eq!(
            s.tick(t0 + Duration::from_secs(60)),
            vec![SessionTransition::Blanked, SessionTransition::Locked]
        );
        assert!(
            s.is_dimmed(),
            "the dim outlives the blank so waking can undo it"
        );
        assert_eq!(
            s.note_activity(t0 + Duration::from_secs(70)),
            vec![SessionTransition::Undimmed, SessionTransition::Woke]
        );
        assert!(
            s.is_locked(),
            "input restores the screen, never the session"
        );
    }

    #[test]
    fn locking_while_dimmed_shows_the_lock_screen_at_full_brightness() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(Some(60), None), t0);
        let _ = s.tick(t0 + Duration::from_secs(50));
        assert_eq!(
            s.request_lock(t0 + Duration::from_secs(51)),
            vec![SessionTransition::Undimmed, SessionTransition::Locked]
        );
    }

    // IdleConfig::from_env_values

    #[test]
    fn idle_config_parses_timeouts_with_secure_defaults() {
        // Phase `config_defaults_blank_and_lock_after_five_minutes`.
        {
            let c = IdleConfig::from_env_values(None, None, None);
            assert_eq!(c.blank_timeout, Some(Duration::from_secs(300)));
            assert_eq!(c.lock_timeout, Some(Duration::from_secs(300)));
            assert_eq!(c.suspend_timeout, None);
        }
        // Phase `config_zero_disables`.
        {
            let c = IdleConfig::from_env_values(Some("0"), Some("0"), Some("0"));
            assert_eq!(c.blank_timeout, None);
            assert_eq!(c.lock_timeout, None);
            assert_eq!(c.suspend_timeout, None);
        }
        // Phase `config_parses_positive_seconds`.
        {
            let c = IdleConfig::from_env_values(Some("60"), Some("120"), Some("900"));
            assert_eq!(c.blank_timeout, Some(Duration::from_secs(60)));
            assert_eq!(c.lock_timeout, Some(Duration::from_secs(120)));
            assert_eq!(c.suspend_timeout, Some(Duration::from_secs(900)));
        }
        // Phase `config_garbage_blank_and_lock_fall_back_to_secure_defaults`.
        {
            let c =
                IdleConfig::from_env_values(Some("nonsense"), Some("nonsense"), Some("nonsense"));
            assert_eq!(c.blank_timeout, Some(Duration::from_secs(300)));
            assert_eq!(c.lock_timeout, Some(Duration::from_secs(300)));
            assert_eq!(c.suspend_timeout, None);
        }
        // Phase `with_suspend_overrides_the_timeout`.
        {
            // The Settings power policy drives suspend, overriding env.
            let c = IdleConfig::from_env_values(Some("300"), None, None)
                .with_suspend(Some(Duration::from_secs(1200)));
            assert_eq!(c.suspend_timeout, Some(Duration::from_secs(1200)));
            assert_eq!(c.blank_timeout, Some(Duration::from_secs(300)));
        }
        // Phase `with_blank_overrides_the_environment_seed`.
        {
            let c = IdleConfig::from_env_values(Some("300"), None, None)
                .with_blank(Some(Duration::from_secs(600)));
            assert_eq!(c.blank_timeout, Some(Duration::from_secs(600)));
        }
    }

    #[test]
    fn live_timeout_changes_preserve_idle_time() {
        // Phase `live_blank_timeout_preserves_idle_time_and_never_disables_future_blanking`.
        {
            let t0 = Instant::now();
            let mut session = SessionManager::new(cfg(None, None), t0);
            assert!(session.tick(t0 + Duration::from_secs(599)).is_empty());

            // A settings reload must apply the new policy against the existing
            // idle episode, rather than making the user wait another full period.
            session.set_blank_timeout(Some(Duration::from_secs(600)));
            assert_eq!(
                session.tick(t0 + Duration::from_secs(600)),
                vec![SessionTransition::Blanked]
            );

            // "Never" applies live too. Activity starts a new episode, then no
            // amount of elapsed idle time can blank it.
            assert_eq!(
                session.note_activity(t0 + Duration::from_secs(601)),
                vec![SessionTransition::Woke]
            );
            session.set_blank_timeout(None);
            assert!(session.tick(t0 + Duration::from_secs(100_000)).is_empty());
            assert!(!session.is_blanked());
        }
        // Phase `settings_policy_blank_timeout_drives_the_live_session`.
        {
            let policy = tilo_power_mgmt::config::policy_from_settings_json_for_profile(
                r#"{"power":{"blank_minutes":1,"auto_suspend":false}}"#,
                None,
            );
            let t0 = Instant::now();
            // Isolate the display policy: lock and suspend are independent
            // transitions and must not make the "Never blank" assertion fail for
            // an unrelated reason.
            let mut session =
                SessionManager::new(cfg(None, None).with_blank(policy.idle_blank), t0);

            assert!(session.tick(t0 + Duration::from_secs(44)).is_empty());
            assert_eq!(
                session.tick(t0 + Duration::from_secs(59)),
                vec![SessionTransition::Dimmed]
            );
            assert_eq!(
                session.tick(t0 + Duration::from_secs(60)),
                vec![SessionTransition::Blanked]
            );

            let never = tilo_power_mgmt::config::policy_from_settings_json_for_profile(
                r#"{"power":{"blank_minutes":0,"auto_suspend":false}}"#,
                None,
            );
            assert_eq!(
                session.note_activity(t0 + Duration::from_secs(61)),
                vec![SessionTransition::Undimmed, SessionTransition::Woke]
            );
            session.set_blank_timeout(never.idle_blank);
            assert!(session.tick(t0 + Duration::from_secs(100_000)).is_empty());
        }
        // Phase `live_suspend_timeout_preserves_idle_time_and_rearms_policy`.
        {
            let t0 = Instant::now();
            let mut session = SessionManager::new(cfg_s(None, None, Some(900)), t0);
            assert!(session.tick(t0 + Duration::from_secs(599)).is_empty());
            session.set_suspend_timeout(Some(Duration::from_secs(600)));
            assert_eq!(
                session.tick(t0 + Duration::from_secs(600)),
                vec![SessionTransition::Suspend]
            );
            session.set_suspend_timeout(None);
            assert!(session.tick(t0 + Duration::from_secs(10_000)).is_empty());
        }
        // Phase `live_lock_timeout_change_uses_current_idle_duration_and_never_disables_only_auto_lock`.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(None, Some(600)), t0);
            assert!(s.tick(t0 + Duration::from_secs(400)).is_empty());

            s.set_lock_timeout(Some(Duration::from_secs(300)));
            assert_eq!(
                s.tick(t0 + Duration::from_secs(400)),
                vec![SessionTransition::Locked],
                "a stricter live setting applies without pretending policy change is input"
            );

            let mut never = SessionManager::new(cfg(None, Some(60)), t0);
            never.set_lock_timeout(None);
            assert!(never.tick(t0 + Duration::from_secs(10_000)).is_empty());
            assert_eq!(
                never.request_lock(t0 + Duration::from_secs(10_001)),
                vec![SessionTransition::Locked],
                "Never disables only idle locking, not the explicit lock action"
            );
        }
    }

    // blank / wake cycle

    #[test]
    fn idle_blanking_fires_past_threshold_and_input_wakes() {
        // Phase `idle_past_threshold_blanks_then_input_wakes`.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(Some(300), None), t0);
            assert!(!s.is_blanked());

            // Before the dim window: no transition.
            assert!(s.tick(t0 + Duration::from_secs(284)).is_empty());
            // Just under the threshold: dimmed, not blanked.
            assert_eq!(
                s.tick(t0 + Duration::from_secs(299)),
                vec![SessionTransition::Dimmed]
            );
            assert!(!s.is_blanked());

            // At/after the threshold: blanks once.
            let trans = s.tick(t0 + Duration::from_secs(300));
            assert_eq!(trans, vec![SessionTransition::Blanked]);
            assert!(s.is_blanked());

            // Subsequent ticks while blanked emit nothing (idempotent).
            assert!(s.tick(t0 + Duration::from_secs(600)).is_empty());
            assert!(s.is_blanked());

            // Any input restores brightness, wakes the display and resets the timer.
            let trans = s.note_activity(t0 + Duration::from_secs(601));
            assert_eq!(
                trans,
                vec![SessionTransition::Undimmed, SessionTransition::Woke]
            );
            assert!(!s.is_dimmed());
            assert!(!s.is_blanked());
            assert_eq!(s.idle_for(t0 + Duration::from_secs(601)), Duration::ZERO);
        }
        // Phase `blank_disabled_never_blanks`.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(None, None), t0);
            assert!(s.tick(t0 + Duration::from_secs(100_000)).is_empty());
            assert!(!s.is_blanked());
        }
        // Phase `note_activity_without_blank_emits_nothing`.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(Some(300), None), t0);
            assert!(s.note_activity(t0 + Duration::from_secs(1)).is_empty());
            assert!(!s.is_blanked());
        }
        // Phase `equal_timeouts_emit_both_blank_and_lock_same_tick`.
        {
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
    }

    // auto-lock

    #[test]
    fn locking_requires_authentication_to_clear() {
        // Phase `idle_past_lock_threshold_locks`.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(Some(300), Some(600)), t0);

            let trans = s.tick(t0 + Duration::from_secs(300));
            assert_eq!(trans, vec![SessionTransition::Blanked]);
            assert!(!s.is_locked());

            let trans = s.tick(t0 + Duration::from_secs(600));
            assert_eq!(trans, vec![SessionTransition::Locked]);
            assert!(s.is_locked());
        }
        // Phase `request_lock_locks_and_is_idempotent`.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(Some(300), None), t0);
            let trans = s.request_lock(t0 + Duration::from_secs(5));
            assert_eq!(trans, vec![SessionTransition::Locked]);
            assert!(s.is_locked());
            assert!(s.request_lock(t0 + Duration::from_secs(6)).is_empty());
        }
        // Phase `request_unlock_clears_lock_and_wakes_if_blanked`.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(Some(300), Some(300)), t0);
            let _ = s.tick(t0 + Duration::from_secs(300));
            assert!(s.is_locked() && s.is_blanked());

            let trans = s.request_unlock(t0 + Duration::from_secs(305));
            // Wake comes before Unlocked so the display is on before the chrome resumes painting.
            assert_eq!(
                trans,
                vec![SessionTransition::Woke, SessionTransition::Unlocked]
            );
            assert!(!s.is_locked());
            assert!(!s.is_blanked());
        }
        // Phase `request_unlock_when_unlocked_is_noop`.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(Some(300), None), t0);
            assert!(s.request_unlock(t0).is_empty());
        }
        // Phase `input_while_locked_wakes_but_does_not_unlock`.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(Some(300), None), t0);
            let _ = s.request_lock(t0);
            // Idle past blank while locked → still blanks.
            let trans = s.tick(t0 + Duration::from_secs(300));
            assert_eq!(trans, vec![SessionTransition::Blanked]);
            assert!(s.is_locked() && s.is_blanked());

            // Input wakes the display (to show the lock overlay) but keeps the session locked.
            let trans = s.note_activity(t0 + Duration::from_secs(305));
            assert_eq!(trans, vec![SessionTransition::Woke]);
            assert!(s.is_locked());
            assert!(!s.is_blanked());
        }
        // Phase `externally_requested_sleep_locks_even_when_idle_lock_is_disabled`.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(None, None), t0);
            assert_eq!(s.prepare_for_sleep(t0), vec![SessionTransition::Locked]);
            assert!(s.prepare_for_sleep(t0).is_empty());
            assert!(s.resume_from_sleep(t0 + Duration::from_secs(1)).is_empty());
            assert!(s.is_locked());
        }
        // Phase `locked_session_can_still_blank_after_wake`.
        {
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

    // idle-to-suspend

    #[test]
    fn idle_suspend_fires_once_per_episode_and_resume_stays_locked() {
        // Phase `idle_past_suspend_threshold_emits_suspend_once`.
        {
            let t0 = Instant::now();
            // Blank at 300, suspend at 900, no lock.
            let mut s = SessionManager::new(cfg_s(Some(300), None, Some(900)), t0);

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
        // Phase `suspend_disabled_never_suspends`.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg_s(Some(300), None, None), t0);
            let trans = s.tick(t0 + Duration::from_secs(100_000));
            assert_eq!(trans, vec![SessionTransition::Blanked]);
            assert!(!s.is_suspend_pending());
        }
        // Phase `activity_after_suspend_rearms_for_the_next_episode`.
        {
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
        // Phase `cancel_suspend_reemits_next_tick_without_resetting_timer`.
        {
            // The inhibited case: a suspend was emitted, the caller vetoed it (inhibitor held), so it
            // un-latches and re-emits on the very next tick — the machine suspends the moment the inhibitor releases, without waiting for fresh activity.
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg_s(None, None, Some(600)), t0);
            assert_eq!(
                s.tick(t0 + Duration::from_secs(600)),
                vec![SessionTransition::Suspend]
            );
            s.cancel_suspend();
            assert!(!s.is_suspend_pending());
            assert_eq!(
                s.tick(t0 + Duration::from_secs(601)),
                vec![SessionTransition::Suspend]
            );
        }
        // Phase `suspend_resume_wakes_but_remains_locked_until_authentication`.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg_s(Some(60), None, Some(120)), t0);
            assert_eq!(
                s.tick(t0 + Duration::from_secs(120)),
                vec![SessionTransition::Blanked, SessionTransition::Suspend]
            );
            assert_eq!(
                s.prepare_for_sleep(t0 + Duration::from_secs(121)),
                vec![SessionTransition::Locked]
            );
            assert!(s.is_locked() && s.is_blanked() && s.is_suspend_pending());

            assert_eq!(
                s.resume_from_sleep(t0 + Duration::from_secs(130)),
                vec![SessionTransition::Woke]
            );
            assert!(s.is_locked());
            assert!(!s.is_blanked());
            assert!(!s.is_suspend_pending());

            assert_eq!(
                s.request_unlock(t0 + Duration::from_secs(131)),
                vec![SessionTransition::Unlocked]
            );
            assert!(!s.is_locked());
        }
    }

    // manual lock / unlock
}
