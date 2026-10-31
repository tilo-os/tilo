//! The machine's power: the timeouts the person set, the lid, the power key
//! and the battery, and the sleep the service over logind asks for.
//!
//! The compositor keeps the idle state machine — it knows what a person's
//! input is and when the displays blanked — and is told the timeouts here;
//! a suspend it decides on comes back through its session events. The
//! service ([`tilo_session_power`]) owns logind and `UPower`: a lid, a
//! critical battery, the power key, a suspend request and the resume edge
//! arrive as [`PowerSignal`]s, and every decision about the machine is the
//! [`PowerPolicy`] the person's settings resolved.
//!
//! Two things are the session's own here, because they need what only its
//! loop has: the lock screen's frame, which logind's sleep delay waits for,
//! and the idle latch, which a hold taken between two ticks has to release.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tilo_compositor::{Compositor, IdleConfig};
use tilo_power_mgmt::PowerSignal;
use tilo_session_power::PowerLink;

use crate::{Error, Options, Session, SessionConfig, a11y_settings};

/// What the session keeps of its power between frames.
pub(crate) struct PowerRun {
    /// The policy, the mailbox and the holds of [`tilo_session_power`];
    /// `None` when this session was given no power of its own.
    link: Option<PowerLink>,
    /// Whether the person's settings changed since the last pass.
    reload: Arc<AtomicBool>,
    /// The settings the loader last resolved, waiting for the next pass.
    next: Arc<Mutex<Option<SessionConfig>>>,
    /// What the sleep in flight installed, so a failed one undoes only that.
    sleep: SleepLock,
    /// Whether the sleep in flight is the idle timer's: its lock stays on
    /// this device, as the idle timer's own lock does.
    idle_sleep: bool,
    /// Keeps the settings watch alive: the loader's one watch, which the
    /// compositor's assists and the power policy both follow.
    watch: Option<settings_store::watch::SettingsWatch>,
}

impl PowerRun {
    /// A session that runs no power of its own: the one in a window of
    /// another desktop, which owns the machine's.
    fn none() -> Self {
        Self {
            link: None,
            reload: Arc::new(AtomicBool::new(false)),
            next: Arc::new(Mutex::new(None)),
            sleep: SleepLock {
                prepared: false,
                locked_before: false,
                awaiting: false,
            },
            idle_sleep: false,
            watch: None,
        }
    }

    /// Follow the person's settings, whether or not this session owns the
    /// machine's power: the compositor's assists and the idle timers both
    /// come from the one reading.
    fn follow_settings(
        &self,
        waker: std::task::Waker,
    ) -> std::io::Result<settings_store::watch::SettingsWatch> {
        let reload = Arc::clone(&self.reload);
        let next = Arc::clone(&self.next);
        SessionConfig::follow(move |config| {
            *next.lock().unwrap_or_else(PoisonError::into_inner) = Some(config);
            reload.store(true, Ordering::Release);
            waker.wake_by_ref();
        })
    }
}

/// A sleep in flight.
#[derive(Clone, Copy, Debug, Default)]
struct SleepLock {
    /// Whether a sleep is waiting for its lock screen's frame.
    prepared: bool,
    /// Whether the session was already locked when the sleep began: that
    /// lock is not this sleep's to lift.
    locked_before: bool,
    /// Whether the sleep still waits for the frame that shows the lock.
    awaiting: bool,
}

/// Start the session's power from `options`: the policy its link was made
/// with — the one the settings loader resolved — applied to the idle timers
/// now and again whenever the person changes their settings.
pub(crate) fn install(compositor: &mut Compositor, options: &Options) -> PowerRun {
    let waker = compositor.waker();
    let mut run = PowerRun::none();
    let watch = run
        .follow_settings(waker)
        .inspect_err(|error| tracing::info!(%error, "the settings are not followed"))
        .ok();
    run.watch = watch;
    let Some(link) = options.power.clone() else {
        return run;
    };
    link.bridge().set_signal_wake(Arc::new({
        let waker = compositor.waker();
        move || waker.wake_by_ref()
    }));
    // A hold appearing or lapsing is not a signal the service sends: it
    // changes what the loop applies to the compositor and what the bar
    // says, so the loop wakes to read it rather than waiting for the next
    // frame something else causes.
    link.registry().on_change({
        let waker = compositor.waker();
        move || waker.wake_by_ref()
    });
    apply(compositor, &link);
    run.link = Some(link);
    run
}

/// Run `link`'s policy from now on: the idle timers the compositor keeps.
fn apply(compositor: &mut Compositor, link: &PowerLink) {
    let policy = link.policy();
    compositor.set_idle_config(
        IdleConfig::never()
            .with_blank(policy.idle_blank)
            .with_lock(policy.idle_lock)
            .with_suspend(policy.idle_suspend),
    );
}

impl Session {
    /// Follow the machine's power and the person's settings: what holds it
    /// awake, what they changed, and what the service over logind asked.
    pub(crate) fn follow_power(&mut self) -> Result<(), Error> {
        // The loader's one reading: the compositor's assists apply in every
        // session, the machine's policy only where this session owns it.
        if self.machine.reload.swap(false, Ordering::AcqRel) {
            let next = self
                .machine
                .next
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(config) = next {
                a11y_settings::apply(&mut self.compositor, &config.a11y)?;
                if let Some(link) = &self.machine.link {
                    link.set_policy(config.power);
                    apply(&mut self.compositor, link);
                }
            }
        }
        let Some(link) = self.machine.link.clone() else {
            return Ok(());
        };
        // Nothing sleeps while something holds the machine, and nothing
        // dims, blanks or locks while an app says the person is watching: a
        // video, a download, a remote session.
        self.compositor.hold_awake(link.suspend_held());
        self.compositor.hold_idle(link.idle_held());
        for signal in link.bridge().drain_signals() {
            self.follow_power_signal(&link, signal)?;
        }
        Ok(())
    }

    /// Carry out one instruction from the service over logind.
    fn follow_power_signal(&mut self, link: &PowerLink, signal: PowerSignal) -> Result<(), Error> {
        match signal {
            PowerSignal::Lock => self.lock_for_power()?,
            PowerSignal::PreparingForSleep => {
                let locked_before = self.compositor.locked();
                // A sleep the idle timer asked for locks as the idle timer
                // does: its lock stays on this device, while the one a lid,
                // a key or the power service asked for is the person's own.
                let locked_by = if std::mem::take(&mut self.machine.idle_sleep) {
                    tilo_compositor::LockedBy::IdleTimer
                } else {
                    tilo_compositor::LockedBy::Person
                };
                self.compositor
                    .prepare_for_sleep(link.policy().lock_on_sleep, locked_by)?;
                self.machine.sleep = SleepLock {
                    prepared: true,
                    locked_before,
                    awaiting: false,
                };
                // logind's sleep delay waits for the frame that shows the
                // lock: the lock screen's window is started here, and the
                // session admits the machine may sleep once it drew
                // (`follow_lock`).
                self.follow_lock()?;
                if self.compositor.lock_drawn() {
                    link.bridge().lock_screen_drawn();
                } else {
                    self.machine.sleep.awaiting = true;
                }
                self.compositor.render()?;
            }
            PowerSignal::SuspendFailed => {
                let sleep = std::mem::take(&mut self.machine.sleep);
                self.machine.idle_sleep = false;
                self.compositor.cancel_suspend();
                if sleep.prepared && !sleep.locked_before {
                    self.compositor.unlock()?;
                    // With the lock lifted, the screen that showed it goes.
                    self.stop_lock_screen();
                    self.follow_lock()?;
                }
                tracing::info!("the machine did not sleep; the session stays awake");
            }
            PowerSignal::ResumedFromSleep => {
                self.compositor.resume_from_sleep()?;
                self.machine.sleep = SleepLock::default();
            }
            PowerSignal::IdleSuspendChanged(idle_suspend) => {
                // The machine's power profile or its source changed under the
                // person's policy: its idle-suspend timer is the one that
                // now applies.
                let mut policy = link.policy();
                policy.idle_suspend = idle_suspend;
                link.set_policy(policy);
                apply(&mut self.compositor, link);
            }
            PowerSignal::Notify(notice) => link.note_notice(&notice),
        }
        Ok(())
    }

    /// logind's sleep delay waits for the frame that shows the lock: the
    /// machine may sleep once the lock screen's own frame is in, and one
    /// that never came is a sleep logind's own delay ends.
    pub(crate) fn admit_sleep_if_locked(&mut self) {
        if !self.machine.sleep.awaiting || !self.compositor.lock_drawn() {
            return;
        }
        let Some(link) = &self.machine.link else {
            return;
        };
        self.machine.sleep.awaiting = false;
        link.bridge().lock_screen_drawn();
    }

    /// Lock the session for the machine: a lid, a critical battery, the
    /// power key. A session that cannot check a password never locks, and a
    /// lock asked of a locked session turns the display off instead, so the
    /// press always does something the person can see.
    fn lock_for_power(&mut self) -> Result<(), Error> {
        if !self.unlocking.verifier.available() {
            tracing::warn!("no password can be checked here: not locking");
            return Ok(());
        }
        if self.compositor.locked() {
            self.compositor.blank_now()?;
        } else {
            self.compositor.lock_now()?;
        }
        Ok(())
    }

    /// The idle timers ran out: sleep the machine, unless something took a
    /// hold since the tick that decided it.
    pub(crate) fn suspend_for_idle(&mut self) {
        if self.shared.hosted {
            tracing::info!("a session in a window does not suspend its host");
            return;
        }
        if let Some(link) = &self.machine.link {
            if link.suspend_held() {
                // A hold taken between the tick and now: the period restarts
                // from the hold, not from a sleep that never happened.
                self.compositor.cancel_suspend();
                return;
            }
            // The sleep is the idle timer's: its lock stays on this device,
            // as the idle timer's own lock does.
            self.machine.idle_sleep = true;
            if link.is_attached() {
                link.bridge().request_idle_suspend();
                return;
            }
        }
        // Without a service over logind (a machine without one) the
        // session's own action is what sleeping is.
        self.machine.idle_sleep = true;
        if let Err(error) = (self.shared.session_actions)(crate::SessionAction::Suspend) {
            tracing::warn!(%error, "suspend");
        }
    }
}
