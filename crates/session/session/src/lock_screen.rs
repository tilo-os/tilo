//! The lock screen, as the session keeps it: the screen's app goes up when the person's lock locks
//! and comes down when it unlocks, is started again when it is gone, is given what to show, and is
//! given the passwords typed on it off the loop; one that unlocks lifts the lock. When the app
//! cannot show the lock at all — no such program, or one that will not stay up — the session draws
//! the same view itself ([`LockView`]), so a lock never lifts itself.

use tilo_compositor::Error;
use tilo_desktop_surface::notifications::{Notification, NotificationSeverity};
use tilo_sheets::Seat as _;

use crate::lock_service;
use crate::unlock;
use crate::{LOCK_SCREEN_TRIES, SessionAction};

/// The lock screen drawn in the session's own process, for the locks its app cannot show: the same
/// sheet the lock screen's app is, lent the same seat. What it asks goes to the session over a
/// channel of this process, and what the session publishes is written into the view it draws.
pub(crate) struct LockView {
    seat: tilo_sheets::lock::LockSeat,
    effects: std::sync::mpsc::Receiver<tilo_sheets::lock::LockEffect>,
}

impl LockView {
    /// The view of `seat` , taking what it asks here.
    pub(crate) fn new(seat: tilo_sheets::lock::LockSeat) -> Self {
        let (outgoing, effects) = std::sync::mpsc::channel();
        seat.ask_with(outgoing);
        Self { seat, effects }
    }

    /// What the view asked since the session last looked, oldest first.
    pub(crate) fn take_effects(&self) -> Vec<tilo_sheets::lock::LockEffect> {
        self.effects.try_iter().collect()
    }

    /// Put the view up on every display and show it `shown` , keeping what only the view can know —
    /// whether Caps Lock is on, and whether it draws its touch keyboard.
    fn open(&self, shown: tilo_sheets::lock::Shown) {
        let presentation = self.seat.presentation();
        if !presentation.is_open() {
            presentation.open(false);
        }
        self.seat.show(tilo_sheets::lock::Shown {
            caps_lock: self.seat.shown().caps_lock,
            keyboard: self.seat.shown().keyboard,
            ..shown
        });
    }

    /// The view has left the screen: nothing of it is drawn.
    fn close(&self) {
        if self.seat.presentation().is_drawn() {
            self.seat.presentation().gone();
        }
    }
}

/// What shows the lock while the session is locked: the lock screen's app, or the session's own
/// view when that app cannot show it at all.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum LockShown {
    /// The lock screen's app shows the lock.
    #[default]
    App,
    /// The app cannot show the lock: the session draws the same view itself.
    View,
}

impl crate::Session {
    /// Carry out what the compositor's session did since the last pass: a lock puts the screen up
    /// and tells the person's other devices of a lock they took here, an unlock takes it down, and
    /// the idle timers' suspend goes to the system.
    fn follow_session_transitions(&mut self, lock: &lock_service::Lock) {
        use tilo_compositor::SessionTransition;
        for event in self.compositor.take_session_events() {
            match event {
                SessionTransition::Locked => {
                    // The idle timer's lock, and one this device followed from another, stay here:
                    // only the person's own lock is told to their other devices.
                    if self.compositor.lock_by_idle() {
                        self.shared.owner_session.locked_by_idle();
                    } else if !self.shared.owner_session.followed_lock() {
                        self.shared
                            .owner_session
                            .changed_by_person(tilo_clock::unix_now_ms());
                    }
                    // The lock screen shows afresh, and is started when the session runs none.
                    self.lock_starts = 0;
                    self.lock_shown = LockShown::App;
                    lock.lifted();
                    self.start_lock_screen();
                }
                SessionTransition::Unlocked => {
                    // The person unlocked here, whether they typed it or another device of theirs
                    // did: their other devices are told.
                    self.shared
                        .owner_session
                        .changed_by_person(tilo_clock::unix_now_ms());
                    self.lock_starts = 0;
                    self.lock_shown = LockShown::App;
                    lock.lifted();
                    // The lock ends with the screen that showed it: one that is still up goes, and
                    // the compositor lifts the lock with it.
                    self.stop_lock_screen();
                }
                SessionTransition::Suspend => {
                    // The idle timers ran out: the machine sleeps, unless something holds it awake.
                    // A session in a window of another desktop runs no idle timers, and never
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
    }

    /// Follow the session's lock: put the lock screen up when it locks and down when it unlocks,
    /// start it again when it is gone, and draw the same view here when the app cannot show the
    /// lock at all; keep what it shows current, carry out what it asks, check the passwords it was
    /// given off the loop, and lift the lock for one that unlocks. Nothing but an accepted password
    /// lifts a lock. A suspend the idle timers asked for goes to the system.
    pub(crate) fn follow_lock(&mut self) -> Result<(), Error> {
        let lock = self.shared.lock.clone();
        self.follow_session_transitions(&lock);
        // What the session's own view asked goes in first: it asks the same things the app does,
        // over the same path, so the same check answers it.
        if let Some(view) = &self.lock_view {
            for effect in view.take_effects() {
                lock.take_in(&effect);
            }
        }
        // What the screen asked of the session besides unlocking.
        for asked in lock.take_asked() {
            self.answer_lock(&asked);
        }
        // A locked session is shown by its lock screen: the person's own lock, the idle timer and
        // the machine going to sleep all lock it here, and one place starts what shows it. Nothing
        // but the screen offers an action while it is up. A lock a client put up of its own is that
        // client's, and ends with it.
        let locked = self.compositor.locked();
        self.shared.doors.locked(locked);
        if self.compositor.session_locked()
            && !lock.shown().unlocking
            && self.lock_shown == LockShown::App
            && let Some(name) = self.lock_screen().map(|surface| surface.name.clone())
            && !self.surfaces.running(&name)
        {
            // A screen that will not stay up is not one the session can lean on: after a few starts
            // the session draws the lock itself, rather than leave it with nothing able to unlock
            // it.
            if self.lock_starts >= LOCK_SCREEN_TRIES {
                self.show_lock_view("the lock screen would not stay up");
            } else {
                self.start_lock_screen();
            }
        }
        // The session's own view stands on every display while it shows the lock, and leaves with
        // the lock.
        if let Some(view) = &self.lock_view {
            if self.compositor.session_locked() && self.lock_shown == LockShown::View {
                view.open(lock.shown());
            } else if !self.compositor.session_locked() {
                view.close();
            }
        }
        // The machine may sleep once the lock screen's frame is in.
        self.admit_sleep_if_locked();
        lock.show(tilo_sheets::lock::Shown {
            clock: ui::clock::time_of_day(&ui::clock::local_now()),
            date: ui::format::day_heading(ui::clock::today(), false),
            person: self.unlocking.user.clone(),
            status: self.lock_status(),
            media: lock_service::media(
                &self.shared.readings,
                self.shared.handoff.borrow().as_ref(),
            ),
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
        // goes with the lock. A screen that is not up has nothing to leave,
        // and the lock lifts at once rather than waiting on an animation
        // nothing draws.
        if lock.shown().unlocking {
            let shown_by = self
                .lock_screen()
                .is_some_and(|surface| self.surfaces.running(&surface.name))
                || (self.lock_shown == LockShown::View && self.lock_view.is_some());
            if shown_by && !lock.dissolve_finished() {
                // Keep the loop turning while the screen leaves; it is one short animation, and
                // nothing else is going on.
                self.unlocking.wake.wake_by_ref();
                return Ok(());
            }
            self.compositor.unlock()?;
            // An unlock lands as a transition: follow it at once.
            return self.follow_lock();
        }
        Ok(())
    }

    /// The person's session was unlocked — here with their password, or on another of their
    /// devices: the screen that showed the lock leaves, and the compositor lifts the lock once it
    /// has, so a lock followed from elsewhere comes down the way one taken here does.
    pub(crate) fn unlock_by_person(&mut self) -> Result<(), Error> {
        self.shared.lock.unlocked();
        self.follow_lock()
    }

    /// The lock screen's app cannot show the lock `reason` : the session draws the same view itself
    /// from now on, and tells the person once why the screen they see is not the app's.
    fn show_lock_view(&mut self, reason: &'static str) {
        if self.lock_shown == LockShown::View {
            return;
        }
        self.lock_shown = LockShown::View;
        if self.lock_view.is_some() {
            tracing::error!(
                reason,
                "the lock screen cannot show the lock: the session draws it"
            );
        } else {
            tracing::error!(
                reason,
                "the lock screen cannot show the lock, and the session has no view of its own: the lock stays"
            );
        }
        self.notices().post(
            Notification::new(
                crate::i18n::t("lock-screen-failed-title"),
                crate::i18n::t("lock-screen-failed-body"),
            )
            .with_severity(NotificationSeverity::Error),
        );
    }

    /// Start the lock screen, if it does not run: a locked session is shown by its screen, whoever
    /// locked it, and the screen is what takes the password that lifts the lock. One that is not
    /// installed, or will not start, is one the session draws itself instead.
    pub(crate) fn start_lock_screen(&mut self) {
        let Some(surface) = self.lock_screen().cloned() else {
            self.show_lock_view("no lock screen is installed");
            return;
        };
        if self.surfaces.running(&surface.name) {
            return;
        }
        self.lock_starts = self.lock_starts.saturating_add(1);
        if let Err(error) = self.start_surface(&surface) {
            tracing::warn!(%error, "the lock screen did not start");
            self.show_lock_view("the lock screen did not start");
        }
    }

    /// Stop the lock screen, if it runs: the compositor lifts the lock with the client that held
    /// it.
    pub(crate) fn stop_lock_screen(&mut self) {
        if let Some(name) = self.lock_screen().map(|surface| surface.name.clone()) {
            self.surfaces.stop(&name);
        }
    }

    /// What the device says about itself, as the lock screen shows it.
    pub(crate) fn lock_status(&self) -> tilo_sheets::lock::Status {
        let readings = self.shared.readings.borrow();
        tilo_sheets::lock::Status {
            link: readings.link(),
            battery: readings
                .charge()
                .map(|(percent, charging)| tilo_sheets::lock::Battery {
                    percent: Some(percent),
                    charging,
                }),
            layout: self.lock_layout(self.shared.lock_layout.get()),
            next_layout: self.next_lock_layout(),
            layouts: self.shared.lock_layouts.len(),
            // The screen draws its own keyboard: a touch-only device can always ask for it.
            keyboard: true,
            power: !self.shared.hosted,
            // What waits behind the unlock, as the bar's bell counts it: a locked machine still
            // says the person has news.
            notifications: self.shared.notifications.borrow().desktop().unread(),
        }
    }

    /// The keyboard layout at `index` , as the lock screen names it (`DE`).
    pub(crate) fn lock_layout(&self, index: usize) -> Option<String> {
        let source = self.shared.lock_layouts.get(index)?;
        Some(source.split(':').next().unwrap_or(source).to_uppercase())
    }

    /// The layout the screen's switch goes to, the way its button says where it leads.
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

    /// Carry out what the lock screen asked of the session besides unlocking.
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
                crate::controls::apply_keyboard(
                    &mut self.compositor,
                    &tilo_compositor::KeyboardLayout {
                        layout,
                        variant,
                        ..tilo_compositor::KeyboardLayout::default()
                    },
                );
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
