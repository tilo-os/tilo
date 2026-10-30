//! Idle: when the displays dim, blank and the session locks or suspends.
//!
//! A pure state machine: it owns no hardware and does no I/O. It tracks the
//! time since the last input and whether the panels are dimmed, blanked and
//! the session locked, and emits [`SessionTransition`]s. The compositor
//! applies the display ones itself (dim overlay or backlight, output power);
//! locking and suspending are the session's, which starts its lock client
//! or asks logind.
//!
//! Shortly before the display blanks it dims ([`SessionTransition::Dimmed`]),
//! so someone reading without touching anything gets a warning and a single
//! input to keep the screen on ([`SessionTransition::Undimmed`]) instead of a
//! sudden black screen.
//!
//! Two orthogonal booleans: `blanked` (displays powered down; any input wakes
//! them) and `locked` (input goes to the lock client until it unlocks). An
//! idle locked session still blanks to save power, and waking from blank
//! while locked shows the lock surface rather than unlocking.

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

/// An effect the compositor (display) or the session (lock, suspend) must apply after calling into the [`SessionManager`]; a pure value — the manager never touches hardware itself.
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

/// What holds the session awake at a tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Holds {
    /// Someone watches without touching anything: an idle-inhibit surface
    /// on screen, a video, a presentation. Nothing dims, blanks, locks or
    /// suspends.
    pub idle: bool,
    /// The machine must stay awake (a download, a remote session, a
    /// keep-awake lease); the screen still dims, blanks and locks.
    pub suspend: bool,
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

impl IdleConfig {
    /// No idle policy at all: nothing blanks, locks or suspends on its own.
    #[must_use]
    pub const fn never() -> Self {
        Self {
            blank_timeout: None,
            lock_timeout: None,
            suspend_timeout: None,
        }
    }
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
    /// The person's last input here. Unlike `last_activity`, nothing but
    /// input moves it, so it says when they last used this device.
    last_input: Instant,
    /// Explicit panel wake is not input: only the display timeout restarts.
    last_display_wake: Option<Instant>,
    panel: Panel,
    locked: bool,
    /// Latched once the idle-suspend threshold fires, so `tick` emits [`SessionTransition::Suspend`]
    /// at most once per idle episode. Cleared by [`Self::note_activity`] (input after resume) and [`Self::cancel_suspend`] (a vetoed suspend request).
    suspended: bool,
    /// The last tick at which something held the machine awake
    /// (`INHIBIT_SUSPEND`: a keep-awake lease, a remote session, a download).
    /// The idle-suspend period counts from the later of this and the last
    /// input, so letting go of a hold starts a full period instead of
    /// suspending the instant it ends.
    suspend_held_at: Option<Instant>,
}

impl SessionManager {
    /// Construct a fresh, active (un-blanked, un-locked) manager whose idle timer starts at `now`.
    pub fn new(config: IdleConfig, now: Instant) -> Self {
        Self {
            config,
            last_activity: now,
            last_input: now,
            last_display_wake: None,
            panel: Panel::On,
            locked: false,
            suspended: false,
            suspend_held_at: None,
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

    /// Record the display as still dimmed: lifting the dim did not take
    /// effect, so the next input lifts it again ([`SessionTransition::Undimmed`]).
    pub fn keep_dimmed(&mut self) {
        self.panel = match self.panel {
            Panel::On | Panel::Dimmed => Panel::Dimmed,
            Panel::Off { .. } => Panel::Off { dimmed: true },
        };
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
        self.last_input = now;
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
        let suspend_idle = now.saturating_duration_since(
            self.last_activity
                .max(self.suspend_held_at.unwrap_or(self.last_activity)),
        );
        if !self.suspended
            && let Some(t) = self.config.suspend_timeout
            && suspend_idle >= t
        {
            self.suspended = true;
            out.push(SessionTransition::Suspend);
        }
        out
    }

    /// [`Self::tick`], honouring what holds the session awake. An idle
    /// inhibitor (a video playing, a presentation, an app's `Inhibit IDLE`)
    /// means the user is there without touching anything, so the idle episode
    /// restarts from `now` and nothing dims, blanks, locks or suspends. A
    /// dim already showing when the inhibitor arrives is lifted (the video
    /// started, so the warning is moot); a blank is not — only input wakes
    /// the panel. A suspend-only hold leaves dim, blank and lock alone, but
    /// the idle-suspend period restarts when it lets go: a machine kept awake
    /// for hours is not "idle for hours" the moment the hold ends.
    pub fn tick_inhibited(&mut self, now: Instant, holds: Holds) -> Vec<SessionTransition> {
        let mut out = Vec::new();
        if holds.suspend {
            self.suspend_held_at = Some(now);
        }
        if holds.idle {
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

    /// The person used another of their devices nearby at `at`: idle time
    /// here counts from then if that is later, as though it were input, but
    /// a blanked display stays blanked.
    pub fn note_use_elsewhere(&mut self, at: Instant) {
        self.last_activity = self.last_activity.max(at);
    }

    /// How long since the person's last input on this device.
    pub fn input_idle_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_input)
    }

    /// Whether the idle timer has not yet reached the lock timeout: the
    /// person was using the device, here or on one nearby.
    pub fn in_use(&self, now: Instant) -> bool {
        self.config
            .lock_timeout
            .is_none_or(|timeout| self.idle_for(now) < timeout)
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

    /// Un-latch a pending suspend the caller decided **not** to act on (an inhibitor that
    /// appeared after this tick's check). Leaves the idle timer untouched, so the next `tick`
    /// re-emits [`SessionTransition::Suspend`] unless [`Self::tick_inhibited`] sees the hold and
    /// restarts the period from it. No-op if none was latched.
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

    /// Turn the display off now, as the idle timeout would: the power key on
    /// a session that is already locked. The lock and timers stay as they
    /// are; input wakes the display again. Nothing changes when it is off.
    pub fn blank_now(&mut self, now: Instant) -> Vec<SessionTransition> {
        self.last_activity = now;
        if self.is_blanked() {
            return Vec::new();
        }
        self.panel = Panel::Off {
            dimmed: self.panel == Panel::Dimmed,
        };
        vec![SessionTransition::Blanked]
    }

    /// Enter the pre-sleep state. This is separate from ordinary activity:
    /// unless the person turned off the password after sleep
    /// (the session's `lock_on_sleep` policy), sleep leaves the
    /// session locked even when automatic idle locking is disabled. Either
    /// way a session that is already locked stays locked.
    pub fn prepare_for_sleep(
        &mut self,
        now: Instant,
        lock_on_sleep: bool,
    ) -> Vec<SessionTransition> {
        self.last_activity = now;
        self.suspended = true;
        if self.locked || !lock_on_sleep {
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
    /// The *post-authentication* transition: the `ext-session-lock-v1` client
    /// unlocked after verifying the person. This machine stays credential-agnostic.
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
    use std::cell::Cell;

    const INHIBIT_IDLE: bool = true;
    const INHIBIT_SUSPEND: bool = false;

    /// Counted holds, released when their guard drops, as the session's
    /// inhibitor registry hands them out.
    #[derive(Default)]
    struct Registry {
        idle: Cell<usize>,
        suspend: Cell<usize>,
    }

    struct Hold<'a>(&'a Cell<usize>);

    impl Drop for Hold<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get().saturating_sub(1));
        }
    }

    impl Registry {
        fn acquire_for(&self, _why: &str, idle: bool) -> Hold<'_> {
            let count = if idle { &self.idle } else { &self.suspend };
            count.set(count.get().saturating_add(1));
            Hold(count)
        }

        fn holds(&self) -> Holds {
            Holds {
                idle: self.idle.get() > 0,
                suspend: self.suspend.get() > 0,
            }
        }
    }

    #[test]
    fn blanking_now_turns_the_display_off_once_and_input_wakes_it_still_locked() {
        let start = Instant::now();
        let mut session = SessionManager::new(cfg(Some(600), None), start);
        session.request_lock(start);
        let now = start + Duration::from_secs(1);
        assert_eq!(session.blank_now(now), vec![SessionTransition::Blanked]);
        assert!(session.is_blanked() && session.is_locked());
        assert_eq!(session.blank_now(now), Vec::new(), "already off");
        assert_eq!(
            session.note_activity(now + Duration::from_secs(1)),
            vec![SessionTransition::Woke]
        );
        assert!(!session.is_blanked() && session.is_locked());
    }

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
        let registry = Registry::default();
        let mut session = SessionManager::new(cfg(Some(60), Some(120)), t0);
        let video = registry.acquire_for("Videos", INHIBIT_IDLE);

        for minute in 1..=10 {
            assert!(
                session
                    .tick_inhibited(t0 + Duration::from_secs(minute * 60), registry.holds())
                    .is_empty(),
                "minute {minute}: a held idle inhibitor must neither blank nor lock"
            );
        }

        drop(video);
        let released = t0 + Duration::from_secs(600);
        assert!(
            session
                .tick_inhibited(released + Duration::from_secs(44), registry.holds())
                .is_empty(),
            "the idle episode restarts when the inhibitor lets go, not from the last input"
        );
        assert_eq!(
            session.tick_inhibited(released + Duration::from_secs(45), registry.holds()),
            vec![SessionTransition::Dimmed]
        );
        assert_eq!(
            session.tick_inhibited(released + Duration::from_secs(60), registry.holds()),
            vec![SessionTransition::Blanked]
        );
        assert_eq!(
            session.tick_inhibited(released + Duration::from_secs(120), registry.holds()),
            vec![SessionTransition::Locked]
        );
    }

    #[test]
    fn a_suspend_only_hold_still_blanks_and_locks() {
        let t0 = Instant::now();
        let registry = Registry::default();
        let mut session = SessionManager::new(cfg(Some(60), Some(60)), t0);
        let _download = registry.acquire_for("Downloading", INHIBIT_SUSPEND);
        assert_eq!(
            session.tick_inhibited(t0 + Duration::from_secs(60), registry.holds()),
            vec![SessionTransition::Blanked, SessionTransition::Locked],
            "keeping the machine awake is not a reason to leave the screen unlocked"
        );
    }

    #[test]
    fn letting_go_of_a_keep_awake_hold_starts_a_full_idle_period_before_suspending() {
        // A machine held awake overnight (a VM, a remote session, an agent's
        // lease) suspended the second the hold ended, because local idle
        // time kept counting under the hold. That reads as a random suspend.
        let t0 = Instant::now();
        let registry = Registry::default();
        let mut session = SessionManager::new(cfg_s(None, None, Some(900)), t0);
        let hold = registry.acquire_for("Keep awake", INHIBIT_SUSPEND);
        for minute in [15, 60, 600] {
            assert!(
                session
                    .tick_inhibited(t0 + Duration::from_secs(minute * 60), registry.holds())
                    .is_empty(),
                "minute {minute}: a held machine never suspends"
            );
        }
        drop(hold);
        let released = t0 + Duration::from_secs(600 * 60 + 1);
        assert!(
            session
                .tick_inhibited(released, registry.holds())
                .is_empty(),
            "the moment the hold ends is not fifteen idle minutes"
        );
        assert!(
            session
                .tick_inhibited(released + Duration::from_secs(898), registry.holds())
                .is_empty()
        );
        assert_eq!(
            session.tick_inhibited(released + Duration::from_secs(900), registry.holds()),
            vec![SessionTransition::Suspend],
            "a full idle period after the hold, the machine suspends as configured"
        );
    }

    #[test]
    fn an_idle_inhibitor_taken_while_blanked_does_not_wake_the_display() {
        let t0 = Instant::now();
        let registry = Registry::default();
        let mut session = SessionManager::new(cfg(Some(60), None), t0);
        assert_eq!(
            session.tick_inhibited(t0 + Duration::from_secs(60), registry.holds()),
            vec![SessionTransition::Blanked]
        );
        let _late = registry.acquire_for("Music", INHIBIT_IDLE);
        assert!(
            session
                .tick_inhibited(t0 + Duration::from_secs(61), registry.holds())
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
        let registry = Registry::default();
        let mut session = SessionManager::new(cfg(Some(60), None), t0);
        assert_eq!(
            session.tick_inhibited(t0 + Duration::from_secs(50), registry.holds()),
            vec![SessionTransition::Dimmed]
        );
        let video = registry.acquire_for("Videos", INHIBIT_IDLE);
        assert_eq!(
            session.tick_inhibited(t0 + Duration::from_secs(51), registry.holds()),
            vec![SessionTransition::Undimmed],
            "playback starting while dimmed restores the screen"
        );
        for second in (60..=600).step_by(5) {
            assert!(
                session
                    .tick_inhibited(t0 + Duration::from_secs(second), registry.holds())
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
        // Config defaults blank and lock after five minutes.
        {
            let c = IdleConfig::from_env_values(None, None, None);
            assert_eq!(c.blank_timeout, Some(Duration::from_secs(300)));
            assert_eq!(c.lock_timeout, Some(Duration::from_secs(300)));
            assert_eq!(c.suspend_timeout, None);
        }
        // Config zero disables.
        {
            let c = IdleConfig::from_env_values(Some("0"), Some("0"), Some("0"));
            assert_eq!(c.blank_timeout, None);
            assert_eq!(c.lock_timeout, None);
            assert_eq!(c.suspend_timeout, None);
        }
        // Config parses positive seconds.
        {
            let c = IdleConfig::from_env_values(Some("60"), Some("120"), Some("900"));
            assert_eq!(c.blank_timeout, Some(Duration::from_secs(60)));
            assert_eq!(c.lock_timeout, Some(Duration::from_secs(120)));
            assert_eq!(c.suspend_timeout, Some(Duration::from_secs(900)));
        }
        // Config garbage blank and lock fall back to secure defaults.
        {
            let c =
                IdleConfig::from_env_values(Some("nonsense"), Some("nonsense"), Some("nonsense"));
            assert_eq!(c.blank_timeout, Some(Duration::from_secs(300)));
            assert_eq!(c.lock_timeout, Some(Duration::from_secs(300)));
            assert_eq!(c.suspend_timeout, None);
        }
        // With suspend overrides the timeout.
        {
            // The Settings power policy drives suspend, overriding env.
            let c = IdleConfig::from_env_values(Some("300"), None, None)
                .with_suspend(Some(Duration::from_secs(1200)));
            assert_eq!(c.suspend_timeout, Some(Duration::from_secs(1200)));
            assert_eq!(c.blank_timeout, Some(Duration::from_secs(300)));
        }
        // With blank overrides the environment seed.
        {
            let c = IdleConfig::from_env_values(Some("300"), None, None)
                .with_blank(Some(Duration::from_secs(600)));
            assert_eq!(c.blank_timeout, Some(Duration::from_secs(600)));
        }
    }

    #[test]
    fn live_timeout_changes_preserve_idle_time() {
        // Live blank timeout preserves idle time and never disables future blanking.
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
        // Settings policy blank timeout drives the live session.
        {
            let t0 = Instant::now();
            // Isolate the display policy: lock and suspend are independent
            // transitions and must not make the "Never blank" assertion fail for
            // an unrelated reason.
            let mut session = SessionManager::new(
                cfg(None, None).with_blank(Some(Duration::from_secs(60))),
                t0,
            );

            assert!(session.tick(t0 + Duration::from_secs(44)).is_empty());
            assert_eq!(
                session.tick(t0 + Duration::from_secs(59)),
                vec![SessionTransition::Dimmed]
            );
            assert_eq!(
                session.tick(t0 + Duration::from_secs(60)),
                vec![SessionTransition::Blanked]
            );

            assert_eq!(
                session.note_activity(t0 + Duration::from_secs(61)),
                vec![SessionTransition::Undimmed, SessionTransition::Woke]
            );
            session.set_blank_timeout(None);
            assert!(session.tick(t0 + Duration::from_secs(100_000)).is_empty());
        }
        // Live suspend timeout preserves idle time and rearms policy.
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
        // Live lock timeout change uses current idle duration and never disables only auto lock.
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
        // Idle past threshold blanks then input wakes.
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
        // Blank disabled never blanks.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(None, None), t0);
            assert!(s.tick(t0 + Duration::from_secs(100_000)).is_empty());
            assert!(!s.is_blanked());
        }
        // Note activity without blank emits nothing.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(Some(300), None), t0);
            assert!(s.note_activity(t0 + Duration::from_secs(1)).is_empty());
            assert!(!s.is_blanked());
        }
        // Equal timeouts emit both blank and lock same tick.
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
    fn use_of_a_device_nearby_defers_the_idle_lock_but_is_not_input_here() {
        let t0 = Instant::now();
        let mut s = SessionManager::new(cfg(None, Some(600)), t0);
        s.note_use_elsewhere(t0 + Duration::from_secs(500));
        assert!(s.tick(t0 + Duration::from_secs(600)).is_empty());
        assert!(s.in_use(t0 + Duration::from_secs(1000)));
        assert_eq!(
            s.input_idle_for(t0 + Duration::from_secs(1000)),
            Duration::from_secs(1000)
        );
        assert_eq!(
            s.tick(t0 + Duration::from_secs(1100)),
            vec![SessionTransition::Locked]
        );
        assert!(!s.in_use(t0 + Duration::from_secs(1100)));
    }

    #[test]
    fn locking_requires_authentication_to_clear() {
        // Idle past lock threshold locks.
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
        // Request lock locks and is idempotent.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(Some(300), None), t0);
            let trans = s.request_lock(t0 + Duration::from_secs(5));
            assert_eq!(trans, vec![SessionTransition::Locked]);
            assert!(s.is_locked());
            assert!(s.request_lock(t0 + Duration::from_secs(6)).is_empty());
        }
        // Request unlock clears lock and wakes if blanked.
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
        // Request unlock when unlocked is noop.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(Some(300), None), t0);
            assert!(s.request_unlock(t0).is_empty());
        }
        // Input while locked wakes but does not unlock.
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
        // Externally requested sleep locks even when idle lock is disabled.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(None, None), t0);
            assert_eq!(
                s.prepare_for_sleep(t0, true),
                vec![SessionTransition::Locked]
            );
            assert!(s.prepare_for_sleep(t0, true).is_empty());
            assert!(s.resume_from_sleep(t0 + Duration::from_secs(1)).is_empty());
            assert!(s.is_locked());
        }
        // Sleep leaves an unlocked session unlocked when the person asked.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(None, None), t0);
            assert!(s.prepare_for_sleep(t0, false).is_empty());
            assert!(!s.is_locked());
            assert!(s.is_suspend_pending());
            assert!(s.resume_from_sleep(t0 + Duration::from_secs(1)).is_empty());
            assert!(!s.is_locked(), "waking shows the work left on screen");
        }
        // Sleep keeps a lock that was already there.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg(None, None), t0);
            assert_eq!(s.request_lock(t0), vec![SessionTransition::Locked]);
            assert!(s.prepare_for_sleep(t0, false).is_empty());
            assert!(s.is_locked());
            assert!(s.resume_from_sleep(t0 + Duration::from_secs(1)).is_empty());
            assert!(s.is_locked());
        }
        // Locked session can still blank after wake.
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

    /// Lock "Never" and suspend when idle off: however long and however
    /// often the person walks away, idleness only darkens the screen.
    #[test]
    fn with_lock_never_and_idle_suspend_off_idleness_only_darkens_the_screen() {
        let t0 = Instant::now();
        let registry = Registry::default();
        let mut s = SessionManager::new(cfg_s(Some(600), None, None), t0);
        let mut seen = Vec::new();
        for episode in 0..3_u64 {
            let start = t0 + Duration::from_secs(episode * 100_000);
            if episode > 0 {
                seen.extend(s.note_activity(start));
            }
            for minute in 1..=24 * 60_u64 {
                seen.extend(
                    s.tick_inhibited(start + Duration::from_secs(minute * 60), registry.holds()),
                );
            }
        }
        assert!(seen.contains(&SessionTransition::Blanked));
        assert!(!seen.contains(&SessionTransition::Locked), "{seen:?}");
        assert!(!seen.contains(&SessionTransition::Suspend), "{seen:?}");
        assert!(!s.is_locked());
        assert!(!s.is_suspend_pending());
    }

    #[test]
    fn idle_suspend_fires_once_per_episode_and_resume_stays_locked() {
        // Idle past suspend threshold emits suspend once.
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
        // Suspend disabled never suspends.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg_s(Some(300), None, None), t0);
            let trans = s.tick(t0 + Duration::from_secs(100_000));
            assert_eq!(trans, vec![SessionTransition::Blanked]);
            assert!(!s.is_suspend_pending());
        }
        // Activity after suspend rearms for the next episode.
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
        // Cancel suspend reemits next tick without resetting timer.
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
        // Suspend resume wakes but remains locked until authentication.
        {
            let t0 = Instant::now();
            let mut s = SessionManager::new(cfg_s(Some(60), None, Some(120)), t0);
            assert_eq!(
                s.tick(t0 + Duration::from_secs(120)),
                vec![SessionTransition::Blanked, SessionTransition::Suspend]
            );
            assert_eq!(
                s.prepare_for_sleep(t0 + Duration::from_secs(121), true),
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
