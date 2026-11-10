//! The person's notifications on the desktop: what apps sent the notifications service, and what
//! the session's own roles tell, shown by the banners app and kept in the desk's centre. Every
//! notification passes through one log, and what an app sent also continues on the person's other
//! devices ([`mirror`]).

mod mirror;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::platform::fabric::ContinuityNotificationProjection;
use crate::platform::fabric::SharedLink;
use crate::platform::fabric::{DeviceId, PersonalFabricAction};
use chrono::{Datelike as _, Timelike as _};
use serde_json::{Map, Value};
use settings_store::notification_policy::{EffectiveNotificationPolicy, NotificationPolicy};
use settings_store::system_sounds::{QuietState, SystemSoundEvent};
use tilo_actions::{ActionAddress, ActionOutput, DoorError, ResourceSpec};
use tilo_desktop_surface::notifications::{
    self, BannerAsk, ContinuityNotificationId, Notification, NotificationIconKind,
    NotificationPerson, QuietReason, RemoteNotificationRoute, ScheduledNotificationTarget,
};
use tilo_desktop_surface::{DesktopNotifications, NotificationOutcome};
use tilo_lock::MutexRecover as _;
use tilo_notification_banners::{Ask, Link};
use tilo_notifications_service::{Arrival, Caller, Callers, CloseReason, Notifications, Sender};
use tilo_quick_settings::hosted::NotificationCentre;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_session_power::PowerLink;

use crate::app_icons;
use crate::i18n::{t, t_args};
use crate::platform::notice::{Closed, Notice, Notices, Place};
use crate::portal_link::PortalSide;
use crate::system_sounds::SystemSounds;

/// `notice` as the notifications service carries it: the session's own word about the machine, or a
/// role's, shown as any other notification.
#[must_use]
pub(crate) fn served(notice: &Notice, id: u32) -> tilo_notifications_service::Notification {
    tilo_notifications_service::Notification {
        id,
        app_name: notice.app.clone(),
        summary: notice.title.clone(),
        body: notice.body.clone(),
        actions: notice.actions.clone(),
        expire_timeout: notice.expires_after.map_or(-1, |after| {
            i32::try_from(after.as_millis()).unwrap_or(i32::MAX)
        }),
        urgency: notice.urgency,
        machine_wide: notice.machine_wide,
        suppress_sound: notice.sound.is_some(),
        // The session raises its own notice: it is the system's word about the machine, not a
        // sender's.
        system: true,
        ..tilo_notifications_service::Notification::new(0)
    }
}

/// Show `notice` through the notifications service, and say which notification went out: a producer
/// whose notice the person acts on finds their answer by that id.
pub(crate) fn tell_daemon(notice: &Notice, notifications: &Notifications) -> u32 {
    let id = notifications.alloc_id();
    notifications.push(served(notice, id));
    id
}

/// Mount the banners app on the desk with the `banners` link it asks back on. Under the shell none
/// is mounted: the shell draws the banner itself from the desk it reads (`Desk.banner`), so a stack
/// of its own would draw a second one and take its presses.
pub(crate) fn mount_banners(
    compositor: &mut tilo_compositor::Compositor,
    doors: &mut tilo_session_core::door::Doors,
    notices: &DesktopNotifications,
    desk: &tilo_desktop_surface::DesktopHandle,
    options: &crate::Options,
    stack: crate::panels::Reserved,
    banners: Link,
) {
    if options.shell {
        notices.show_banners_elsewhere();
        return;
    }
    match tilo_notification_banners::prepare(banners.clone(), &options.device, &options.person)
        .and_then(|component| ui::App::from_component(component, (options.presentation)()))
    {
        Ok(app) => {
            let surface = Rc::new(RefCell::new(crate::panels::Banners::new(
                app,
                banners,
                desk.clone(),
                options.scale,
                stack,
            )));
            compositor.add_host(surface.clone());
            doors.add(surface);
            notices.show_banners_elsewhere();
        }
        Err(error) => tracing::warn!(%error, "notification banners not mounted"),
    }
}

/// The most unread notifications one app's icon counts to.
const MAX_BADGE: u32 = 9_999;

/// Where the centre's history is kept between sessions: this person's state directory, as the
/// emoji's recents are.
pub(crate) fn history_file() -> Option<std::path::PathBuf> {
    tilo_paths::state_dir().map(|root| root.join("tilo/notifications.json"))
}

/// Keep what the centre holds, so the next session opens the centre this one had: its titles, its
/// bodies, its senders and how old each is. What a row could act on does not survive: the sender,
/// its actions and the session's routes are gone with it.
pub(crate) fn save_history(notices: &DesktopNotifications, path: &std::path::Path) {
    let history = notices.log().history().to_vec();
    let encoded = match tilo_desktop_surface::notifications::encode_history(
        &history,
        ui::time::SystemTime::now(),
        ui::time::now(),
    ) {
        Ok(encoded) => encoded,
        Err(error) => {
            tracing::warn!(%error, "the notification centre was not written");
            return;
        }
    };
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(%error, "no place for the notification centre");
        return;
    }
    // Written beside and moved over, so a session that ends mid-write never leaves half a file to
    // be read back.
    let beside = path.with_extension("json.new");
    if let Err(error) =
        std::fs::write(&beside, &encoded).and_then(|()| std::fs::rename(&beside, path))
    {
        tracing::warn!(%error, "the notification centre was not written");
    }
}

/// Open the centre the last session left, and hand back where it is kept so this session writes it
/// back there.
pub(crate) fn restore_history(notices: &DesktopNotifications) -> Option<std::path::PathBuf> {
    let path = history_file()?;
    restore_kept(notices, &path);
    Some(path)
}

/// Open the centre kept at `path` : what it kept, as old as it was.
fn restore_kept(notices: &DesktopNotifications, path: &std::path::Path) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    match tilo_desktop_surface::notifications::decode_history(
        &bytes,
        ui::time::SystemTime::now(),
        ui::time::now(),
    ) {
        Ok(history) => {
            if !history.is_empty() {
                notices.restore_history(history);
            }
        }
        Err(error) => tracing::warn!(%error, "the notification centre kept was not read"),
    }
}

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

/// The desktop's notification for what an app sent the daemon, or for the notice the session told
/// (`told`): the tier and the time on screen are the sender's, as `stated` reads them. Critical
/// urgency stays until the person acts, as the specification asks; otherwise `-1` is the desktop's
/// default, `0` stays, and a positive timeout is that many milliseconds on screen.
pub(crate) fn shown(
    sent: tilo_notifications_service::Notification,
    generation: u64,
    told: Option<&Notice>,
) -> Notification {
    let app_id = sent.caller.as_ref().map(|caller| caller.app_id.clone());
    let expires = match sent.expire_timeout {
        timeout if timeout < 0 => None,
        0 => Some(Duration::ZERO),
        ms => Some(Duration::from_millis(u64::try_from(ms).unwrap_or(0))),
    };
    let mut notification = Notification::new(sent.summary, sent.body).stated(sent.urgency, expires);
    notification.sender_id = Some(sent.id);
    // `app_name` is the sender's own word: an attributed app is named by what the device calls that
    // app — a program with no app behind it by what it calls itself — and an unattributed sender's
    // name is shown as its claim, never as who it is.
    // The desktop's own notice is neither: the session raises it in its own name, so it keeps the
    // name it carries and leads with what the telling said it is about, or the system's mark rather
    // than being read as some app's claim.
    if sent.system {
        notification.app_name = (!sent.app_name.trim().is_empty()).then(|| sent.app_name.clone());
        notification.icon_kind = told.map_or(NotificationIconKind::System, |notice| notice.leading);
    } else {
        notification.app_name = match app_id.as_deref() {
            Some(id) => Some(crate::app_names::sender_name(id, &sent.app_name)),
            None => notifications::sender_claim(&sent.app_name),
        };
    }
    // Artwork from what proves the app first, then lower-trust hints that can only supply pixels,
    // then the sender's own decoded image. Only ever pixels: it never names or groups the app.
    notification.app_image = app_icons::notification(
        format!("notification:{}:{generation}", sent.id),
        app_id.as_deref(),
        sent.desktop_entry.as_deref(),
        sent.app_icon.as_deref(),
        sent.image_data
            .map(|pixels| ui::raster::rgba8(pixels.rgba, pixels.width, pixels.height)),
        notification.app_name.as_deref(),
    );
    notification.app_id = app_id;
    // A person named by an attributed communications app leads the banner; the endpoint only picks
    // their colour and is never shown.
    notification.from = sent
        .sender_name
        .zip(sent.contact_endpoint)
        .map(|(name, identity)| NotificationPerson { name, identity });
    // A call rings for the person it is from; the category alone is only the app's word.
    if sent.incoming_call && notification.from.is_some() {
        notification.rings = true;
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
    // Never from the wire: an app that could pierce Do Not Disturb would ask every time.
    notification.show_on_lock_screen = false;
    notification
}

/// The keys a call's answer or decline goes by, as senders name them.
const ANSWER_KEYS: [&str; 2] = ["accept", "answer"];
const DECLINE_KEYS: [&str; 3] = ["decline", "reject", "hangup"];

/// What the bar reads of the notifications each frame: how many are unread, whether fresh banners
/// are held, and each app's own count.
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

    /// How many notifications each app (by verified id) is showing as unread, where it shows any
    /// and this person keeps its badges on. The count on `app_id` 's badge.
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

/// Who a notification came from: the app the session sees in the sender's process (the one it shows
/// a window for), or, for a sender with no window of its own, the app its process is named by — its
/// systemd unit, the installed entry it runs, or its program — as the portal names one. Nothing the
/// sender says about itself is believed.
pub(crate) struct AppCallers {
    /// The apps the session sees run, by process.
    running: Arc<Mutex<BTreeMap<u32, String>>>,
    /// The desk: which workspace an app runs in.
    side: PortalSide,
    /// Where a process with no window of its own is named from.
    requesters: tilo_portal::Requesters,
    /// What a process was already named: naming one reads its cgroup, its program and the installed
    /// entries.
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

/// Where a chosen scheduled notification opens, as `(workspace, uri)` .
type Opened = Rc<RefCell<Vec<(String, String)>>>;

/// The senders whose notification the person chose, as `(sender, action)` .
type Chosen = Rc<RefCell<Vec<(u32, Option<String>)>>>;

/// Where each notice a role told opens, by the desk's notification showing it.
type Noted = Rc<RefCell<BTreeMap<u64, (String, Option<Place>)>>>;

/// The places the person chose from a role's notices since the last look.
type PlacesChosen = Rc<RefCell<Vec<Place>>>;

/// The actions the person chose on a role's notices since the last look, by the notice's tag.
type ActionsChosen = Rc<RefCell<Vec<(String, String)>>>;

/// The notices that closed since the host last took them, each with why.
type ClosedTags = Rc<RefCell<Vec<(String, Closed)>>>;

/// What ties the daemon, the desktop's log and the banners app together.
pub(crate) struct SessionNotifications {
    daemon: Notifications,
    desktop: DesktopNotifications,
    banners: Link,
    /// What holding the machine from going idle says: someone is watching (a slideshow, a call).
    power: Option<PowerLink>,
    /// The person's notification policy, kept current by the session's settings watch: what each
    /// arrival is judged by.
    policy: Arc<Mutex<NotificationPolicy>>,
    sounds: SystemSounds,
    /// Each attributed app's latest unread count, kept while its badges are off.
    badges: BTreeMap<String, u32>,
    signals: Rc<NoticeSignals>,
    opened: Opened,
    opening: RefCell<Vec<u64>>,
    chosen: Chosen,
    by_sender: BTreeMap<u32, u64>,
    up: BTreeSet<u32>,
    /// The desk's banners up at the last look: a new one is news since then.
    banners_up: BTreeSet<u64>,
    /// Desktop notifications kept out of the centre once their banner left.
    transient: BTreeSet<u64>,
    /// The log revision the banners were last handed.
    published: Option<u64>,
    /// Counts arrivals, keeping each one's artwork apart.
    generation: u64,
    /// The Do Not Disturb change since the readout last said one: whoever flipped the switch, on
    /// screen it says so.
    quiet_changed: Cell<Option<bool>>,
    /// Where the centre is written between sessions; `None` keeps it in memory.
    history: Option<std::path::PathBuf>,
    /// What continues on the person's other devices, and what they showed here.
    mirror: Rc<mirror::Mirroring>,
    noted: Noted,
    places_chosen: PlacesChosen,
    /// The actions the person chose on them since it was last taken, by the notice's tag.
    actions_chosen: ActionsChosen,
    /// The notification showing each notice a role told, by its tag.
    showing: RefCell<BTreeMap<String, u32>>,
    closed: ClosedTags,
    /// The notice each notification the session told carries, until the desk shows it.
    told: RefCell<BTreeMap<u32, Notice>>,
}

impl SessionNotifications {
    /// Show `daemon` 's notifications in `desktop` 's log, its banners on `banners` .
    pub(crate) fn new(
        daemon: Notifications,
        desktop: DesktopNotifications,
        banners: Link,
        policy: Arc<Mutex<NotificationPolicy>>,
        signals: Rc<NoticeSignals>,
        power: Option<PowerLink>,
        history: Option<std::path::PathBuf>,
    ) -> Self {
        let opened: Opened = Rc::default();
        let choosing = Rc::clone(&opened);
        let chosen: Chosen = Rc::default();
        let answering = Rc::clone(&chosen);
        let mirror = mirror::Mirroring::new();
        let answered = Rc::clone(&mirror);
        let noted: Noted = Rc::default();
        let showing_noted = Rc::clone(&noted);
        let places_chosen: PlacesChosen = Rc::default();
        let telling_places = Rc::clone(&places_chosen);
        let actions_chosen: ActionsChosen = Rc::default();
        let telling_actions = Rc::clone(&actions_chosen);
        let closed: ClosedTags = Rc::default();
        let telling_closed = Rc::clone(&closed);
        desktop.on_outcome(move |outcome| match outcome {
            NotificationOutcome::Activated {
                notification,
                action,
            } => {
                // A notice a role told: the person went where it named, and it leaves with the
                // banner they chose.
                if let Some((tag, place)) = showing_noted.borrow_mut().remove(&notification.id) {
                    if let Some(place) = place {
                        telling_places.borrow_mut().push(place);
                    }
                    if let Some(action) = action {
                        telling_actions.borrow_mut().push((tag, action.clone()));
                    }
                }
                // A notification another device projected is answered there: the action the person
                // chose, or the end of it here when they only opened it. The origins that device's
                // id names are the only ones it can reach.
                if let Some(route) = notification.remote_route.clone() {
                    let chosen = action.as_deref().filter(|key| {
                        notification
                            .actions
                            .iter()
                            .any(|(offered, _)| offered == key)
                    });
                    answered.answer_route(&route, chosen);
                }
                // Choosing a scheduled notification opens what it rang for,
                // in the workspace that scheduled it.
                if let Some(target) = notification
                    .scheduled_target
                    .as_ref()
                    .filter(|target| target.valid())
                {
                    choosing
                        .borrow_mut()
                        .push((target.workspace.clone(), target.uri.clone()));
                }
                // Choosing answers the sender, with a token its app may raise its window by: the
                // loop mints it.
                if let Some(sender) = notification.sender_id {
                    answering.borrow_mut().push((sender, action.clone()));
                }
            }
            NotificationOutcome::Dismissed { id } => {
                if let Some((tag, _)) = showing_noted.borrow_mut().remove(id) {
                    telling_closed.borrow_mut().push((tag, Closed::Dismissed));
                }
                // Dismissing another device's notification here dismisses it there too: the app's
                // notification closes, and every other copy goes with it.
                answered.answer(*id, |_, id| PersonalFabricAction::NotificationDismiss {
                    id,
                });
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
            history,
            mirror,
            noted,
            places_chosen,
            actions_chosen,
            showing: RefCell::default(),
            closed,
            told: RefCell::default(),
        }
    }

    /// The policy as it stands for `app_id` at the local time now; `None` asks for the desktop's
    /// own, which no per-app rule narrows.
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

    /// Whether an app holds the machine from going idle: someone is watching a slideshow or a call.
    fn presenting(&self) -> bool {
        self.power.as_ref().is_some_and(PowerLink::idle_held)
    }

    /// How the person's calm settings stand at the local time now, as the event sounds read them.
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

    /// Why fresh banners are held, as the bar and the centre header say it. A focus schedule is
    /// pierced by priority senders, so it is applied per arrival too.
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

    /// Turn Do Not Disturb on or off: the one persisted switch, applied here at once so what the
    /// person just flipped is what everything else reads.
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

    /// The desk's log of what it shows: the live banners and the centre kept behind them.
    pub(crate) fn desktop(&self) -> DesktopNotifications {
        self.desktop.clone()
    }

    /// Carry this device's notifications over `link` from now on, and show what it carries.
    pub(crate) fn attach_fabric(&self, link: SharedLink) {
        self.mirror.attach(link);
    }

    /// Show what another of the person's devices projected: an app's notification there, named
    /// after that device, when this person lets this app's notifications cross.
    pub(crate) fn remote_project(
        &self,
        source: &DeviceId,
        source_label: &str,
        projection: &ContinuityNotificationProjection,
    ) {
        let now = tilo_clock::unix_now_ms();
        if !projection.validate_at(now) {
            return;
        }
        let effective = self.effective(Some(projection.app_id.as_str()), false);
        if !effective.cross_device {
            return;
        }
        let route = RemoteNotificationRoute {
            source_device: source.as_str().to_owned(),
            source_label: source_label.to_owned(),
            notification_id: projection.id,
        };
        let mut notification = Notification::new(
            projection.title.clone(),
            if projection.body_redacted {
                String::new()
            } else {
                projection.body.clone()
            },
        )
        .stated(
            projection.urgency,
            Some(Duration::from_millis(
                projection.expires_unix_ms.saturating_sub(now),
            )),
        );
        // The app's own name beside the device it was told on: a row here says what was told, and
        // whose device told it.
        notification.app_name = Some(match projection.app_name.trim() {
            "" => t_args(
                "notification-from-device-only",
                &[("device", source_label.into())],
            ),
            app => t_args(
                "notification-from-device",
                &[("app", app.into()), ("device", source_label.into())],
            ),
        });
        notification.app_id = Some(projection.app_id.clone());
        notification.actions.clone_from(&projection.actions);
        notification.remote_route = Some(route.clone());
        // A device's notification never expands what the lock screen says here, and its text only
        // shows when previews may.
        notification.show_on_lock_screen = false;
        notification.show_preview = !projection.body_redacted;
        let id = if let Some(existing) = self.desktop.replace_remote(&notification) {
            existing
        } else if effective.banners && !self.presenting() && !self.desktop.centre_open() {
            self.desktop.post(notification)
        } else {
            self.desktop.post_quiet(notification)
        };
        self.mirror.shown(id, route);
    }

    /// Take away what another of the person's devices projected.
    pub(crate) fn remote_retire(&self, source: &DeviceId, id: ContinuityNotificationId) {
        self.mirror.retire_shown(source, id);
        self.desktop.retire_remote(source.as_str(), id);
    }

    /// Answer what another device asked of the notification this one projected: `action` the person
    /// there chose, or `None` for being done with it. Every other copy goes with it.
    pub(crate) fn answer_local(&mut self, daemon_id: u32, action: Option<&str>) {
        if let Some(key) = action {
            self.daemon.action_invoked(daemon_id, key, None);
        }
        self.daemon.closed(daemon_id, CloseReason::Dismissed);
        if let Some(id) = self.by_sender.remove(&daemon_id) {
            self.desktop.withdraw(id);
            self.transient.remove(&id);
        }
        self.up.remove(&daemon_id);
        self.mirror.retire(daemon_id);
    }

    /// Carry out a choice another device made on a notification this one projected. Returns
    /// whether it named one this device projected, with an action the app offered.
    pub(crate) fn answered(&mut self, action: &PersonalFabricAction) -> bool {
        let Some((daemon_id, chosen)) = self.mirror.answered(action) else {
            return false;
        };
        self.answer_local(daemon_id, chosen.as_deref());
        true
    }

    /// The notifications the desk shows now, live or kept.
    fn shown_ids(&self) -> BTreeSet<u64> {
        let log = self.desktop.log();
        log.active()
            .iter()
            .chain(log.history().iter())
            .map(|notification| notification.id)
            .collect()
    }

    /// The targets of scheduled notifications the person chose since the last look, as
    /// `(workspace, uri)` , for the session to open.
    pub(crate) fn take_opened(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.opened.borrow_mut())
    }

    /// The notification centre as the session publishes it: how many it keeps. The list itself is
    /// the shell's centre, which reads the desk's own log; what the panel needs is the count that
    /// opens it.
    pub(crate) fn centre(&self) -> NotificationCentre {
        NotificationCentre {
            count: self.desktop.log().history().len(),
            quiet: self.quiet_status(),
        }
    }

    /// Choose a retained notification in the centre, as the panel asks.
    pub(crate) fn open(&self, id: u64) -> notifications::HistoryActivation {
        self.desktop.activate_history(id)
    }

    /// Open the retained notification `id` in the app that sent it: what a press on it in the
    /// centre asks, carried out by the loop that opens apps.
    pub(crate) fn ask_open(&self, id: u64) {
        self.opening.borrow_mut().push(id);
    }

    /// The notifications the person chose to open since the last look.
    pub(crate) fn take_opening(&self) -> Vec<u64> {
        std::mem::take(&mut *self.opening.borrow_mut())
    }

    /// Dismiss a retained notification, or every one an app grouped: the centre and the panel list
    /// them no more.
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

    /// Tell the person `notice`, replacing one showing under its tag. The notifications service
    /// carries it as it carries an app's, and the desk shows it from there.
    pub(crate) fn tell(&self, notice: &Notice) {
        // The session raises the notice, so it plays its sound here: a sender's category never
        // reaches the events only the session may claim, and the notification carries none for the
        // arrival to repeat.
        if let Some(event) = notice.sound {
            self.sounds.play(event, self.quiet_state(), true);
        }
        self.withdraw_notice(&notice.tag);
        let id = tell_daemon(notice, &self.daemon);
        self.told.borrow_mut().insert(id, notice.clone());
        self.showing.borrow_mut().insert(notice.tag.clone(), id);
    }

    /// Take the notice under `tag` back: what it told is gone.
    pub(crate) fn withdraw_notice(&self, tag: &str) {
        let Some(id) = self.showing.borrow_mut().remove(tag) else {
            return;
        };
        self.told.borrow_mut().remove(&id);
        self.daemon.request_close(id);
        self.closed
            .borrow_mut()
            .push((tag.to_owned(), Closed::Withdrawn));
    }

    /// When the desk's banners next change with no one asking: one running out, or a Clear all's
    /// undo closing.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.desktop.next_deadline()
    }

    /// The places the person chose from a role's notices since this was last taken.
    pub(crate) fn take_places_chosen(&self) -> Vec<Place> {
        std::mem::take(&mut *self.places_chosen.borrow_mut())
    }

    /// The actions the person chose on a role's notices since this was last taken, by the notice's
    /// tag.
    pub(crate) fn take_actions_chosen(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.actions_chosen.borrow_mut())
    }

    /// The notices that closed since this was last taken, each with why.
    pub(crate) fn take_closed(&self) -> Vec<(String, Closed)> {
        std::mem::take(&mut *self.closed.borrow_mut())
    }

    /// Take in what arrived and what the banners asked, and hand the banners what they show now.
    /// `token` mints the token an app raises its window by. Answers whether a banner the person was
    /// not shown before is up now, for their visual alert.
    pub(crate) fn step(&mut self, token: &mut dyn FnMut() -> Option<String>) -> bool {
        // Banners whose time on screen ran out leave first.
        self.desktop.expire(ui::time::now());
        self.settle(CloseReason::Expired);
        // Whether fresh banners are held, for the bar's bell and the centre header, before they are
        // published.
        self.desktop.set_quiet(self.quiet_status());
        // The machine's graphics stopped for good: the person is told once, in the pass the
        // renderers' record first says so.
        crate::notices::tell_graphics_fallback(&self.daemon);
        for arrival in self.daemon.drain() {
            self.arrive(arrival);
        }
        for ask in self.banners.take_asks() {
            self.ask(ask);
        }
        self.answer_chosen(token);
        self.settle(CloseReason::Dismissed);
        // What the desk no longer shows of this device's notifications leaves the person's other
        // devices with it.
        let shown = self.shown_ids();
        self.mirror.prune(&shown, tilo_clock::unix_now_ms());
        // A notice the service no longer holds and the desk no longer shows is done with: nothing
        // is taken back under its tag again, so it is not kept for that.
        self.showing
            .borrow_mut()
            .retain(|_, id| self.daemon.notification(*id).is_some());
        // A notice whose row left the desk keeps nothing of it.
        self.noted.borrow_mut().retain(|id, _| shown.contains(id));
        // Every banner the desk puts up passes through its log, whether an app sent it or the
        // session raised it: a new one there is what the person was just told.
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
        // What the centre kept since the last look is written to this person's state, so a restart
        // opens the centre it had.
        if self.desktop.take_history_dirty()
            && let Some(path) = &self.history
        {
            save_history(&self.desktop, path);
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
                let system = sent.system;
                let badge_count = sent.badge_count;
                let sound = sound_event(&sent);
                let effective = self.effective(app_id.as_deref(), sent.priority_contact);
                // A notification under a shown one's id replaces it.
                if let Some(old) = self.by_sender.remove(&sender) {
                    self.desktop.withdraw(old);
                    self.transient.remove(&old);
                }
                self.generation = self.generation.wrapping_add(1);
                // What only the telling knows of this notification: its mark, its place, and the
                // tag it is taken back under.
                let told = self.told.borrow_mut().remove(&sender);
                let mut notification = shown(sent, self.generation, told.as_ref());
                notification.show_preview = effective.previews;
                notification.show_on_lock_screen = effective.lock_screen;
                // What the person's other devices show of it, judged by the same policy: their own
                // rules decide whether it is shown there, and its text leaves this device only when
                // previews may. The desktop's own news is about this machine and stays here.
                if effective.cross_device && !system {
                    self.mirror.project(
                        sender,
                        (
                            notification.app_id.as_deref().unwrap_or_default(),
                            notification.app_name.as_deref().unwrap_or_default(),
                        ),
                        &notification,
                        effective.previews,
                        tilo_clock::unix_now_ms(),
                    );
                }
                // Held banners go straight to the centre, unread: quiet asks for calm, not to be
                // kept uninformed. Presenting is held whatever the sender's own rule says, and so
                // is a banner while the centre stands open: it joins the list the person is reading
                // rather than covering it.
                let id = if effective.banners && !self.presenting() && !self.desktop.centre_open() {
                    self.desktop.post(notification)
                } else {
                    self.desktop.post_quiet(notification)
                };
                self.by_sender.insert(sender, id);
                // A notice the session told is the desk's notification `id` now: that is what
                // choosing it names.
                if let Some(told) = told {
                    self.noted.borrow_mut().insert(id, (told.tag, told.place));
                }
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
                    // Notifications and messages carry the policy's own verdict, which lets a
                    // priority sender through focus time; other events follow System sounds alone.
                    let allows = !matches!(
                        event,
                        SystemSoundEvent::Notification | SystemSoundEvent::Message
                    ) || effective.sounds;
                    self.sounds.play(event, self.quiet_state(), allows);
                }
            }
            Arrival::Close(sender) => {
                self.told.borrow_mut().remove(&sender);
                if let Some(id) = self.by_sender.remove(&sender) {
                    self.noted.borrow_mut().remove(&id);
                    self.desktop.withdraw(id);
                    self.transient.remove(&id);
                }
                self.up.remove(&sender);
            }
        }
    }

    /// Answer the apps whose notification the person chose: the token, the action, then the close.
    fn answer_chosen(&self, token: &mut dyn FnMut() -> Option<String>) {
        for (sender, action) in std::mem::take(&mut *self.chosen.borrow_mut()) {
            if let Some(key) = &action {
                self.daemon.action_invoked(sender, key, token());
            }
            self.daemon.closed(sender, CloseReason::Dismissed);
        }
    }

    /// The live banner `id` .
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
                if let (Some(sender), true) = (banner.sender_id, takes_reply) {
                    self.daemon.replied(sender, &text);
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

    /// Answer or decline the call banner `id` rings for: the caller's own answer or decline when it
    /// offers one, else opening the call (its default action) or closing the banner.
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

    /// Tell the senders of banners no longer up that they closed, for `reason` ; keep a transient
    /// one out of the centre.
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
            self.daemon.closed(sender, reason);
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

/// What kind of resource [`RESOURCE`] is: the session's own kind, as the clipboard is, so the grant
/// the device's panel reads the session's own readings through reaches it.
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

/// The centre as the session's services publish it: what it keeps, why fresh banners are held, and
/// the verbs that change it.
pub(crate) struct Served {
    notifications: Rc<RefCell<SessionNotifications>>,
    /// The reading last reported, so a pass that changed nothing is no revision.
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
    /// What the session's own roles tell the person, as the one place that shows it.
    pub(crate) fn telling(&self) -> Rc<dyn Notices> {
        RoleNotices::of(&self.notifications)
    }

    /// Open what the person chose on a scheduled notification, in the workspace that scheduled it,
    /// and the centre's kept ones in the app that sent them: the app that takes its scheme.
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
                            &mut tilo_session_core::door::Look::none(),
                        )
                        .map(drop)
                },
            );
            if let Err(error) = opened {
                self.answer_failed(&crate::i18n::t("action-failed-that-notification"), &error);
            }
        }
        for id in self.notifications.borrow().take_opening() {
            let workspace = self.active_workspace_id();
            let activation = self.notifications.borrow().open(id);
            match activation {
                notifications::HistoryActivation::OpenApp { app_id, .. } => {
                    let opened = self.search.perform(
                        &workspace,
                        tilo_desktop_search::Target::Launch {
                            app: app_id,
                            place: tilo_desktop_search::LaunchPlace::Focus,
                        },
                        &mut |own| {
                            self.doors
                                .invoke(
                                    &own.address,
                                    &own.arguments,
                                    &mut tilo_session_core::door::Look::none(),
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

/// The person's notifications as a role's own: what a role tells them, and where they chose to go.
pub(crate) struct RoleNotices(Rc<RefCell<SessionNotifications>>);

impl RoleNotices {
    /// The notices `notifications` show.
    #[cfg(test)]
    pub(crate) fn over(notifications: Rc<RefCell<SessionNotifications>>) -> Self {
        Self(notifications)
    }

    /// The notices `notifications` show, as a role tells them.
    pub(crate) fn of(
        notifications: &Rc<RefCell<SessionNotifications>>,
    ) -> Rc<dyn crate::platform::notice::Notices> {
        Rc::new(Self(Rc::clone(notifications)))
    }
}

impl Notices for RoleNotices {
    fn show(&self, notice: &Notice) {
        self.0.borrow().tell(notice);
    }

    fn withdraw(&self, tag: &str) {
        self.0.borrow().withdraw_notice(tag);
    }

    fn take_chosen(&self) -> Vec<Place> {
        self.0.borrow().take_places_chosen()
    }

    fn take_chosen_actions(&self) -> Vec<(String, String)> {
        self.0.borrow().take_actions_chosen()
    }

    fn take_closed(&self) -> Vec<(String, Closed)> {
        self.0.borrow().take_closed()
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
#[cfg(test)]
pub(crate) mod told;
