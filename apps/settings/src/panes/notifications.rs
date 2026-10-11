//! Notifications pane — the editor for the **shared notification policy**.
//!
//! This panel edits [`crate::prefs::NotificationPrefs`], the `notifications`
//! section of the person's settings. The compositor's notification layer
//! reads that section when it decides whether to raise a banner, reveal its
//! contents, sound it, or mirror it to the lock screen — so the toggles here
//! are the single source of truth the rest of the system follows, exactly
//! like Appearance owns the theme.
//!
//! Every control is bound to a [`NotificationEffect`]:
//! - **Do Not Disturb** — master mute for banners and sounds.
//! - **Show banners** — pop-up banners for incoming notifications.
//! - **Show previews** — reveal the message body (off shows only the app name).
//! - **Play sounds** — an audible cue when a notification arrives.
//! - **Show on lock screen** — mirror notifications onto the lock screen.
//! - **Focus** — a scheduled quiet period that only priority apps pierce.
//! - **Per app** — overrides of the delivery choices for one verified app,
//!   priority during focus, and a temporary mute. An app's rows are live rows
//!   (`settings.notification-app:<id>`) offering the same effects bound to it.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, HashMap};

use settings_store::notification_policy::AppNotificationRule;
use taffy::{NodeId, TaffyTree};
use tilo_clock::unix_now_i64;
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, LiveRow};
use ui::theme::Theme;
use ui::tilo_resource::{self, ActionRef, Effect};
use ui::widget::{AppModel, Dropdown, ModelPath, Toggle, Widget};

use crate::effects::{SettingsEffect, effect};
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::prefs::NotificationPrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    El, SettingsSection, StatusTone, card, choice_row, error_row, note, rows, semantic_status_pill,
    setting_row, setting_row_described, value_row,
};

const FOCUS_DAYS: &[(&str, u8)] = &[
    ("Every day", 0),
    ("Weekdays", 0b0001_1111),
    ("Weekends", 0b0110_0000),
];

const MUTE_OPTIONS: &[(&str, i64)] = &[
    ("Not muted", 0),
    ("For 1 hour", 60 * 60),
    ("For 8 hours", 8 * 60 * 60),
    ("For 24 hours", 24 * 60 * 60),
];

/// The most apps the published tree lists; an app beyond it is still
/// reachable by id.
const MAX_APP_ROWS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstalledNotificationApp {
    id: String,
    name: String,
}

fn valid_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn installed_notification_apps(
    existing: &std::collections::BTreeMap<
        String,
        settings_store::notification_policy::AppNotificationRule,
    >,
) -> Vec<InstalledNotificationApp> {
    installed_notification_apps_from(existing, &desktop_entry::cache::cached_scan().entries)
}

/// Merge the Tilo registry with the system's freedesktop application catalog.
/// A host app's desktop-file id and a Flatpak's `X-Flatpak` id are the same
/// stable identities the notification service verifies from launch/cgroup
/// metadata; display names are presentation only.
fn installed_notification_apps_from(
    existing: &std::collections::BTreeMap<
        String,
        settings_store::notification_policy::AppNotificationRule,
    >,
    desktop_entries: &[desktop_entry::DesktopEntry],
) -> Vec<InstalledNotificationApp> {
    let mut apps = BTreeMap::new();
    for entry in package_store::list_installed().unwrap_or_default() {
        let Ok(manifest) = tilo_app_package::view::read_manifest(&entry.manifest_path) else {
            continue;
        };
        if manifest.entrypoint.gui_bin().is_none() {
            continue;
        }
        let id = if manifest.package_id.is_empty() {
            entry.id
        } else {
            manifest.package_id
        };
        if !valid_app_id(&id) {
            continue;
        }
        let name = manifest
            .name
            .unwrap_or_else(|| package_store::display_name_for_package_id(&id));
        apps.entry(id).or_insert(name);
    }
    for entry in desktop_entries {
        let id = match &entry.source {
            desktop_entry::EntrySource::Flatpak { app_id } => app_id.clone(),
            desktop_entry::EntrySource::Host => entry.id.clone(),
        };
        if valid_app_id(&id) {
            apps.entry(id).or_insert_with(|| entry.name.clone());
        }
    }
    // Preserve access to a rule when an app has just been uninstalled. It can
    // be reset without trusting the sender-controlled notification app_name.
    for id in existing.keys().filter(|id| valid_app_id(id)) {
        apps.entry(id.clone())
            .or_insert_with(|| package_store::display_name_for_package_id(id));
    }
    let mut apps: Vec<_> = apps
        .into_iter()
        .map(|(id, name)| InstalledNotificationApp { id, name })
        .collect();
    apps.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    apps
}

fn english<T>(table: &'static [(&'static str, T)]) -> impl Iterator<Item = &'static str> {
    table.iter().map(|(label, _)| *label)
}

/// How notifications reach the person: globally, during focus, and per app.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum NotificationEffect {
    /// Turn Do Not Disturb on or off.
    #[effect(idempotent)]
    SetDoNotDisturb { on: bool },
    /// Show notification banners, or stop.
    #[effect(idempotent)]
    SetBanners { on: bool },
    /// Show message previews in notifications, or only the app's name.
    #[effect(idempotent)]
    SetPreviews { on: bool },
    /// Play a sound when a notification arrives, or stop.
    #[effect(idempotent)]
    SetSounds { on: bool },
    /// Show notifications on the lock screen, or stop.
    #[effect(idempotent)]
    SetLockScreen { on: bool },
    /// Show notification badges on app icons, or stop.
    #[effect(idempotent)]
    SetBadges { on: bool },
    /// Turn the scheduled focus period on or off.
    #[effect(idempotent)]
    SetFocus { on: bool },
    /// Set when the focus period begins.
    #[effect(idempotent)]
    SetFocusStart {
        #[param("The time, HH:MM", choices = english(FOCUS_TIMES))]
        time: String,
    },
    /// Set when the focus period ends.
    #[effect(idempotent)]
    SetFocusEnd {
        #[param("The time, HH:MM", choices = english(FOCUS_TIMES))]
        time: String,
    },
    /// Set on which days the focus period applies.
    #[effect(idempotent)]
    SetFocusDays {
        #[param("The days", choices = english(FOCUS_DAYS))]
        days: String,
    },
    /// Show one app's notification banners, or stop.
    #[effect(idempotent)]
    SetAppBanners {
        #[param("The app's id")]
        app: String,
        on: bool,
    },
    /// Show previews in one app's notifications, or stop.
    #[effect(idempotent)]
    SetAppPreviews {
        #[param("The app's id")]
        app: String,
        on: bool,
    },
    /// Play one app's notification sounds, or stop.
    #[effect(idempotent)]
    SetAppSounds {
        #[param("The app's id")]
        app: String,
        on: bool,
    },
    /// Show one app's notification badges, or stop.
    #[effect(idempotent)]
    SetAppBadges {
        #[param("The app's id")]
        app: String,
        on: bool,
    },
    /// Let one app's notifications through during focus, or stop.
    #[effect(idempotent)]
    SetAppPriority {
        #[param("The app's id")]
        app: String,
        on: bool,
    },
    /// Let one app's notifications from priority contacts through during
    /// focus, or stop.
    #[effect(idempotent)]
    SetAppPriorityContacts {
        #[param("The app's id")]
        app: String,
        on: bool,
    },
    /// Mute one app's notifications for a while, or unmute it.
    #[effect(idempotent)]
    MuteApp {
        #[param("The app's id")]
        app: String,
        #[param("For how long", choices = english(MUTE_OPTIONS))]
        duration: String,
    },
}

/// One app's override of a delivery choice.
fn app_rule(
    app: &str,
    change: impl FnOnce(&mut AppNotificationRule),
) -> Result<impl FnOnce(&mut NotificationPrefs), String> {
    if !valid_app_id(app) {
        return Err(format!("`{app}` is not an app id"));
    }
    let app = app.to_owned();
    Ok(move |prefs: &mut NotificationPrefs| change(prefs.per_app.entry(app).or_default()))
}

/// Answer `effect`.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: NotificationEffect,
    _cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    use NotificationEffect as E;
    let host = app.host();
    match effect {
        E::SetDoNotDisturb { on } => host.update(|p: &mut NotificationPrefs| p.do_not_disturb = on),
        E::SetBanners { on } => host.update(|p: &mut NotificationPrefs| p.show_banners = on),
        E::SetPreviews { on } => host.update(|p: &mut NotificationPrefs| p.show_previews = on),
        E::SetSounds { on } => host.update(|p: &mut NotificationPrefs| p.play_sounds = on),
        E::SetLockScreen { on } => {
            host.update(|p: &mut NotificationPrefs| p.show_on_lock_screen = on)
        }
        E::SetBadges { on } => host.update(|p: &mut NotificationPrefs| p.show_badges = on),
        E::SetFocus { on } => host.update(|p: &mut NotificationPrefs| p.focus.enabled = on),
        E::SetFocusStart { time } => {
            let minute =
                focus_time_minutes(&time).ok_or_else(|| format!("`{time}` is not offered"))?;
            host.update(|p: &mut NotificationPrefs| p.focus.start_minute = minute)
        }
        E::SetFocusEnd { time } => {
            let minute =
                focus_time_minutes(&time).ok_or_else(|| format!("`{time}` is not offered"))?;
            host.update(|p: &mut NotificationPrefs| p.focus.end_minute = minute)
        }
        E::SetFocusDays { days } => {
            let days =
                focus_days_from_label(&days).ok_or_else(|| format!("`{days}` is not offered"))?;
            host.update(|p: &mut NotificationPrefs| p.focus.weekdays = days)
        }
        E::SetAppBanners { app, on } => host.update(app_rule(&app, |r| r.banners = Some(on))?),
        E::SetAppPreviews { app, on } => host.update(app_rule(&app, |r| r.previews = Some(on))?),
        E::SetAppSounds { app, on } => host.update(app_rule(&app, |r| r.sounds = Some(on))?),
        E::SetAppBadges { app, on } => host.update(app_rule(&app, |r| r.badges = Some(on))?),
        E::SetAppPriority { app, on } => host.update(app_rule(&app, |r| r.priority = on)?),
        E::SetAppPriorityContacts { app, on } => {
            host.update(app_rule(&app, |r| r.priority_contacts = on)?)
        }
        E::MuteApp { app, duration } => {
            let seconds =
                mute_seconds(&duration).ok_or_else(|| format!("`{duration}` is not offered"))?;
            let until = (seconds > 0).then(|| unix_now_i64().saturating_add(seconds));
            host.update(app_rule(&app, |r| r.muted_until = until)?)
        }
    }
}

/// What the notification area keeps for the window: the policy as last
/// read, and the apps it lists.
pub struct NotificationsArea {
    prefs: RefCell<Followed<NotificationPrefs>>,
    apps: OnceCell<Vec<InstalledNotificationApp>>,
    sample: bool,
    /// The installed apps, read off the UI thread after the window's first
    /// frame ([`Self::start_read`]).
    read: crate::device_read::DeviceRead<Vec<InstalledNotificationApp>>,
}

impl NotificationsArea {
    /// The area over `cx`'s settings. A capture lists a fixed set of apps
    /// rather than the ones this device has installed.
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            prefs: RefCell::new(cx.follow::<NotificationPrefs>()),
            apps: OnceCell::new(),
            sample: !cx.reaches_devices(),
            read: crate::device_read::DeviceRead::default(),
        }
    }

    /// The apps whose notifications have rules: installed ones, and any
    /// with a rule left behind.
    fn apps(&self) -> &[InstalledNotificationApp] {
        self.apps.get_or_init(|| {
            self.read
                .take()
                .unwrap_or_else(|| Self::read_apps(self.sample, &self.prefs.borrow().get().per_app))
        })
    }

    /// The installed apps (every package manifest and desktop entry), or
    /// the sample a capture lists.
    fn read_apps(
        sample: bool,
        per_app: &BTreeMap<String, AppNotificationRule>,
    ) -> Vec<InstalledNotificationApp> {
        if sample {
            sample_apps()
        } else {
            installed_notification_apps(per_app)
        }
    }

    /// Read the installed apps off the UI thread; `false` when they have
    /// been read or are being read.
    pub(crate) fn start_read(&self) -> bool {
        if self.is_open() {
            return false;
        }
        let sample = self.sample;
        let per_app = self.prefs.borrow().get().per_app.clone();
        self.read.start(move || Self::read_apps(sample, &per_app))
    }

    /// Whether the read [`Self::start_read`] started has ended.
    pub(crate) fn read_finished(&self) -> bool {
        self.read.is_finished()
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::effects::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.apps.get().is_some()
    }

    /// One live row per app, offering its delivery choices flipped and its
    /// mute lifted or set.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        self.prefs.borrow_mut().refresh();
        let apps = self.apps();
        let prefs = self.prefs.borrow();
        let prefs = prefs.get();
        apps.iter()
            .take(MAX_APP_ROWS)
            .map(|app| app_row(prefs, app))
            .collect()
    }
}

/// The verified apps a capture lists.
fn sample_apps() -> Vec<InstalledNotificationApp> {
    [
        ("so.tilo.calendar", "Calendar"),
        ("so.tilo.chat", "Chat"),
        ("so.tilo.mail", "Mail"),
    ]
    .into_iter()
    .map(|(id, name)| InstalledNotificationApp {
        id: id.to_string(),
        name: name.to_string(),
    })
    .collect()
}

/// `effect` offered on a row as `label`.
fn offered(effect: impl Into<SettingsEffect>, label: String) -> tilo_actions::ActionSpec {
    let mut spec = effect.into().bound();
    spec.label = label;
    spec
}

fn flip(on: bool, noun: &str) -> String {
    format!("Turn {} {noun}", if on { "on" } else { "off" })
}

/// The live row of `app` under `prefs`: each delivery choice offered as the
/// flip of what applies to it now.
fn app_row(prefs: &NotificationPrefs, app: &InstalledNotificationApp) -> LiveRow {
    let rule = prefs.per_app.get(&app.id).copied().unwrap_or_default();
    let id = app.id.clone();
    let banners = !rule.banners.unwrap_or(prefs.show_banners);
    let previews = !rule.previews.unwrap_or(prefs.show_previews);
    let sounds = !rule.sounds.unwrap_or(prefs.play_sounds);
    let badges = !rule.badges.unwrap_or(prefs.show_badges);
    let muted = rule.muted_until.is_some_and(|until| until > unix_now_i64());
    let mute = if muted {
        offered(
            NotificationEffect::MuteApp {
                app: id.clone(),
                duration: "Not muted".into(),
            },
            "Unmute notifications".into(),
        )
    } else {
        offered(
            NotificationEffect::MuteApp {
                app: id.clone(),
                duration: "For 1 hour".into(),
            },
            "Mute notifications for an hour".into(),
        )
    };
    LiveRow::new(
        format!("settings.notification-app:{}", app.id),
        "settings.notification-app",
        app.name.clone(),
    )
    .operation(offered(
        NotificationEffect::SetAppBanners {
            app: id.clone(),
            on: banners,
        },
        flip(banners, "notification banners"),
    ))
    .operation(offered(
        NotificationEffect::SetAppPreviews {
            app: id.clone(),
            on: previews,
        },
        flip(previews, "message previews"),
    ))
    .operation(offered(
        NotificationEffect::SetAppSounds {
            app: id.clone(),
            on: sounds,
        },
        flip(sounds, "notification sounds"),
    ))
    .operation(offered(
        NotificationEffect::SetAppBadges {
            app: id.clone(),
            on: badges,
        },
        flip(badges, "notification badges"),
    ))
    .operation(offered(
        NotificationEffect::SetAppPriority {
            app: id.clone(),
            on: !rule.priority,
        },
        flip(!rule.priority, "priority during focus"),
    ))
    .operation(offered(
        NotificationEffect::SetAppPriorityContacts {
            app: id,
            on: !rule.priority_contacts,
        },
        flip(!rule.priority_contacts, "priority contacts during focus"),
    ))
    .operation(mute)
}

fn mute_ftl(english: &str) -> &'static str {
    match english {
        "For 1 hour" => "notif-mute-1h",
        "For 8 hours" => "notif-mute-8h",
        "For 24 hours" => "notif-mute-24h",
        _ => "notif-mute-off",
    }
}

fn mute_shown(english: &str) -> String {
    t(mute_ftl(english))
}

fn mute_label(deadline: Option<i64>, now: i64) -> String {
    let english = match deadline.filter(|deadline| *deadline > now) {
        None => "Not muted",
        Some(deadline) => {
            let remaining = deadline - now;
            if remaining <= 60 * 60 {
                "For 1 hour"
            } else if remaining <= 8 * 60 * 60 {
                "For 8 hours"
            } else {
                "For 24 hours"
            }
        }
    };
    mute_shown(english)
}

fn mute_seconds(label: &str) -> Option<i64> {
    MUTE_OPTIONS.iter().find_map(|(english, seconds)| {
        (*english == label || mute_shown(english) == label).then_some(*seconds)
    })
}

/// The English name of a mute duration shown in either language.
fn mute_english(shown: &str) -> String {
    english(MUTE_OPTIONS)
        .find(|english| *english == shown || mute_shown(english) == shown)
        .unwrap_or(shown)
        .to_owned()
}

fn mute_options() -> Vec<String> {
    MUTE_OPTIONS
        .iter()
        .map(|(english, _)| mute_shown(english))
        .collect()
}

const FOCUS_TIMES: &[(&str, u16)] = &[
    ("18:00", 18 * 60),
    ("20:00", 20 * 60),
    ("22:00", 22 * 60),
    ("23:00", 23 * 60),
    ("00:00", 0),
    ("06:00", 6 * 60),
    ("07:00", 7 * 60),
    ("08:00", 8 * 60),
    ("09:00", 9 * 60),
];

fn focus_time_label(minutes: u16) -> &'static str {
    FOCUS_TIMES
        .iter()
        .min_by_key(|(_, value)| value.abs_diff(minutes.min(1439)))
        .map_or("22:00", |(label, _)| *label)
}

fn focus_time_minutes(label: &str) -> Option<u16> {
    FOCUS_TIMES
        .iter()
        .find_map(|(candidate, value)| (*candidate == label).then_some(*value))
}

fn focus_days_ftl(english: &str) -> &'static str {
    match english {
        "Weekdays" => "notif-days-weekdays",
        "Weekends" => "notif-days-weekends",
        _ => "notif-days-every",
    }
}

fn focus_days_shown(english: &str) -> String {
    t(focus_days_ftl(english))
}

fn focus_days_label(days: u8) -> String {
    let english = FOCUS_DAYS
        .iter()
        .find_map(|(label, value)| (*value == days).then_some(*label))
        .unwrap_or("Every day");
    focus_days_shown(english)
}

fn focus_days_from_label(label: &str) -> Option<u8> {
    FOCUS_DAYS.iter().find_map(|(english, days)| {
        (*english == label || focus_days_shown(english) == label).then_some(*days)
    })
}

/// The English name of a day set shown in either language.
fn focus_days_english(shown: &str) -> String {
    english(FOCUS_DAYS)
        .find(|english| *english == shown || focus_days_shown(english) == shown)
        .unwrap_or(shown)
        .to_owned()
}

fn focus_days_options() -> Vec<String> {
    FOCUS_DAYS
        .iter()
        .map(|(english, _)| focus_days_shown(english))
        .collect()
}

/// What one app's controls show.
struct AppShown {
    banners: LocalState<bool>,
    previews: LocalState<bool>,
    sounds: LocalState<bool>,
    badges: LocalState<bool>,
    priority: LocalState<bool>,
    priority_contacts: LocalState<bool>,
    mute: LocalState<String>,
}

impl AppShown {
    fn new() -> Self {
        Self {
            banners: LocalState::new(false),
            previews: LocalState::new(false),
            sounds: LocalState::new(false),
            badges: LocalState::new(false),
            priority: LocalState::new(false),
            priority_contacts: LocalState::new(false),
            mute: LocalState::new(String::new()),
        }
    }

    /// Show `rule`, an override falling back to the global choice.
    fn show(&self, prefs: &NotificationPrefs, rule: &AppNotificationRule, now: i64) {
        self.banners.set(rule.banners.unwrap_or(prefs.show_banners));
        self.previews
            .set(rule.previews.unwrap_or(prefs.show_previews));
        self.sounds.set(rule.sounds.unwrap_or(prefs.play_sounds));
        self.badges.set(rule.badges.unwrap_or(prefs.show_badges));
        self.priority.set(rule.priority);
        self.priority_contacts.set(rule.priority_contacts);
        self.mute.set(mute_label(rule.muted_until, now));
    }
}

/// What the pane's global controls show.
struct Shown {
    do_not_disturb: LocalState<bool>,
    banners: LocalState<bool>,
    previews: LocalState<bool>,
    sounds: LocalState<bool>,
    lock_screen: LocalState<bool>,
    badges: LocalState<bool>,
    focus: LocalState<bool>,
    focus_start: LocalState<String>,
    focus_end: LocalState<String>,
    focus_days: LocalState<String>,
    apps: HashMap<String, AppShown>,
}

impl Shown {
    fn new(apps: &[InstalledNotificationApp]) -> Self {
        let flag = || LocalState::new(false);
        let text = || LocalState::new(String::new());
        Self {
            do_not_disturb: flag(),
            banners: flag(),
            previews: flag(),
            sounds: flag(),
            lock_screen: flag(),
            badges: flag(),
            focus: flag(),
            focus_start: text(),
            focus_end: text(),
            focus_days: text(),
            apps: apps
                .iter()
                .map(|app| (app.id.clone(), AppShown::new()))
                .collect(),
        }
    }

    fn show(&self, prefs: &NotificationPrefs) {
        self.do_not_disturb.set(prefs.do_not_disturb);
        self.banners.set(prefs.show_banners);
        self.previews.set(prefs.show_previews);
        self.sounds.set(prefs.play_sounds);
        self.lock_screen.set(prefs.show_on_lock_screen);
        self.badges.set(prefs.show_badges);
        self.focus.set(prefs.focus.enabled);
        self.focus_start
            .set(focus_time_label(prefs.focus.start_minute).to_owned());
        self.focus_end
            .set(focus_time_label(prefs.focus.end_minute).to_owned());
        self.focus_days.set(focus_days_label(prefs.focus.weekdays));
        let now = unix_now_i64();
        for (id, shown) in &self.apps {
            let rule = prefs.per_app.get(id).copied().unwrap_or_default();
            shown.show(prefs, &rule, now);
        }
    }
}

pub struct NotificationsPane {
    cx: PaneCx,
    prefs: Followed<NotificationPrefs>,
    apps: Vec<InstalledNotificationApp>,
    shown: Shown,
    structure_dirty: bool,
    composed: ui::widget::Composed,
}

impl NotificationsPane {
    /// The pane over `cx`'s notification policy, listing `area`'s apps.
    pub fn new(cx: PaneCx, area: &NotificationsArea) -> Self {
        let prefs = cx.follow::<NotificationPrefs>();
        let apps = area.apps().to_vec();
        let shown = Shown::new(&apps);
        shown.show(prefs.get());
        Self {
            cx,
            prefs,
            apps,
            shown,
            structure_dirty: false,
            composed: ui::widget::Composed::default(),
        }
    }

    pub fn prefs(&self) -> &NotificationPrefs {
        self.prefs.get()
    }

    /// A one-word delivery status, so the effect of the toggles reads at a
    /// glance without re-parsing every row.
    fn status(&self) -> (String, StatusTone) {
        let prefs = self.prefs();
        if prefs.do_not_disturb {
            (t("notif-status-silenced"), StatusTone::Neutral)
        } else if !prefs.show_banners {
            (t("notif-status-banners-off"), StatusTone::Warning)
        } else {
            (t("notif-status-active"), StatusTone::Success)
        }
    }

    fn toggle(
        &self,
        id: &str,
        state: &LocalState<bool>,
        on: ActionRef<NotificationEffect, bool>,
    ) -> El {
        Box::new(
            self.cx
                .bind(Toggle::from_state(state).with_id(id), effect(on)),
        )
    }

    /// A per-app toggle: the app is bound, the control supplies the value.
    fn app_toggle(
        &self,
        app: &str,
        field: &str,
        state: &LocalState<bool>,
        set: ActionRef<NotificationEffect, (String, bool)>,
    ) -> El {
        let id = format!("notifications.app.{app}.{field}");
        let app = app.to_owned();
        Box::new(self.cx.bind(
            Toggle::from_state(state).with_id(&id),
            effect(set).map(move |on: bool| (app.clone(), on)),
        ))
    }

    fn choice(
        options: Vec<String>,
        state: &LocalState<String>,
        bind: impl FnOnce(Dropdown) -> Dropdown,
    ) -> El {
        let placeholder = state.get();
        Box::new(bind(
            Dropdown::from_state(options, state).with_placeholder(&placeholder),
        ))
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let app_cards = self.build_app_cards();
        let mut cards = vec![
            self.build_dnd_card(),
            self.build_focus_card(),
            self.build_banners_card(),
            self.build_delivery_card(),
            Self::build_apps_card(app_cards.is_empty()),
            Self::build_explainer_card(),
        ];
        cards.extend(app_cards);
        cards.push(Self::build_priority_contacts_card());
        crate::widgets::cards(cards)
    }

    fn build_dnd_card(&self) -> SettingsSection {
        let (status_label, status_tone) = self.status();

        // A label-left / status-pill-right row, so the combined effect of the
        // toggles is legible at the top of the pane.
        let status_row = setting_row(
            &t("notif-delivery"),
            semantic_status_pill(&status_label, status_tone),
        );

        // Master Do Not Disturb, with the visible delivery-status pill.
        card(
            &t("notif-dnd-title"),
            rows(vec![
                status_row,
                setting_row_described(
                    &t("notif-dnd"),
                    &t("notif-dnd-desc"),
                    self.toggle(
                        "notifications.dnd",
                        &self.shown.do_not_disturb,
                        NotificationEffect::SET_DO_NOT_DISTURB,
                    ),
                ),
            ]),
        )
    }

    fn build_banners_card(&self) -> SettingsSection {
        card(
            &t("notif-banners-title"),
            rows(vec![
                setting_row_described(
                    &t("notif-show-banners"),
                    &t("notif-show-banners-desc"),
                    self.toggle(
                        "notifications.banners",
                        &self.shown.banners,
                        NotificationEffect::SET_BANNERS,
                    ),
                ),
                setting_row_described(
                    &t("notif-previews"),
                    &t("notif-previews-desc"),
                    self.toggle(
                        "notifications.previews",
                        &self.shown.previews,
                        NotificationEffect::SET_PREVIEWS,
                    ),
                ),
            ]),
        )
    }

    fn build_delivery_card(&self) -> SettingsSection {
        let sounds_row = match (
            media::event_sound::sound_backend(),
            super::installed_programs_unreadable(),
        ) {
            (media::event_sound::SoundBackend::PipeWire, _) => {
                value_row(&t("notif-sounds"), &t("notif-sounds-pipewire"))
            }
            (media::event_sound::SoundBackend::Unavailable, Some(error)) => error_row(
                &t("notif-sounds"),
                &t_args("settings-programs-error", &[("error", error.into())]),
            ),
            (media::event_sound::SoundBackend::Unavailable, None) => {
                value_row(&t("notif-sounds"), &t("notif-sounds-missing"))
            }
        };
        card(
            &t("notif-delivery-title"),
            rows(vec![
                setting_row_described(
                    &t("notif-lock-screen"),
                    &t("notif-lock-screen-desc"),
                    self.toggle(
                        "notifications.lock_screen",
                        &self.shown.lock_screen,
                        NotificationEffect::SET_LOCK_SCREEN,
                    ),
                ),
                sounds_row,
                setting_row_described(
                    &t("notif-play-sounds"),
                    &t("notif-play-sounds-desc"),
                    self.toggle(
                        "notifications.sounds",
                        &self.shown.sounds,
                        NotificationEffect::SET_SOUNDS,
                    ),
                ),
                setting_row_described(
                    &t("notif-badges"),
                    &t("notif-badges-desc"),
                    self.toggle(
                        "notifications.badges",
                        &self.shown.badges,
                        NotificationEffect::SET_BADGES,
                    ),
                ),
            ]),
        )
    }

    fn build_focus_card(&self) -> SettingsSection {
        let cx = &self.cx;
        let focus_options: Vec<String> = english(FOCUS_TIMES).map(str::to_owned).collect();
        card(
            &t("notif-focus-title"),
            rows(vec![
                setting_row_described(
                    &t("notif-focus-enabled"),
                    &t("notif-focus-enabled-desc"),
                    self.toggle(
                        "notifications.focus.enabled",
                        &self.shown.focus,
                        NotificationEffect::SET_FOCUS,
                    ),
                ),
                choice_row(
                    &t("notif-focus-from"),
                    Self::choice(focus_options.clone(), &self.shown.focus_start, |control| {
                        cx.bind(
                            control.with_id("notifications.focus.start"),
                            effect(NotificationEffect::SET_FOCUS_START),
                        )
                    }),
                ),
                choice_row(
                    &t("notif-focus-days"),
                    Self::choice(focus_days_options(), &self.shown.focus_days, |control| {
                        cx.bind(
                            control.with_id("notifications.focus.days"),
                            effect(NotificationEffect::SET_FOCUS_DAYS)
                                .map(|shown: String| focus_days_english(&shown)),
                        )
                    }),
                ),
                choice_row(
                    &t("notif-focus-until"),
                    Self::choice(focus_options, &self.shown.focus_end, |control| {
                        cx.bind(
                            control.with_id("notifications.focus.end"),
                            effect(NotificationEffect::SET_FOCUS_END),
                        )
                    }),
                ),
                note(&t("notif-focus-caption")),
            ]),
        )
    }

    fn build_app_cards(&self) -> Vec<SettingsSection> {
        let prefs = self.prefs();
        let mut app_cards = Vec::new();
        for app in &self.apps {
            let Some(shown) = self.shown.apps.get(&app.id) else {
                continue;
            };
            let rule = prefs.per_app.get(&app.id).copied().unwrap_or_default();
            let banners = rule.banners.unwrap_or(prefs.show_banners);
            let previews = rule.previews.unwrap_or(prefs.show_previews);
            let app_caption = if banners && previews {
                t("notif-app-allowed")
            } else if banners {
                t("notif-app-hidden")
            } else {
                t("notif-app-history")
            };
            let id = app.id.clone();
            let mute = Self::choice(mute_options(), &shown.mute, |control| {
                self.cx.bind(
                    control.with_id(&format!("notifications.app.{}.mute", app.id)),
                    effect(NotificationEffect::MUTE_APP)
                        .map(move |shown: String| (id.clone(), mute_english(&shown))),
                )
            });
            let toggle = |field, description: &str, label: &str, state, set| {
                setting_row_described(
                    &t(label),
                    &t(description),
                    self.app_toggle(&app.id, field, state, set),
                )
            };
            app_cards.push(card(
                &app.name,
                rows(vec![
                    value_row(&t("notif-app-verified"), &app.id),
                    toggle(
                        "banners",
                        "notif-app-banners-desc",
                        "notif-app-banners",
                        &shown.banners,
                        NotificationEffect::SET_APP_BANNERS,
                    ),
                    toggle(
                        "previews",
                        "notif-app-previews-desc",
                        "notif-app-previews",
                        &shown.previews,
                        NotificationEffect::SET_APP_PREVIEWS,
                    ),
                    toggle(
                        "sounds",
                        "notif-app-sounds-desc",
                        "notif-app-sounds",
                        &shown.sounds,
                        NotificationEffect::SET_APP_SOUNDS,
                    ),
                    toggle(
                        "badges",
                        "notif-app-badges-desc",
                        "notif-app-badges",
                        &shown.badges,
                        NotificationEffect::SET_APP_BADGES,
                    ),
                    toggle(
                        "priority",
                        "notif-app-priority-desc",
                        "notif-app-priority",
                        &shown.priority,
                        NotificationEffect::SET_APP_PRIORITY,
                    ),
                    toggle(
                        "priority_contacts",
                        "notif-app-contacts-desc",
                        "notif-app-contacts",
                        &shown.priority_contacts,
                        NotificationEffect::SET_APP_PRIORITY_CONTACTS,
                    ),
                    choice_row(&t("notif-app-mute"), mute),
                    note(&app_caption),
                ]),
            ));
        }
        app_cards
    }

    fn build_apps_card(is_empty: bool) -> SettingsSection {
        if is_empty {
            card(
                &t("notif-apps-title"),
                rows(vec![note(&t("notif-apps-empty"))]),
            )
        } else {
            card(
                &t("notif-apps-title"),
                rows(vec![note(&t("notif-apps-caption"))]),
            )
        }
    }

    fn build_explainer_card() -> SettingsSection {
        card(
            &t("notif-how-title"),
            rows(vec![note(&t("notif-how-caption"))]),
        )
    }

    fn build_priority_contacts_card() -> SettingsSection {
        card(
            &t("notif-priority-title"),
            rows(vec![
                value_row(
                    &t("notif-priority-source"),
                    &t("notif-priority-source-value"),
                ),
                note(&t("notif-priority-caption")),
            ]),
        )
    }
}

impl Widget for NotificationsPane {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if self.prefs.refresh() {
            self.shown.show(self.prefs.get());
            // The status pill and the per-app captions follow the values.
            self.structure_dirty = true;
        }
        self.composed.sync(state, root);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let composition = self.build_composition();
        let child = self
            .composed
            .build_layout(composition, taffy, theme, node_map);
        crate::panes::pane_node(taffy, child, node_map, self)
    }

    ui::forward_to_composed!(composed);

    fn name(&self) -> &'static str {
        "NotificationsPane"
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self.composed.take_structure_dirty();
        own || child
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    fn set(app: &mut TestApp<SettingsShell>, effect: NotificationEffect) {
        app.effect(&SettingsEffect::from(effect))
            .expect("the notification effect is answered");
    }

    /// Rules enumerate system and Flatpak desktop identities.
    #[test]
    fn notification_rules_enumerate_desktop_identities() {
        {
            let tmp = tempfile::tempdir().unwrap();
            let apps_dir = tmp.path().join("applications");
            std::fs::create_dir_all(&apps_dir).unwrap();
            std::fs::write(
                apps_dir.join("org.example.Writer.desktop"),
                "[Desktop Entry]\nType=Application\nName=Writer\nExec=/bin/true\n",
            )
            .unwrap();
            std::fs::write(
                apps_dir.join("org.example.Chat.desktop"),
                "[Desktop Entry]\nType=Application\nName=Chat\nExec=/bin/true\nX-Flatpak=org.example.Chat\n",
            )
            .unwrap();
            let entries = desktop_entry::scan_dirs(
                &[apps_dir],
                &[desktop_entry::scan::CURRENT_DESKTOP.to_string()],
            )
            .entries;

            let apps = installed_notification_apps_from(&BTreeMap::default(), &entries);
            assert_eq!(
                apps.iter()
                    .find(|app| app.id == "org.example.Chat")
                    .map(|app| app.name.as_str()),
                Some("Chat"),
                "Flatpak rules use X-Flatpak rather than the desktop-file id"
            );
            assert_eq!(
                apps.iter()
                    .find(|app| app.id == "org.example.Writer")
                    .map(|app| app.name.as_str()),
                Some("Writer"),
                "system rules use the desktop-file id"
            );
        }
    }

    /// The global policy, the focus schedule and per-app overrides are the
    /// person's `notifications` section, where the compositor's notification
    /// layer reads them; an override touches only its own field.
    #[test]
    fn notification_effects_write_the_policy() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        set(&mut app, NotificationEffect::SetDoNotDisturb { on: true });
        set(&mut app, NotificationEffect::SetBanners { on: false });
        set(&mut app, NotificationEffect::SetFocus { on: true });
        set(
            &mut app,
            NotificationEffect::SetFocusStart {
                time: "20:00".into(),
            },
        );
        set(
            &mut app,
            NotificationEffect::SetFocusDays {
                days: "Weekdays".into(),
            },
        );
        set(
            &mut app,
            NotificationEffect::SetAppSounds {
                app: "so.tilo.chat".into(),
                on: false,
            },
        );
        set(
            &mut app,
            NotificationEffect::MuteApp {
                app: "so.tilo.mail".into(),
                duration: "For 8 hours".into(),
            },
        );
        let prefs = app.settings::<NotificationPrefs>().get();
        assert!(prefs.do_not_disturb);
        assert!(!prefs.show_banners);
        assert!(prefs.focus.enabled);
        assert_eq!(prefs.focus.start_minute, 20 * 60);
        assert_eq!(prefs.focus.weekdays, 0b0001_1111);
        let chat = prefs.per_app["so.tilo.chat"];
        assert_eq!(chat.sounds, Some(false));
        assert_eq!(chat.banners, None, "only the changed field is an override");
        let muted = prefs.per_app["so.tilo.mail"].muted_until.unwrap();
        assert!(muted > unix_now_i64() + 7 * 60 * 60);

        for refused in [
            NotificationEffect::SetFocusStart {
                time: "13:37".into(),
            },
            NotificationEffect::SetAppBadges {
                app: "not an id".into(),
                on: true,
            },
        ] {
            assert!(app.effect(&SettingsEffect::from(refused)).is_err());
        }
        assert_eq!(app.settings::<NotificationPrefs>().get(), prefs);
    }

    /// Every app is a live row offering the flip of what applies to it now,
    /// through the same effects as the pane's toggles; opening the pane
    /// writes nothing.
    #[test]
    fn apps_are_live_rows_offering_their_rules() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let settings = app.settings::<NotificationPrefs>();
        assert!(
            settings.document().section("notifications").is_empty(),
            "showing the inherited values is not an override"
        );
        app.effect_on(
            "settings.notification-app:so.tilo.chat",
            &SettingsEffect::from(NotificationEffect::SetAppBanners {
                app: "so.tilo.chat".into(),
                on: false,
            }),
        )
        .expect("the row offers turning banners off");
        assert_eq!(settings.get().per_app["so.tilo.chat"].banners, Some(false));
        let surface = app.agent_surface().to_string();
        assert!(
            surface.contains("settings.notification-app:so.tilo.mail"),
            "{surface}"
        );
        assert!(
            surface.contains("Turn on notification banners"),
            "{surface}"
        );
    }

    /// The pane shows the stored policy and follows a change made elsewhere.
    #[test]
    fn the_pane_follows_the_policy() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let area = NotificationsArea {
            prefs: RefCell::new(cx.follow::<NotificationPrefs>()),
            apps: OnceCell::from(sample_apps()),
            sample: true,
            read: crate::device_read::DeviceRead::default(),
        };
        let mut pane = NotificationsPane::new(cx.clone(), &area);
        assert!(pane.shown.apps["so.tilo.chat"].banners.get());
        cx.settings::<NotificationPrefs>()
            .update(|prefs| {
                prefs.show_banners = false;
                prefs
                    .per_app
                    .entry("so.tilo.mail".into())
                    .or_default()
                    .banners = Some(true);
            })
            .unwrap();
        let doc = ui::automerge::AutoCommit::new();
        pane.sync(&doc, &ui::automerge::ROOT);
        assert!(!pane.shown.banners.get());
        assert!(!pane.shown.apps["so.tilo.chat"].banners.get(), "inherited");
        assert!(pane.shown.apps["so.tilo.mail"].banners.get(), "overridden");
        assert_eq!(pane.status().0, t("notif-status-banners-off"));

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }
}
