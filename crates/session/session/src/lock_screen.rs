//! The lock screen, as the session keeps it: the screen's app goes up when the
//! person's lock locks and comes down when it unlocks, is started again when it
//! is gone, is given what to show, and is given the passwords typed on it off
//! the loop; one that unlocks lifts the lock.

use tilo_compositor::Error;

use crate::lock_service;
use crate::unlock;
use crate::{LOCK_SCREEN_TRIES, SessionAction};

impl crate::Session {
    /// Follow the session's lock: put the lock screen up when it locks and
    /// down when it unlocks, start it again when it is gone, keep what it
    /// shows current, carry out what it asks, check the passwords it was
    /// given off the loop, and lift the lock for one that unlocks. A suspend
    /// the idle timers asked for goes to the system.
    pub(crate) fn follow_lock(&mut self) -> Result<(), Error> {
        use tilo_compositor::SessionTransition;
        let lock = self.shared.lock.clone();
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
                    // The lock screen shows afresh, and is started when the
                    // session runs none.
                    self.lock_starts = 0;
                    lock.lifted();
                    self.start_lock_screen();
                }
                SessionTransition::Unlocked => {
                    // The person unlocked here, whether they typed it or
                    // another device of theirs did: their other devices are
                    // told.
                    self.shared
                        .owner_session
                        .changed_by_person(tilo_clock::unix_now_ms());
                    self.lock_starts = 0;
                    lock.lifted();
                    // The lock ends with the screen that showed it: one that
                    // is still up goes, and the compositor lifts the lock
                    // with it.
                    self.stop_lock_screen();
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
        // What the screen asked of the session besides unlocking.
        for asked in lock.take_asked() {
            self.answer_lock(&asked);
        }
        // A locked session is shown by its lock screen: the person's own
        // lock, the idle timer and the machine going to sleep all lock it
        // here, and one place starts what shows it. Nothing but the screen
        // offers an action while it is up.
        let locked = self.compositor.locked();
        self.shared.doors.locked(locked);
        // The screen shows the session's own lock. A lock a client put up of
        // its own is that client's, and ends with it.
        if self.compositor.session_locked()
            && !lock.shown().unlocking
            && let Some(name) = self.lock_screen().map(|surface| surface.name.clone())
            && !self.surfaces.running(&name)
        {
            // A screen that will not stay up leaves the machine locked with
            // nothing able to unlock it: the lock is lifted instead.
            if self.lock_starts >= LOCK_SCREEN_TRIES {
                tracing::error!(
                    starts = self.lock_starts,
                    "the lock screen would not stay up: the lock is lifted rather than left with nothing to unlock it"
                );
                self.lock_starts = 0;
                self.compositor.unlock()?;
                return self.follow_lock();
            }
            self.start_lock_screen();
        }
        // The machine may sleep once the lock screen's frame is in.
        self.admit_sleep_if_locked();
        lock.show(tilo_sheets::lock::Shown {
            clock: ui::clock::time_of_day(&ui::clock::local_now()),
            date: ui::format::day_heading(ui::clock::today(), false),
            person: self.unlocking.user.clone(),
            status: self.lock_status(),
            media: lock_service::media(&self.shared.readings),
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
                unlock::Verdict::Unlocks => lock.unlocked(),
                unlock::Verdict::Wrong => lock.refuse(tilo_sheets::lock::wrong_password()),
                unlock::Verdict::Unavailable => lock.refuse(tilo_sheets::lock::unavailable()),
            }
        }
        // The screen leaves before the desk comes back: the lock ends once it
        // has dissolved, or once it has had long enough to, and the screen
        // goes with the lock.
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

    /// Start the lock screen, if it does not run: a locked session is shown by
    /// its screen, whoever locked it, and the screen is what takes the
    /// password that lifts the lock. Without one there would be a locked
    /// session nobody could unlock, so the session says so once instead of
    /// asking every pass.
    pub(crate) fn start_lock_screen(&mut self) {
        let lock = self.shared.lock.clone();
        if lock.start_failed() {
            return;
        }
        let Some(surface) = self.lock_screen().cloned() else {
            lock.start_failed_now();
            tracing::warn!("no lock screen is installed: the session locks with nothing to show");
            return;
        };
        if self.surfaces.running(&surface.name) {
            return;
        }
        self.lock_starts = self.lock_starts.saturating_add(1);
        if let Err(error) = self.start_surface(&surface) {
            lock.start_failed_now();
            tracing::warn!(%error, "the lock screen did not start");
        }
    }

    /// Stop the lock screen, if it runs: the compositor lifts the lock with
    /// the client that held it.
    pub(crate) fn stop_lock_screen(&mut self) {
        if let Some(name) = self.lock_screen().map(|surface| surface.name.clone()) {
            self.surfaces.stop(&name);
        }
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
                lock_service::transport(asked, &self.shared.readings, &self.shared.services);
            }
            LockEffect::Suspend => self.session_action(SessionAction::Suspend),
            LockEffect::Restart => self.session_action(SessionAction::Restart),
            LockEffect::ShutDown => self.session_action(SessionAction::ShutDown),
            // What was typed is the field's until the screen asks to unlock,
            // what the field held is taken when it does, the keyboard it
            // draws is the screen's to show or put away, and it says itself
            // when it has left.
            LockEffect::TypePassword { .. }
            | LockEffect::Unlock
            | LockEffect::ShowKeyboard
            | LockEffect::Dissolved => {}
        }
    }
}
