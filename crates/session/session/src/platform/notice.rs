//! One notice a role tells the person, as this host shows it.
//!
//! The notice and the [`Notices`] seam are the session core's ([`tilo_session_core::notice`]):
//! what it says, where choosing it lands, and what the person chose. This is the page's own way
//! of showing one — the notifier its host serves — and the address an open of a notice's place is
//! carried at.

use std::sync::{Arc, Mutex, PoisonError};

pub use tilo_session_core::notice::{
    Closed, Notice, Notices, OPEN_ACTION, Place, SCHEME, open_target,
};

/// A host's notifier as a role's notices: the notification platform the toolkit serves, which is
/// the page's `ui::host::current().notifications`.
pub struct Toolkit {
    notifier: Arc<dyn ui::notify::Notifier>,
    /// The places the person chose since this was last taken.
    chosen: Arc<Mutex<Vec<Place>>>,
    /// The notices that closed since this was last taken, each with why.
    closed: Arc<Mutex<Vec<(String, Closed)>>>,
}

impl Toolkit {
    /// The notices `notifier` shows under tags starting `tags`, reporting every place the person
    /// chooses from one and why every one of them closed.
    #[must_use]
    pub fn new(notifier: Arc<dyn ui::notify::Notifier>, tags: &str) -> Self {
        let chosen: Arc<Mutex<Vec<Place>>> = Arc::default();
        let reporting = Arc::clone(&chosen);
        ui::notify::set_activation_handler(SCHEME, move |uri| {
            if let Some(place) = Place::from_activation(uri) {
                lock(&reporting).push(place);
            }
        });
        let closed: Arc<Mutex<Vec<(String, Closed)>>> = Arc::default();
        let closing = Arc::clone(&closed);
        let prefix = tags.to_owned();
        ui::notify::set_close_handler(tags, move |tag, reason| {
            if tag.starts_with(&prefix) {
                lock(&closing).push((tag.to_owned(), closed_as(reason)));
            }
        });
        Self {
            notifier,
            chosen,
            closed,
        }
    }
}

/// Why the platform closed a notification, as a role reads it.
fn closed_as(reason: ui::notify::CloseReason) -> Closed {
    match reason {
        ui::notify::CloseReason::Expired => Closed::Expired,
        ui::notify::CloseReason::Dismissed => Closed::Dismissed,
        ui::notify::CloseReason::Closed => Closed::Withdrawn,
    }
}

impl Notices for Toolkit {
    fn show(&self, notice: &Notice) {
        if let Err(error) = self.notifier.show(&notice.notification()) {
            tracing::warn!(%error, tag = %notice.tag, "a notice was not shown");
        }
    }

    fn withdraw(&self, tag: &str) {
        if let Err(error) = self.notifier.withdraw(tag) {
            tracing::warn!(%error, %tag, "a notice was not taken back");
        }
    }

    fn take_chosen(&self) -> Vec<Place> {
        std::mem::take(&mut *lock(&self.chosen))
    }

    fn take_closed(&self) -> Vec<(String, Closed)> {
        std::mem::take(&mut *lock(&self.closed))
    }
}

fn lock<T>(held: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    held.lock().unwrap_or_else(PoisonError::into_inner)
}
