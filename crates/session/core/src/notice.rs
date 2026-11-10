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
use tilo_desktop_surface::notifications::{NOT_NOW_ACTION, Notification, NotificationIconKind};
use tilo_desktop_surface::surface::HostAction;

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
    /// What it is about, where the desk leads it with the glyph for that rather than the app's own
    /// mark ([`Self::about`]): the system's bell for the machine's own news, which is the default.
    pub leading: NotificationIconKind,
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
            leading: NotificationIconKind::System,
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

    /// The person is asked to do `action`, and told in `body` what it is about: it offers the
    /// action and "Not now", and stays until they answer it or put it off.
    #[must_use]
    pub fn asks(action: &HostAction, title: impl Into<String>, body: impl Into<String>) -> Self {
        Self::new(action.source_label(), 1, title, body)
            .about(NotificationIconKind::Permission)
            .choosing(vec![
                (action.key(), action.title.clone()),
                (
                    NOT_NOW_ACTION.to_owned(),
                    tilo_desktop_surface::i18n::t("notification-not-now"),
                ),
            ])
            .staying(Some(Duration::ZERO))
    }

    /// Told under `tag`, so a later telling of the same thing replaces it.
    #[must_use]
    pub fn tagged(mut self, tag: impl Into<String>) -> Self {
        self.tag = tag.into();
        self
    }

    /// About `kind` of news, where it leads with that glyph rather than the app's own mark.
    #[must_use]
    pub fn about(mut self, kind: NotificationIconKind) -> Self {
        self.leading = kind;
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

    /// The row a host's own desk shows for it, where the host keeps the person's notifications on a
    /// desk of its own: what choosing it does comes back through [`Notices::take_chosen`].
    #[must_use]
    pub fn shown(&self) -> Notification {
        let mut shown = Notification::new(self.title.clone(), self.body.clone())
            .stated(self.urgency, self.expires_after)
            .with_icon_kind(self.leading);
        shown.actions.clone_from(&self.actions);
        if !self.app.is_empty() {
            shown.app_name = Some(self.app.clone());
        }
        if self.machine_wide {
            shown = shown.machine_wide();
        }
        shown
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

    /// The actions the person chose since this was last taken, by the tag of the notice that
    /// offered them.
    fn take_chosen_actions(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// The notices that closed since this was last taken, each with why.
    fn take_closed(&self) -> Vec<(String, Closed)> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_surface::HostActionSource;

    fn action(source: Option<HostActionSource>) -> HostAction {
        HostAction {
            id: "keep-identity".into(),
            title: "Keep with passkey".into(),
            subtitle: None,
            icon: None,
            destructive: false,
            unasked: false,
            source,
        }
    }

    #[test]
    fn an_ask_offers_its_answer_and_not_now_under_the_part_of_the_desktop_that_asks() {
        let ask = Notice::asks(
            &action(Some(HostActionSource::Devices)),
            "Pair “Phone”?",
            "",
        );
        assert_eq!(
            ask.app,
            tilo_desktop_surface::i18n::t("host-action-source-devices")
        );
        assert_eq!(
            ask.actions,
            [
                (
                    tilo_desktop_surface::host_action_key("keep-identity"),
                    "Keep with passkey".to_owned()
                ),
                (
                    NOT_NOW_ACTION.to_owned(),
                    tilo_desktop_surface::i18n::t("notification-not-now")
                ),
            ]
        );
        assert_eq!(ask.expires_after, Some(Duration::ZERO), "until they answer");
        assert_eq!(ask.leading, NotificationIconKind::Permission);

        let shown = ask.shown();
        assert_eq!(
            shown.sender_name(),
            tilo_desktop_surface::i18n::t("host-action-source-devices")
        );
        assert_eq!(
            shown.default_action_key().as_deref(),
            Some("host:keep-identity")
        );
        let plain = Notice::asks(&action(None), "X", "").shown();
        assert_eq!(
            plain.sender_name(),
            tilo_desktop_surface::i18n::t("notification-group-system"),
            "an ask from no named part of the desktop still says who it is from"
        );
    }

    #[test]
    fn the_machines_own_news_leads_with_the_systems_mark() {
        let news = Notice::new("Power", 2, "Battery low", "");
        assert_eq!(news.leading, NotificationIconKind::System);
        let shown = news.shown();
        assert_eq!(
            shown.severity,
            tilo_desktop_surface::notifications::NotificationSeverity::Error
        );
        assert_eq!(
            shown.dismiss_after,
            tilo_desktop_surface::notifications::PERSISTENT_DISMISS,
            "critical news stays until the person acts"
        );
        assert!(shown.machine_wide);
        assert_eq!(shown.mark().icon, Some(ui::icons::Icon::Bell));
        assert_eq!(shown.app_name.as_deref(), Some("Power"));
        assert_eq!(Notice::new("", 1, "T", "").shown().app_name, None);
    }
}
