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
use std::time::{Duration, Instant};

use chrono::{Datelike as _, Timelike as _};
use serde_json::{Map, Value};
use settings_store::notification_policy::{EffectiveNotificationPolicy, NotificationPolicy};
use settings_store::system_sounds::{QuietState, SystemSoundEvent};
use tilo_actions::{ActionAddress, ActionOutput, DoorError, ResourceSpec};
use tilo_desktop_surface::notifications::{
    self, Addressed, BannerAsk, DEFAULT_DISMISS_AFTER, Notification, NotificationPerson,
    NotificationSeverity, PERSISTENT_DISMISS, QuietReason, ScheduledNotificationTarget,
};
use tilo_desktop_surface::{DesktopNotifications, NotificationOutcome};
use tilo_lock::MutexRecover as _;
use tilo_notification_banners::{Ask, Link};
use tilo_notifications_service::{Arrival, Caller, Callers, CloseReason, Notifications, Sender};
use tilo_quick_settings::hosted::{KeptNotification, NotificationCentre};
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_session_power::PowerLink;

use crate::app_icons;
use crate::i18n::t;
use crate::portal_link::PortalSide;
use crate::system_sounds::SystemSounds;

/// Mount the banners app on the desk: what it shows of the desktop's
/// notifications, and its own link to them.
///
/// Under the shell nothing of it is mounted: the shell draws the banner a
/// notice rises as from the desk it reads (`Desk.banner`), so a banner stack of
/// its own would draw a second one over it and take the presses meant for what
/// the shell shows there.
pub(crate) fn mount_banners(
    compositor: &mut tilo_compositor::Compositor,
    doors: &mut tilo_surface_host::Doors,
    notices: &DesktopNotifications,
    desk: &tilo_desktop_surface::DesktopHandle,
    options: &crate::Options,
    stack: crate::panels::Reserved,
) -> Link {
    let banners = Link::new();
    if options.shell {
        notices.show_banners_elsewhere();
        return banners;
    }
    match tilo_notification_banners::prepare(banners.clone(), &options.device, &options.person)
        .and_then(|component| ui::App::from_component(component, (options.presentation)()))
    {
        Ok(app) => {
            let surface = Rc::new(RefCell::new(crate::panels::Banners::new(
                app,
                banners.clone(),
                desk.clone(),
                options.scale,
                stack,
            )));
            compositor.add_host(Box::new(crate::panels::Held(Rc::clone(&surface))));
            doors.add(surface);
            notices.show_banners_elsewhere();
        }
        Err(error) => tracing::warn!(%error, "notification banners not mounted"),
    }
    banners
}

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
    // what the device calls that app, an unattributed sender's name is shown
    // as its claim, never as who it is.
    notification.app_name = match app_id.as_deref() {
        Some(id) => Some(crate::app_names::app_name(id)),
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
            workspace: occurrence.workspace,
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

    /// How many notifications each app (by verified id) is showing as
    /// unread, where it shows any and this person keeps its badges on.
    /// The count on `app_id`'s badge.
    pub(crate) fn badge(&self, app_id: &str) -> u32 {
        self.badges.borrow().get(app_id).copied().unwrap_or(0)
    }

    pub(crate) fn badges(&self) -> BTreeMap<String, u32> {
        self.badges
            .borrow()
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(app, count)| (app.clone(), *count))
            .collect()
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

/// Where a chosen scheduled notification opens, as `(workspace, uri)`.
type Opened = Rc<RefCell<Vec<(String, String)>>>;

/// The senders whose notification the person chose, as `(sender, action)`.
type Chosen = Rc<RefCell<Vec<(u32, Option<String>)>>>;

/// What ties the daemon, the desktop's log and the banners app together.
pub(crate) struct SessionNotifications {
    daemon: Option<Notifications>,
    desktop: DesktopNotifications,
    banners: Link,
    /// The machine's power, when this session owns it: what holds it from
    /// going idle is how a presentation, a video or a call tells the desk
    /// the screen is being watched by others.
    power: Option<PowerLink>,
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
    /// The kept notifications the person chose to open, waiting for the loop
    /// that opens their apps.
    opening: RefCell<Vec<u64>>,
    /// The senders whose notification the person chose, waiting to be
    /// answered with an activation token.
    chosen: Chosen,
    /// The desktop's notification for each daemon notification it shows.
    by_sender: BTreeMap<u32, u64>,
    /// The daemon notifications whose banner was up at the last look.
    up: BTreeSet<u32>,
    /// The desk's banners that were up at the last look: a new one is
    /// something the person was told since then.
    banners_up: BTreeSet<u64>,
    /// Desktop notifications kept out of the centre once their banner left.
    transient: BTreeSet<u64>,
    /// The log revision the banners were last handed.
    published: Option<u64>,
    /// Counts arrivals, keeping each one's artwork apart.
    generation: u64,
    /// The Do Not Disturb change since the readout last said one: whoever
    /// flipped the switch, on screen it says so.
    quiet_changed: Cell<Option<bool>>,
    /// The agent a kept notification can be handed to, where one is live in
    /// the workspace in front: what the centre's rows offer.
    taking_agent: Option<String>,
    /// The kept notifications the person handed to that agent, waiting for
    /// the loop that asks it.
    handed: Vec<u64>,
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
        power: Option<PowerLink>,
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
                    .push((target.workspace.clone(), target.uri.clone()));
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
            power,
            policy,
            sounds: SystemSounds::new(),
            badges: BTreeMap::new(),
            signals,
            opened,
            opening: RefCell::new(Vec::new()),
            chosen,
            by_sender: BTreeMap::new(),
            up: BTreeSet::new(),
            banners_up: BTreeSet::new(),
            transient: BTreeSet::new(),
            published: None,
            generation: 0,
            quiet_changed: Cell::new(None),
            taking_agent: None,
            handed: Vec::new(),
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

    /// Whether the person is presenting right now: an app holding the
    /// machine from going idle says someone is watching the screen without
    /// touching anything, which is a slideshow, a video or a call.
    fn presenting(&self) -> bool {
        self.power.as_ref().is_some_and(PowerLink::idle_held)
    }

    /// How the person's calm settings stand at the local time now, as the
    /// event sounds read them.
    fn quiet_state(&self) -> QuietState {
        let presenting = self.presenting();
        let (_, minute, weekday) = local_now();
        let policy = self.policy.lock_recover();
        QuietState {
            do_not_disturb: policy.do_not_disturb,
            focus: policy.focus.active_at(minute, weekday),
            presenting,
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
        let focus = policy.focus.active_until(minute, weekday);
        drop(policy);
        if self.presenting() {
            return Some(notifications::quiet_label(&QuietReason::Presenting));
        }
        focus.map(|end| {
            notifications::quiet_label(&QuietReason::FocusUntil(ui::clock::hour_minute(
                (end / 60).into(),
                (end % 60).into(),
            )))
        })
    }

    /// Turn Do Not Disturb on or off, as the person asked from quick
    /// settings or the desktop's own menu: the one persisted switch, which
    /// the session's settings watch reloads into every later arrival, and
    /// which this session applies at once so the switch the person just
    /// flipped is the state everything else here reads.
    ///
    /// # Errors
    /// The settings document could not be written.
    pub(crate) fn set_do_not_disturb(&self, on: bool) -> std::io::Result<()> {
        self.policy.lock_recover().do_not_disturb = on;
        self.quiet_changed.set(Some(on));
        NotificationPolicy::save_do_not_disturb(on)
    }

    /// The Do Not Disturb change since the readout last confirmed one.
    pub(crate) fn take_quiet_change(&self) -> Option<bool> {
        self.quiet_changed.take()
    }

    /// The desk's log of what it shows: the live banners and the centre
    /// kept behind them.
    pub(crate) fn desktop(&self) -> DesktopNotifications {
        self.desktop.clone()
    }

    /// The targets of scheduled notifications the person chose since the
    /// last look, as `(workspace, uri)`, for the session to open.
    pub(crate) fn take_opened(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.opened.borrow_mut())
    }

    /// The notification centre as the session publishes it: what it keeps,
    /// newest first, why fresh banners are held and whether a Clear all can
    /// be undone.
    pub(crate) fn centre(&self) -> NotificationCentre {
        let now = ui::time::now();
        let unix_ms = ui::time::unix_now_ms();
        let log = self.desktop.log();
        let rows: Vec<KeptNotification> = log
            .history()
            .iter()
            .rev()
            .map(|notification| KeptNotification {
                id: notification.id,
                group: notifications::group_key(notification)
                    .unwrap_or_else(|| format!("notification-{}", notification.id)),
                app: notifications::group_label(notification),
                title: notification.title.clone(),
                body: notification.body.clone(),
                // When it arrived, as the clock the panel counts ages by
                // reads it: the log keeps the instant it was posted.
                created_ms: unix_ms.saturating_sub(
                    u64::try_from(
                        now.saturating_duration_since(notification.created_at)
                            .as_millis(),
                    )
                    .unwrap_or(u64::MAX),
                ),
                actions: notification.actions.clone(),
                agent: self.taking_agent.clone(),
                opens: notification.app_id.is_some() || !notification.actions.is_empty(),
            })
            .collect();
        let can_undo = log.can_undo_clear();
        drop(log);
        NotificationCentre {
            count: rows.len(),
            rows,
            quiet: self.quiet_status(),
            can_undo,
        }
    }

    /// Say which agent a kept notification can be handed to, where one is
    /// live here: the workspace in front's first agent, as the runs know it.
    pub(crate) fn set_taking_agent(&mut self, agent: Option<String>) {
        self.taking_agent = agent;
    }

    /// Hand the kept notification `id` to the agent that takes one here,
    /// waiting for the loop that asks it.
    pub(crate) fn ask_hand_to_agent(&mut self, id: u64) {
        self.handed.push(id);
    }

    /// The notifications handed to that agent since the last look, as the
    /// agent's name, the title it is told and the body it reads: a
    /// notification no longer kept is not handed on.
    pub(crate) fn take_handed(&mut self) -> Vec<(String, String, String)> {
        let ids = std::mem::take(&mut self.handed);
        let Some(agent) = self.taking_agent.clone() else {
            return Vec::new();
        };
        let log = self.desktop.log();
        ids.into_iter()
            .filter_map(|id| {
                let notification = log.history_entry(id)?;
                Some((
                    agent.clone(),
                    notification.title.clone(),
                    notification.body.clone(),
                ))
            })
            .collect()
    }

    /// Choose a retained notification in the centre, as the panel asks.
    pub(crate) fn open(&self, id: u64) -> notifications::HistoryActivation {
        self.desktop.activate_history(id)
    }

    /// Open the retained notification `id` in the app that sent it: what a
    /// press on it in the centre asks, carried out by the loop that opens
    /// apps.
    pub(crate) fn ask_open(&self, id: u64) {
        self.opening.borrow_mut().push(id);
    }

    /// The notifications the person chose to open since the last look.
    pub(crate) fn take_opening(&self) -> Vec<u64> {
        std::mem::take(&mut *self.opening.borrow_mut())
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

    /// Tell the person `notice`, which the session itself raised rather than
    /// one of their apps.
    pub(crate) fn tell(&self, notice: crate::notices::Notice) {
        // The session raises the notice, so it plays its sound here: a
        // sender's category never reaches the events only the session may
        // claim, and the notification carries none for the arrival to
        // repeat.
        if let Some(event) = notice.sound {
            self.sounds.play(event, self.quiet_state(), true);
        }
        match &self.daemon {
            Some(daemon) => {
                notice.tell_as(daemon);
            }
            // A session without a notifications service still draws its
            // desk: what it raises is kept there, so nothing it says is
            // lost.
            None => {
                notice.tell_desk(&self.desktop);
            }
        }
    }

    /// When the desk's banners next change with no one asking: one running
    /// out, or a Clear all's undo closing.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.desktop.next_deadline()
    }

    /// Take in what arrived and what the banners asked, and hand the
    /// banners what they show now. `token` mints an activation token for
    /// each notification the person chose, so its app may raise its window.
    ///
    /// Answers whether a banner the person was not shown before is up now:
    /// the loop flashes the screen for it when they asked for a visual
    /// alert.
    pub(crate) fn step(&mut self, token: &mut dyn FnMut() -> Option<String>) -> bool {
        // Banners whose time on screen ran out leave first.
        self.desktop.expire(ui::time::now());
        self.settle(CloseReason::Expired);
        // Whether fresh banners are held, for the bar's bell and the centre
        // header, before they are published.
        self.desktop.set_quiet(self.quiet_status());
        if let Some(daemon) = self.daemon.clone() {
            // The machine's graphics stopped for good: the person is told
            // once, in the pass the renderers' record first says so.
            crate::notices::tell_graphics_fallback(&daemon);
            for arrival in daemon.drain() {
                self.arrive(arrival);
            }
        }
        for ask in self.banners.take_asks() {
            self.ask(ask);
        }
        self.answer_chosen(token);
        self.settle(CloseReason::Dismissed);
        // Every banner the desk puts up passes through its log, whether an
        // app sent it or the session raised it: a new one there is what the
        // person was just told.
        let banners: BTreeSet<u64> = self
            .desktop
            .log()
            .active()
            .iter()
            .map(|notification| notification.id)
            .collect();
        let appeared = banners.difference(&self.banners_up).next().is_some();
        self.banners_up = banners;
        let revision = self.desktop.revision();
        if self.published != Some(revision) {
            self.published = Some(revision);
            self.banners.publish(self.desktop.feed());
        }
        self.publish_signals();
        appeared
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
                // asks for calm, not to be kept uninformed. Presenting is
                // held whatever the sender's own rule says.
                let id = if effective.banners && !self.presenting() {
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

/// The resource the notification centre is published under.
pub const RESOURCE: &str = "session:notifications";

/// What kind of resource [`RESOURCE`] is: the session's own kind, as the
/// clipboard is, so the grant the device's panel reads the session's own
/// readings through reaches it.
pub const KIND: &str = tilo_action_policy::roles::SESSION_KIND;

/// What the centre resource offers: what a notification kept there does.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.notifications")]
pub enum CentreAction {
    /// Dismiss a notification, or every one an app grouped
    #[effect(own_device, idempotent)]
    Dismiss {
        /// The notification's id, as the rows carry it.
        #[param("The notification")]
        notification: Option<u64>,
        /// The app group its rows are listed under.
        #[param("The app group")]
        group: Option<String>,
    },
    /// Run one of a notification's own actions
    #[effect(own_device)]
    Act {
        /// The notification's id, as the rows carry it.
        #[param("The notification")]
        notification: u64,
        /// The key of one of the actions it offers.
        #[param("One of its actions")]
        action: String,
    },
    /// Clear the centre, keeping what the person pinned
    #[effect(own_device, idempotent)]
    Clear,
    /// Bring back what the last Clear all took
    #[effect(own_device, idempotent)]
    UndoClear,
    /// Hand a notification the centre kept to an agent as a task
    #[effect(own_device)]
    HandToAgent {
        /// The notification's id, as the rows carry it.
        #[param("The notification")]
        notification: u64,
    },
    /// Open one of the notifications the centre kept
    #[effect(own_device)]
    Open {
        /// The notification's id, as the rows carry it.
        #[param("The notification")]
        notification: u64,
    },
    /// Hold fresh banners back until turned off again
    #[effect(own_device, idempotent)]
    SetQuiet {
        /// Whether to hold them back
        #[param("Whether to hold banners back")]
        on: bool,
    },
}

/// The centre as the session's services publish it: what it keeps, why fresh
/// banners are held, and the verbs that change it.
pub(crate) struct Served {
    notifications: Rc<RefCell<SessionNotifications>>,
    /// The reading last reported, so a pass that changed nothing is no
    /// revision.
    published: RefCell<Option<NotificationCentre>>,
}

impl Served {
    /// The service publishing what `notifications` keeps.
    pub(crate) fn new(notifications: Rc<RefCell<SessionNotifications>>) -> Self {
        Self {
            notifications,
            published: RefCell::new(None),
        }
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        let mut resource = offer::<CentreAction>(ResourceSpec::new(
            RESOURCE,
            KIND,
            t("session-notifications"),
        ))
        .with_granted_reading();
        // The panel keeps the words the rest of the desktop names them by.
        for spec in &mut resource.actions {
            let label = match spec.id.as_str() {
                "dismiss" => t("session-notifications-dismiss"),
                "act" => t("session-notifications-act"),
                "clear" => t("session-notifications-clear"),
                "undo-clear" => t("session-notifications-undo-clear"),
                "hand-to-agent" => t("session-notifications-hand-to-agent"),
                "open" => t("session-notifications-open"),
                _ => continue,
            };
            spec.type_mut().label = label;
        }
        vec![resource]
    }

    /// What the centre keeps, newest first: what people said to this person,
    /// so only a grant the workspace gives reads it.
    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| {
            let published = self.published.borrow().clone();
            let centre = published.unwrap_or_else(|| self.notifications.borrow().centre());
            reading(&centre)
        })
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        if resource != RESOURCE {
            return Err(DoorError::Unknown(ActionAddress::new(resource, action)));
        }
        match decode::<CentreAction>(&self.resources(), resource, action, arguments)? {
            CentreAction::Dismiss {
                notification,
                group,
            } => self
                .notifications
                .borrow()
                .dismiss(notification, group.as_deref()),
            CentreAction::Act {
                notification,
                action,
            } => self
                .notifications
                .borrow()
                .run_action(notification, &action),
            CentreAction::Clear => self.notifications.borrow().clear(),
            CentreAction::UndoClear => self.notifications.borrow().undo_clear(),
            CentreAction::Open { notification } => {
                self.notifications.borrow().ask_open(notification);
            }
            CentreAction::HandToAgent { notification } => {
                self.notifications
                    .borrow_mut()
                    .ask_hand_to_agent(notification);
            }
            CentreAction::SetQuiet { on } => {
                if let Err(error) = self.notifications.borrow().set_do_not_disturb(on) {
                    tracing::warn!(%error, "do not disturb was not kept");
                }
            }
        }
        Ok(Started::Done(ActionOutput::default()))
    }

    fn poll(&mut self) -> bool {
        let now = self.notifications.borrow().centre();
        let mut published = self.published.borrow_mut();
        if published.as_ref() == Some(&now) {
            return false;
        }
        *published = Some(now);
        true
    }
}

impl crate::Shared {
    /// Open what the person chose on a scheduled notification, in the
    /// workspace that scheduled it, and the centre's kept ones in the app
    /// that sent them: the app that takes its scheme.
    pub(crate) fn open_notifications(&self) {
        for (workspace, uri) in self.notifications.borrow().take_opened() {
            let opened = self.search.perform(
                &workspace,
                tilo_desktop_search::Target::Open {
                    uri,
                    fallback: None,
                },
                &mut |own| {
                    self.doors
                        .invoke(
                            &own.address,
                            &own.arguments,
                            &mut tilo_surface_host::Look::none(),
                        )
                        .map(drop)
                },
            );
            if let Err(error) = opened {
                self.answer_failed(&crate::i18n::t("action-failed-that-notification"), &error);
            }
        }
        // What the person handed to an agent becomes the task it acts on, in
        // the workspace in front: the notification's title is what it is
        // asked, and the whole of it is what the agent reads.
        for (agent, title, body) in self.notifications.borrow_mut().take_handed() {
            let title: String = title.chars().take(240).collect();
            let body = if body.is_empty() {
                title.clone()
            } else {
                format!("{title}\n\n{body}")
            };
            let ask = tilo_agent_runs::Ask {
                agent,
                title,
                body: Some(body),
                quick: Some(true),
                ..tilo_agent_runs::Ask::default()
            };
            if let Err(error) = self.agent_runs.ask(ask) {
                tracing::warn!(%error, "a notification was not handed to an agent");
            }
        }
        for id in self.notifications.borrow().take_opening() {
            let workspace = self.active_workspace_id();
            let activation = self.notifications.borrow().open(id);
            match activation {
                notifications::HistoryActivation::OpenApp { app_id, .. } => {
                    let opened = self.search.perform(
                        &workspace,
                        tilo_desktop_search::Target::Launch { app: app_id },
                        &mut |own| {
                            self.doors
                                .invoke(
                                    &own.address,
                                    &own.arguments,
                                    &mut tilo_surface_host::Look::none(),
                                )
                                .map(drop)
                        },
                    );
                    if let Err(error) = opened {
                        self.answer_failed(&crate::i18n::t("action-failed-that-app"), &error);
                    }
                }
                other => tracing::debug!(?other, "notification chosen"),
            }
        }
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
