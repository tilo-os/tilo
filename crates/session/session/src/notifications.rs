//! The person's notifications on the desktop: what apps sent the
//! notifications daemon, shown by the banners app and kept in the desk's
//! notification centre.
//!
//! The daemon's arrivals (each with its pixels: a decoded `image-data`,
//! `image-path` or portal icon) become the desktop's notifications in one
//! [`DesktopNotifications`] log. The banners app is handed the live banners
//! from it and what it asks is carried out on it; the desk surface shows
//! the centre over the same log. What the person does reaches the sending
//! app through the daemon: an action or a reply as they choose it, and the
//! close once the banner leaves (dismissed, chosen or run out).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use tilo_desktop_surface::notifications::{
    self, Addressed, BannerAsk, DEFAULT_DISMISS_AFTER, Notification, NotificationPerson,
    NotificationSeverity, PERSISTENT_DISMISS, ScheduledNotificationTarget,
};
use tilo_desktop_surface::{DesktopNotifications, NotificationOutcome};
use tilo_notification_banners::{Ask, Link};
use tilo_notifications_service::{Arrival, CloseReason, ImageData, Notifications};

/// Artwork a sender handed over as pixels (`image-data`, or an image file
/// or portal icon the daemon decoded). Only ever pixels: it never names or
/// groups the app.
struct SenderImage {
    key: String,
    image: ui::peniko::ImageData,
}

impl ui::icons::AppImageSource for SenderImage {
    fn key(&self) -> &str {
        &self.key
    }

    fn pixels(&self, _device_px: u32) -> Option<ui::peniko::ImageData> {
        Some(self.image.clone())
    }
}

/// `pixels` as artwork, keyed by notification `id` and the arrival's
/// `generation` so a replaced image is never mistaken for the old one.
fn artwork(id: u32, generation: u64, pixels: ImageData) -> ui::icons::AppImage {
    let image = ui::raster::rgba8(pixels.rgba, pixels.width, pixels.height);
    ui::icons::AppImage::new(SenderImage {
        key: format!("notification:{id}:{generation}"),
        image,
    })
}

/// The desktop's notification for what an app sent the daemon. Critical
/// urgency stays until the person acts, as the specification asks;
/// otherwise `-1` is the desktop's default, `0` stays, and a positive
/// timeout is that many milliseconds on screen.
pub(crate) fn shown(
    sent: tilo_notifications_service::Notification,
    generation: u64,
) -> Notification {
    let app_id = sent.caller.as_ref().map(|caller| caller.app_id.clone());
    let mut notification = Notification::new(sent.summary, sent.body);
    notification.severity = if sent.urgency == 2 {
        NotificationSeverity::Error
    } else {
        NotificationSeverity::Info
    };
    notification.dismiss_after = if sent.urgency == 2 {
        PERSISTENT_DISMISS
    } else {
        match sent.expire_timeout {
            timeout if timeout < 0 => DEFAULT_DISMISS_AFTER,
            0 => PERSISTENT_DISMISS,
            ms => Duration::from_millis(u64::try_from(ms).unwrap_or(0)),
        }
    };
    notification.sender_id = Some(sent.id);
    // `app_name` is the sender's own word: an attributed app is named by
    // its id, an unattributed sender's name is shown as its claim, never
    // as who it is.
    notification.app_name = match app_id.as_deref() {
        Some(id) => Some(id.to_owned()),
        None => notifications::sender_claim(&sent.app_name),
    };
    notification.app_image = sent
        .image_data
        .map(|pixels| artwork(sent.id, generation, pixels));
    notification.app_id = app_id;
    // A person named by an attributed communications app leads the
    // banner; the endpoint only picks their colour and is never shown.
    notification.from = sent
        .sender_name
        .zip(sent.contact_endpoint)
        .map(|(name, identity)| NotificationPerson { name, identity });
    // A call rings for the person it is from; the category alone is only
    // the app's word.
    if sent.incoming_call && notification.from.is_some() {
        notification.addressed = Some(Addressed::IncomingCall);
    }
    notification.machine_wide = sent.machine_wide;
    let alarm = sent
        .scheduled
        .as_ref()
        .is_some_and(|occurrence| occurrence.calendar_alarm);
    let alarm_labels = if alarm {
        notifications::alarm_action_labels()
    } else {
        Vec::new()
    };
    notification.actions = sent
        .actions
        .into_iter()
        .map(|(key, label)| {
            let label = if key == "default" {
                notifications::open_action_label()
            } else {
                alarm_labels
                    .iter()
                    .find(|(alarm_key, _)| *alarm_key == key)
                    .map_or(label, |(_, said)| said.clone())
            };
            (key, label)
        })
        .collect();
    notification.scheduled_target = sent
        .scheduled
        .map(|occurrence| ScheduledNotificationTarget {
            room: occurrence.room,
            token: occurrence.token,
            occurrence_ms: occurrence.occurrence_ms,
            calendar_alarm: occurrence.calendar_alarm,
            snoozed: false,
            uri: occurrence.uri,
        });
    // Never from the wire: an app that could pierce Do Not Disturb would
    // ask every time.
    notification.bypass_dnd = false;
    notification.show_on_lock_screen = false;
    notification
}

/// The keys a call's answer or decline goes by, as senders name them.
const ANSWER_KEYS: [&str; 2] = ["accept", "answer"];
const DECLINE_KEYS: [&str; 3] = ["decline", "reject", "hangup"];

/// What ties the daemon, the desktop's log and the banners app together.
pub(crate) struct SessionNotifications {
    daemon: Option<Notifications>,
    desktop: DesktopNotifications,
    banners: Link,
    /// The desktop's notification for each daemon notification it shows.
    by_sender: BTreeMap<u32, u64>,
    /// The daemon notifications whose banner was up at the last look.
    up: BTreeSet<u32>,
    /// Desktop notifications kept out of the centre once their banner left.
    transient: BTreeSet<u64>,
    /// The log revision the banners were last handed.
    published: Option<u64>,
    /// Counts arrivals, keeping each one's artwork apart.
    generation: u64,
}

impl SessionNotifications {
    /// Show `daemon`'s notifications in `desktop`'s log, its banners on
    /// `banners`.
    pub(crate) fn new(
        daemon: Option<Notifications>,
        desktop: DesktopNotifications,
        banners: Link,
    ) -> Self {
        if let Some(daemon) = daemon.clone() {
            // Choosing answers the sender at once: its action, then the
            // close, while the surface opens the app.
            desktop.on_outcome(move |outcome| {
                if let NotificationOutcome::Activated {
                    notification,
                    action,
                } = outcome
                    && let Some(sender) = notification.sender_id
                {
                    if let Some(key) = action {
                        daemon.action_invoked(sender, key, None);
                    }
                    daemon.closed(sender, CloseReason::Dismissed);
                }
            });
        }
        Self {
            daemon,
            desktop,
            banners,
            by_sender: BTreeMap::new(),
            up: BTreeSet::new(),
            transient: BTreeSet::new(),
            published: None,
            generation: 0,
        }
    }

    /// Take in what arrived and what the banners asked, and hand the
    /// banners what they show now.
    pub(crate) fn step(&mut self) {
        // Banners whose time on screen ran out leave first.
        self.desktop.expire(ui::time::now());
        self.settle(CloseReason::Expired);
        if let Some(daemon) = self.daemon.clone() {
            for arrival in daemon.drain() {
                self.arrive(arrival);
            }
        }
        for ask in self.banners.take_asks() {
            self.ask(ask);
        }
        self.settle(CloseReason::Dismissed);
        let revision = self.desktop.revision();
        if self.published != Some(revision) {
            self.published = Some(revision);
            self.banners.publish(self.desktop.feed());
        }
    }

    fn arrive(&mut self, arrival: Arrival) {
        match arrival {
            Arrival::Notify(sent) => {
                let sender = sent.id;
                let transient = sent.transient;
                // A notification under a shown one's id replaces it.
                if let Some(old) = self.by_sender.remove(&sender) {
                    self.desktop.withdraw(old);
                    self.transient.remove(&old);
                }
                self.generation = self.generation.wrapping_add(1);
                let id = self.desktop.post(shown(sent, self.generation));
                self.by_sender.insert(sender, id);
                if transient {
                    self.transient.insert(id);
                }
            }
            Arrival::Close(sender) => {
                if let Some(id) = self.by_sender.remove(&sender) {
                    self.desktop.withdraw(id);
                    self.transient.remove(&id);
                }
                self.up.remove(&sender);
            }
        }
    }

    /// The live banner `id`.
    fn banner(&self, id: u64) -> Option<Notification> {
        self.desktop
            .log()
            .active()
            .iter()
            .find(|notification| notification.id == id)
            .cloned()
    }

    fn ask(&self, ask: Ask) {
        match ask {
            Ask::Banner(ask) => self.desktop.answer(ask),
            Ask::Reply { id, text } => {
                let Some(banner) = self.banner(id) else {
                    return;
                };
                let takes_reply = banner
                    .actions
                    .iter()
                    .any(|(key, _)| key == tilo_notification_banners::INLINE_REPLY);
                if let (Some(daemon), Some(sender), true) =
                    (&self.daemon, banner.sender_id, takes_reply)
                {
                    daemon.replied(sender, &text);
                    self.desktop.answer(BannerAsk::Dismiss(id));
                }
            }
            Ask::Read(id) => {
                // It moves to the centre, which opens on it.
                if self.banner(id).is_some() {
                    self.desktop.answer(BannerAsk::Dismiss(id));
                }
                self.desktop.open_center();
            }
            Ask::AnswerCall(id) => self.call(id, true),
            Ask::DeclineCall(id) => self.call(id, false),
        }
    }

    /// Answer or decline the call banner `id` rings for: the caller's own
    /// answer or decline when it offers one, else opening the call (its
    /// default action) or closing the banner.
    fn call(&self, id: u64, answer: bool) {
        let Some(banner) = self.banner(id) else {
            return;
        };
        let keys: &[&str] = if answer { &ANSWER_KEYS } else { &DECLINE_KEYS };
        let offered = banner
            .actions
            .iter()
            .map(|(key, _)| key.clone())
            .find(|key| keys.contains(&key.as_str()));
        self.desktop.answer(match offered {
            Some(key) => BannerAsk::Invoke(id, key),
            None if answer => BannerAsk::Activate(id),
            None => BannerAsk::Dismiss(id),
        });
    }

    /// Tell the senders of banners no longer up that they closed, for
    /// `reason`; keep a transient one out of the centre.
    fn settle(&mut self, reason: CloseReason) {
        let up: BTreeSet<u32> = self
            .desktop
            .log()
            .active()
            .iter()
            .filter_map(|notification| notification.sender_id)
            .collect();
        let left: Vec<u32> = self.up.difference(&up).copied().collect();
        self.up = up;
        for sender in left {
            if let Some(daemon) = &self.daemon {
                daemon.closed(sender, reason);
            }
            if let Some(id) = self.by_sender.get(&sender).copied()
                && self.transient.remove(&id)
            {
                self.desktop.withdraw(id);
                self.by_sender.remove(&sender);
            }
        }
        // Retained ones the person dismissed from the centre are gone.
        let log = self.desktop.log();
        let shown: BTreeSet<u64> = log
            .active()
            .iter()
            .chain(log.history())
            .map(|notification| notification.id)
            .collect();
        drop(log);
        self.by_sender.retain(|_, id| shown.contains(id));
    }
}

#[cfg(test)]
mod tests;
