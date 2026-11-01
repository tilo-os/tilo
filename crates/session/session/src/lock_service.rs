//! The lock screen's state, published as the session's own resource: the
//! clock, what the device says about itself, what plays, and the answer to the
//! last password.
//!
//! The lock screen is a process of its own, so nothing here crosses an `Rc`
//! between the two: the session keeps the state, publishes it as [`LOCK`], and
//! the app reads it and invokes its actions as any app reaches a service. The
//! password is the screen's until the person asks to unlock: it is typed into
//! the screen's zeroizing field, and [`LockEffect::Unlock`] hands the session
//! what was typed, which checks it off the loop and publishes what came of it.
//!
//! The screen keeps two things of its own and fills them in as it shows them:
//! whether Caps Lock is on (only its own typing can tell) and whether it draws
//! its touch keyboard.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionOutput, DoorError, ResourceSpec};
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_sheets::lock::{LockEffect, Media, Shown, no_password};
use zeroize::{Zeroize, Zeroizing};

use crate::MEDIA;
use crate::panels;

/// The resource the lock screen reads and asks.
pub const LOCK: &str = "lock";

/// Its kind, as a reader with no words of its own knows it.
const LOCK_KIND: &str = "lock.screen";

/// How long the screen is given to dissolve once the password unlocked it:
/// leaving is an animation, and an animation never holds a session locked.
const DISSOLVE_LIMIT: Duration = Duration::from_secs(1);

/// What the lock screen shows and what it asked for. The session's loop says
/// what the device reads; the service hands that to the app and takes what it
/// asks.
#[derive(Clone, Default)]
pub struct Lock {
    shown: Rc<RefCell<Shown>>,
    /// What the screen typed, until it submits it.
    typed: Rc<RefCell<Zeroizing<String>>>,
    /// The passwords submitted since the session last checked, oldest first;
    /// each zeroes itself when dropped.
    attempts: Rc<RefCell<Vec<Zeroizing<String>>>>,
    /// What the screen asked beyond unlocking, oldest first.
    asked: Rc<RefCell<Vec<LockEffect>>>,
    /// The password was accepted, and when the screen started leaving.
    dissolving: Rc<Cell<Option<ui::time::Instant>>>,
    /// The screen has finished dissolving.
    gone: Rc<Cell<bool>>,
    /// The screen was told it may go: it is asked once, not every frame.
    told: Rc<Cell<bool>>,
    /// The session could not start the screen, and says so once instead of
    /// every pass.
    start_failed: Rc<Cell<bool>>,
}

impl Lock {
    /// Show `shown`, when it is not what shows already.
    pub fn show(&self, shown: Shown) {
        if *self.shown.borrow() == shown {
            return;
        }
        *self.shown.borrow_mut() = shown;
    }

    /// What the lock screen shows now.
    #[must_use]
    pub fn shown(&self) -> Shown {
        self.shown.borrow().clone()
    }

    /// The password did not unlock: say `why` and let the person try again.
    pub fn refuse(&self, why: String) {
        self.show(Shown {
            error: Some(why),
            checking: false,
            ..self.shown()
        });
    }

    /// Whether the session already failed to start the lock screen.
    #[must_use]
    pub fn start_failed(&self) -> bool {
        self.start_failed.get()
    }

    /// The session could not start the lock screen: it says so once.
    pub fn start_failed_now(&self) {
        self.start_failed.set(true);
    }

    /// The lock is lifted: forget what was typed and what was said.
    pub fn lifted(&self) {
        if let Ok(mut typed) = self.typed.try_borrow_mut() {
            typed.zeroize();
        }
        self.dissolving.set(None);
        self.gone.set(false);
        self.told.set(false);
        self.start_failed.set(false);
        self.show(Shown {
            error: None,
            checking: false,
            unlocking: false,
            ..self.shown()
        });
    }

    /// The password was accepted: the screen dissolves and says when it is
    /// gone with [`LockEffect::Dissolved`].
    pub fn unlocked(&self) {
        self.gone.set(false);
        self.told.set(false);
        self.dissolving.set(Some(ui::time::now()));
        self.show(Shown {
            error: None,
            checking: false,
            unlocking: true,
            ..self.shown()
        });
    }

    /// Whether the lock may be lifted now: the screen finished leaving, or it
    /// was given long enough to leave.
    #[must_use]
    pub fn dissolve_finished(&self) -> bool {
        let Some(since) = self.dissolving.get() else {
            return false;
        };
        self.gone.get()
            || ui::time::now()
                .checked_duration_since(since)
                .is_some_and(|leaving| leaving >= DISSOLVE_LIMIT)
    }

    /// The passwords given to unlock since the session last asked, oldest
    /// first; each zeroes itself when dropped.
    pub fn take_attempts(&self) -> Vec<Zeroizing<String>> {
        std::mem::take(&mut *self.attempts.borrow_mut())
    }

    /// What the screen asked of the session beyond unlocking — the next
    /// keyboard layout, a power action, the screen leaving — oldest first.
    pub fn take_asked(&self) -> Vec<LockEffect> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }

    /// Take in what the screen asked: the password it typed, or the attempt
    /// to unlock with what is in its field.
    fn take_in(&self, effect: &LockEffect) {
        match effect {
            LockEffect::TypePassword { password } => {
                if let Ok(mut typed) = self.typed.try_borrow_mut() {
                    typed.zeroize();
                    typed.push_str(password);
                }
            }
            LockEffect::Unlock => {
                let typed = std::mem::take(&mut *self.typed.borrow_mut());
                if typed.is_empty() {
                    self.refuse(no_password());
                    return;
                }
                self.attempts.borrow_mut().push(typed);
                self.show(Shown {
                    error: None,
                    checking: true,
                    ..self.shown()
                });
            }
            LockEffect::Dissolved => self.gone.set(true),
            _ => {}
        }
        self.asked.borrow_mut().push(effect.clone());
    }
}

/// The lock screen's state, as the session's services publish it.
pub struct LockService {
    state: Lock,
    /// What the screen was last shown, so a change of it is one revision.
    shown: Option<Shown>,
}

impl LockService {
    /// The service publishing `state`.
    #[must_use]
    pub fn new(state: Lock) -> Self {
        let shown = Some(state.shown());
        Self { state, shown }
    }
}

impl Service for LockService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<LockEffect>(ResourceSpec::new(
            LOCK,
            LOCK_KIND,
            "Lock screen",
        ))]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == LOCK).then(|| reading(&*self.state.shown.borrow()))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        if resource != LOCK {
            return Err(DoorError::Unknown(ActionAddress::new(resource, action)));
        }
        let published = self.resources();
        let effect = decode::<LockEffect>(&published, resource, action, arguments)?;
        self.state.take_in(&effect);
        Ok(Started::Done(ActionOutput::default()))
    }

    fn poll(&mut self) -> bool {
        let shown = self.state.shown();
        if self.shown.as_ref() == Some(&shown) {
            return false;
        }
        self.shown = Some(shown);
        true
    }
}

/// What plays, as the lock screen shows it: the player the now-playing card
/// follows, and only what a locked screen may do to it — no queue, no
/// scrubber, no choice of player.
#[must_use]
pub fn media(readings: &panels::Readings) -> Option<Media> {
    let readings = readings.borrow();
    let readings = readings.as_ref()?.current();
    let now = readings.now_playing.as_ref()?;
    Some(Media {
        title: now.title.clone(),
        subtitle: now.subtitle.clone(),
        playing: now.playing,
        can_toggle: now.can_toggle,
        can_previous: now.can_previous,
        can_next: now.can_next,
    })
}

/// Carry out what the screen's transport asked: play, pause, or the song
/// before or after this one. `false` when `asked` was not the transport's, so
/// the caller knows nothing was done.
///
/// A locked session withholds every published action but the screen's own, so
/// this cannot go through the door a media key uses: it is the session asking
/// the service that plays, straight. With nothing playing the screen shows no
/// transport, and nothing is asked of a player.
pub fn transport(
    asked: &LockEffect,
    readings: &panels::Readings,
    services: &Rc<RefCell<tilo_service::Services>>,
) -> bool {
    let action = match asked {
        LockEffect::PlayPause => "play-pause",
        LockEffect::NextTrack => "next",
        LockEffect::PreviousTrack => "previous",
        _ => return false,
    };
    if media(readings).is_none() {
        return true;
    }
    let asked = services
        .borrow_mut()
        .invoke(MEDIA, action, &serde_json::Map::new());
    match asked {
        Some(Ok(_)) => {}
        Some(Err(error)) => tracing::warn!(%error, "the lock screen's transport"),
        None => tracing::debug!("nothing publishes the player"),
    }
    true
}
