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

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Datelike as _, Timelike as _};
use settings_store::notification_policy::{EffectiveNotificationPolicy, NotificationPolicy};
use settings_store::system_sounds::{QuietState, SystemSoundEvent};
use tilo_desktop_surface::notifications::{
    self, Addressed, BannerAsk, DEFAULT_DISMISS_AFTER, Notification, NotificationPerson,
    NotificationSeverity, PERSISTENT_DISMISS, QuietReason, ScheduledNotificationTarget,
};
use tilo_desktop_surface::{DesktopNotifications, NotificationOutcome};
use tilo_lock::MutexRecover as _;
use tilo_notification_banners::{Ask, Link};
use tilo_notifications_service::{Arrival, Caller, Callers, CloseReason, Notifications, Sender};

use crate::app_icons;
use crate::portal_link::PortalSide;
use crate::system_sounds::SystemSounds;

/// The most unread notifications one app's icon counts to.
const MAX_BADGE: u32 = 9_999;

/// The event sound an arrival asks for: a `suppress-sound` sender asks for
/// none, a critical one gets the machine's error voice — something failed,
/// and that sound plays through quiet modes — and anything else gets its
/// category's.
fn sound_event(sent: &tilo_notifications_service::Notification) -> Option<SystemSoundEvent> {
    if sent.suppress_sound {
        return None;
    }
    Some(if sent.urgency == 2 {
        SystemSoundEvent::Error
    } else {
        SystemSoundEvent::for_notification_category(sent.category.as_deref())
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
    // Artwork from what proves the app first, then lower-trust hints that
    // can only supply pixels, then the sender's own decoded image. Only
    // ever pixels: it never names or groups the app.
    notification.app_image = app_icons::notification(
        format!("notification:{}:{generation}", sent.id),
        app_id.as_deref(),
        sent.desktop_entry.as_deref(),
        sent.app_icon.as_deref(),
        sent.image_data
            .map(|pixels| ui::raster::rgba8(pixels.rgba, pixels.width, pixels.height)),
    );
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

/// What the bar reads of the notifications each frame: how many are
/// unread, whether fresh banners are held, and each app's own count.
#[derive(Default)]
pub(crate) struct NoticeSignals {
    unread: Cell<u32>,
    quiet: Cell<bool>,
    badges: RefCell<BTreeMap<String, u32>>,
}

impl NoticeSignals {
    fn set(&self, unread: u32, quiet: bool, badges: BTreeMap<String, u32>) {
        self.unread.set(unread);
        self.quiet.set(quiet);
        *self.badges.borrow_mut() = badges;
    }

    /// The unread notifications in the centre, as the bar's bell counts.
    pub(crate) fn unread(&self) -> u32 {
        self.unread.get()
    }

    /// Whether fresh banners are held right now, for the silenced bell.
    pub(crate) fn quiet(&self) -> bool {
        self.quiet.get()
    }

    /// How many notifications the app with this verified id is showing as
    /// unread: zero when it has none, or when this person turned its badges
    /// off.
    pub(crate) fn badge(&self, app_id: &str) -> u32 {
        self.badges.borrow().get(app_id).copied().unwrap_or(0)
    }
}

/// Who a notification came from: the app the session sees in the sender's
/// process (the one it shows a window for), or, for a sender with no window
/// of its own, the app its process is named by — its systemd unit, the
/// installed entry it runs, or its program — as the portal names one.
/// Nothing the sender says about itself is believed.
pub(crate) struct AppCallers {
    /// The apps the session sees run, by process.
    running: Arc<Mutex<BTreeMap<u32, String>>>,
    /// The desk: which workspace an app runs in.
    side: PortalSide,
    /// Where a process with no window of its own is named from.
    requesters: tilo_portal::Requesters,
    /// What a process was already named: naming one reads its cgroup, its
    /// program and the installed entries.
    named: Mutex<BTreeMap<u32, Option<String>>>,
}

impl AppCallers {
    pub(crate) fn new(running: Arc<Mutex<BTreeMap<u32, String>>>, side: PortalSide) -> Self {
        Self {
            running,
            side,
            requesters: tilo_portal::Requesters::host(),
            named: Mutex::new(BTreeMap::new()),
        }
    }

    /// The app the process `pid` belongs to, as the session knows it.
    fn app_of(&self, pid: u32) -> Option<String> {
        if let Some(app_id) = self.running.lock_recover().get(&pid) {
            return Some(app_id.clone());
        }
        let mut named = self.named.lock_recover();
        named
            .entry(pid)
            .or_insert_with(|| self.requesters.identify(pid).map(|caller| caller.app_id))
            .clone()
    }
}

impl Callers for AppCallers {
    fn caller(&self, sender: Sender<'_>) -> Option<Caller> {
        let app_id = self.app_of(sender.pid?)?;
        Some(Caller {
            workspace: self.side.workspace_of(&app_id),
            app_id,
        })
    }
}

/// Where a chosen scheduled notification opens, as `(room, uri)`.
type Opened = Rc<RefCell<Vec<(String, String)>>>;

/// The senders whose notification the person chose, as `(sender, action)`.
type Chosen = Rc<RefCell<Vec<(u32, Option<String>)>>>;

/// What ties the daemon, the desktop's log and the banners app together.
pub(crate) struct SessionNotifications {
    daemon: Option<Notifications>,
    desktop: DesktopNotifications,
    banners: Link,
    /// The person's notification policy, kept current by the session's
    /// settings watch: what each arrival is judged by.
    policy: Arc<Mutex<NotificationPolicy>>,
    /// The event sounds arrivals make, judged by the same policy.
    sounds: SystemSounds,
    /// Each attributed app's latest unread count where its sender said one,
    /// kept even while this person has its badges off, so turning them back
    /// on shows the number now rather than the last one painted.
    badges: BTreeMap<String, u32>,
    /// What the bar reads each frame.
    signals: Rc<NoticeSignals>,
    /// What choosing a scheduled notification opens.
    opened: Opened,
    /// The senders whose notification the person chose, waiting to be
    /// answered with an activation token.
    chosen: Chosen,
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
        policy: Arc<Mutex<NotificationPolicy>>,
        signals: Rc<NoticeSignals>,
    ) -> Self {
        let opened: Opened = Rc::default();
        let choosing = Rc::clone(&opened);
        let chosen: Chosen = Rc::default();
        let answering = Rc::clone(&chosen);
        desktop.on_outcome(move |outcome| {
            let NotificationOutcome::Activated {
                notification,
                action,
            } = outcome
            else {
                return;
            };
            // Choosing a scheduled notification opens what it rang for, in
            // the workspace that scheduled it.
            if let Some(target) = notification
                .scheduled_target
                .as_ref()
                .filter(|target| target.valid())
            {
                choosing
                    .borrow_mut()
                    .push((target.room.clone(), target.uri.clone()));
            }
            // Choosing answers the sender, with a token its app may raise
            // its window by: the loop mints it.
            if let Some(sender) = notification.sender_id {
                answering.borrow_mut().push((sender, action.clone()));
            }
        });
        Self {
            daemon,
            desktop,
            banners,
            policy,
            sounds: SystemSounds::new(),
            badges: BTreeMap::new(),
            signals,
            opened,
            chosen,
            by_sender: BTreeMap::new(),
            up: BTreeSet::new(),
            transient: BTreeSet::new(),
            published: None,
            generation: 0,
        }
    }

    /// The policy as it stands for `app_id` at the local time now; `None`
    /// asks for the desktop's own, which no per-app rule narrows.
    fn effective(
        &self,
        app_id: Option<&str>,
        priority_contact: bool,
    ) -> EffectiveNotificationPolicy {
        let (unix, minute, weekday) = local_now();
        self.policy.lock_recover().effective_for_context(
            app_id,
            priority_contact,
            unix,
            minute,
            weekday,
        )
    }

    /// How the person's calm settings stand at the local time now, as the
    /// event sounds read them.
    fn quiet_state(&self) -> QuietState {
        let (_, minute, weekday) = local_now();
        let policy = self.policy.lock_recover();
        QuietState {
            do_not_disturb: policy.do_not_disturb,
            focus: policy.focus.active_at(minute, weekday),
            // Nothing tells the session a presentation is on yet, so the
            // automatic hold while presenting is never on.
            presenting: false,
        }
    }

    /// Why fresh banners are held, as the bar and the centre header say it;
    /// `None` when they show as they arrive. A focus schedule can be pierced
    /// by priority senders, so it is applied per arrival as well.
    fn quiet_status(&self) -> Option<String> {
        let (_, minute, weekday) = local_now();
        let policy = self.policy.lock_recover();
        if policy.do_not_disturb {
            return Some(notifications::quiet_label(&QuietReason::DoNotDisturb));
        }
        policy.focus.active_until(minute, weekday).map(|end| {
            notifications::quiet_label(&QuietReason::FocusUntil(ui::clock::hour_minute(
                (end / 60).into(),
                (end % 60).into(),
            )))
        })
    }

    /// Turn Do Not Disturb on or off, as the person asked from quick
    /// settings or the desktop's own menu: the one persisted switch, which
    /// the session's settings watch reloads into every later arrival.
    ///
    /// # Errors
    /// The settings document could not be written.
    pub(crate) fn set_do_not_disturb(on: bool) -> std::io::Result<()> {
        NotificationPolicy::save_do_not_disturb(on)
    }

    /// The desk's log of what it shows: the live banners and the centre
    /// kept behind them.
    pub(crate) fn desktop(&self) -> DesktopNotifications {
        self.desktop.clone()
    }

    /// The targets of scheduled notifications the person chose since the
    /// last look, as `(room, uri)`, for the session to open.
    pub(crate) fn take_opened(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.opened.borrow_mut())
    }

    /// The notification centre as the quick settings panel lists it, newest
    /// first, with why banners are held and whether a Clear all can be
    /// undone.
    pub(crate) fn view(&self) -> tilo_quick_settings::NotificationsView {
        let now = ui::time::now();
        let log = self.desktop.log();
        let rows = log
            .history()
            .iter()
            .rev()
            .map(|notification| tilo_quick_settings::NotificationRow {
                id: notification.id,
                group: notifications::group_key(notification)
                    .unwrap_or_else(|| format!("notification-{}", notification.id)),
                app: notifications::group_label(notification),
                title: notification.title.clone(),
                body: notification.body.clone(),
                age: notifications::relative_time(notification, now),
                actions: notification.actions.iter().take(2).cloned().collect(),
                // No agent is live on this desktop to hand one to.
                agent: None,
                opens: notification.app_id.is_some() || !notification.actions.is_empty(),
            })
            .collect();
        let can_undo = log.can_undo_clear();
        drop(log);
        tilo_quick_settings::NotificationsView {
            rows,
            quiet: self.quiet_status(),
            can_undo,
        }
    }

    /// Choose a retained notification in the centre, as the panel asks.
    pub(crate) fn open(&self, id: u64) -> notifications::HistoryActivation {
        self.desktop.activate_history(id)
    }

    /// Dismiss a retained notification, or every one an app grouped: the
    /// centre and the panel list them no more.
    pub(crate) fn dismiss(&self, id: Option<u64>, group: Option<&str>) {
        match (id, group) {
            (Some(id), _) => self.desktop.dismiss_history(id),
            (None, Some(group)) => {
                let ids: Vec<u64> = self
                    .desktop
                    .log()
                    .history()
                    .iter()
                    .filter(|notification| {
                        notifications::group_key(notification).as_deref() == Some(group)
                    })
                    .map(|notification| notification.id)
                    .collect();
                for id in ids {
                    self.desktop.dismiss_history(id);
                }
            }
            (None, None) => {}
        }
    }

    /// Run one of a retained notification's own actions.
    pub(crate) fn run_action(&self, id: u64, key: &str) {
        self.desktop.invoke_action(id, key);
    }

    /// Clear the centre, or bring back what the last Clear all took.
    pub(crate) fn clear(&self) {
        self.desktop.clear_history();
    }

    pub(crate) fn undo_clear(&self) {
        self.desktop.undo_clear();
    }

    /// Take in what arrived and what the banners asked, and hand the
    /// banners what they show now. `token` mints an activation token for
    /// each notification the person chose, so its app may raise its window.
    pub(crate) fn step(&mut self, token: &mut dyn FnMut() -> Option<String>) {
        // Banners whose time on screen ran out leave first.
        self.desktop.expire(ui::time::now());
        self.settle(CloseReason::Expired);
        // Whether fresh banners are held, for the bar's bell and the centre
        // header, before they are published.
        self.desktop.set_quiet(self.quiet_status());
        if let Some(daemon) = self.daemon.clone() {
            for arrival in daemon.drain() {
                self.arrive(arrival);
            }
        }
        for ask in self.banners.take_asks() {
            self.ask(ask);
        }
        self.answer_chosen(token);
        self.settle(CloseReason::Dismissed);
        let revision = self.desktop.revision();
        if self.published != Some(revision) {
            self.published = Some(revision);
            self.banners.publish(self.desktop.feed());
        }
        self.publish_signals();
    }

    fn arrive(&mut self, arrival: Arrival) {
        match arrival {
            Arrival::Notify(sent) => {
                let sender = sent.id;
                let transient = sent.transient;
                let app_id = sent.caller.as_ref().map(|caller| caller.app_id.clone());
                let badge_count = sent.badge_count;
                let sound = sound_event(&sent);
                let effective = self.effective(app_id.as_deref(), sent.priority_contact);
                // A notification under a shown one's id replaces it.
                if let Some(old) = self.by_sender.remove(&sender) {
                    self.desktop.withdraw(old);
                    self.transient.remove(&old);
                }
                self.generation = self.generation.wrapping_add(1);
                let mut notification = shown(sent, self.generation);
                notification.show_preview = effective.previews;
                notification.show_on_lock_screen = effective.lock_screen;
                // Held banners go straight to the centre, unread: quiet
                // asks for calm, not to be kept uninformed.
                let id = if effective.banners {
                    self.desktop.post(notification)
                } else {
                    self.desktop.post_quiet(notification)
                };
                self.by_sender.insert(sender, id);
                if transient {
                    self.transient.insert(id);
                }
                // The app's latest count is kept even while this person has
                // its badges off: turning them back on shows the number now,
                // not the last one painted.
                if let (Some(app_id), Some(count)) = (app_id, badge_count) {
                    if count == 0 {
                        self.badges.remove(&app_id);
                    } else {
                        self.badges.insert(app_id, count.min(MAX_BADGE));
                    }
                }
                if let Some(event) = sound {
                    // Notifications and messages carry the policy's own
                    // verdict, which lets a priority sender through focus
                    // time; other events follow System sounds alone.
                    let allows = !matches!(
                        event,
                        SystemSoundEvent::Notification | SystemSoundEvent::Message
                    ) || effective.sounds;
                    self.sounds.play(event, self.quiet_state(), allows);
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

    /// Answer the apps whose notification the person chose since the last
    /// pass: the activation token first, then the action, then the close.
    fn answer_chosen(&self, token: &mut dyn FnMut() -> Option<String>) {
        for (sender, action) in std::mem::take(&mut *self.chosen.borrow_mut()) {
            let Some(daemon) = &self.daemon else {
                continue;
            };
            if let Some(key) = &action {
                daemon.action_invoked(sender, key, token());
            }
            daemon.closed(sender, CloseReason::Dismissed);
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

/// What the bar reads after each pass: the unread total the bell counts,
/// whether fresh banners are held, and each app's count as policy shows it.
impl SessionNotifications {
    fn publish_signals(&self) {
        let badges: BTreeMap<String, u32> = {
            let policy = self.policy.lock_recover();
            self.badges
                .iter()
                .map(|(app_id, count)| {
                    let shown = policy
                        .per_app
                        .get(app_id)
                        .and_then(|rule| rule.badges)
                        .unwrap_or(policy.show_badges);
                    (app_id.clone(), if shown { *count } else { 0 })
                })
                .collect()
        };
        let unread = u32::try_from(self.desktop.unread())
            .unwrap_or(u32::MAX)
            .min(MAX_BADGE);
        self.signals.set(unread, self.desktop.quiet(), badges);
    }
}

/// The local wall clock as the notification policy reads it: unix seconds,
/// minutes since midnight and the weekday counted from Monday.
fn local_now() -> (i64, u16, u8) {
    let now = chrono::Local::now();
    (
        now.timestamp(),
        u16::try_from(now.hour().saturating_mul(60).saturating_add(now.minute()))
            .unwrap_or(u16::MAX),
        u8::try_from(now.weekday().num_days_from_monday()).unwrap_or(u8::MAX),
    )
}

#[cfg(test)]
mod tests;
