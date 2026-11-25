//! The person's notifications on the desktop: what apps sent the notifications service, and what
//! the session's own roles tell, shown by the banners app and kept in the shell's centre. Every
//! notification passes through one log, and what an app sent also continues on the person's other
//! devices ([`across`]).

mod across;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tilo_session_core::desktop::state::banners::Banners as _;

use crate::hosted::NotificationCentre;
use serde_json::{Map, Value};
use settings_store::notification_policy::{EffectiveNotificationPolicy, NotificationPolicy};
use settings_store::system_sounds::{QuietState, SystemSoundEvent};
use tilo_actions::{ActionAddress, ActionOutput, ApiError, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_notifications_service::{Arrival, Caller, Callers, CloseReason, Notifications, Sender};
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_session_client::device_records::{Arrived, DeviceRecord};
use tilo_session_core::desktop::notifications;
use tilo_session_core::desktop::notifications::{
    Notification, NotificationIconKind, NotificationPerson, QuietReason, RemoteNotificationRoute,
    ScheduledNotificationTarget,
};
use tilo_session_core::desktop::state::banners::BannerAsk;
use tilo_session_core::desktop::state::{DesktopNotifications, NotificationOutcome};
use tilo_session_power::PowerLink;
use tilo_workspace_service::workspaces::person_workspace::devices::SharedNotice;

use crate::app_icons;
use crate::i18n::{t, t_args};
use crate::platform::notice::{Closed, Notice, Notices, Place};
use crate::portal_link::PortalSide;
use crate::system_sounds::SystemSounds;

pub(crate) const fn severity_name(severity: notifications::NotificationSeverity) -> &'static str {
    match severity {
        notifications::NotificationSeverity::Info => "info",
        notifications::NotificationSeverity::Warning => "warning",
        notifications::NotificationSeverity::Error => "error",
    }
}

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

/// The most unread notifications one app's icon counts to.
const MAX_BADGE: u32 = 9_999;

/// Keep what the centre holds, so the next session opens the centre this one had: its titles, its
/// bodies, its senders and how old each is. What a row could act on does not survive: the sender,
/// its actions and the session's routes are gone with it.
pub(crate) fn save_history(notices: &DesktopNotifications, kept: &DeviceRecord) {
    let encoded = match notices
        .kept(ui::time::wall())
        .and_then(|encoded| serde_json::from_slice::<serde_json::Value>(&encoded))
    {
        Ok(encoded) => encoded,
        Err(error) => {
            tracing::warn!(%error, "the notification centre was not written");
            return;
        }
    };
    kept.set(encoded);
}

/// The device record the centre is kept in.
const KEPT: &str = "so.tilo.session-notifications";

/// Where the centre is kept: one of the device's records, followed, so a change Settings or an
/// agent writes there while the session runs is the centre the person sees.
pub(crate) struct KeptCentre {
    record: DeviceRecord,
    arrived: Arrived,
}

impl KeptCentre {
    /// Show what was written elsewhere since the last pass as the centre's history.
    fn take_up(&self, desktop: &DesktopNotifications) {
        let Some(value) = self.arrived.take() else {
            return;
        };
        let replaced = serde_json::to_vec(
            &value.unwrap_or_else(|| serde_json::json!({ "notifications": [] })),
        )
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            desktop
                .replace_kept(&bytes, ui::time::wall())
                .map_err(|error| error.to_string())
        });
        if let Err(error) = replaced {
            tracing::warn!(%error, "the notification centre written elsewhere was not read");
        }
    }
}

/// Open the centre the last session left, as old as it was, and hand back where it is kept so this
/// session writes it back there and follows it: one of the device's records, as the rest of what
/// the session keeps.
pub(crate) fn restore_history(
    notices: &DesktopNotifications,
    options: &crate::Options,
) -> Option<KeptCentre> {
    let kept = options.records.as_ref()?.record(KEPT);
    match kept.read() {
        Ok(Some(value)) => {
            let restored = serde_json::to_vec(&value)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    notices
                        .restore_kept(&bytes, ui::time::wall())
                        .map_err(|error| error.to_string())
                });
            if let Err(error) = restored {
                tracing::warn!(%error, "the notification centre kept was not read");
            }
        }
        Ok(None) => {}
        Err(error) => tracing::warn!(%error, "the notification centre kept was not read"),
    }
    let (record, arrived) = kept
        .follow_waking(options.wake.handle())
        .inspect_err(|error| tracing::warn!(%error, "the notification centre is not followed"))
        .unwrap_or_else(|_| (kept, Arrived::default()));
    Some(KeptCentre { record, arrived })
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
            // A program the launcher proved with no installed app behind it is a command line
            // sender: it is named by the name it gives, the entry it says it comes from, or the
            // terminal it was typed into, never by its file name.
            Some(id) if id.trim().starts_with('/') => Some(crate::app_names::program_sender_name(
                id,
                &sent.app_name,
                sent.desktop_entry.as_deref(),
            )),
            Some(id) => Some(crate::app_names::app_name(id)),
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
        .map(|(name, identity)| NotificationPerson {
            name,
            identity,
            agent: false,
        });
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
}

/// Who a notification came from: the app the session sees in the sender's process (the one it shows
/// a window for), or, for a sender with no window of its own, the app its process is named by — its
/// systemd unit, the installed entry it runs, or its program — as the portal names one. Nothing the
/// sender says about itself is believed.
pub(crate) struct AppCallers {
    /// The apps the session sees run, by process.
    running: Arc<Mutex<BTreeMap<u32, String>>>,
    /// The shell: which workspace an app runs in.
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

/// Where each notice a role told opens, by the shell's notification showing it.
type Noted = Rc<RefCell<BTreeMap<u64, (String, Option<Place>)>>>;

/// The places the person chose from a role's notices since the last look.
type PlacesChosen = Rc<RefCell<Vec<Place>>>;

/// The actions the person chose on a role's notices since the last look, by the notice's tag.
type ActionsChosen = Rc<RefCell<Vec<(String, String)>>>;

/// The notices that closed since the host last took them, each with why.
type ClosedTags = Rc<RefCell<Vec<(String, Closed)>>>;

/// What a person does on a banner the shell shows: each one reaches the desktop's log, the app
/// that sent it or the call it rings for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ask {
    Banner(BannerAsk),
    Reply { id: u64, text: String },
    Read(u64),
    AnswerCall(u64),
    DeclineCall(u64),
}

/// What ties the daemon, the desktop's log and the banners the shell draws together.
pub(crate) struct SessionNotifications {
    daemon: Notifications,
    desktop: DesktopNotifications,
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
    /// The kept notifications the person handed to an agent, by id, until the pass sends them.
    handed: RefCell<Vec<u64>>,
    chosen: Chosen,
    by_sender: BTreeMap<u32, u64>,
    up: BTreeSet<u32>,
    /// The shell's banners up at the last look: a new one is news since then.
    banners_up: BTreeSet<u64>,
    /// Desktop notifications kept out of the centre once their banner left.
    transient: BTreeSet<u64>,
    /// Whether the tilolang shell draws the banners itself: then the session ages them here, as a
    /// banner's stack would, so a banner goes a few seconds after it arrived.
    shell_banners: Cell<bool>,
    /// Counts arrivals, keeping each one's artwork apart.
    generation: u64,
    /// The Do Not Disturb change since the readout last said one: whoever flipped the switch, on
    /// screen it says so.
    quiet_changed: Cell<Option<bool>>,
    /// The banner the person sent keyboard focus to ([`BannerAction::FocusNewest`]), while it
    /// stands: the shell draws it as the one the keys act on.
    focused: Cell<Option<u64>>,
    /// Where the centre is written between sessions; `None` keeps it in memory.
    history: Option<KeptCentre>,
    /// What continues on the person's other devices, and what they showed here.
    mirror: Rc<across::Across>,
    noted: Noted,
    places_chosen: PlacesChosen,
    /// The actions the person chose on them since it was last taken, by the notice's tag.
    actions_chosen: ActionsChosen,
    /// The notification showing each notice a role told, by its tag.
    showing: RefCell<BTreeMap<String, u32>>,
    closed: ClosedTags,
    /// The notice each notification the session told carries, until the shell shows it.
    told: RefCell<BTreeMap<u32, Notice>>,
}

/// Now, in ms since the epoch, as the record of the person's devices keeps times.
fn unix_ms() -> i64 {
    i64::try_from(tilo_clock::unix_now_ms()).unwrap_or(i64::MAX)
}

impl SessionNotifications {
    /// Show `daemon` 's notifications in `desktop` 's log.
    pub(crate) fn new(
        daemon: Notifications,
        desktop: DesktopNotifications,
        policy: Arc<Mutex<NotificationPolicy>>,
        signals: Rc<NoticeSignals>,
        power: Option<PowerLink>,
        history: Option<KeptCentre>,
    ) -> Self {
        let opened: Opened = Rc::default();
        let choosing = Rc::clone(&opened);
        let chosen: Chosen = Rc::default();
        let answering = Rc::clone(&chosen);
        let mirror = Rc::new(across::Across::new());
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
                // A notification another device shows here is answered there: the action the
                // person chose, or the end of it when they only opened it.
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
                answered.dismissed(*id);
            }
        });
        Self {
            daemon,
            desktop,
            power,
            policy,
            sounds: SystemSounds::new(),
            badges: BTreeMap::new(),
            signals,
            opened,
            opening: RefCell::new(Vec::new()),
            handed: RefCell::default(),
            chosen,
            by_sender: BTreeMap::new(),
            up: BTreeSet::new(),
            banners_up: BTreeSet::new(),
            transient: BTreeSet::new(),
            shell_banners: Cell::new(false),
            generation: 0,
            quiet_changed: Cell::new(None),
            focused: Cell::new(None),
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

    /// The banner `named` names, or the newest when it names none; `None` when nothing is on
    /// screen or the named one has left.
    fn live_banner(&self, named: Option<&str>) -> Option<Notification> {
        let log = self.desktop.log();
        let active = log.active();
        match named {
            Some(named) => {
                let id = named.trim().parse::<u64>().ok()?;
                active.iter().find(|banner| banner.id == id).cloned()
            }
            None => active.last().cloned(),
        }
    }

    /// Send keyboard focus to the newest banner, and say which one took it.
    ///
    /// # Errors
    /// Nothing is on screen to take it.
    pub(crate) fn focus_newest(&self) -> Result<u64, String> {
        let id = self
            .live_banner(None)
            .ok_or_else(|| "no notification is on screen".to_owned())?
            .id;
        self.focused.set(Some(id));
        Ok(id)
    }

    /// The banner the keyboard is on, while it stands.
    fn focused_banner(&self) -> Option<u64> {
        let id = self.focused.get()?;
        let log = self.desktop.log();
        let stands = log.active().iter().any(|banner| banner.id == id);
        stands.then_some(id)
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

    /// The shell's log of what it shows: the live banners and the centre kept behind them.
    pub(crate) fn desktop(&self) -> DesktopNotifications {
        self.desktop.clone()
    }

    /// Keep this device's notifications in the record of the person's devices `resources` reads
    /// and writes from now on, while their `continuity` lets them continue, and show what their
    /// other devices keep there: each move of the record wakes the session's pass with `wake`.
    pub(crate) fn attach_record(
        &self,
        resources: crate::PersonResources,
        continuity: crate::platform::fabric::Continuity,
        wake: tilo_service::Wake,
    ) {
        let mut moved = resources.changes();
        tilo_rt::app().spawn(async move {
            while moved.changed().await.is_ok() {
                wake();
            }
        });
        self.mirror.attach(resources, continuity);
    }

    /// Show here what the record of the person's devices says moved: what they answered elsewhere
    /// of this device's notifications is carried out, and what their other devices show is shown,
    /// or taken away.
    fn follow_devices(&mut self) {
        let now = unix_ms();
        let Some(followed) = self.mirror.follow(now) else {
            return;
        };
        for (daemon_id, chosen) in followed.answered {
            self.answer_local(daemon_id, chosen.as_deref());
        }
        for (route, notice) in followed.show {
            self.remote_project(route, notice, now);
        }
        for (source, id) in followed.gone {
            self.desktop.retire_remote(&source, id);
        }
    }

    /// Show what another of the person's devices shows, which `route` leads back to: an app's
    /// notification there, named after that device, when this person lets this app's
    /// notifications cross.
    fn remote_project(&self, route: RemoteNotificationRoute, notice: SharedNotice, now_ms: i64) {
        let effective = self.effective(Some(notice.app_id.as_str()), false);
        if !effective.cross_device {
            return;
        }
        let source_label = route.source_label.clone();
        let mut notification = Notification::new(notice.title.clone(), notice.body.clone()).stated(
            u8::try_from(notice.urgency).unwrap_or(0),
            Some(Duration::from_millis(
                u64::try_from(notice.until_ms.saturating_sub(now_ms)).unwrap_or(0),
            )),
        );
        // The app's own name beside the device it was told on: a row here says what was told, and
        // whose device told it.
        notification.app_name = Some(match notice.app_name.trim() {
            "" => t_args(
                "notification-from-device-only",
                &[("device", source_label.into())],
            ),
            app => t_args(
                "notification-from-device",
                &[("app", app.into()), ("device", source_label.into())],
            ),
        });
        notification.app_id = Some(notice.app_id.clone());
        notification.actions = notice
            .actions
            .iter()
            .map(|action| (action.key.clone(), action.label.clone()))
            .collect();
        notification.remote_route = Some(route.clone());
        // A device's notification never expands what the lock screen says here, and its text only
        // shows when previews may.
        notification.show_on_lock_screen = false;
        notification.show_preview = !notice.redacted;
        let id = if let Some(existing) = self.desktop.replace_remote(&notification) {
            existing
        } else if effective.banners && !self.presenting() && !self.desktop.centre_open() {
            self.desktop.post(notification)
        } else {
            self.desktop.post_quiet(notification)
        };
        self.mirror.shown(id, &route, notice);
    }

    /// Carry out what the person answered on another device of the notification this one keeps in
    /// the record: `action` the one they chose there, or `None` for being done with it. Every
    /// other copy goes with it.
    fn answer_local(&mut self, daemon_id: u32, action: Option<&str>) {
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

    /// The notifications the shell shows now, live or kept.
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

    /// The notification centre as the session publishes it.
    pub(crate) fn centre(&self) -> NotificationCentre {
        let log = self.desktop.log();
        let notices = log
            .active()
            .iter()
            .chain(log.history().iter())
            .rev()
            .map(|notification| crate::hosted::Notice {
                number: notification.id,
                title: notification.title.clone(),
                body: notification.body.clone(),
                from: notification.app_name.clone().unwrap_or_default(),
                kind: severity_name(notification.severity).to_owned(),
                seen: !self.desktop.is_unread(notification.id),
                actions: notification
                    .actions
                    .iter()
                    .map(|(key, label)| crate::hosted::NoticeAction {
                        key: key.clone(),
                        label: label.clone(),
                    })
                    .collect(),
            })
            .collect();
        NotificationCentre {
            count: self.desktop.unread(),
            quiet: self.quiet_status(),
            notices,
            banners: log.active().iter().rev().map(|notice| notice.id).collect(),
            banner: log.active().last().map(|notice| notice.id),
            undo: self.desktop.can_undo_clear(),
        }
    }

    /// Tell the person something, as an app's `notify` does: `kind` is the centre's severity
    /// (`info` unless `warning` or `error`), and `from` who it is from when it is named.
    pub(crate) fn post(&self, title: &str, body: &str, from: &str, kind: &str) {
        use notifications::{Notification, NotificationSeverity};
        let severity = match kind {
            "warning" => NotificationSeverity::Warning,
            "error" => NotificationSeverity::Error,
            _ => NotificationSeverity::Info,
        };
        let notification =
            Notification::new(title.to_owned(), body.to_owned()).with_severity(severity);
        self.desktop.post(if from.is_empty() {
            notification
        } else {
            notification.with_app_name(from)
        });
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

    /// Hand the notification `id` the centre shows to the agent the person keeps in the
    /// workspace in front: what the row asks, carried out by the pass that sends it to the agent
    /// ([`Shared::deliver_hands`]). A notification no longer shown is not handed on.
    pub(crate) fn hand(&self, id: u64) {
        if self.notice_of(id).is_some() {
            self.handed.borrow_mut().push(id);
        }
    }

    /// The notification `id`, still up as a banner or kept in the centre.
    pub(crate) fn notice_of(&self, id: u64) -> Option<notifications::Notification> {
        let log = self.desktop.log();
        log.active()
            .iter()
            .find(|notification| notification.id == id)
            .or_else(|| log.history_entry(id))
            .cloned()
    }

    /// The notifications handed to an agent since the last look.
    pub(crate) fn take_handed(&self) -> Vec<u64> {
        std::mem::take(&mut *self.handed.borrow_mut())
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

    /// Reply in a banner that offers a reply: what the person typed reaches its sender, and the
    /// banner goes, as the banners' own reply field carries it.
    pub(crate) fn reply(&self, id: u64, text: &str) {
        let Some(banner) = self.banner(id) else {
            return;
        };
        let takes_reply = banner
            .actions
            .iter()
            .any(|(key, _)| key == tilo_session_core::desktop::notifications::INLINE_REPLY);
        if let (Some(sender), true) = (banner.sender_id, takes_reply) {
            self.daemon.replied(sender, text);
            self.desktop
                .answer(tilo_session_core::desktop::state::banners::BannerAsk::Dismiss(id));
        }
    }

    /// Clear the centre, or bring back what the last Clear all took.
    pub(crate) fn clear(&self) {
        self.desktop.clear_history();
    }

    pub(crate) fn undo_clear(&self) {
        self.desktop.undo_clear();
    }

    /// Tell the person `notice`, replacing one showing under its tag. The notifications service
    /// carries it as it carries an app's, and the shell shows it from there.
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

    /// When the shell's banners next change with no one asking: one running out, or a Clear all's
    /// undo closing.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.desktop.next_deadline()
    }

    /// The tilolang shell draws this device's banners itself: the session ages them here, so a
    /// banner goes a few seconds after it arrived rather than waiting for a dismissal.
    pub(crate) fn set_shell_banners(&self) {
        self.shell_banners.set(true);
    }

    /// The places the person chose from a role's notices since this was last taken.
    pub(crate) fn take_places_chosen(&self) -> Vec<Place> {
        std::mem::take(&mut *self.places_chosen.borrow_mut())
    }

    /// The actions the person chose on a role's notices since this was last taken, by the notice's
    /// tag: those whose tag starts with `tagged`, or every other one for `None`, so that each role
    /// takes only the answers to its own notices.
    pub(crate) fn take_actions_chosen(&self, tagged: Option<&str>) -> Vec<(String, String)> {
        let mut chosen = self.actions_chosen.borrow_mut();
        let (mine, others): (Vec<_>, Vec<_>) =
            std::mem::take(&mut *chosen)
                .into_iter()
                .partition(|(tag, _)| match tagged {
                    Some(prefix) => tag.starts_with(prefix),
                    None => !OWN_TAGS.iter().any(|prefix| tag.starts_with(prefix)),
                });
        *chosen = others;
        mine
    }

    /// The notices that closed since this was last taken, each with why.
    pub(crate) fn take_closed(&self) -> Vec<(String, Closed)> {
        std::mem::take(&mut *self.closed.borrow_mut())
    }

    /// Take in what arrived and what the banners asked, and hand the banners what they show now.
    /// `token` mints the token an app raises its window by. Answers whether a banner the person was
    /// not shown before is up now, for their visual alert.
    pub(crate) fn step(&mut self, token: &mut dyn FnMut() -> Option<String>) -> bool {
        // The tilolang shell draws its banners from the log, so the session ages them: every live
        // banner counts as on screen, so one goes a few seconds after it arrived, with nothing
        // over it holding it.
        if self.shell_banners.get() {
            let now = ui::time::now();
            let on_screen: Vec<u64> = self
                .desktop
                .log()
                .active()
                .iter()
                .rev()
                .map(|notice| notice.id)
                .collect();
            self.desktop.present(now, &on_screen, false);
        }
        // What the person's other devices show, and what they answered of this device's
        // notifications, comes in before the banners age.
        self.follow_devices();
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
        self.answer_chosen(token);
        self.settle(CloseReason::Dismissed);
        // What the shell no longer shows of this device's notifications leaves the person's other
        // devices with it.
        let shown = self.shown_ids();
        self.mirror.prune(&shown, unix_ms());
        // A notice the service no longer holds and the shell no longer shows is done with: nothing
        // is taken back under its tag again, so it is not kept for that.
        self.showing
            .borrow_mut()
            .retain(|_, id| self.daemon.notification(*id).is_some());
        // A notice whose row left the shell keeps nothing of it.
        self.noted.borrow_mut().retain(|id, _| shown.contains(id));
        // Every banner the shell puts up passes through its log, whether an app sent it or the
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
        // What the centre kept since the last look is written to this person's state, so a restart
        // opens the centre it had.
        if let Some(kept) = &self.history {
            kept.take_up(&self.desktop);
        }
        if self.desktop.take_history_dirty()
            && let Some(kept) = &self.history
        {
            save_history(&self.desktop, &kept.record);
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
                        unix_ms(),
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
                // A notice the session told is the shell's notification `id` now: that is what
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
                    .any(|(key, _)| key == tilo_session_core::desktop::notifications::INLINE_REPLY);
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
                self.desktop.ask_centre(true);
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

/// The notification centre as the session publishes it: what was kept there and what each
/// notification does.
#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Notifications", effects = CentreAction)]
pub struct NotificationCentreRoot;

pub(crate) use tilo_device_surfaces::notifications::{
    BannerAction, CentreAction, NotificationsView, view,
};

/// The resource the live banners are published under: what a banner on screen offers, with the
/// newest banner standing for one nothing names.
pub const BANNERS: &str = "session:banners";

/// The live banners as the session publishes them, where a press, the palette, an agent and a
/// script close one, choose on it, reply to it or answer the call it rings for.
#[derive(tilo_resource::Row)]
#[row(
    kind = "notifications.banners",
    label = "Notification banners",
    effects = BannerAction
)]
pub struct BannersRoot;

/// The centre as the session's services publish it: what it keeps, why fresh banners are held, and
/// the verbs that change it.
pub(crate) struct Served {
    notifications: Rc<RefCell<SessionNotifications>>,
    /// The reading last reported, so a pass that changed nothing is no revision: what moved, and
    /// how old each notice was when that reading was made.
    published: RefCell<Option<Value>>,
}

impl Served {
    /// The service publishing what `notifications` keeps.
    pub(crate) fn new(notifications: Rc<RefCell<SessionNotifications>>) -> Self {
        Self {
            notifications,
            published: RefCell::new(None),
        }
    }

    /// What the centre and the banners read now, as one view: the centre's own record, how long
    /// ago each notice arrived, which may show, and the banner the keyboard is on.
    fn render(&self) -> Value {
        let centre = {
            let notifications = self.notifications.borrow();
            notifications.centre()
        };
        let mut view = self.view_of(&centre);
        self.add_when(&mut view);
        reading(&view)
    }

    /// Add to each notice how long ago it arrived and whether its body may show, which a view
    /// reads and the centre's own record does not carry. The rest of the reading is the centre's,
    /// so a reader that knows only its fields reads it as before.
    fn add_when(&self, view: &mut NotificationsView) {
        let notifications = self.notifications.borrow();
        let log = notifications.desktop.log();
        let now = Instant::now();
        let when: std::collections::HashMap<u64, (u64, bool)> = log
            .active()
            .iter()
            .chain(log.history().iter())
            .map(|notification| {
                (
                    notification.id,
                    (
                        now.saturating_duration_since(notification.created_at)
                            .as_secs(),
                        notification.show_preview,
                    ),
                )
            })
            .collect();
        drop(log);
        for notice in &mut view.notices {
            let Some((age, preview)) = when.get(&notice.number) else {
                continue;
            };
            notice.age_seconds = *age;
            notice.preview = *preview;
        }
    }

    /// The centre as a view of another language reads it: its fields and its notices', the
    /// notices without a time until [`Self::add_when`] gives each one.
    fn view_of(&self, centre: &NotificationCentre) -> NotificationsView {
        NotificationsView {
            count: centre.count,
            quiet: centre.quiet.clone(),
            notices: centre
                .notices
                .iter()
                .map(|notice| view::Notice {
                    number: notice.number,
                    title: notice.title.clone(),
                    body: notice.body.clone(),
                    from: notice.from.clone(),
                    kind: notice.kind.clone(),
                    seen: notice.seen,
                    actions: notice
                        .actions
                        .iter()
                        .map(|action| view::NoticeAction {
                            key: action.key.clone(),
                            label: action.label.clone(),
                        })
                        .collect(),
                    age_seconds: 0,
                    preview: true,
                })
                .collect(),
            banners: centre.banners.clone(),
            banner: centre.banner,
            undo: centre.undo,
            focused: self.notifications.borrow().focused_banner(),
        }
    }
}

impl Served {
    /// Run one of the banners' actions: what the banner `named` names (the newest when it names
    /// none) does, as the Rust shell's banners did, through the same asks the desktop answers.
    fn run_banner(&self, action: &str, arguments: &Map<String, Value>) -> Result<(), ApiError> {
        let notifications = self.notifications.borrow();
        match decode::<BannerAction>(&self.resources(), BANNERS, action, arguments)? {
            BannerAction::OpenBanner { banner } => {
                let id = live_id(&notifications, banner.as_deref())?;
                notifications.ask(Ask::Banner(BannerAsk::Activate(id)));
            }
            BannerAction::Dismiss { banner } => {
                let id = live_id(&notifications, banner.as_deref())?;
                notifications.ask(Ask::Banner(BannerAsk::Dismiss(id)));
            }
            BannerAction::Act { banner, choice } => {
                let live = live_banner(&notifications, banner.as_deref())?;
                let key = choice_key(&live, choice.trim())
                    .ok_or_else(|| refused(format!("this notification offers no {choice}")))?;
                notifications.ask(Ask::Banner(BannerAsk::Invoke(live.id, key)));
            }
            BannerAction::Reply { banner, text } => {
                if text.trim().is_empty() {
                    return Err(ApiError::InvalidArguments(
                        "a reply needs some text".to_owned(),
                    ));
                }
                let live = live_banner(&notifications, banner.as_deref())?;
                if !live
                    .actions
                    .iter()
                    .any(|(key, _)| key == tilo_session_core::desktop::notifications::INLINE_REPLY)
                {
                    return Err(refused("this notification takes no reply".to_owned()));
                }
                notifications.ask(Ask::Reply { id: live.id, text });
            }
            BannerAction::Snooze { banner } => {
                let live = live_banner(&notifications, banner.as_deref())?;
                if choice_key(&live, "snooze").is_none() {
                    return Err(refused("this notification cannot be snoozed".to_owned()));
                }
                notifications.ask(Ask::Banner(BannerAsk::Invoke(live.id, "snooze".to_owned())));
            }
            BannerAction::FocusNewest => {
                notifications.focus_newest().map_err(refused)?;
            }
            BannerAction::Read { banner } => {
                let id = live_id(&notifications, banner.as_deref())?;
                notifications.ask(Ask::Read(id));
            }
            BannerAction::AnswerCall { banner } => {
                let live = live_banner(&notifications, banner.as_deref())?;
                if !live.rings {
                    return Err(refused("no call is ringing".to_owned()));
                }
                notifications.ask(Ask::AnswerCall(live.id));
            }
            BannerAction::DeclineCall { banner } => {
                let live = live_banner(&notifications, banner.as_deref())?;
                if !live.rings {
                    return Err(refused("no call is ringing".to_owned()));
                }
                notifications.ask(Ask::DeclineCall(live.id));
            }
        }
        Ok(())
    }
}

/// The banner `named` names, or the newest when it names none.
fn live_banner(
    notifications: &SessionNotifications,
    named: Option<&str>,
) -> Result<Notification, ApiError> {
    notifications.live_banner(named).ok_or_else(|| {
        refused(match named {
            Some(named) => format!("notification {named} is no longer on screen"),
            None => "no notification is on screen".to_owned(),
        })
    })
}

/// The id of the banner `named` names, or the newest when it names none.
fn live_id(notifications: &SessionNotifications, named: Option<&str>) -> Result<u64, ApiError> {
    live_banner(notifications, named).map(|live| live.id)
}

/// The key of the action `choice` names by its key or its label, as a person reads it.
fn choice_key(banner: &Notification, choice: &str) -> Option<String> {
    banner
        .actions
        .iter()
        .find(|(key, label)| key == choice || label.eq_ignore_ascii_case(choice))
        .map(|(key, _)| key.clone())
}

/// Why the banners would not do it, as the caller reads it.
fn refused(reason: String) -> ApiError {
    ApiError::Refused {
        code: "notifications".to_owned(),
        reason,
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        let mut resource = offer::<CentreAction>(
            tilo_resource::LiveRow::typed::<NotificationCentreRoot>(
                RESOURCE,
                t("session-notifications"),
            )
            .into_resource()
            .with_personal_reading(),
        );
        // The panel keeps the words the rest of the desktop names them by.
        for spec in &mut resource.actions {
            let label = match spec.id.as_str() {
                "dismiss" => t("session-notifications-dismiss"),
                "act" => t("session-notifications-act"),
                "clear" => t("session-notifications-clear"),
                "undo-clear" => t("session-notifications-undo-clear"),
                "reply" => t("session-notifications-reply"),
                "open-notice" => t("session-notifications-open"),
                "hand-to-agent" => t("session-notifications-hand-to-agent"),
                _ => continue,
            };
            spec.type_mut().label = label;
        }
        // The live banners are their own resource, as the Rust shell published them: a row on
        // screen offers what the centre's kept notice does not (answer the call, reply, snooze,
        // focus it), and the newest banner stands for one no row names.
        let banners = offer::<BannerAction>(
            tilo_resource::LiveRow::typed::<BannersRoot>(BANNERS, t("session-banners"))
                .into_resource()
                .with_personal_reading()
                .with_view(tilo_actions::ResourceViewSpec::new(
                    "banners",
                    t("session-banners"),
                    "open-banner",
                )),
        );
        vec![resource, banners]
    }

    /// What the centre keeps, newest first: what people said to this person,
    /// so only a grant the workspace gives reads it.
    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE || resource == BANNERS).then(|| self.render())
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if resource == BANNERS {
            self.run_banner(action, arguments)?;
            return Ok(Started::Done(ActionOutput::default()));
        }
        if resource != RESOURCE {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
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
            CentreAction::Reply { notification, text } => {
                self.notifications.borrow().reply(notification, &text);
            }
            CentreAction::OpenNotice { notification } => {
                self.notifications.borrow().ask_open(notification);
            }
            CentreAction::HandToAgent { notification } => {
                self.notifications.borrow().hand(notification);
            }
            CentreAction::Notify {
                title,
                body,
                from,
                kind,
            } => self.notifications.borrow().post(
                &title,
                body.as_deref().unwrap_or_default(),
                from.as_deref().unwrap_or_default(),
                kind.as_deref().unwrap_or_default(),
            ),
            CentreAction::SetQuiet { on } => {
                if let Err(error) = self.notifications.borrow().set_do_not_disturb(on) {
                    tracing::warn!(%error, "do not disturb was not kept");
                }
            }
        }
        Ok(Started::Done(ActionOutput::default()))
    }

    fn poll(&mut self) -> bool {
        let now = self.render();
        let mut published = self.published.borrow_mut();
        if published.as_ref() == Some(&now) {
            return false;
        }
        *published = Some(now);
        true
    }

    /// A reading says how long ago each notice arrived, so it is read again as each minute turns.
    fn wakes_in(&self) -> Option<Duration> {
        let second = ui::clock::local_now()
            .timestamp()
            .as_second()
            .rem_euclid(60);
        Some(Duration::from_secs(
            60_u64.saturating_sub(u64::try_from(second).unwrap_or(0)),
        ))
    }
}

impl crate::Shared {
    /// What the session's own roles tell the person, as the one place that shows it.
    pub(crate) fn telling(&self) -> Rc<dyn Notices> {
        RoleNotices::of(&self.notifications)
    }

    /// Send each kept notification the person handed to an agent ([`SessionNotifications::hand`])
    /// to the agent they keep in the workspace in front, through the session's one send: the
    /// notification's title is what it is asked, and the whole of it is what the agent reads. A
    /// new agent is kept where the workspace keeps none, as the canvas and search do.
    pub(crate) fn deliver_hands(&self) {
        let handed = self.notifications.borrow().take_handed();
        for id in handed {
            let Some(notification) = self.notifications.borrow().notice_of(id) else {
                continue;
            };
            let title: String = notification.title.chars().take(240).collect();
            let body = if notification.body.is_empty() {
                title.clone()
            } else {
                format!("{title}\n\n{}", notification.body)
            };
            let workspace = self.active_workspace_id();
            let to = crate::agents::agent_recipient(self.search.kept_agent(&workspace));
            let mut arguments = serde_json::Map::new();
            arguments.insert("to".into(), serde_json::json!([to]));
            arguments.insert("text".into(), serde_json::json!(body));
            arguments.insert("workspace".into(), serde_json::json!(workspace));
            // The send goes out in the pass that took it, as any other: its answer is nobody's
            // here, and the person reads where it landed in Chat.
            if let Err(error) = self.sends.send(None, &arguments) {
                tracing::warn!(%error, "a notification was not handed to an agent");
            }
        }
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
                    self.published
                        .invoke(
                            &own.address,
                            &own.arguments,
                            &mut tilo_session_core::surfaces::Look::none(),
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
                            self.published
                                .invoke(
                                    &own.address,
                                    &own.arguments,
                                    &mut tilo_session_core::surfaces::Look::none(),
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

/// The tag beginnings of the roles whose notices answer only to themselves
/// ([`RoleNotices::tagged`]).
const OWN_TAGS: &[&str] = &[crate::program_asks::NOTICE_TAG];

/// The person's notifications as a role's own: what a role tells them, and where they chose to go.
/// A role that tags its notices with its own beginning takes only the actions chosen on those.
pub(crate) struct RoleNotices(Rc<RefCell<SessionNotifications>>, Option<&'static str>);

impl RoleNotices {
    /// The notices `notifications` show.
    #[cfg(test)]
    pub(crate) fn over(notifications: Rc<RefCell<SessionNotifications>>) -> Self {
        Self(notifications, None)
    }

    /// The notices `notifications` show, as a role tells them.
    pub(crate) fn of(
        notifications: &Rc<RefCell<SessionNotifications>>,
    ) -> Rc<dyn crate::platform::notice::Notices> {
        Rc::new(Self(Rc::clone(notifications), None))
    }

    /// The notices `notifications` show, for the role whose notice tags all start with `tags`.
    pub(crate) fn tagged(
        notifications: &Rc<RefCell<SessionNotifications>>,
        tags: &'static str,
    ) -> Rc<dyn crate::platform::notice::Notices> {
        Rc::new(Self(Rc::clone(notifications), Some(tags)))
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
        self.0.borrow().take_actions_chosen(self.1)
    }

    fn take_closed(&self) -> Vec<(String, Closed)> {
        self.0.borrow().take_closed()
    }
}

/// The local wall clock as the notification policy reads it: unix seconds,
/// minutes since midnight and the weekday counted from Monday.
fn local_now() -> (i64, u16, u8) {
    let now = jiff::Zoned::now();
    (
        now.timestamp().as_second(),
        u16::try_from(
            i16::from(now.hour())
                .saturating_mul(60)
                .saturating_add(i16::from(now.minute())),
        )
        .unwrap_or(u16::MAX),
        u8::try_from(now.weekday().to_monday_zero_offset()).unwrap_or(u8::MAX),
    )
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod told;
