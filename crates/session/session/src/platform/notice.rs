//! One notice a role tells the person, and where choosing it lands.
//!
//! A role that knows something the person should know — a mention of them in a record, a change
//! they asked to hear of — tells it through [`Notices`]: a tag it shows under, whether the notice
//! is showing already, and the [`Place`] choosing it opens. What a host does with it is the
//! host's: the native session posts it to the person's notifications, the page hands it to the
//! notifier its host serves.
//!
//! What the person chose comes back through the same seam ([`Notices::take_chosen`]), so a role
//! never reads a platform's notification log. Opening the place is the host's own door, as any
//! other open is: [`open_target`] is the address it is opened at.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, RemoteActionTarget, application_record_target,
};

/// Where choosing a notice lands, and the URI a host carries it by.
pub use ui::mentions::{Place, SCHEME};

/// The request that opens `place` in `workspace` as the person: the app's `open` of its item on
/// the record the item is in.
#[must_use]
pub fn open_target(workspace: &str, place: &Place) -> RemoteActionTarget {
    if place.record.is_empty() {
        tilo_actions::remote::application_action_target(
            workspace,
            &place.app,
            place.resource.clone(),
            APPLICATION_OPEN_ACTION,
        )
    } else {
        application_record_target(
            workspace,
            &place.app,
            &place.record,
            place.resource.clone(),
            APPLICATION_OPEN_ACTION,
        )
    }
}

/// One notice to tell the person: what it says, and what choosing it opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The name it shows under: one per notice, so telling it twice tells it once and taking it
    /// back names it.
    pub tag: String,
    /// The name the person sees it came from.
    pub app: String,
    pub title: String,
    pub body: String,
    /// Where choosing it lands.
    pub place: Place,
}

impl Notice {
    /// The notification a platform notifier shows for it: what the person clicks carries the
    /// place's activation.
    #[must_use]
    pub fn notification(&self) -> ui::notify::Notification {
        ui::notify::Notification::new(&self.tag, &self.title, &self.body)
            .with_activation(self.place.activation())
    }
}

/// Showing a role's notices on one host, and taking back what the person chose.
pub trait Notices {
    /// Show `notice`, replacing one showing under its tag.
    fn show(&self, notice: &Notice);
    /// Take the notice under `tag` back: what it told is gone.
    fn withdraw(&self, tag: &str);
    /// The notices the person chose since this was last taken, each with its tag.
    fn take_chosen(&self) -> Vec<(String, Place)>;
}

/// A host's notifier as a role's notices: the notification platform the toolkit serves, which is
/// the page's `ui::host::current().notifications`.
pub struct Toolkit {
    notifier: Arc<dyn ui::notify::Notifier>,
    /// What is showing, by tag: the place a click on it names.
    showing: Arc<Mutex<BTreeMap<String, Place>>>,
    /// What the person chose since this was last taken.
    chosen: Arc<Mutex<Vec<(String, Place)>>>,
}

impl Toolkit {
    /// The notices `notifier` shows, reporting every place the person chooses from one.
    #[must_use]
    pub fn new(notifier: Arc<dyn ui::notify::Notifier>) -> Self {
        let showing: Arc<Mutex<BTreeMap<String, Place>>> = Arc::default();
        let chosen: Arc<Mutex<Vec<(String, Place)>>> = Arc::default();
        let held = Arc::clone(&showing);
        let reporting = Arc::clone(&chosen);
        ui::notify::set_activation_handler(SCHEME, move |uri| {
            let Some(place) = Place::from_activation(uri) else {
                return;
            };
            let tag = lock(&held)
                .iter()
                .find(|(_, shown)| **shown == place)
                .map(|(tag, _)| tag.clone())
                .unwrap_or_default();
            lock(&reporting).push((tag, place));
        });
        Self {
            notifier,
            showing,
            chosen,
        }
    }
}

impl Notices for Toolkit {
    fn show(&self, notice: &Notice) {
        // The tag first: a click may be reported before the platform confirms the show.
        lock(&self.showing).insert(notice.tag.clone(), notice.place.clone());
        if let Err(error) = self.notifier.show(&notice.notification()) {
            tracing::warn!(%error, tag = %notice.tag, "a notice was not shown");
        }
    }

    fn withdraw(&self, tag: &str) {
        lock(&self.showing).remove(tag);
        if let Err(error) = self.notifier.withdraw(tag) {
            tracing::warn!(%error, %tag, "a notice was not taken back");
        }
    }

    fn take_chosen(&self) -> Vec<(String, Place)> {
        std::mem::take(&mut *lock(&self.chosen))
    }
}

fn lock<T>(held: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    held.lock().unwrap_or_else(PoisonError::into_inner)
}
