//! Notifications pane — the editor for the **shared notification policy**.
//!
//! This panel edits [`crate::prefs::NotificationPrefs`], the durable
//! notification-delivery section of the shared `settings.json`
//! ([`crate::region::RegionSettings::store_path`]). The compositor's
//! notification layer reads that section when it decides whether to raise a
//! banner, reveal its contents, sound it, or mirror it to the lock screen — so
//! the toggles here are the single source of truth the rest of the system
//! follows, exactly like Appearance owns the theme.
//!
//! Controls (each bound to a CRDT key; `sync` folds the change into the loaded
//! prefs and persists the shared store):
//! - **Do Not Disturb** — master mute for banners and sounds.
//! - **Show banners** — pop-up banners for incoming notifications.
//! - **Show previews** — reveal the message body (off shows only the app name).
//! - **Play sounds** — an audible cue when a notification arrives.
//! - **Show on lock screen** — mirror notifications onto the lock screen.

use std::collections::{BTreeMap, HashMap};
use tilo_clock::unix_now_i64;
use ui::doc::{read_bool, read_str};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash, ObjId, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use tilo_actions::EncodedAction;
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Dropdown, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::actions::{SETTINGS_ACTION_TYPE, SettingsAction, set_bool_action, set_text_action};
use crate::i18n::t;
use crate::prefs::{NotificationPrefs, SystemPrefs};
use crate::widgets::{
    El, StatusTone, caption, card, choice_row, rows, semantic_status_pill, setting_row_described,
    value_row,
};

pub const DND_KEY: &str = "notifications.do_not_disturb";
pub const BANNERS_KEY: &str = "notifications.show_banners";
pub const PREVIEWS_KEY: &str = "notifications.show_previews";
pub const SOUNDS_KEY: &str = "notifications.play_sounds";
pub const LOCK_SCREEN_KEY: &str = "notifications.show_on_lock_screen";
pub const BADGES_KEY: &str = "notifications.show_badges";
pub const FOCUS_ENABLED_KEY: &str = "notifications.focus.enabled";
pub const FOCUS_START_KEY: &str = "notifications.focus.start";
pub const FOCUS_END_KEY: &str = "notifications.focus.end";
pub const FOCUS_DAYS_KEY: &str = "notifications.focus.days";

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct InstalledNotificationApp {
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

/// Secret-free, bounded app inventory used by Settings' canonical resource
/// tree.  This deliberately returns only stable identity plus display name:
/// notification contents and any sender data never leave the pane.
pub(crate) fn resource_notification_apps(prefs: &NotificationPrefs) -> Vec<(String, String)> {
    installed_notification_apps(&prefs.per_app)
        .into_iter()
        .map(|app| (app.id, app.name))
        .collect()
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
        let Ok(manifest) = launcher::manifest::read_manifest(&entry.manifest_path) else {
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

fn app_key(id: &str, field: &str) -> String {
    format!("notifications.app.{id}.{field}")
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

fn focus_days_options() -> Vec<String> {
    FOCUS_DAYS
        .iter()
        .map(|(english, _)| focus_days_shown(english))
        .collect()
}

pub struct NotificationsPane {
    prefs: NotificationPrefs,
    apps: Vec<InstalledNotificationApp>,
    mute_labels: HashMap<String, String>,
    /// The per-app override toggles as last seeded into or read from the
    /// document. A toggle seeded from the inherited global value is not an
    /// override; only a value that differs from what the pane last saw is.
    app_toggles: HashMap<String, bool>,
    needs_seed: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
    /// Framework-owned action dispatch: brackets the undo boundary (bubbled up by
    /// `SettingsApp` via [`CommittedActions::take_committed_action`](ui::undo::CommittedActions::take_committed_action), so the host records
    /// exactly one boundary per event), encodes and dispatches each settings
    /// action, and remembers the last one so a test can prove an on-screen toggle
    /// builds the same [`EncodedAction`] the palette does.
    actions: ui::ActionHost,
}

impl Default for NotificationsPane {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationsPane {
    pub fn new() -> Self {
        Self::with_prefs(SystemPrefs::load().notifications)
    }

    /// Construct from explicit preferences over the machine's installed
    /// apps.
    pub fn with_prefs(prefs: NotificationPrefs) -> Self {
        let apps = installed_notification_apps(&prefs.per_app);
        Self::with_prefs_and_apps(prefs, apps)
    }

    /// The `--shots` pane: default policy over a fixed app list, so a capture
    /// shows the same per-app cards on every machine rather than one per
    /// desktop entry the host happens to have.
    pub fn for_shot() -> Self {
        let apps = [
            ("so.tilo.calendar", "Calendar"),
            ("so.tilo.chat", "Chat"),
            ("so.tilo.mail", "Mail"),
        ]
        .into_iter()
        .map(|(id, name)| InstalledNotificationApp {
            id: id.to_string(),
            name: name.to_string(),
        })
        .collect();
        Self::with_prefs_and_apps(NotificationPrefs::default(), apps)
    }

    fn with_prefs_and_apps(prefs: NotificationPrefs, apps: Vec<InstalledNotificationApp>) -> Self {
        let now = unix_now_i64();
        let mute_labels = apps
            .iter()
            .map(|app| {
                let deadline = prefs.per_app.get(&app.id).and_then(|rule| rule.muted_until);
                (app.id.clone(), mute_label(deadline, now))
            })
            .collect();
        Self {
            prefs,
            apps,
            mute_labels,
            app_toggles: HashMap::new(),
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
            actions: ui::ActionHost::new(),
        }
    }

    /// The most recently dispatched settings action's wire form (test hook).
    pub fn last_dispatched(&self) -> Option<&EncodedAction> {
        self.actions.last_dispatched()
    }

    /// The scratch (edit-intent) key a toggle binds to for a given canonical
    /// setting key. The widget writes the scratch; `sync` reconciles it into the
    /// canonical value through an action, and mirrors the canonical value back
    /// into the scratch when it changes underneath us (undo / remote / palette).
    fn scratch_key(canonical: &str) -> String {
        format!("{canonical}.edit")
    }

    /// Dispatch one [`SettingsAction`] — the single CRDT-mutation path shared
    /// with the Ctrl+K palette. The framework [`ActionHost`](ui::ActionHost)
    /// brackets the undo boundary and applies the action through the Tilo executor
    /// (a pinned actor + committed change, so it replays byte-identically on
    /// peers).
    ///
    /// This is *only* the CRDT write. Carrying the preference to the compositor's
    /// notification layer (the shared `settings.json`, see [`Self::persist`]) is a
    /// separate side effect driven by the resulting document state — never part
    /// of an action.
    fn perform_action(&mut self, state: &mut AutoCommit, action: &SettingsAction) {
        self.actions.dispatch_with_context(
            state,
            SETTINGS_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.settings", "Settings", action),
        );
    }

    /// Reconcile one boolean setting between its widget scratch key and its
    /// canonical (action-owned) key. Returns `(new_value, changed)`.
    ///
    /// * canonical changed under us (undo / remote / palette) → adopt it and
    ///   mirror it into the scratch so the toggle reflects it;
    /// * else the user flipped the toggle (scratch ≠ canonical) → dispatch the
    ///   action that writes the canonical value.
    fn reconcile_bool(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        canonical: &str,
        field: bool,
    ) -> (bool, bool) {
        let scratch = Self::scratch_key(canonical);
        let cv = read_bool(state, root, canonical).unwrap_or(field);
        let sv = read_bool(state, root, &scratch).unwrap_or(cv);
        if cv != field {
            let _ = state.put(root, scratch.as_str(), automerge::ScalarValue::Boolean(cv));
            (cv, true)
        } else if sv != cv {
            self.perform_action(state, &set_bool_action(canonical, sv));
            (sv, true)
        } else {
            (field, false)
        }
    }

    fn reconcile_text(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        canonical: &str,
        field: &str,
    ) -> (String, bool) {
        let scratch = Self::scratch_key(canonical);
        let cv = read_str(state, root, canonical).unwrap_or_else(|| field.to_string());
        let sv = read_str(state, root, &scratch).unwrap_or_else(|| cv.clone());
        if cv != field {
            let _ = state.put(root, scratch.as_str(), cv.as_str());
            (cv, true)
        } else if sv != cv {
            self.perform_action(state, &set_text_action(canonical, sv.clone()));
            (sv, true)
        } else {
            (field.to_string(), false)
        }
    }

    pub fn prefs(&self) -> &NotificationPrefs {
        &self.prefs
    }

    /// A one-word delivery status, so the effect of the toggles reads at a
    /// glance without re-parsing every row.
    fn status(&self) -> (String, StatusTone) {
        if self.prefs.do_not_disturb {
            (t("notif-status-silenced"), StatusTone::Neutral)
        } else if !self.prefs.show_banners {
            (t("notif-status-banners-off"), StatusTone::Warning)
        } else {
            (t("notif-status-active"), StatusTone::Success)
        }
    }

    fn toggle(id: &str, canonical: &str) -> El {
        // The toggle writes the scratch (edit-intent) key; `sync` turns that
        // edit into an action on the canonical key. The palette dispatches the
        // same action on the same canonical key — one source of truth.
        Box::new(Toggle::new(id, &Self::scratch_key(canonical)).with_id(id))
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let app_cards = self.build_app_cards();
        let mut cards = vec![
            self.build_dnd_card(),
            self.build_focus_card(),
            Self::build_banners_card(),
            Self::build_delivery_card(),
            Self::build_apps_card(app_cards.is_empty()),
            Self::build_explainer_card(),
        ];
        cards.extend(app_cards);
        cards.push(Self::build_priority_contacts_card());
        crate::widgets::cards(cards)
    }

    fn build_dnd_card(&self) -> Box<dyn Widget> {
        let (status_label, status_tone) = self.status();

        // A label-left / status-pill-right row, so the combined effect of the
        // toggles is legible at the top of the pane.
        let status_row: El = Box::new(
            ui::widget::Row::new()
                .push(Box::new(
                    ui::widget::Text::new(&t("notif-delivery")).with_size(13.0),
                ))
                .push(Box::new(ui::widget::Spacer::new()))
                .push(semantic_status_pill(&status_label, status_tone)),
        );

        // Master Do Not Disturb, with the visible delivery-status pill.
        card(
            &t("notif-dnd-title"),
            rows(vec![
                status_row,
                setting_row_described(
                    &t("notif-dnd"),
                    &t("notif-dnd-desc"),
                    Self::toggle("notifications.dnd", DND_KEY),
                ),
            ]),
        )
    }

    fn build_banners_card() -> Box<dyn Widget> {
        card(
            &t("notif-banners-title"),
            rows(vec![
                setting_row_described(
                    &t("notif-show-banners"),
                    &t("notif-show-banners-desc"),
                    Self::toggle("notifications.banners", BANNERS_KEY),
                ),
                setting_row_described(
                    &t("notif-previews"),
                    &t("notif-previews-desc"),
                    Self::toggle("notifications.previews", PREVIEWS_KEY),
                ),
            ]),
        )
    }

    fn build_delivery_card() -> Box<dyn Widget> {
        let sounds_value = match media::event_sound::sound_backend() {
            media::event_sound::SoundBackend::PipeWire => t("notif-sounds-pipewire"),
            media::event_sound::SoundBackend::Unavailable => t("notif-sounds-missing"),
        };
        card(
            &t("notif-delivery-title"),
            rows(vec![
                setting_row_described(
                    &t("notif-lock-screen"),
                    &t("notif-lock-screen-desc"),
                    Self::toggle("notifications.lock_screen", LOCK_SCREEN_KEY),
                ),
                value_row(&t("notif-sounds"), &sounds_value),
                setting_row_described(
                    &t("notif-play-sounds"),
                    &t("notif-play-sounds-desc"),
                    Self::toggle("notifications.sounds", SOUNDS_KEY),
                ),
                setting_row_described(
                    &t("notif-badges"),
                    &t("notif-badges-desc"),
                    Self::toggle("notifications.badges", BADGES_KEY),
                ),
            ]),
        )
    }

    fn build_focus_card(&self) -> Box<dyn Widget> {
        let focus_options: Vec<String> = FOCUS_TIMES
            .iter()
            .map(|(label, _)| (*label).to_string())
            .collect();
        card(
            &t("notif-focus-title"),
            rows(vec![
                setting_row_described(
                    &t("notif-focus-enabled"),
                    &t("notif-focus-enabled-desc"),
                    Self::toggle("notifications.focus.enabled", FOCUS_ENABLED_KEY),
                ),
                crate::widgets::choice_row(
                    &t("notif-focus-from"),
                    Box::new(
                        Dropdown::new(focus_options.clone(), &Self::scratch_key(FOCUS_START_KEY))
                            .with_placeholder(focus_time_label(self.prefs.focus.start_minute)),
                    ),
                ),
                choice_row(
                    &t("notif-focus-days"),
                    Box::new(
                        Dropdown::new(focus_days_options(), &Self::scratch_key(FOCUS_DAYS_KEY))
                            .with_placeholder(&focus_days_label(self.prefs.focus.weekdays)),
                    ),
                ),
                crate::widgets::choice_row(
                    &t("notif-focus-until"),
                    Box::new(
                        Dropdown::new(focus_options, &Self::scratch_key(FOCUS_END_KEY))
                            .with_placeholder(focus_time_label(self.prefs.focus.end_minute)),
                    ),
                ),
                caption(&t("notif-focus-caption")),
            ]),
        )
    }

    fn build_app_cards(&self) -> Vec<Box<dyn Widget>> {
        let mut app_cards = Vec::new();
        let now = unix_now_i64();
        for app in &self.apps {
            let rule = self.prefs.per_app.get(&app.id).copied().unwrap_or_default();
            let banners = rule.banners.unwrap_or(self.prefs.show_banners);
            let previews = rule.previews.unwrap_or(self.prefs.show_previews);
            let mute_options = mute_options();
            let app_caption = if banners && previews {
                t("notif-app-allowed")
            } else if banners {
                t("notif-app-hidden")
            } else {
                t("notif-app-history")
            };
            app_cards.push(card(
                &app.name,
                rows(vec![
                    value_row(&t("notif-app-verified"), &app.id),
                    setting_row_described(
                        &t("notif-app-banners"),
                        &t("notif-app-banners-desc"),
                        Box::new(
                            Toggle::new(
                                &format!("notifications.app.{}.banners", app.id),
                                &app_key(&app.id, "banners"),
                            )
                            .with_id(&format!("notifications.app.{}.banners", app.id)),
                        ),
                    ),
                    setting_row_described(
                        &t("notif-app-previews"),
                        &t("notif-app-previews-desc"),
                        Box::new(
                            Toggle::new(
                                &format!("notifications.app.{}.previews", app.id),
                                &app_key(&app.id, "previews"),
                            )
                            .with_id(&format!("notifications.app.{}.previews", app.id)),
                        ),
                    ),
                    setting_row_described(
                        &t("notif-app-sounds"),
                        &t("notif-app-sounds-desc"),
                        Box::new(
                            Toggle::new(
                                &format!("notifications.app.{}.sounds", app.id),
                                &app_key(&app.id, "sounds"),
                            )
                            .with_id(&format!("notifications.app.{}.sounds", app.id)),
                        ),
                    ),
                    setting_row_described(
                        &t("notif-app-badges"),
                        &t("notif-app-badges-desc"),
                        Box::new(
                            Toggle::new(
                                &format!("notifications.app.{}.badges", app.id),
                                &app_key(&app.id, "badges"),
                            )
                            .with_id(&format!("notifications.app.{}.badges", app.id)),
                        ),
                    ),
                    setting_row_described(
                        &t("notif-app-priority"),
                        &t("notif-app-priority-desc"),
                        Box::new(
                            Toggle::new(
                                &format!("notifications.app.{}.priority", app.id),
                                &app_key(&app.id, "priority"),
                            )
                            .with_id(&format!("notifications.app.{}.priority", app.id)),
                        ),
                    ),
                    setting_row_described(
                        &t("notif-app-contacts"),
                        &t("notif-app-contacts-desc"),
                        Box::new(
                            Toggle::new(
                                &format!("notifications.app.{}.priority_contacts", app.id),
                                &app_key(&app.id, "priority_contacts"),
                            )
                            .with_id(&format!("notifications.app.{}.priority_contacts", app.id)),
                        ),
                    ),
                    choice_row(
                        &t("notif-app-mute"),
                        Box::new(
                            Dropdown::new(mute_options, &app_key(&app.id, "mute"))
                                .with_placeholder(&mute_label(rule.muted_until, now)),
                        ),
                    ),
                    // These are seeded below; keeping the local values here
                    // makes the intended inheritance visible in code and tests.
                    caption(&app_caption),
                ]),
            ));
        }
        app_cards
    }

    fn build_apps_card(is_empty: bool) -> Box<dyn Widget> {
        if is_empty {
            card(
                &t("notif-apps-title"),
                rows(vec![caption(&t("notif-apps-empty"))]),
            )
        } else {
            card(
                &t("notif-apps-title"),
                rows(vec![caption(&t("notif-apps-caption"))]),
            )
        }
    }

    fn build_explainer_card() -> Box<dyn Widget> {
        card(
            &t("notif-how-title"),
            rows(vec![caption(&t("notif-how-caption"))]),
        )
    }

    fn build_priority_contacts_card() -> Box<dyn Widget> {
        card(
            &t("notif-priority-title"),
            rows(vec![
                value_row(
                    &t("notif-priority-source"),
                    &t("notif-priority-source-value"),
                ),
                caption(&t("notif-priority-caption")),
            ]),
        )
    }

    /// Persist the current prefs into the shared store, preserving other
    /// sections. Reads fresh so a change here never clobbers another pane's.
    fn persist(&self) {
        let mut p = SystemPrefs::load();
        p.notifications = self.prefs.clone();
        if let Err(e) = p.save() {
            warn!(target: "tilo::settings", op = "notifications_save", error = %e, "failed to persist notification prefs");
        }
    }

    fn seed_if_needed(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !self.needs_seed {
            return;
        }
        // Seed both the canonical (action-owned) key and the scratch
        // (widget-bound) key, so the toggle shows the current value and the
        // reconciliation starts settled.
        let seed = |state: &mut AutoCommit, canonical: &str, v: bool| {
            let _ = state.put(root, canonical, automerge::ScalarValue::Boolean(v));
            let _ = state.put(
                root,
                Self::scratch_key(canonical).as_str(),
                automerge::ScalarValue::Boolean(v),
            );
        };
        seed(state, DND_KEY, self.prefs.do_not_disturb);
        seed(state, BANNERS_KEY, self.prefs.show_banners);
        seed(state, PREVIEWS_KEY, self.prefs.show_previews);
        seed(state, SOUNDS_KEY, self.prefs.play_sounds);
        seed(state, LOCK_SCREEN_KEY, self.prefs.show_on_lock_screen);
        seed(state, BADGES_KEY, self.prefs.show_badges);
        seed(state, FOCUS_ENABLED_KEY, self.prefs.focus.enabled);
        let seed_text = |state: &mut AutoCommit, canonical: &str, value: &str| {
            let _ = state.put(root, canonical, value);
            let _ = state.put(root, Self::scratch_key(canonical), value);
        };
        seed_text(
            state,
            FOCUS_START_KEY,
            focus_time_label(self.prefs.focus.start_minute),
        );
        seed_text(
            state,
            FOCUS_END_KEY,
            focus_time_label(self.prefs.focus.end_minute),
        );
        let days_shown = focus_days_label(self.prefs.focus.weekdays);
        seed_text(state, FOCUS_DAYS_KEY, &days_shown);
        let now = unix_now_i64();
        for app in &self.apps {
            let rule = self.prefs.per_app.get(&app.id).copied().unwrap_or_default();
            for (field, value) in [
                ("banners", rule.banners.unwrap_or(self.prefs.show_banners)),
                (
                    "previews",
                    rule.previews.unwrap_or(self.prefs.show_previews),
                ),
                ("sounds", rule.sounds.unwrap_or(self.prefs.play_sounds)),
                ("badges", rule.badges.unwrap_or(self.prefs.show_badges)),
            ] {
                let key = app_key(&app.id, field);
                seed(state, &key, value);
                self.app_toggles.insert(key, value);
            }
            seed(state, &app_key(&app.id, "priority"), rule.priority);
            seed(
                state,
                &app_key(&app.id, "priority_contacts"),
                rule.priority_contacts,
            );
            let mute_shown = mute_label(rule.muted_until, now);
            let _ = state.put(root, app_key(&app.id, "mute"), mute_shown.as_str());
        }
        self.needs_seed = false;
    }

    // Reconcile every toggle's scratch → canonical through an action (a user
    // flip) or mirror the canonical → scratch (an undo / remote / palette
    // change). Each binding is (canonical key, current field value).
    fn reconcile_dnd_and_focus_days(&mut self, state: &mut AutoCommit, root: &ObjId) -> bool {
        let mut changed = false;
        let (v, c) = self.reconcile_bool(state, root, DND_KEY, self.prefs.do_not_disturb);
        self.prefs.do_not_disturb = v;
        changed |= c;
        let current = focus_days_label(self.prefs.focus.weekdays);
        let (value, c) = self.reconcile_text(state, root, FOCUS_DAYS_KEY, &current);
        if let Some(days) = focus_days_from_label(&value) {
            self.prefs.focus.weekdays = days;
        }
        changed |= c;
        changed
    }

    fn reconcile_per_app_rules(&mut self, state: &AutoCommit, root: &ObjId) -> bool {
        let mut changed = false;
        let now = unix_now_i64();
        for app in &self.apps {
            // An override is a toggle whose value moved since the pane last
            // saw it — a user flip, an undo or a peer's edit. The value seeded
            // from the global preference is not one, or merely opening the pane
            // would pin every app to today's global defaults.
            let mut overrides = [None; 4];
            for (slot, field) in overrides
                .iter_mut()
                .zip(["banners", "previews", "sounds", "badges"])
            {
                let key = app_key(&app.id, field);
                if let Some(value) = read_bool(state, root, &key)
                    && self.app_toggles.get(&key) != Some(&value)
                {
                    self.app_toggles.insert(key, value);
                    *slot = Some(value);
                }
            }
            let rule = self.prefs.per_app.entry(app.id.clone()).or_default();
            for (target, value) in [
                &mut rule.banners,
                &mut rule.previews,
                &mut rule.sounds,
                &mut rule.badges,
            ]
            .into_iter()
            .zip(overrides)
            {
                if let Some(value) = value
                    && *target != Some(value)
                {
                    *target = Some(value);
                    changed = true;
                }
            }
            if let Some(value) = read_bool(state, root, app_key(&app.id, "priority"))
                && rule.priority != value
            {
                rule.priority = value;
                changed = true;
            }
            if let Some(value) = read_bool(state, root, app_key(&app.id, "priority_contacts"))
                && rule.priority_contacts != value
            {
                rule.priority_contacts = value;
                changed = true;
            }
            if let Some(label) = read_str(state, root, app_key(&app.id, "mute"))
                && let Some(seconds) = mute_seconds(&label)
                && self.mute_labels.get(&app.id).map(String::as_str) != Some(label.as_str())
            {
                let desired = (seconds > 0).then_some(now.saturating_add(seconds));
                rule.muted_until = desired;
                self.mute_labels.insert(app.id.clone(), label);
                changed = true;
            }
        }
        changed
    }

    fn reconcile_delivery_toggles(&mut self, state: &mut AutoCommit, root: &ObjId) -> bool {
        let mut changed = false;
        let (v, c) = self.reconcile_bool(state, root, BANNERS_KEY, self.prefs.show_banners);
        self.prefs.show_banners = v;
        changed |= c;
        let (v, c) = self.reconcile_bool(state, root, PREVIEWS_KEY, self.prefs.show_previews);
        self.prefs.show_previews = v;
        changed |= c;
        let (v, c) = self.reconcile_bool(state, root, SOUNDS_KEY, self.prefs.play_sounds);
        self.prefs.play_sounds = v;
        changed |= c;
        let (v, c) =
            self.reconcile_bool(state, root, LOCK_SCREEN_KEY, self.prefs.show_on_lock_screen);
        self.prefs.show_on_lock_screen = v;
        changed |= c;
        let (v, c) = self.reconcile_bool(state, root, BADGES_KEY, self.prefs.show_badges);
        self.prefs.show_badges = v;
        changed |= c;
        let (v, c) = self.reconcile_bool(state, root, FOCUS_ENABLED_KEY, self.prefs.focus.enabled);
        self.prefs.focus.enabled = v;
        changed |= c;
        let current = focus_time_label(self.prefs.focus.start_minute);
        let (value, c) = self.reconcile_text(state, root, FOCUS_START_KEY, current);
        if let Some(minutes) = focus_time_minutes(&value) {
            self.prefs.focus.start_minute = minutes;
        }
        changed |= c;
        let current = focus_time_label(self.prefs.focus.end_minute);
        let (value, c) = self.reconcile_text(state, root, FOCUS_END_KEY, current);
        if let Some(minutes) = focus_time_minutes(&value) {
            self.prefs.focus.end_minute = minutes;
        }
        changed |= c;
        changed
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(NotificationsPane::new())
}

impl Widget for NotificationsPane {
    ui::provides!(dyn ui::undo::CommittedActions);

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        self.seed_if_needed(state, root);

        let mut changed = self.reconcile_dnd_and_focus_days(state, root);
        changed |= self.reconcile_per_app_rules(state, root);
        changed |= self.reconcile_delivery_toggles(state, root);

        if changed {
            self.persist();
        }

        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "NotificationsPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::undo::CommittedActions for NotificationsPane {
    /// Hand the host the heads captured before an on-screen toggle dispatched its
    /// settings action, so it records one undo boundary for it (bubbled up to the
    /// root by `SettingsApp`). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rule enumeration, inherited per-app rules, seeding, DND persistence, status text and layout.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn notification_pane_seeds_rules_and_status_without_writing() {
        // Case `notification_rules_enumerate_system_and_flatpak_desktop_identities`.
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

        // Case `opening_the_pane_keeps_per_app_rules_inherited_and_writes_nothing`.
        {
            let config = crate::prefs::test_env::ConfigGuard::new();
            let id = "org.example.Inherits";
            let mut per_app = BTreeMap::new();
            per_app.insert(
                id.to_string(),
                settings_store::notification_policy::AppNotificationRule::default(),
            );
            let mut pane = NotificationsPane::with_prefs(NotificationPrefs {
                show_banners: false,
                per_app,
                ..NotificationPrefs::default()
            });
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            pane.sync(&mut doc, &root);
            assert_eq!(
                read_bool(&doc, &root, app_key(id, "banners")),
                Some(false),
                "the toggle shows the inherited global value"
            );
            assert_eq!(
                pane.prefs().per_app[id].banners,
                None,
                "showing the inherited value is not an override"
            );
            assert!(
                !config.config_root().join("settings.json").exists(),
                "opening the pane persists nothing"
            );

            let _ = doc.put(
                &root,
                app_key(id, "banners").as_str(),
                automerge::ScalarValue::Boolean(true),
            );
            pane.sync(&mut doc, &root);
            let rule = pane.prefs().per_app[id];
            assert_eq!(rule.banners, Some(true), "a flip is an override");
            assert_eq!(rule.sounds, None, "and only for the flipped field");
            assert_eq!(
                SystemPrefs::load().notifications.per_app[id].banners,
                Some(true)
            );
        }

        // Case `seed_writes_current_prefs_into_crdt`.
        {
            let mut pane = NotificationsPane::with_prefs(NotificationPrefs {
                do_not_disturb: true,
                show_banners: false,
                show_previews: true,
                play_sounds: false,
                show_on_lock_screen: true,
                ..NotificationPrefs::default()
            });
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            assert_eq!(read_bool(&doc, &root, DND_KEY), Some(true));
            assert_eq!(read_bool(&doc, &root, BANNERS_KEY), Some(false));
            assert_eq!(read_bool(&doc, &root, LOCK_SCREEN_KEY), Some(true));
        }

        // Case `toggling_dnd_updates_and_persists_prefs`.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let mut pane = NotificationsPane::with_prefs(NotificationPrefs::default());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            assert!(!pane.prefs().do_not_disturb);
            // User flips DND on.
            let _ = doc.put(&root, DND_KEY, automerge::ScalarValue::Boolean(true));
            pane.sync(&mut doc, &root);
            assert!(pane.prefs().do_not_disturb, "the pane adopted the toggle");
            // And it persisted to the shared store (loaded back through the store).
            assert!(
                SystemPrefs::load().notifications.do_not_disturb,
                "persisted"
            );
        }

        // Case `status_reflects_the_toggle_combination`.
        {
            let active = NotificationsPane::with_prefs(NotificationPrefs::default());
            assert_eq!(active.status().0, "Active");
            let silenced = NotificationsPane::with_prefs(NotificationPrefs {
                do_not_disturb: true,
                ..NotificationPrefs::default()
            });
            assert_eq!(silenced.status().0, "Silenced");
            let banners_off = NotificationsPane::with_prefs(NotificationPrefs {
                show_banners: false,
                ..NotificationPrefs::default()
            });
            assert_eq!(banners_off.status().0, "Banners off");
        }

        // Case `builds_layout`.
        {
            let mut pane = build();
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        }
    }

    /// On-screen toggles dispatch the palette action and record one undo boundary.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn notification_toggles_dispatch_palette_actions_with_one_undo() {
        // Case `on_screen_toggle_dispatches_same_action_as_palette`.
        {
            let config = crate::prefs::test_env::ConfigGuard::new();
            let mut pane = NotificationsPane::with_prefs(NotificationPrefs::default()); // DND off
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root); // seeds both keys; nothing to dispatch
            assert!(
                pane.last_dispatched().is_none(),
                "seeding dispatches no action"
            );

            // The palette's DND flip spec, read at the pre-flip state (DND off).
            let app = config.app();
            let res = ui::resources::resources(&app, &doc);
            let notif = res[0]
                .children
                .iter()
                .find(|r| r.id.as_str() == "settings.section:notifications")
                .unwrap();
            let dnd_spec = notif.actions.iter().find(|a| a.id == "dnd").unwrap();
            let from_spec = match &dnd_spec.input {
                tilo_actions::ActionInput::Immediate(enc) => enc.clone(),
                tilo_actions::ActionInput::Prompted { .. }
                | tilo_actions::ActionInput::Deferred { .. } => {
                    panic!("the DND flip is an immediate action")
                }
            };

            // Simulate the Toggle writing its scratch key on a flip, then sync.
            let scratch = NotificationsPane::scratch_key(DND_KEY);
            let _ = doc.put(
                &root,
                scratch.as_str(),
                automerge::ScalarValue::Boolean(true),
            );
            pane.sync(&mut doc, &root);

            let dispatched = pane
                .last_dispatched()
                .expect("the flip dispatched an action")
                .clone();
            let from_builder =
                crate::actions::encode(&crate::actions::set_bool_action(DND_KEY, true));
            assert_eq!(
                dispatched, from_builder,
                "on-screen flip == the shared builder"
            );
            assert_eq!(
                dispatched, from_spec,
                "on-screen flip == the palette ActionSpec"
            );

            // The action landed on the canonical key and the pane adopted + persisted it.
            assert_eq!(read_bool(&doc, &root, DND_KEY), Some(true));
            assert!(pane.prefs().do_not_disturb);
        }

        // Case `on_screen_toggle_records_one_undo_boundary_and_reverts`.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let mut pane = NotificationsPane::with_prefs(NotificationPrefs::default());
            let mut state = ui::State::new();
            pane.sync(&mut state.doc, &state.root); // seed

            // Flip the toggle's scratch key, then sync → the pane dispatches.
            let scratch = NotificationsPane::scratch_key(DND_KEY);
            let _ = state.doc.put(
                &state.root,
                scratch.as_str(),
                automerge::ScalarValue::Boolean(true),
            );
            pane.sync(&mut state.doc, &state.root);

            // The host's own recorder drains the pane's captured bracket and records
            // the boundary. Going through it rather than pushing an entry by hand is
            // the point: attribution happens in there, and undo is author-scoped.
            // The node map is empty because the pane is the root widget here.
            let local_author = ui::collaboration::local_user_id();
            let root: *mut dyn ui::widget::Widget = &mut pane;
            assert!(
                ui::undo::record_dispatched_action(
                    &mut state,
                    root,
                    &std::collections::HashMap::new(),
                    &local_author,
                ),
                "the host recorded an undo boundary for the dispatched flip"
            );
            assert_eq!(
                read_bool(&state.doc, &state.root, DND_KEY),
                Some(true),
                "the action set DND on"
            );
            assert!(pane.prefs().do_not_disturb);
            let entry = state
                .peek_last_undo()
                .expect("exactly one undo boundary recorded");
            assert_eq!(
                entry.author.as_ref(),
                Some(&local_author),
                "attributed to the local user, so their Ctrl+Z can reach it"
            );
            assert!(
                ui::undo::take_committed_action(&mut pane).is_none(),
                "the bracket is rising-edge"
            );

            // Ctrl+Z reverts precisely the setting.
            state
                .undo_as(Some(&local_author), None, None)
                .expect("an undo entry to pop");
            assert_eq!(
                read_bool(&state.doc, &state.root, DND_KEY),
                Some(false),
                "undo reverted the setting"
            );
            // Next frame the pane mirrors the reverted canonical back into the scratch
            // and adopts it, so the toggle follows — and it does not re-dispatch.
            pane.sync(&mut state.doc, &state.root);
            assert!(!pane.prefs().do_not_disturb, "the pane adopted the revert");
            assert_eq!(
                read_bool(&state.doc, &state.root, scratch.as_str()),
                Some(false),
                "the toggle's scratch mirrors the reverted value"
            );
            assert!(
                ui::undo::take_committed_action(&mut pane).is_none(),
                "mirroring the revert dispatches nothing"
            );
        }
    }
}
