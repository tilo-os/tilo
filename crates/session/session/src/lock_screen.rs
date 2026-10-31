//! The lock screen, as the session keeps it: the screen goes up when the
//! person's lock locks and comes down when it unlocks, is drawn again when its
//! app or host is gone, keeps its clock current, is given the passwords typed
//! on it off the loop, and lifts the lock for one that unlocks.

use std::time::Instant;

use tilo_compositor::Error;
use tilo_sheets::Seat as _;

use crate::mount;
use crate::unlock;
use crate::{LOCK_SCREEN_GRACE, LOCK_SCREEN_TRIES, LockWatch, SessionAction};

impl crate::Session {
    /// Follow the session's lock: put the lock screen up when it locks
    /// and down when it unlocks, put it back when its app or the host
    /// drawing it is gone, keep its clock current, check the passwords it
    /// was given off the loop, and lift the lock for one that unlocks. A
    /// suspend the idle timers asked for goes to the system.
    pub(crate) fn follow_lock(&mut self) -> Result<(), Error> {
        use tilo_compositor::SessionTransition;
        for event in self.compositor.take_session_events() {
            match event {
                SessionTransition::Locked => {
                    // The idle timer's lock, and one this device followed
                    // from another, stay here: only the person's own lock is
                    // told to their other devices.
                    if self.compositor.lock_by_idle() {
                        self.shared.owner_session.locked_by_idle();
                    } else if !self.shared.owner_session.followed_lock() {
                        self.shared
                            .owner_session
                            .changed_by_person(tilo_clock::unix_now_ms());
                    }
                    // The lock screen shows afresh, and is mounted when the
                    // session has none.
                    if let Some(lock) = &self.shared.lock {
                        lock.lifted();
                    }
                    self.raise_lock_screen();
                }
                SessionTransition::Unlocked => {
                    self.shared
                        .owner_session
                        .changed_by_person(tilo_clock::unix_now_ms());
                    if let Some(lock) = self.shared.lock.clone() {
                        lock.lifted();
                        lock.presentation().close();
                        lock.presentation().gone();
                    }
                }
                SessionTransition::Suspend => {
                    // The idle timers ran out: the machine sleeps, unless
                    // something holds it awake. A session in a window of
                    // another desktop runs no idle timers, and never
                    // suspends the host that desktop owns.
                    if self.shared.hosted {
                        tracing::info!("a session in a window does not suspend its host");
                    } else {
                        self.suspend_for_idle();
                    }
                }
                _ => {}
            }
        }
        self.keep_lock_screen()?;
        let Some(lock) = self
            .shared
            .lock
            .clone()
            .filter(|lock| lock.presentation().is_open())
        else {
            return Ok(());
        };
        // What the screen asked of the session besides unlocking.
        for asked in lock.take_asked() {
            self.answer_lock(&asked);
        }
        lock.show(tilo_sheets::lock::Shown {
            clock: ui::clock::time_of_day(&ui::clock::local_now()),
            date: ui::format::day_heading(ui::clock::today(), false),
            person: self.unlocking.user.clone(),
            status: self.lock_status(),
            media: mount::lock::media(&self.shared.readings),
            ..lock.shown()
        });
        for password in lock.take_attempts() {
            let verifier = std::sync::Arc::clone(&self.unlocking.verifier);
            let user = self.unlocking.user.clone();
            let verdicts = self.unlocking.verdicts.0.clone();
            let wake = self.unlocking.wake.clone();
            let checking = std::thread::Builder::new()
                .name("tilo-unlock".into())
                .spawn(move || {
                    let _ = verdicts.send(verifier.verify(&user, &password));
                    wake.wake();
                });
            if let Err(error) = checking {
                tracing::warn!(%error, "the password could not be checked");
                lock.refuse(tilo_sheets::lock::unavailable());
            }
        }
        while let Ok(verdict) = self.unlocking.verdicts.1.try_recv() {
            match verdict {
                unlock::Verdict::Unlocks => lock.unlocking(),
                unlock::Verdict::Wrong => lock.refuse(tilo_sheets::lock::wrong_password()),
                unlock::Verdict::Unavailable => lock.refuse(tilo_sheets::lock::unavailable()),
            }
        }
        // The screen leaves before the desk comes back: lift the lock once it
        // has dissolved, or once it has had long enough to.
        if lock.shown().unlocking {
            if !lock.dissolve_finished() {
                // Keep the loop turning while the screen leaves; it is one
                // short animation, and nothing else is going on.
                self.unlocking.wake.wake_by_ref();
                return Ok(());
            }
            self.compositor.unlock()?;
            // An unlock lands as a transition: follow it at once.
            return self.follow_lock();
        }
        Ok(())
    }

    /// Keep the lock screen on the screen while the session is locked. The
    /// compositor draws nothing but the lock screen while it is locked, so a
    /// screen whose app or host is gone leaves the desk's background showing
    /// and no way to unlock: it is put up again, and a screen that will not
    /// stay up lifts the lock rather than leave the machine locked with
    /// nothing able to unlock it.
    pub(crate) fn keep_lock_screen(&mut self) -> Result<(), Error> {
        // A lock client's lock is the client's: its surfaces are what it
        // shows, and nothing of the session's is put over them.
        if !self.compositor.session_locked() {
            self.lock_watch = LockWatch::default();
            return Ok(());
        }
        if self.compositor.lock_screen_drawn() {
            // The screen is there. The tries it took to get it back stand for
            // this lock; the next one starts afresh.
            self.lock_watch.gone_since = None;
            return Ok(());
        }
        // Nothing of the lock screen is on the screen. The frame that
        // follows putting it up is drawn in the same turn, so this is only
        // believed once it has been so for a moment.
        let Some(gone_since) = self.lock_watch.gone_since else {
            self.lock_watch.gone_since = Some(Instant::now());
            return Ok(());
        };
        if gone_since.elapsed() < LOCK_SCREEN_GRACE {
            return Ok(());
        }
        if self.lock_watch.raised >= LOCK_SCREEN_TRIES {
            tracing::error!(
                raised = self.lock_watch.raised,
                "the lock screen would not stay up: the lock is lifted rather than left with no way to unlock it"
            );
            self.lock_watch = LockWatch::default();
            self.compositor.unlock()?;
            // An unlock lands as a transition: follow it at once.
            return self.follow_lock();
        }
        self.lock_watch.raised = self.lock_watch.raised.saturating_add(1);
        self.lock_watch.gone_since = None;
        self.raise_lock_screen();
        Ok(())
    }

    /// Put the lock screen up: its presentation open, and its sheet mounted
    /// when the session has none. The one way a screen reaches the locked
    /// session, for the lock that starts and for one that comes back.
    pub(crate) fn raise_lock_screen(&mut self) {
        if let Some(lock) = self.shared.lock.clone() {
            // What was typed into it and what it was asked stay as they
            // were: only the showing is asked for again.
            lock.presentation().open(true);
            return;
        }
        let mounted = self
            .shared
            .lock_mount
            .mount(&mut self.compositor, &mut self.shared.doors);
        self.shared.lock = mounted;
    }

    /// What the device says about itself, as the lock screen shows it.
    pub(crate) fn lock_status(&self) -> tilo_sheets::lock::Status {
        let readings = self
            .shared
            .readings
            .borrow()
            .as_ref()
            .map(tilo_quick_settings::host::Feed::current);
        let Some(readings) = readings else {
            return tilo_sheets::lock::Status {
                layout: self.lock_layout(self.shared.lock_layout.get()),
                next_layout: self.next_lock_layout(),
                layouts: self.shared.lock_layouts.len(),
                keyboard: true,
                power: self.shared.lock_power,
                ..tilo_sheets::lock::Status::default()
            };
        };
        let status = &readings.status;
        tilo_sheets::lock::Status {
            network: status.link.as_ref().and_then(|link| link.name.clone()),
            battery: status
                .battery
                .filter(|battery| battery.present)
                .map(|battery| tilo_sheets::lock::Battery {
                    percent: Some(battery.percent),
                    charging: status.battery_is_charging(),
                }),
            layout: self.lock_layout(self.shared.lock_layout.get()),
            next_layout: self.next_lock_layout(),
            layouts: self.shared.lock_layouts.len(),
            // The screen draws its own keyboard: a touch-only device can
            // always ask for it.
            keyboard: true,
            power: self.shared.lock_power,
        }
    }

    /// The keyboard layout at `index`, as the lock screen names it (`DE`).
    pub(crate) fn lock_layout(&self, index: usize) -> Option<String> {
        let source = self.shared.lock_layouts.get(index)?;
        Some(source.split(':').next().unwrap_or(source).to_uppercase())
    }

    /// The layout the screen's switch goes to, the way its button says where
    /// it leads.
    pub(crate) fn next_lock_layout(&self) -> Option<String> {
        let count = self.shared.lock_layouts.len();
        let next = self
            .shared
            .lock_layout
            .get()
            .checked_add(1)
            .filter(|next| *next < count)
            .unwrap_or(0);
        self.lock_layout(next)
    }

    /// Carry out what the lock screen asked of the session besides
    /// unlocking.
    pub(crate) fn answer_lock(&mut self, asked: &tilo_sheets::lock::LockEffect) {
        use tilo_sheets::lock::LockEffect;
        match asked {
            LockEffect::NextLayout => {
                let count = self.shared.lock_layouts.len();
                if count == 0 {
                    return;
                }
                let index = self.shared.lock_layout.get();
                let next = index
                    .checked_add(1)
                    .filter(|next| *next < count)
                    .unwrap_or(0);
                let Some(source) = self.shared.lock_layouts.get(next).cloned() else {
                    return;
                };
                self.shared.lock_layout.set(next);
                let (layout, variant) = match source.split_once(':') {
                    Some((layout, variant)) => (layout.to_owned(), variant.to_owned()),
                    None => (source, String::new()),
                };
                self.compositor
                    .set_keyboard_layout(&tilo_compositor::KeyboardLayout {
                        layout,
                        variant,
                        ..tilo_compositor::KeyboardLayout::default()
                    });
            }
            LockEffect::PlayPause | LockEffect::NextTrack | LockEffect::PreviousTrack => {
                mount::lock::transport(asked, &self.shared.readings, &self.shared.services);
            }
            LockEffect::Suspend => self.session_action(SessionAction::Suspend),
            LockEffect::Restart => self.session_action(SessionAction::Restart),
            LockEffect::ShutDown => self.session_action(SessionAction::ShutDown),
            // What was typed is the field's until the screen asks to unlock,
            // and the keyboard it draws is the screen's to show or put away.
            LockEffect::TypePassword { .. } | LockEffect::Unlock | LockEffect::ShowKeyboard => {}
        }
    }
}
