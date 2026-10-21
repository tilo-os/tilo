//! Notifications pane — the editor for the **shared notification policy**.
//!
//! This panel edits [`crate::prefs::NotificationPrefs`], the `notifications`
//! section of the person's settings. The compositor's notification layer
//! reads that section when it decides whether to raise a banner, reveal its
//! contents, sound it, or mirror it to the lock screen — so the toggles here
//! are the single source of truth the rest of the system follows, exactly
//! like Appearance owns the theme.
//!
//! Every control is a field edit of that section, or its `mute-app` verb:
//! - **Do Not Disturb** — master mute for banners and sounds.
//! - **Show banners** — pop-up banners for incoming notifications.
//! - **Show previews** — reveal the message body (off shows only the app name).
//! - **Play sounds** — an audible cue when a notification arrives.
//! - **Show on lock screen** — mirror notifications onto the lock screen.
//! - **Focus** — a scheduled quiet period that only priority apps pierce.
//! - **Per app** — overrides of the delivery choices for one verified app,
//!   priority during focus, and a temporary mute. An app's rows are live rows
//!   (`settings.notification-app:<id>`) offering the same edits bound to it.

use std::cell::{OnceCell, RefCell};
use std::collections::BTreeMap;

use settings_store::notification_policy::AppNotificationRule;
use tilo_clock::unix_now_i64;
use ui::LocalState;
use ui::framework::LiveRow;
use ui::tilo_resource;
use ui::widget::{AppModel, ModelPath, Widget};

use crate::host::{Followed, PaneCx};
use crate::i18n::{english, t, t_args};
use crate::prefs::{MUTE_OPTIONS, NotificationPrefs, mute_shown, valid_app_id};
use crate::widgets::{
    Item, SettingsSection, StatusTone, error_row, note, row, rows, section, semantic_status_pill,
    setting_row, value_row,
};

crate::area::pane_area!(NotificationsArea);

/// The window's [`NotificationsArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&NotificationsArea> {
    app.areas().find()
}

const FOCUS_DAYS: &[(&str, u8)] = &[
    ("Every day", 0),
    ("Weekdays", 0b0001_1111),
    ("Weekends", 0b0110_0000),
];

/// The most apps the published tree lists; an app beyond it is still
/// reachable by id.
const MAX_APP_ROWS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstalledNotificationApp {
    id: String,
    name: String,
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

/// One of an app's delivery choices a person turns on or off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppChoice {
    Banners,
    Previews,
    Sounds,
    Badges,
    Priority,
    PriorityContacts,
}

impl AppChoice {
    /// Every choice, in the order an app's rules list them.
    const ALL: [Self; 6] = [
        Self::Banners,
        Self::Previews,
        Self::Sounds,
        Self::Badges,
        Self::Priority,
        Self::PriorityContacts,
    ];

    /// Whether the choice is on for an app with `rule`: its own override,
    /// else the policy's.
    fn on(self, prefs: &NotificationPrefs, rule: &AppNotificationRule) -> bool {
        match self {
            Self::Banners => rule.banners.unwrap_or(prefs.show_banners),
            Self::Previews => rule.previews.unwrap_or(prefs.show_previews),
            Self::Sounds => rule.sounds.unwrap_or(prefs.play_sounds),
            Self::Badges => rule.badges.unwrap_or(prefs.show_badges),
            Self::Priority => rule.priority,
            Self::PriorityContacts => rule.priority_contacts,
        }
    }

    /// What it is called where an agent turns it on or off.
    const fn noun(self) -> &'static str {
        match self {
            Self::Banners => "notification banners",
            Self::Previews => "message previews",
            Self::Sounds => "notification sounds",
            Self::Badges => "notification badges",
            Self::Priority => "priority during focus",
            Self::PriorityContacts => "priority contacts during focus",
        }
    }

    /// The field of an app's rule it is, as its control's id ends.
    const fn field(self) -> &'static str {
        match self {
            Self::Banners => "banners",
            Self::Previews => "previews",
            Self::Sounds => "sounds",
            Self::Badges => "badges",
            Self::Priority => "priority",
            Self::PriorityContacts => "priority_contacts",
        }
    }

    /// The catalogue key of its row's label; its line is `-desc`.
    const fn label(self) -> &'static str {
        match self {
            Self::Banners => "notif-app-banners",
            Self::Previews => "notif-app-previews",
            Self::Sounds => "notif-app-sounds",
            Self::Badges => "notif-app-badges",
            Self::Priority => "notif-app-priority",
            Self::PriorityContacts => "notif-app-contacts",
        }
    }

    /// The field edit turning this choice of `app` on or off: the one field
    /// of its rule, the rest untouched (a rule the app has none of yet
    /// starts from the default).
    fn edit(self, app: &str, on: bool) -> tilo_resource::Edit<NotificationPrefs> {
        let rule = NotificationPrefs::paths().per_app.key(app);
        match self {
            Self::Banners => rule.banners.set(Some(on)),
            Self::Previews => rule.previews.set(Some(on)),
            Self::Sounds => rule.sounds.set(Some(on)),
            Self::Badges => rule.badges.set(Some(on)),
            Self::Priority => rule.priority.set(on),
            Self::PriorityContacts => rule.priority_contacts.set(on),
        }
    }

    /// The id a row offers this choice under.
    const fn id(self) -> &'static str {
        match self {
            Self::Banners => "set-app-banners",
            Self::Previews => "set-app-previews",
            Self::Sounds => "set-app-sounds",
            Self::Badges => "set-app-badges",
            Self::Priority => "set-app-priority",
            Self::PriorityContacts => "set-app-priority-contacts",
        }
    }
}

/// What the notification area keeps for the window: the policy as last
/// read, and the apps it lists.
pub struct NotificationsArea {
    cx: PaneCx,
    prefs: RefCell<Followed<NotificationPrefs>>,
    apps: OnceCell<Vec<InstalledNotificationApp>>,
    sample: bool,
    /// The installed apps, read off the UI thread after the window's first
    /// frame ([`Self::start_read`]).
    read: crate::device_read::DeviceRead<Vec<InstalledNotificationApp>>,
    /// The rows, kept for the policy version and locale they show, until
    /// the first mute they show runs out.
    rows: crate::host::RowsMemo<(ui::storage::SettingsRevision, u64, Option<i64>)>,
}

impl NotificationsArea {
    /// The area over `cx`'s settings. A capture lists a fixed set of apps
    /// rather than the ones this device has installed.
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            prefs: RefCell::new(cx.follow::<NotificationPrefs>()),
            apps: OnceCell::new(),
            sample: !cx.reaches_devices(),
            read: crate::device_read::DeviceRead::default(),
            rows: crate::host::RowsMemo::default(),
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
    /// its first frame ([`crate::panes::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.apps.get().is_some()
    }

    /// One live row per app, offering its delivery choices flipped and its
    /// mute lifted or set.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        self.prefs.borrow_mut().refresh();
        let apps = self.apps();
        let prefs = self.prefs.borrow();
        let now = unix_now_i64();
        // A row offers to unmute an app until its mute runs out: the rows
        // hold until the first such deadline passes.
        let next_unmute = prefs
            .get()
            .per_app
            .values()
            .filter_map(|rule| rule.muted_until)
            .filter(|until| *until > now)
            .min();
        let key = (
            prefs.revision(),
            ui::i18n::I18nManager::revision(),
            next_unmute,
        );
        self.rows.rows(key, || {
            apps.iter()
                .take(MAX_APP_ROWS)
                .map(|app| app_row(&self.cx, prefs.get(), app))
                .collect()
        })
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

/// The live row of `app` under `prefs`: each delivery choice offered as the
/// flip of what applies to it now, and its mute lifted or set, all changes
/// of the app's rule in the notification section.
fn app_row(cx: &PaneCx, prefs: &NotificationPrefs, app: &InstalledNotificationApp) -> LiveRow {
    let rule = prefs.per_app.get(&app.id).copied().unwrap_or_default();
    let muted = rule.muted_until.is_some_and(|until| until > unix_now_i64());
    let (duration, mute_label) = if muted {
        ("Not muted", "Unmute notifications")
    } else {
        ("For 1 hour", "Mute notifications for an hour")
    };
    let params = crate::prefs::notification_prefs::MuteApp::paths();
    let mute = NotificationPrefs::MUTE_APP
        .bind(params.app, app.id.clone())
        .and(params.duration, duration.to_owned())
        .declaration("mute-app", mute_label);
    let offered = AppChoice::ALL
        .into_iter()
        .map(|choice| {
            let on = !choice.on(prefs, &rule);
            tilo_resource::ActionDecl::edit(
                choice.id(),
                flip(on, choice.noun()),
                choice.edit(&app.id, on),
            )
        })
        .chain(mute);
    offered.fold(
        LiveRow::new(
            format!("settings.notification-app:{}", app.id),
            "settings.notification-app",
            app.name.clone(),
        ),
        |row, decl| match cx.row_operation(&decl) {
            Some(operation) => row.operation(operation),
            None => row,
        },
    )
}

fn flip(on: bool, noun: &str) -> String {
    format!("Turn {} {noun}", if on { "on" } else { "off" })
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

/// The English name of a mute duration shown in either language.
/// What an app's notifications do, under its name: shown in full, shown
/// without their content, or kept in the history only.
fn app_caption(banners: bool, previews: bool) -> String {
    if banners && previews {
        t("notif-app-allowed")
    } else if banners {
        t("notif-app-hidden")
    } else {
        t("notif-app-history")
    }
}

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

pub struct NotificationsPane {
    cx: PaneCx,
    prefs: Followed<NotificationPrefs>,
    apps: Vec<InstalledNotificationApp>,
    /// Which app's rules are open, kept while the page is rebuilt.
    app_rules: ui::widget::DisclosureGroup,
    composed: ui::widget::Composed,
}

impl NotificationsPane {
    /// The pane over `cx`'s notification policy, listing `area`'s apps.
    pub fn new(cx: PaneCx, area: &NotificationsArea) -> Self {
        let prefs = cx.follow::<NotificationPrefs>();
        Self {
            cx,
            prefs,
            apps: area.apps().to_vec(),
            app_rules: ui::widget::DisclosureGroup::new(),
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

    fn build_composition(&self) -> Box<dyn Widget> {
        let app_cards = self.build_app_cards();
        let mut cards = vec![
            self.build_dnd_card(),
            self.build_banners_card(),
            self.build_delivery_card(),
        ];
        // The apps' own cards follow; with none yet, one line says why.
        if app_cards.is_empty() {
            cards.push(section(
                &t("notif-apps-title"),
                rows(vec![note(&t("notif-apps-empty"))]),
            ));
        }
        cards.extend(app_cards);
        cards.push(Self::build_priority_contacts_card());
        crate::widgets::page(cards)
    }

    fn build_dnd_card(&self) -> SettingsSection {
        // Do not disturb, and only while something holds notifications back
        // (banners off, an app muted for a while) a line saying so.
        let (status_label, status_tone) = self.status();
        let mut content = Vec::new();
        if status_label != t("notif-status-active") {
            content.push(setting_row(
                &t("notif-delivery"),
                semantic_status_pill(&status_label, status_tone),
            ));
        }
        content.push(
            self.cx
                .row(t("notif-dnd"))
                .described(t("notif-dnd-desc"))
                .id("notifications.dnd")
                .of(&self.prefs, NotificationPrefs::paths().do_not_disturb)
                .toggle(),
        );
        // Quiet hours sit with it: the same silence, on a schedule.
        content.extend(self.focus_rows());
        section(&t("notif-dnd-title"), rows(content))
    }

    fn build_banners_card(&self) -> SettingsSection {
        section(
            &t("notif-banners-title"),
            rows(vec![
                self.cx
                    .row(t("notif-show-banners"))
                    .described(t("notif-show-banners-desc"))
                    .id("notifications.banners")
                    .of(&self.prefs, NotificationPrefs::paths().show_banners)
                    .toggle(),
                self.cx
                    .row(t("notif-previews"))
                    .described(t("notif-previews-desc"))
                    .id("notifications.previews")
                    .of(&self.prefs, NotificationPrefs::paths().show_previews)
                    .toggle(),
            ]),
        )
    }

    fn build_delivery_card(&self) -> SettingsSection {
        // Sounds that work need no line of their own: the switch says it.
        let sounds_row = match (
            media::event_sound::sound_backend(),
            super::installed_programs_unreadable(),
        ) {
            (media::event_sound::SoundBackend::PipeWire, _) => None,
            (media::event_sound::SoundBackend::Unavailable, Some(error)) => Some(error_row(
                &t("notif-sounds"),
                &t_args("settings-programs-error", &[("error", error.into())]),
            )),
            (media::event_sound::SoundBackend::Unavailable, None) => {
                Some(value_row(&t("notif-sounds"), &t("notif-sounds-missing")))
            }
        };
        let mut content = vec![
            self.cx
                .row(t("notif-lock-screen"))
                .described(t("notif-lock-screen-desc"))
                .id("notifications.lock_screen")
                .of(&self.prefs, NotificationPrefs::paths().show_on_lock_screen)
                .toggle(),
        ];
        content.extend(sounds_row);
        content.push(
            self.cx
                .row(t("notif-play-sounds"))
                .described(t("notif-play-sounds-desc"))
                .id("notifications.sounds")
                .of(&self.prefs, NotificationPrefs::paths().play_sounds)
                .toggle(),
        );
        content.push(
            self.cx
                .row(t("notif-badges"))
                .described(t("notif-badges-desc"))
                .id("notifications.badges")
                .of(&self.prefs, NotificationPrefs::paths().show_badges)
                .toggle(),
        );
        section(&t("notif-delivery-title"), rows(content))
    }

    /// Quiet hours: the switch, and while it is on, when they start and
    /// end and on which days.
    fn focus_rows(&self) -> Vec<Item> {
        let focus = NotificationPrefs::paths().focus;
        let focus_time = |minutes: &u16| focus_time_label(*minutes).to_owned();
        let times = || FOCUS_TIMES.iter().map(|(_, minutes)| *minutes);
        let mut content = vec![
            self.cx
                .row(t("notif-focus-enabled"))
                .described(t("notif-focus-enabled-desc"))
                .id("notifications.focus.enabled")
                .of(&self.prefs, focus.enabled)
                .toggle(),
        ];
        if self.prefs().focus.enabled {
            content.extend([
                self.cx
                    .row(t("notif-focus-from"))
                    .id("notifications.focus.start")
                    .of(&self.prefs, focus.start_minute)
                    .among(times(), focus_time),
                self.cx
                    .row(t("notif-focus-until"))
                    .id("notifications.focus.end")
                    .of(&self.prefs, focus.end_minute)
                    .among(times(), focus_time),
                self.cx
                    .row(t("notif-focus-days"))
                    .id("notifications.focus.days")
                    .of(&self.prefs, focus.weekdays)
                    .among(FOCUS_DAYS.iter().map(|(_, days)| *days), |days: &u8| {
                        focus_days_label(*days)
                    }),
            ]);
        }
        content
    }

    /// One app's row: what it may show as its caption, opening in place on
    /// its rules (banners, previews, sounds, badges, priority, priority
    /// contacts) and how long it is muted.
    fn app_card(&self, app: &InstalledNotificationApp, prefs: &NotificationPrefs) -> Item {
        let rule = prefs.per_app.get(&app.id).copied().unwrap_or_default();
        let app_caption = app_caption(
            AppChoice::Banners.on(prefs, &rule),
            AppChoice::Previews.on(prefs, &rule),
        );
        let id = app.id.clone();
        let mute = self
            .cx
            .row(t("notif-app-mute"))
            .id(format!("notifications.app.{}.mute", app.id))
            .choice_verb(
                mute_options(),
                &LocalState::new(mute_label(rule.muted_until, unix_now_i64())),
                NotificationPrefs::MUTE_APP
                    .map(move |shown: String| (id.clone(), mute_english(&shown))),
            );
        // The page is rebuilt with every change of the policy, so each
        // control shows the rule as it is now.
        let mut rules: Vec<Item> = AppChoice::ALL
            .into_iter()
            .map(|choice| {
                let app = app.id.clone();
                self.cx
                    .row(t(choice.label()))
                    .described(t(&format!("{}-desc", choice.label())))
                    .id(format!("notifications.app.{app}.{}", choice.field()))
                    .toggle_edit(
                        &LocalState::new(choice.on(prefs, &rule)),
                        NotificationPrefs::paths().per_app,
                        move |on| choice.edit(&app, on),
                    )
            })
            .collect();
        rules.extend([mute, value_row(&t("notif-app-verified"), &app.id)]);
        let rules = crate::widgets::page(vec![crate::widgets::untitled(rows(rules))]);
        row(Box::new(
            ui::widget::DisclosureRow::keyed(
                app.name.clone(),
                Box::new(
                    ui::widget::Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .with_extra_bottom_padding(crate::widgets::spacing().rhythm.sibling)
                        .push(rules),
                ),
                &self.app_rules,
                &app.id,
            )
            .icon(ui::icons::Icon::AppWindow)
            .secondary(app_caption),
        ))
    }

    /// One row per app that opens its own rules in place, under one
    /// heading: what it may show, and for how long it is muted.
    fn build_app_cards(&self) -> Vec<SettingsSection> {
        let prefs = self.prefs();
        let mut app_rows = Vec::new();
        for app in &self.apps {
            app_rows.push(self.app_card(app, prefs));
        }
        if app_rows.is_empty() {
            return Vec::new();
        }
        vec![section(&t("notif-apps-title"), rows(app_rows))]
    }

    fn build_priority_contacts_card() -> SettingsSection {
        section(
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
            // The status pill and the per-app captions follow the values.
            self.composed.invalidate();
        }
        self.composed.sync(state, root);
    }

    crate::panes::composed_pane!();

    fn name(&self) -> &'static str {
        "NotificationsPane"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testing::{NOTIFICATIONS, edit_field, edit_path, invoke_section};
    use crate::resource::SettingsShell;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::prelude::TestApp;
    use ui::theme::Theme;

    /// Set `field` of the rule of `app_id` through the door, as an agent
    /// does: a field edit of the notification section.
    fn edit_rule(
        app: &mut TestApp<SettingsShell>,
        app_id: &str,
        field: &str,
        value: bool,
    ) -> Result<(), String> {
        app.call_on(
            "settings:settings.notifications",
            "field-edit",
            serde_json::json!({ "edit": {
                "path": [{ "field": "per_app" }, { "key": app_id }, { "field": field }],
                "op": "set",
                "value": value,
            }}),
        )
        .map(drop)
        .map_err(|error| format!("{error:?}"))
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
    fn notification_edits_write_the_policy() {
        // A mute counts from when it was asked for: the clock stands still
        // here, so the end it writes is exact.
        tilo_clock::freeze_wall(tilo_clock::wall());
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        edit_field(&mut app, NOTIFICATIONS, "do_not_disturb", true).unwrap();
        edit_field(&mut app, NOTIFICATIONS, "show_banners", false).unwrap();
        edit_path(&mut app, NOTIFICATIONS, &["focus", "enabled"], true).unwrap();
        edit_path(&mut app, NOTIFICATIONS, &["focus", "start_minute"], 20 * 60).unwrap();
        edit_path(&mut app, NOTIFICATIONS, &["focus", "weekdays"], 0b0001_1111).unwrap();
        // An agent sets one field of a rule the app has none of yet.
        edit_rule(&mut app, "so.tilo.chat", "sounds", false).unwrap();
        edit_rule(&mut app, "so.tilo.chat", "priority", true).unwrap();
        invoke_section(
            &mut app,
            NOTIFICATIONS,
            "mute-app",
            serde_json::json!({ "app": "so.tilo.mail", "duration": "For 8 hours" }),
        )
        .unwrap();
        let prefs = app.settings::<NotificationPrefs>().get();
        assert!(prefs.do_not_disturb);
        assert!(!prefs.show_banners);
        assert!(prefs.focus.enabled);
        assert_eq!(prefs.focus.start_minute, 20 * 60);
        assert_eq!(prefs.focus.weekdays, 0b0001_1111);
        let chat = prefs.per_app["so.tilo.chat"];
        assert_eq!(chat.sounds, Some(false));
        assert!(chat.priority);
        assert_eq!(chat.banners, None, "only the changed fields are overrides");
        let muted = prefs.per_app["so.tilo.mail"].muted_until.unwrap();
        assert_eq!(muted, unix_now_i64() + 8 * 60 * 60);

        assert!(
            edit_path(&mut app, NOTIFICATIONS, &["focus", "start_minute"], 24 * 60).is_err(),
            "a minute past the day is refused"
        );
        assert!(edit_rule(&mut app, "so.tilo.mail", "frobnicate", true).is_err());
        assert!(edit_rule(&mut app, "not an id", "badges", true).is_err());
        assert!(
            invoke_section(
                &mut app,
                NOTIFICATIONS,
                "mute-app",
                serde_json::json!({ "app": "so.tilo.mail", "duration": "forever" }),
            )
            .is_err()
        );
        assert_eq!(app.settings::<NotificationPrefs>().get(), prefs);

        // An agent sets one app's rule as a typed edit of the section, and
        // the section refuses a rule for something that is not an app id.
        let rule = |app_id: &str, value: serde_json::Value| {
            serde_json::json!({ "edit": {
                "path": [{ "field": "per_app" }, { "key": app_id }],
                "op": "set",
                "value": value,
            }})
        };
        app.call_on(
            "settings:settings.notifications",
            "field-edit",
            rule("so.tilo.calendar", serde_json::json!({ "priority": true })),
        )
        .unwrap();
        let calendar = app.settings::<NotificationPrefs>().get().per_app["so.tilo.calendar"];
        assert!(calendar.priority);
        assert_eq!(calendar.banners, None);
        assert!(
            app.call_on(
                "settings:settings.notifications",
                "field-edit",
                rule("not an id", serde_json::json!({ "priority": true })),
            )
            .is_err()
        );
    }

    /// Every app is a live row offering the flip of what applies to it now,
    /// through the same edits as the pane's toggles; opening the pane
    /// writes nothing.
    #[test]
    fn apps_are_live_rows_offering_their_rules() {
        tilo_clock::freeze_wall(tilo_clock::wall());
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let settings = app.settings::<NotificationPrefs>();
        assert!(
            settings.document().section("notifications").is_empty(),
            "showing the inherited values is not an override"
        );
        app.call_on(
            "settings.notification-app:so.tilo.chat",
            "set-app-banners",
            serde_json::json!({}),
        )
        .expect("the row offers turning banners off");
        assert_eq!(settings.get().per_app["so.tilo.chat"].banners, Some(false));
        app.call_on(
            "settings.notification-app:so.tilo.chat",
            "mute-app",
            serde_json::json!({}),
        )
        .expect("the row offers muting the app for an hour");
        assert_eq!(
            settings.get().per_app["so.tilo.chat"].muted_until,
            Some(unix_now_i64() + 60 * 60)
        );
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
            cx: cx.clone(),
            prefs: RefCell::new(cx.follow::<NotificationPrefs>()),
            apps: OnceCell::from(sample_apps()),
            sample: true,
            read: crate::device_read::DeviceRead::default(),
            rows: crate::host::RowsMemo::default(),
        };
        let mut pane = NotificationsPane::new(cx.clone(), &area);
        let banners_on = |pane: &NotificationsPane, app: &str| {
            let prefs = pane.prefs();
            let rule = prefs.per_app.get(app).copied().unwrap_or_default();
            AppChoice::Banners.on(prefs, &rule)
        };
        let banners = pane
            .prefs
            .shown(&NotificationPrefs::paths().show_banners, bool::clone);
        assert!(banners.get());
        assert!(banners_on(&pane, "so.tilo.chat"));
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
        assert!(!banners.get());
        assert!(!banners_on(&pane, "so.tilo.chat"), "inherited");
        assert!(banners_on(&pane, "so.tilo.mail"), "overridden");
        assert_eq!(pane.status().0, t("notif-status-banners-off"));

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }
}
