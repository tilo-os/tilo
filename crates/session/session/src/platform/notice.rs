//! One notice a role tells the person, as this host shows it.
//!
//! The notice and the [`Notices`] seam are the session core's ([`tilo_session_core::notice`]):
//! what it says, where choosing it lands, and what the person chose. This is the page's own way
//! of showing one — the notifier its host serves — and the address an open of a notice's place is
//! carried at.

use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

pub use tilo_session_core::notice::{
    Closed, Notice, Notices, OPEN_ACTION, Place, SCHEME, open_target,
};

/// A host's notifier as a role's notices: the notification platform the toolkit serves, which is
/// the page's `ui::host::current().notifications`. What choosing one lands at comes back here;
/// nothing on the page reads why one closed, so it reports none.
pub struct Toolkit {
    notifier: Arc<dyn ui::notify::Notifier>,
    /// The places the person chose since this was last taken.
    chosen: Arc<Mutex<Vec<Place>>>,
}

impl Toolkit {
    /// The notices `notifier` shows, reporting every place the person chooses from one.
    #[must_use]
    pub fn new(notifier: Arc<dyn ui::notify::Notifier>, _tags: &str) -> Self {
        let chosen: Arc<Mutex<Vec<Place>>> = Arc::default();
        let reporting = Arc::clone(&chosen);
        ui::notify::set_activation_handler(SCHEME, move |uri| {
            if let Some(place) = Place::from_activation(uri) {
                reporting.lock_recover().push(place);
            }
        });
        Self { notifier, chosen }
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
        std::mem::take(&mut *self.chosen.lock_recover())
    }
}
