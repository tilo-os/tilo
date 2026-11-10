//! One notice a role tells the person, and where choosing it lands.
//!
//! A role that knows something the person should know — a mention of them in a record, a
//! filesystem running out — tells it as one [`Notice`] through [`Notices`]. What a host does with
//! it is the host's: the native session posts it to the person's notifications, the page hands it
//! to the notifier its host serves. What the person chose comes back through the same seam, so a
//! role never reads a platform's notification log.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use settings_store::system_sounds::SystemSoundEvent;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, RemoteActionTarget, application_action_target,
    application_record_target,
};

pub use ui::mentions::{Place, SCHEME};

/// The name a notice that opens something offers that choice under.
pub const OPEN_ACTION: &str = "open";

/// Where choosing `place` lands in `workspace`: the app's `open` of the item on the record it is
/// in.
#[must_use]
pub fn open_target(workspace: &str, place: &Place) -> RemoteActionTarget {
    if place.record.is_empty() {
        application_action_target(
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

/// The tag of the next notice told under no name of its own: one per telling, so the same words
/// told twice are two notices.
static RAISED: AtomicU64 = AtomicU64::new(1);

/// One notice to tell the person: what it says, and what choosing it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The name it shows under: one per notice, so telling it twice tells it once and taking it
    /// back names it.
    pub tag: String,
    /// The name the person sees it came from ("Storage").
    pub app: String,
    /// `0` low, `1` normal, `2` critical: what the banner and the centre order it by.
    pub urgency: u8,
    pub title: String,
    pub body: String,
    /// Where choosing it lands, where it opens something ([`Self::opens`]).
    pub place: Option<Place>,
    /// `(key, label)` pairs, as a notification's actions.
    pub actions: Vec<(String, String)>,
    /// How long it stays on screen; `None` leaves that to the person's own notification settings.
    pub expires_after: Option<Duration>,
    /// Whether every display shows it, or only the display the person is using.
    pub machine_wide: bool,
    /// The event sound it makes, played by the host that shows it: a sender's category never
    /// reaches the one an app may not ask for ([`SystemSoundEvent::LowBattery`]).
    pub sound: Option<SystemSoundEvent>,
}

impl Notice {
    /// A notice `app` tells the person at `urgency`, about the whole machine.
    #[must_use]
    pub fn new(
        app: impl Into<String>,
        urgency: u8,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            tag: format!("notice:{}", RAISED.fetch_add(1, Ordering::Relaxed)),
            app: app.into(),
            urgency,
            title: title.into(),
            body: body.into(),
            place: None,
            actions: Vec::new(),
            expires_after: None,
            machine_wide: true,
            sound: None,
        }
    }

    /// The notice of `place`: what `app` says of it, and where choosing it lands.
    #[must_use]
    pub fn opens(
        app: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
        place: Place,
    ) -> Self {
        Self {
            place: Some(place),
            ..Self::new(app, 1, title, body)
        }
    }

    /// Told under `tag`, so a later telling of the same thing replaces it.
    #[must_use]
    pub fn tagged(mut self, tag: impl Into<String>) -> Self {
        self.tag = tag.into();
        self
    }

    /// Offering the person `actions`, as `(key, label)` pairs.
    #[must_use]
    pub fn choosing(mut self, actions: Vec<(String, String)>) -> Self {
        self.actions = actions;
        self
    }

    /// Staying on screen `after`, or until the person lets it go for `Some(Duration::ZERO)`.
    #[must_use]
    pub fn staying(mut self, after: Option<Duration>) -> Self {
        self.expires_after = after;
        self
    }

    /// Shown on the display the person is using, rather than on every display.
    #[must_use]
    pub fn here(mut self) -> Self {
        self.machine_wide = false;
        self
    }

    /// Making `event`'s sound as it is shown.
    #[must_use]
    pub fn sounding(mut self, event: SystemSoundEvent) -> Self {
        self.sound = Some(event);
        self
    }

    /// The notification a host's own notifier shows for it: what the person clicks carries the
    /// place's activation.
    #[must_use]
    pub fn notification(&self) -> ui::notify::Notification {
        let notification = ui::notify::Notification::new(&self.tag, &self.title, &self.body);
        match &self.place {
            Some(place) => notification.with_activation(place.activation()),
            None => notification,
        }
    }
}

/// Why a notice the person was shown is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    /// It ran out of time.
    Expired,
    /// The person dismissed it.
    Dismissed,
    /// The role took it back: what it told is gone.
    Withdrawn,
}

/// Showing a role's notices on one host, and taking back what the person chose.
pub trait Notices {
    /// Show `notice`, replacing one showing under its tag.
    fn show(&self, notice: &Notice);

    /// Take the notice under `tag` back: what it told is gone.
    fn withdraw(&self, tag: &str);

    /// The places the person chose since this was last taken.
    fn take_chosen(&self) -> Vec<Place> {
        Vec::new()
    }

    /// The notices that closed since this was last taken, each with why.
    fn take_closed(&self) -> Vec<(String, Closed)> {
        Vec::new()
    }
}
