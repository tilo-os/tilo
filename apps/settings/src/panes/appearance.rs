//! Appearance pane — the editor for the **shared appearance preferences**.
//!
//! This panel edits [`crate::prefs::AppearancePrefs`], the durable
//! look-and-feel section of the shared `settings.json`
//! ([`crate::region::RegionSettings::store_path`]). The OS theming layer
//! resolves those preferences into the concrete [`ui::Theme`] every app
//! draws against (see [`crate::prefs::AppearancePrefs::resolve_theme`]) — so
//! choosing a theme here is the single source of truth the rest of the
//! system follows, exactly like Region & Formats owns the locale.
//!
//! Controls:
//! - **Wallpaper** — a picture chosen through the file-chooser portal and
//!   applied through the Wallpaper portal, on this workspace or every one.
//! - **Theme** — a named preset ([`crate::prefs::AppearancePrefs::THEMES`]),
//!   or "Automatic": light by day, dark by night.
//! - **Day and night** — when "Automatic" and a scheduled Night Light switch:
//!   at sunset and sunrise, or at custom times.
//! - **High contrast** — overrides the palette with the WCAG-AAA track.
//! - **Reduce motion** — snaps animations to their resting state.
//! - **Text size** — a UI-wide text-scale multiplier for low-vision comfort.
//!
//! Each control is bound to a CRDT key; `sync` reads the key, folds the
//! change into the loaded prefs, and persists the shared store. On first
//! sync it seeds the controls from the loaded prefs so they rest on the
//! user's current choice.

use std::collections::HashMap;
use ui::doc::{read_bool, read_str_or_text};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Button, Dropdown, Row, Spacer, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::actions::{SETTINGS_ACTION_TYPE, SettingsAction, set_bool_action, set_text_action};
use crate::i18n::{t, t_args};
use crate::prefs::{AppearancePrefs, SystemPrefs};
use crate::widgets::{caption, card, choice_row, rows, setting_row_described, value_row};

pub const THEME_KEY: &str = "appearance.theme";
pub const HIGH_CONTRAST_KEY: &str = "appearance.high_contrast";
pub const REDUCE_MOTION_KEY: &str = "appearance.reduce_motion";
pub const TEXT_SIZE_KEY: &str = "appearance.text_size";
pub const UI_SCALE_KEY: &str = "appearance.ui_scale";
pub const SCHEDULE_KEY: &str = "appearance.schedule";
pub const SUNRISE_KEY: &str = "appearance.sunrise";
pub const SUNSET_KEY: &str = "appearance.sunset";
pub const WALLPAPER_SCOPE_KEY: &str = "appearance.wallpaper_scope";
pub const WALLPAPER_TARGET_KEY: &str = "appearance.wallpaper_target";
/// Command bit the "Choose picture…" button sets; the pane opens the chooser
/// and clears it. Never persisted.
pub const WALLPAPER_CHOOSE_KEY: &str = "appearance.wallpaper.choose";

/// `(label, persisted value)` of each day/night schedule.
pub const SCHEDULES: [(&str, &str); 2] = [("Sunset to sunrise", "sun"), ("Custom times", "custom")];
pub const SCHEDULE_LABELS: [&str; 2] = [SCHEDULES[0].0, SCHEDULES[1].0];
/// `(label, persisted value)` of which workspaces a new wallpaper covers.
pub const WALLPAPER_SCOPES: [(&str, &str); 2] =
    [("This workspace", "workspace"), ("Every workspace", "all")];
pub const WALLPAPER_SCOPE_LABELS: [&str; 2] = [WALLPAPER_SCOPES[0].0, WALLPAPER_SCOPES[1].0];
/// `(label, persisted value)` of where a new wallpaper shows.
pub const WALLPAPER_TARGETS: [(&str, &str); 3] = [
    ("Desktop", "background"),
    ("Lock screen", "lockscreen"),
    ("Desktop and lock screen", "both"),
];
/// The offered custom sunrise and sunset times.
pub const TIMES: [&str; 48] = [
    "00:00", "00:30", "01:00", "01:30", "02:00", "02:30", "03:00", "03:30", "04:00", "04:30",
    "05:00", "05:30", "06:00", "06:30", "07:00", "07:30", "08:00", "08:30", "09:00", "09:30",
    "10:00", "10:30", "11:00", "11:30", "12:00", "12:30", "13:00", "13:30", "14:00", "14:30",
    "15:00", "15:30", "16:00", "16:30", "17:00", "17:30", "18:00", "18:30", "19:00", "19:30",
    "20:00", "20:30", "21:00", "21:30", "22:00", "22:30", "23:00", "23:30",
];

fn value_for_shown<'a>(
    table: &[(&'a str, &'a str)],
    label: &str,
    shown: fn(&str) -> String,
) -> Option<&'a str> {
    table
        .iter()
        .find(|(english, value)| *english == label || shown(value) == label)
        .map(|(_, value)| *value)
}

/// The label of a persisted schedule value.
pub fn schedule_label(value: &str) -> String {
    t(match value {
        "custom" => "settings-appearance-schedule-custom",
        _ => "settings-appearance-schedule-sun",
    })
}

/// The label of a persisted wallpaper scope.
pub fn scope_label(value: &str) -> String {
    t(match value {
        "all" => "settings-appearance-scope-all",
        _ => "settings-appearance-scope-workspace",
    })
}

fn target_label(value: &str) -> String {
    t(match value {
        "lockscreen" => "settings-appearance-target-lock",
        "both" => "settings-appearance-target-both",
        _ => "settings-appearance-target-desktop",
    })
}

fn text_size_ftl(english: &str) -> &'static str {
    match english {
        "Small" => "settings-appearance-text-small",
        "Large" => "settings-appearance-text-large",
        "Larger" => "settings-appearance-text-larger",
        _ => "settings-appearance-text-default",
    }
}

/// The offered global UI-scale steps as `(display label, factor)`. The factor is
/// what [`AppearancePrefs::ui_scale`] persists; it grows the whole design system
/// (spacing, radii, and type together) and composes with the per-output
/// fractional device scale. Spans the daily-driver-comfortable 90 %–150 % range.
pub const UI_SCALES: &[(&str, f64)] = &[
    ("90%", 0.90),
    ("100%", 1.0),
    ("110%", 1.10),
    ("125%", 1.25),
    ("150%", 1.50),
];

/// Map a UI-scale factor to the closest offered label (defaults to "100%").
fn ui_scale_label(scale: f64) -> &'static str {
    // `total_cmp`, not `partial_cmp().unwrap()`: a hand-edited settings.json
    // could hold a non-finite scale, and this must degrade to "100%" rather
    // than panic the pane.
    UI_SCALES
        .iter()
        .min_by(|(_, a), (_, b)| (a - scale).abs().total_cmp(&(b - scale).abs()))
        .map_or("100%", |(label, _)| *label)
}

/// Map a UI-scale label back to its factor, if recognised.
fn ui_scale_factor(label: &str) -> Option<f64> {
    UI_SCALES.iter().find(|(l, _)| *l == label).map(|(_, s)| *s)
}

/// The offered text sizes as `(display label, scale factor)`. The label is
/// what the dropdown shows and stores; the factor is what
/// [`AppearancePrefs::text_scale`] persists.
pub const TEXT_SIZES: &[(&str, f64)] = &[
    ("Small", 0.85),
    ("Default", 1.0),
    ("Large", 1.15),
    ("Larger", 1.30),
];

/// Map a text-scale factor to the closest offered label (defaults to
/// "Default"). Shared with the Accessibility pane, which offers the same
/// text-size control over the same [`AppearancePrefs::text_scale`] field.
pub fn text_size_label(scale: f64) -> String {
    // `total_cmp`, not `partial_cmp().unwrap()`: a hand-edited settings.json
    // could hold a non-finite scale, and this must degrade to Default
    // rather than panic the pane.
    let english = TEXT_SIZES
        .iter()
        .min_by(|(_, a), (_, b)| (a - scale).abs().total_cmp(&(b - scale).abs()))
        .map_or("Default", |(label, _)| *label);
    t(text_size_ftl(english))
}

/// Map a text-size label back to its scale factor, if recognised. Shared with
/// the Accessibility pane. Accepts the English source label or the
/// localised one so a test or a German dropdown both resolve.
pub fn text_size_scale(label: &str) -> Option<f64> {
    TEXT_SIZES
        .iter()
        .find(|(english, _)| *english == label || t(text_size_ftl(english)) == label)
        .map(|(_, s)| *s)
}

/// Localised labels for the shared text-size dropdown.
pub fn text_size_options() -> Vec<String> {
    TEXT_SIZES
        .iter()
        .map(|(english, _)| t(text_size_ftl(english)))
        .collect()
}

pub struct AppearancePane {
    prefs: AppearancePrefs,
    needs_seed: bool,
    /// The wallpaper file chooser in flight.
    wallpaper_pick: Option<ui::portal::OpenFileChannel>,
    /// The Wallpaper portal request in flight (the desktop previews and asks).
    wallpaper_apply: Option<ui::portal::WallpaperChannel>,
    /// What happened to the last wallpaper request, shown under the button.
    wallpaper_status: Option<String>,
    structure_dirty: bool,
    commit_context_pending: bool,
    /// The only writer for durable appearance values. Controls edit a local
    /// draft key and this host dispatches the same action exported by Settings'
    /// resource tree; a click, keyboard activation, Ctrl+K, replay, and undo
    /// therefore all share one canonical mutation path.
    actions: ui::ActionHost,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for AppearancePane {
    fn default() -> Self {
        Self::new()
    }
}

impl AppearancePane {
    pub fn new() -> Self {
        Self::with_prefs(SystemPrefs::load().appearance)
    }

    /// Construct from explicit preferences — used by tests and the
    /// deterministic `--shots` render.
    pub fn with_prefs(prefs: AppearancePrefs) -> Self {
        Self {
            prefs,
            needs_seed: true,
            wallpaper_pick: None,
            wallpaper_apply: None,
            wallpaper_status: None,
            structure_dirty: false,
            commit_context_pending: false,
            actions: ui::ActionHost::new(),
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn prefs(&self) -> &AppearancePrefs {
        &self.prefs
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let theme_opts: Vec<String> = AppearancePrefs::THEMES
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        let theme_path = Self::draft_key(THEME_KEY);
        let theme = Dropdown::new(theme_opts, &theme_path)
            .with_id("appearance.theme")
            .with_placeholder(&self.prefs.resolve_theme().name);

        let size_opts = text_size_options();
        let size_path = Self::draft_key(TEXT_SIZE_KEY);
        let size_current = text_size_label(self.prefs.text_scale);
        let size = Dropdown::new(size_opts, &size_path)
            .with_id("appearance.text_size")
            .with_placeholder(&size_current);

        let ui_scale_opts: Vec<String> = UI_SCALES.iter().map(|(l, _)| l.to_string()).collect();
        let ui_scale_path = Self::draft_key(UI_SCALE_KEY);
        let ui_scale = Dropdown::new(ui_scale_opts, &ui_scale_path)
            .with_id("appearance.ui_scale")
            .with_placeholder(ui_scale_label(self.prefs.ui_scale));

        let theme_card = card(
            &t("settings-appearance-theme-card"),
            rows(vec![
                choice_row(&t("settings-appearance-theme"), Box::new(theme)),
                choice_row(&t("settings-appearance-ui-scale"), Box::new(ui_scale)),
                choice_row(&t("settings-appearance-text-size"), Box::new(size)),
            ]),
        );

        let access_card = card(
            &t("settings-appearance-access-card"),
            rows(vec![
                setting_row_described(
                    &t("settings-appearance-high-contrast"),
                    &t("settings-appearance-high-contrast-desc"),
                    Box::new(
                        Toggle::new(
                            "settings.appearance.high_contrast",
                            "appearance.high_contrast.edit",
                        )
                        .with_id("appearance.high_contrast"),
                    ),
                ),
                setting_row_described(
                    &t("settings-appearance-reduce-motion"),
                    &t("settings-appearance-reduce-motion-desc"),
                    Box::new(
                        Toggle::new(
                            "settings.appearance.reduce_motion",
                            "appearance.reduce_motion.edit",
                        )
                        .with_id("appearance.reduce_motion"),
                    ),
                ),
            ]),
        );

        // A live summary of the theme these choices resolve to, so the effect
        // is legible before it is applied system-wide.
        let resolved = self.prefs.resolve_theme();
        let motion = if resolved.reduce_motion {
            t("settings-appearance-on")
        } else {
            t("settings-appearance-off")
        };
        let preview = card(
            &t("settings-appearance-applied-card"),
            rows(vec![
                value_row(&t("settings-appearance-active-theme"), &resolved.name),
                value_row(
                    &t("settings-appearance-ui-scale"),
                    &format!("{:.0}%", f64::from(resolved.ui_scale) * 100.0),
                ),
                value_row(
                    &t("settings-appearance-text-scale"),
                    &format!("{:.0}%", f64::from(resolved.text_scale) * 100.0),
                ),
                value_row(&t("settings-appearance-reduced-motion"), &motion),
            ]),
        );

        let explainer = card(
            &t("settings-appearance-shared-card"),
            rows(vec![caption(&t("settings-appearance-shared-body"))]),
        );

        crate::widgets::cards(vec![
            self.wallpaper_card(),
            theme_card,
            self.schedule_card(),
            access_card,
            preview,
            explainer,
        ])
    }

    /// The wallpaper controls. Tilo ships no stock pictures, so the choice is
    /// any picture the user has, through the desktop's file chooser.
    fn wallpaper_card(&self) -> Box<dyn Widget> {
        let dropdown = |labels: Vec<String>, key: &str, id: &str, current: &str| {
            Box::new(
                Dropdown::new(labels, &Self::draft_key(key))
                    .with_id(id)
                    .with_placeholder(current),
            ) as Box<dyn Widget>
        };
        let scope_current = scope_label(&self.prefs.wallpaper_scope);
        let target_current = target_label(&self.prefs.wallpaper_target);
        let mut body = vec![
            caption(&t("settings-appearance-wallpaper-body")),
            choice_row(
                &t("settings-appearance-use-for"),
                dropdown(
                    vec![
                        t("settings-appearance-scope-workspace"),
                        t("settings-appearance-scope-all"),
                    ],
                    WALLPAPER_SCOPE_KEY,
                    "appearance.wallpaper_scope",
                    &scope_current,
                ),
            ),
            choice_row(
                &t("settings-appearance-show-on"),
                dropdown(
                    vec![
                        t("settings-appearance-target-desktop"),
                        t("settings-appearance-target-lock"),
                        t("settings-appearance-target-both"),
                    ],
                    WALLPAPER_TARGET_KEY,
                    "appearance.wallpaper_target",
                    &target_current,
                ),
            ),
            Box::new(
                Row::new()
                    .push(Box::new(
                        Button::new(
                            "appearance.wallpaper.choose",
                            &t("settings-appearance-choose-picture"),
                            Some(WALLPAPER_CHOOSE_KEY),
                        )
                        .content_sized(),
                    ))
                    .push(Box::new(Spacer::new())),
            ),
        ];
        if let Some(status) = &self.wallpaper_status {
            body.push(caption(status));
        }
        card(&t("settings-appearance-wallpaper-card"), rows(body))
    }

    /// When the day ends, for the Automatic theme and a scheduled Night Light.
    fn schedule_card(&self) -> Box<dyn Widget> {
        let dropdown = |labels: Vec<String>, key: &str, id: &str, current: &str| {
            Box::new(
                Dropdown::new(labels, &Self::draft_key(key))
                    .with_id(id)
                    .with_placeholder(current),
            ) as Box<dyn Widget>
        };
        let times = || {
            TIMES
                .iter()
                .map(|time| (*time).to_string())
                .collect::<Vec<_>>()
        };
        let schedule_current = schedule_label(&self.prefs.schedule);
        let mut body = vec![choice_row(
            &t("settings-appearance-schedule"),
            dropdown(
                vec![
                    t("settings-appearance-schedule-sun"),
                    t("settings-appearance-schedule-custom"),
                ],
                SCHEDULE_KEY,
                "appearance.schedule",
                &schedule_current,
            ),
        )];
        let schedule = self.prefs.schedule();
        if !schedule.config.follow_sun {
            body.push(choice_row(
                &t("settings-appearance-day-starts"),
                dropdown(
                    times(),
                    SUNRISE_KEY,
                    "appearance.sunrise",
                    &self.prefs.sunrise,
                ),
            ));
            body.push(choice_row(
                &t("settings-appearance-night-starts"),
                dropdown(times(), SUNSET_KEY, "appearance.sunset", &self.prefs.sunset),
            ));
        }
        let today = schedule
            .config
            .resolve(schedule.location, ui::theme::local_moment());
        let today_value = t_args(
            "settings-appearance-today-value",
            &[
                (
                    "day",
                    settings_store::day_schedule::format_hhmm(today.sunrise).into(),
                ),
                (
                    "night",
                    settings_store::day_schedule::format_hhmm(today.sunset).into(),
                ),
            ],
        );
        body.push(value_row(&t("settings-appearance-today"), &today_value));
        body.push(caption(&t(
            if schedule.config.follow_sun && schedule.location.is_none() {
                "settings-appearance-schedule-no-location"
            } else {
                "settings-appearance-schedule-follow"
            },
        )));
        card(&t("settings-appearance-day-night-card"), rows(body))
    }

    /// Open the chooser when the button asked for it, and carry a chosen
    /// picture through the Wallpaper portal.
    fn sync_wallpaper(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, WALLPAPER_CHOOSE_KEY).unwrap_or(false) {
            let _ = state.put(root, WALLPAPER_CHOOSE_KEY, false);
            if self.wallpaper_pick.is_none() && self.wallpaper_apply.is_none() {
                let chooser_title = t("settings-appearance-choose-wallpaper");
                let chooser_ok = t("settings-appearance-choose");
                let pictures = t("settings-appearance-pictures");
                self.wallpaper_pick = Some(
                    ui::portal::PickRequest::open(&chooser_title, &chooser_ok)
                        .filter(
                            ui::portal::FileFilter::new(&pictures)
                                .mimetype("image/png")
                                .mimetype("image/jpeg")
                                .mimetype("image/webp"),
                        )
                        .thread_name("settings-wallpaper")
                        .send_open_file(),
                );
            }
        }
        if let Some(pick) = &self.wallpaper_pick
            && let Ok(outcome) = pick.try_recv()
        {
            self.wallpaper_pick = None;
            match outcome {
                ui::portal::OpenFileOutcome::Opened(file) => {
                    let target = match self.prefs.wallpaper_target.as_str() {
                        "lockscreen" => ui::portal::WallpaperTarget::Lockscreen,
                        "both" => ui::portal::WallpaperTarget::Both,
                        _ => ui::portal::WallpaperTarget::Background,
                    };
                    self.wallpaper_apply =
                        Some(ui::portal::send_set_wallpaper(file.into_file(), target));
                    self.set_wallpaper_status(Some(t("settings-appearance-confirm-preview")));
                }
                ui::portal::OpenFileOutcome::Cancelled => {}
                ui::portal::OpenFileOutcome::Failed(error) => {
                    self.set_wallpaper_status(Some(t_args(
                        "settings-appearance-open-failed",
                        &[("error", error.into())],
                    )));
                }
            }
        }
        if let Some(apply) = &self.wallpaper_apply
            && let Ok(outcome) = apply.try_recv()
        {
            self.wallpaper_apply = None;
            self.set_wallpaper_status(Some(match outcome {
                Ok(()) => t("settings-appearance-updated"),
                Err(error) => t_args("settings-appearance-unchanged", &[("error", error.into())]),
            }));
        }
    }

    /// Fold the day/night schedule and wallpaper choices from the document
    /// into the prefs. Returns whether any changed.
    fn reconcile_schedule_and_wallpaper(&mut self, state: &mut AutoCommit, root: &ObjId) -> bool {
        let mut changed = false;
        let structure_before = (
            self.prefs.schedule.clone(),
            self.prefs.sunrise.clone(),
            self.prefs.sunset.clone(),
        );
        let current = schedule_label(&self.prefs.schedule);
        let (label, schedule_changed) = self.reconcile_text(state, root, SCHEDULE_KEY, &current);
        changed |= schedule_changed;
        if let Some(value) = value_for_shown(&SCHEDULES, &label, schedule_label) {
            self.prefs.schedule = value.to_string();
        }
        for (key, is_sunrise) in [(SUNRISE_KEY, true), (SUNSET_KEY, false)] {
            let current = if is_sunrise {
                self.prefs.sunrise.clone()
            } else {
                self.prefs.sunset.clone()
            };
            let (time, time_changed) = self.reconcile_text(state, root, key, &current);
            changed |= time_changed;
            if let Some(minutes) = settings_store::day_schedule::parse_hhmm(&time) {
                let time = settings_store::day_schedule::format_hhmm(minutes);
                if is_sunrise {
                    self.prefs.sunrise = time;
                } else {
                    self.prefs.sunset = time;
                }
            }
        }
        if structure_before
            != (
                self.prefs.schedule.clone(),
                self.prefs.sunrise.clone(),
                self.prefs.sunset.clone(),
            )
        {
            self.structure_dirty = true;
        }
        let current = scope_label(&self.prefs.wallpaper_scope);
        let (label, scope_changed) =
            self.reconcile_text(state, root, WALLPAPER_SCOPE_KEY, &current);
        changed |= scope_changed;
        if let Some(value) = value_for_shown(&WALLPAPER_SCOPES, &label, scope_label) {
            self.prefs.wallpaper_scope = value.to_string();
        }
        let current = target_label(&self.prefs.wallpaper_target);
        let (label, target_changed) =
            self.reconcile_text(state, root, WALLPAPER_TARGET_KEY, &current);
        changed |= target_changed;
        if let Some(value) = value_for_shown(&WALLPAPER_TARGETS, &label, target_label) {
            self.prefs.wallpaper_target = value.to_string();
        }

        changed
    }

    fn set_wallpaper_status(&mut self, status: Option<String>) {
        if self.wallpaper_status != status {
            self.wallpaper_status = status;
            self.structure_dirty = true;
        }
    }

    /// Persist the current prefs into the shared store, preserving other
    /// sections. Reads fresh so a change here never clobbers another pane's.
    fn persist(&self) {
        let mut p = SystemPrefs::load();
        p.appearance = self.prefs.clone();
        if let Err(e) = p.save() {
            warn!(target: "tilo::settings", op = "appearance_save", error = %e, "failed to persist appearance prefs");
        }
    }

    fn draft_key(canonical: &str) -> String {
        format!("{canonical}.edit")
    }

    fn dispatch(&mut self, state: &mut AutoCommit, action: &SettingsAction) {
        self.actions.dispatch_with_context(
            state,
            SETTINGS_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.settings", "Settings", action),
        );
    }

    /// Reconcile a dropdown draft with its action-owned value. The draft is
    /// presentation state only; it is deliberately never read by other apps
    /// or published as a setting.
    fn reconcile_text(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        canonical: &str,
        current: &str,
    ) -> (String, bool) {
        let canonical_value =
            read_str_or_text(state, root, canonical).unwrap_or_else(|| current.to_string());
        let draft = Self::draft_key(canonical);
        let draft_value =
            read_str_or_text(state, root, &draft).unwrap_or_else(|| canonical_value.clone());
        if canonical_value != current {
            let _ = state.put(root, draft, canonical_value.as_str());
            (canonical_value, true)
        } else if draft_value != canonical_value {
            self.dispatch(state, &set_text_action(canonical, draft_value.clone()));
            (draft_value, true)
        } else {
            (current.to_string(), false)
        }
    }

    fn reconcile_bool(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        canonical: &str,
        current: bool,
    ) -> (bool, bool) {
        let canonical_value = read_bool(state, root, canonical).unwrap_or(current);
        let draft = Self::draft_key(canonical);
        let draft_value = read_bool(state, root, &draft).unwrap_or(canonical_value);
        if canonical_value != current {
            let _ = state.put(root, draft, canonical_value);
            (canonical_value, true)
        } else if draft_value != canonical_value {
            self.dispatch(state, &set_bool_action(canonical, draft_value));
            (draft_value, true)
        } else {
            (current, false)
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(AppearancePane::new())
}

impl Widget for AppearancePane {
    ui::provides!(dyn ui::undo::UndoPoints);

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        if self.needs_seed {
            self.dispatch(state, &set_text_action(THEME_KEY, self.prefs.theme.clone()));
            self.dispatch(
                state,
                &set_text_action(TEXT_SIZE_KEY, text_size_label(self.prefs.text_scale)),
            );
            self.dispatch(
                state,
                &set_text_action(UI_SCALE_KEY, ui_scale_label(self.prefs.ui_scale)),
            );
            self.dispatch(
                state,
                &set_bool_action(HIGH_CONTRAST_KEY, self.prefs.high_contrast),
            );
            self.dispatch(
                state,
                &set_bool_action(REDUCE_MOTION_KEY, self.prefs.reduce_motion),
            );
            self.dispatch(
                state,
                &set_text_action(SCHEDULE_KEY, schedule_label(&self.prefs.schedule)),
            );
            self.dispatch(
                state,
                &set_text_action(SUNRISE_KEY, self.prefs.sunrise.clone()),
            );
            self.dispatch(
                state,
                &set_text_action(SUNSET_KEY, self.prefs.sunset.clone()),
            );
            self.dispatch(
                state,
                &set_text_action(
                    WALLPAPER_SCOPE_KEY,
                    scope_label(&self.prefs.wallpaper_scope),
                ),
            );
            self.dispatch(
                state,
                &set_text_action(
                    WALLPAPER_TARGET_KEY,
                    target_label(&self.prefs.wallpaper_target),
                ),
            );
            let _ = state.put(root, WALLPAPER_CHOOSE_KEY, false);
            self.needs_seed = false;
        }
        self.sync_wallpaper(state, root);

        let current_theme = self.prefs.theme.clone();
        let (theme, mut changed) = self.reconcile_text(state, root, THEME_KEY, &current_theme);
        if AppearancePrefs::THEMES.contains(&theme.as_str()) && theme != self.prefs.theme {
            self.prefs.theme = theme;
        }
        let current_text_size = text_size_label(self.prefs.text_scale);
        let (text_size, text_changed) =
            self.reconcile_text(state, root, TEXT_SIZE_KEY, &current_text_size);
        changed |= text_changed;
        if let Some(scale) = text_size_scale(&text_size)
            && (scale - self.prefs.text_scale).abs() > f64::EPSILON
        {
            self.prefs.text_scale = scale;
        }
        let current_ui_scale = ui_scale_label(self.prefs.ui_scale);
        let (ui_scale, scale_changed) =
            self.reconcile_text(state, root, UI_SCALE_KEY, current_ui_scale);
        changed |= scale_changed;
        if let Some(factor) = ui_scale_factor(&ui_scale)
            && (factor - self.prefs.ui_scale).abs() > f64::EPSILON
        {
            self.prefs.ui_scale = factor;
        }
        let (high_contrast, contrast_changed) =
            self.reconcile_bool(state, root, HIGH_CONTRAST_KEY, self.prefs.high_contrast);
        changed |= contrast_changed;
        if high_contrast != self.prefs.high_contrast {
            self.prefs.high_contrast = high_contrast;
        }
        let (reduce_motion, motion_changed) =
            self.reconcile_bool(state, root, REDUCE_MOTION_KEY, self.prefs.reduce_motion);
        changed |= motion_changed;
        if reduce_motion != self.prefs.reduce_motion {
            self.prefs.reduce_motion = reduce_motion;
        }
        changed |= self.reconcile_schedule_and_wallpaper(state, root);
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
        let l = taffy.layout(node).copied().unwrap_or_default();
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
        let was_handled = *ctx.handled;
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
        self.commit_context_pending |= !was_handled && *ctx.handled;
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
        "AppearancePane"
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty());
        own || child
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Surface the user's current appearance choice so the running Settings
    /// window re-scales and re-themes live the instant a control changes. The
    /// host [`ui::App`] (with `appearance_reconcile` enabled by `main`) polls
    /// this each frame and applies any field that differs. We hand back the
    /// *base* colour theme unscaled — the App folds in high-contrast, the UI
    /// scale, and the text scale itself, keeping that bookkeeping in one place.
    fn appearance_intent(&self) -> Option<ui::theme::AppearanceIntent> {
        Some(ui::theme::AppearanceIntent {
            base_theme: self.prefs.base_theme(),
            high_contrast: self.prefs.high_contrast,
            reduce_motion: self.prefs.reduce_motion,
            text_scale: self.prefs.text_scale as f32,
            ui_scale: self.prefs.ui_scale as f32,
        })
    }
}

impl ui::undo::UndoPoints for AppearancePane {
    fn take_commit_context(&mut self) -> Option<ui::tilo_actions::ActionContext> {
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| ui::undo::needs_commit(content.as_ref()));
        crate::panes::preference_commit_context(
            pending,
            "appearance",
            "Appearance",
            "Change appearance preferences",
        )
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    /// The pane seeds its prefs into the document, hands the shell its live
    /// appearance intent, and builds a layout.
    #[test]
    fn appearance_pane_seeds_prefs_and_expresses_its_intent() {
        let mut pane = AppearancePane::with_prefs(AppearancePrefs {
            theme: "Nord".into(),
            high_contrast: true,
            reduce_motion: false,
            text_scale: 1.15,
            ui_scale: 1.25,
            ..AppearancePrefs::default()
        });
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert_eq!(
            read_str_or_text(&doc, &root, THEME_KEY).as_deref(),
            Some("Nord")
        );
        assert_eq!(
            read_str_or_text(&doc, &root, TEXT_SIZE_KEY).as_deref(),
            Some("Large"),
            "1.15 seeds the 'Large' label"
        );
        assert_eq!(
            read_str_or_text(&doc, &root, UI_SCALE_KEY).as_deref(),
            Some("125%"),
            "1.25 seeds the '125%' UI-scale label"
        );
        assert_eq!(read_bool(&doc, &root, HIGH_CONTRAST_KEY), Some(true));

        let pane = AppearancePane::with_prefs(AppearancePrefs {
            theme: "Nord".into(),
            high_contrast: false,
            reduce_motion: true,
            text_scale: 1.15,
            ui_scale: 1.25,
            ..AppearancePrefs::default()
        });
        let intent = ui::widget::Widget::appearance_intent(&pane)
            .expect("the appearance pane always expresses an intent");
        assert_eq!(
            intent.base_theme.name, "Nord",
            "base theme is handed back unscaled"
        );
        assert_eq!(intent.ui_scale, 1.25);
        assert_eq!(intent.text_scale, 1.15);
        assert!(intent.reduce_motion);
        assert!(!intent.high_contrast);

        let mut pane = build();
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }

    #[test]
    fn searching_for_wallpaper_lands_on_the_wallpaper_control() {
        use crate::categories::Category;
        let hits: Vec<Category> = Category::ALL
            .iter()
            .copied()
            .filter(|category| category.matches("wallpaper"))
            .collect();
        assert_eq!(hits, vec![Category::Appearance]);

        let mut pane = AppearancePane::with_prefs(AppearancePrefs::default());
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        let ids: Vec<String> = node_map
            .values()
            // SAFETY: the map was just built from the live pane tree.
            .filter_map(|&widget| unsafe { (*widget).test_id() }.map(str::to_string))
            .collect();
        for id in ["appearance.wallpaper.choose", "appearance.wallpaper_scope"] {
            assert!(
                ids.iter().any(|candidate| candidate == id),
                "{id} missing from {ids:?}"
            );
        }
    }

    #[test]
    fn the_day_and_night_schedule_round_trips_through_the_document_and_settings() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let mut pane = AppearancePane::with_prefs(AppearancePrefs::default());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert_eq!(
            read_str_or_text(&doc, &root, SCHEDULE_KEY).as_deref(),
            Some("Sunset to sunrise")
        );
        assert_eq!(
            read_str_or_text(&doc, &root, SUNRISE_KEY).as_deref(),
            Some("07:00")
        );

        let _ = doc.put(&root, THEME_KEY, "Automatic");
        let _ = doc.put(&root, SCHEDULE_KEY, "Custom times");
        let _ = doc.put(&root, SUNRISE_KEY, "06:30");
        let _ = doc.put(&root, SUNSET_KEY, "21:00");
        let _ = doc.put(&root, WALLPAPER_SCOPE_KEY, "Every workspace");
        pane.sync(&mut doc, &root);
        assert_eq!(pane.prefs().theme, "Automatic");
        assert_eq!(pane.prefs().schedule, "custom");

        let saved = SystemPrefs::load().appearance;
        assert_eq!(saved.theme, "Automatic");
        assert_eq!(saved.schedule, "custom");
        assert_eq!(saved.sunrise, "06:30");
        assert_eq!(saved.sunset, "21:00");
        assert_eq!(saved.wallpaper_scope, "all");

        // What the compositor and every app read back from the shared file.
        let path = settings_store::settings_path().expect("test config dir");
        let section = settings_store::load_section_at(&path, "appearance");
        let config = settings_store::day_schedule::ScheduleConfig::from_section(&section);
        assert!(!config.follow_sun);
        assert_eq!(config.fixed.sunrise, 6 * 60 + 30);
        assert_eq!(config.fixed.sunset, 21 * 60);

        // A fresh pane shows the saved choice, not the defaults.
        let mut reopened = AppearancePane::new();
        let mut doc = AutoCommit::new();
        reopened.sync(&mut doc, &root);
        assert_eq!(
            read_str_or_text(&doc, &root, SCHEDULE_KEY).as_deref(),
            Some("Custom times")
        );
        assert_eq!(
            read_str_or_text(&doc, &root, SUNSET_KEY).as_deref(),
            Some("21:00")
        );
    }

    /// A default pane (persisting through a scratch settings file) after the
    /// document carries `edits`.
    fn pane_after_edits(edits: &[(&str, automerge::ScalarValue)]) -> AppearancePane {
        let mut pane = AppearancePane::with_prefs(AppearancePrefs::default());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        for (key, value) in edits {
            let _ = doc.put(&root, *key, value.clone());
        }
        pane.sync(&mut doc, &root);
        pane
    }

    /// Dropdown and toggle edits in the document update the prefs, which
    /// resolve to the real theme; unknown values are ignored.
    #[test]
    fn appearance_edits_update_prefs_and_resolve() {
        use automerge::ScalarValue::{Boolean, Str};
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let pane = pane_after_edits(&[(UI_SCALE_KEY, Str("150%".into()))]);
        assert_eq!(pane.prefs().ui_scale, 1.50);
        // The pref folds into the resolved theme's ui_scale + scaled tokens.
        let resolved = pane.prefs().resolve_theme();
        assert_eq!(resolved.ui_scale, 1.50);
        let base_gap = ui::Theme::dark().tokens.spacing.s3;
        assert!((resolved.tokens.spacing.s3 - base_gap * 1.50).abs() < 1e-2);

        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let pane = pane_after_edits(&[(THEME_KEY, Str("Gruvbox".into()))]);
        assert_eq!(pane.prefs().theme, "Gruvbox");
        assert_eq!(pane.prefs().resolve_theme().name, "Gruvbox");

        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let pane = pane_after_edits(&[
            (TEXT_SIZE_KEY, Str("Larger".into())),
            (HIGH_CONTRAST_KEY, Boolean(true)),
            (REDUCE_MOTION_KEY, Boolean(true)),
        ]);
        assert_eq!(pane.prefs().text_scale, 1.30);
        assert!(pane.prefs().high_contrast);
        assert!(pane.prefs().reduce_motion);
        // high_contrast wins the resolved palette.
        assert_eq!(pane.prefs().resolve_theme().name, "High Contrast");

        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let pane = pane_after_edits(&[(THEME_KEY, Str("not-a-theme".into()))]);
        assert_eq!(pane.prefs().theme, "Dark", "stayed on the default");
    }

    #[test]
    fn scale_and_text_size_labels_round_trip() {
        for (factor, label) in [(1.0, "100%"), (1.25, "125%"), (1.50, "150%")] {
            assert_eq!(ui_scale_label(factor), label);
        }
        assert_eq!(ui_scale_factor("90%"), Some(0.90));
        assert_eq!(ui_scale_factor("nope"), None);
        assert_eq!(text_size_label(1.0), "Default");
        assert_eq!(text_size_label(1.30), "Larger");
        assert_eq!(text_size_scale("Small"), Some(0.85));
        assert_eq!(text_size_scale("Nonsense"), None);
    }
}
