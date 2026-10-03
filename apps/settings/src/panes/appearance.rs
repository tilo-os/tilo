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
//! - **Appearance** — light, dark, or automatic (light by day, dark by
//!   night), and the theme each of the two polarities shows.
//! - **Day and night** — when automatic: at sunset and sunrise, or at custom
//!   times.
//! - **Your colour** — the presence colour of the person at this device.
//! - **Size** — density, UI scale and text size.
//! - **Accessibility** — high contrast, reduce transparency, reduce motion.
//! - **Wallpaper** — a picture chosen through the file-chooser portal and
//!   applied through the Wallpaper portal, on this workspace or every one.
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
use ui::tokens::PresenceHue;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Button, ColorSwatch, ColorSwatchPicker, Dropdown, Row, Spacer, Toggle, ToggleGroup, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::actions::{SETTINGS_ACTION_TYPE, SettingsAction, set_bool_action, set_text_action};
use crate::i18n::{t, t_args};
use crate::prefs::{AppearancePrefs, MODES, SystemPrefs};
use crate::widgets::{
    caption, card, choice_row, rows, setting_row, setting_row_described, value_row,
};

pub const MODE_KEY: &str = "appearance.mode";
pub const DARK_THEME_KEY: &str = "appearance.dark_theme";
pub const LIGHT_THEME_KEY: &str = "appearance.light_theme";
pub const DENSITY_KEY: &str = "appearance.density";
pub const COLOUR_KEY: &str = "identity.colour";
pub const HIGH_CONTRAST_KEY: &str = "appearance.high_contrast";
pub const REDUCE_MOTION_KEY: &str = "appearance.reduce_motion";
pub const REDUCE_TRANSPARENCY_KEY: &str = "appearance.reduce_transparency";
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

/// The labels of the three appearance modes, in display order.
pub const MODE_LABELS: [&str; 3] = ["Light", "Dark", "Automatic"];
/// The labels of the dark themes a person can pick (the presets; their own
/// themes join the live list).
pub const DARK_THEME_NAMES: [&str; 4] = [
    ui::theme::Preset::Dark.name(),
    ui::theme::Preset::DarkCool.name(),
    ui::theme::Preset::DarkGraphite.name(),
    ui::theme::Preset::DarkSoft.name(),
];
/// The labels of the light themes a person can pick.
pub const LIGHT_THEME_NAMES: [&str; 1] = [ui::theme::Preset::Light.name()];
/// `(label, factor)` of the density steps.
pub const DENSITIES: [(&str, f64); 3] = [("Compact", 0.85), ("Regular", 1.0), ("Roomy", 1.2)];
/// The labels of the density steps, in display order.
pub const DENSITY_LABELS: [&str; 3] = [DENSITIES[0].0, DENSITIES[1].0, DENSITIES[2].0];

/// The label of a stored mode.
pub fn mode_label(value: &str) -> String {
    MODES
        .iter()
        .find(|(stored, _)| *stored == value)
        .map_or_else(|| t(MODES[1].1), |(_, key)| t(key))
}

/// The stored mode of a label (the English source label or the localised one).
pub fn mode_value(label: &str) -> Option<&'static str> {
    MODES
        .iter()
        .zip(MODE_LABELS)
        .find(|((_, key), english)| *english == label || t(key) == label)
        .map(|((stored, _), _)| *stored)
}

fn density_ftl(english: &str) -> &'static str {
    match english {
        "Compact" => "settings-appearance-density-compact",
        "Roomy" => "settings-appearance-density-roomy",
        _ => "settings-appearance-density-regular",
    }
}

/// The label of the density step nearest `factor`.
pub fn density_label(factor: f64) -> String {
    let english = DENSITIES
        .iter()
        .min_by(|(_, a), (_, b)| (a - factor).abs().total_cmp(&(b - factor).abs()))
        .map_or("Regular", |(label, _)| *label);
    t(density_ftl(english))
}

/// The factor of a density label (the English source label or the localised
/// one).
pub fn density_factor(label: &str) -> Option<f64> {
    DENSITIES
        .iter()
        .find(|(english, _)| *english == label || t(density_ftl(english)) == label)
        .map(|(_, factor)| *factor)
}

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

/// A swatch picker that reports the person's choice through a local state,
/// so the pane (which rebuilds its composition every layout) can read it.
struct ColourChoice {
    picker: ColorSwatchPicker,
    request: ui::LocalState<Option<usize>>,
}

impl ui::widget::Decorator for ColourChoice {
    type Inner = ColorSwatchPicker;

    fn inner(&self) -> &ColorSwatchPicker {
        &self.picker
    }

    fn inner_mut(&mut self) -> &mut ColorSwatchPicker {
        &mut self.picker
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        Widget::handle_event(&mut self.picker, event, ctx);
        if let Some(choice) = self.picker.take_choice() {
            self.request.set(Some(choice));
        }
    }
}

pub struct AppearancePane {
    prefs: AppearancePrefs,
    /// The presence colour the person chose (`identity.colour`), or `None`
    /// while it is still the colour derived from their identity.
    colour: Option<PresenceHue>,
    needs_seed: bool,
    /// The wallpaper file chooser in flight.
    wallpaper_pick: Option<ui::portal::OpenFileChannel>,
    /// The Wallpaper portal request in flight (the desktop previews and asks).
    wallpaper_apply: Option<ui::portal::WallpaperChannel>,
    /// What happened to the last wallpaper request, shown under the button.
    wallpaper_status: Option<String>,
    structure_dirty: bool,
    commit_context_pending: bool,
    /// A segmented control's choice (`key`, label), waiting for `sync` to
    /// write it into the control's draft. Segmented strips and the swatch
    /// grid have no document binding of their own.
    segment_request: ui::LocalState<Option<(&'static str, String)>>,
    colour_request: ui::LocalState<Option<usize>>,
    /// The only writer for durable appearance values. Controls edit a local
    /// draft key and this host dispatches the same action exported by Settings'
    /// resource tree; a click, keyboard activation, Ctrl+K, replay, and undo
    /// therefore all share one canonical mutation path.
    actions: crate::resource::ActionSink,
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
        let prefs = SystemPrefs::load();
        let colour = prefs.identity_colour();
        Self::with_prefs(prefs.appearance).with_colour(colour)
    }

    /// Construct from explicit preferences — used by tests and the
    /// deterministic `--shots` render.
    pub fn with_prefs(prefs: AppearancePrefs) -> Self {
        Self {
            prefs,
            colour: None,
            needs_seed: true,
            wallpaper_pick: None,
            wallpaper_apply: None,
            wallpaper_status: None,
            structure_dirty: false,
            commit_context_pending: false,
            segment_request: ui::LocalState::new(None),
            colour_request: ui::LocalState::new(None),
            actions: crate::resource::ActionSink::default(),
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// The person's chosen presence colour, if they have chosen one.
    #[must_use]
    pub fn with_colour(mut self, colour: Option<PresenceHue>) -> Self {
        self.colour = colour;
        self
    }

    pub fn prefs(&self) -> &AppearancePrefs {
        &self.prefs
    }

    /// The chosen presence colour, if any.
    pub fn colour(&self) -> Option<PresenceHue> {
        self.colour
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one composition holds every appearance control in section order"
    )]
    fn build_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let mode_labels: Vec<String> = MODES.iter().map(|(_, key)| t(key)).collect();
        // The control shows the appearance the window is actually in: the
        // stored mode, except that a polarity the window has not caught up
        // with (a setting changed under it) follows the window.
        let shown_mode = match self.prefs.mode.as_str() {
            "automatic" => "automatic",
            _ if theme.document.is_dark() => "dark",
            _ => "light",
        };
        let mode_index = MODES
            .iter()
            .position(|(stored, _)| *stored == shown_mode)
            .unwrap_or(1);
        let mode_view: Vec<&str> = mode_labels.iter().map(String::as_str).collect();
        let mode = {
            let request = self.segment_request.clone();
            let segments = mode_view.iter().map(|label| Segment::new(*label)).collect();
            Box::new(
                ToggleGroup::new(segments, mode_index)
                    .with_id("appearance.mode")
                    .on_change(move |index| {
                        if let Some(label) = MODE_LABELS.get(index) {
                            request.set(Some((MODE_KEY, (*label).to_string())));
                        }
                    }),
            ) as Box<dyn Widget>
        };

        let theme_dropdown = |dark: bool, key: &str, id: &str, current: &str| {
            let options: Vec<String> = self
                .prefs
                .theme_choices(dark)
                .into_iter()
                .map(|choice| choice.name)
                .collect();
            Box::new(
                Dropdown::new(options, &Self::draft_key(key))
                    .with_id(id)
                    .with_placeholder(current),
            ) as Box<dyn Widget>
        };
        let dark_current = self.prefs.theme_name(&self.prefs.dark_theme);
        let light_current = self.prefs.theme_name(&self.prefs.light_theme);

        let theme_rows = vec![
            setting_row(&t("settings-appearance-mode"), mode),
            choice_row(
                &t("settings-appearance-dark-theme"),
                theme_dropdown(true, DARK_THEME_KEY, "appearance.dark_theme", &dark_current),
            ),
            choice_row(
                &t("settings-appearance-light-theme"),
                theme_dropdown(
                    false,
                    LIGHT_THEME_KEY,
                    "appearance.light_theme",
                    &light_current,
                ),
            ),
        ];
        let theme_card = card(&t("settings-appearance-theme-card"), rows(theme_rows));

        let swatches: Vec<ColorSwatch> = PresenceHue::ALL
            .iter()
            .map(|hue| ColorSwatch::named(theme.tokens.colors.presence.hue(*hue), hue.label()))
            .collect();
        let chosen = self.colour.unwrap_or(theme.tokens.colors.you);
        let picker = ColourChoice {
            picker: ColorSwatchPicker::new(swatches, 6).with_selected(chosen.slot()),
            request: self.colour_request.clone(),
        };
        let colour_card = card(
            &t("settings-appearance-colour-card"),
            rows(vec![Box::new(picker)]),
        );

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

        let density_labels: Vec<String> = DENSITY_LABELS
            .iter()
            .map(|english| t(density_ftl(english)))
            .collect();
        let density_view: Vec<&str> = density_labels.iter().map(String::as_str).collect();
        let density_index = DENSITIES
            .iter()
            .position(|(_, factor)| (factor - self.prefs.density).abs() < 0.05)
            .unwrap_or(1);
        let density = {
            let request = self.segment_request.clone();
            let segments = density_view
                .iter()
                .map(|label| Segment::new(*label))
                .collect();
            Box::new(
                ToggleGroup::new(segments, density_index)
                    .with_id("appearance.density")
                    .on_change(move |index| {
                        if let Some(label) = DENSITY_LABELS.get(index) {
                            request.set(Some((DENSITY_KEY, (*label).to_string())));
                        }
                    }),
            ) as Box<dyn Widget>
        };

        let size_card = card(
            &t("settings-appearance-size-card"),
            rows(vec![
                setting_row(&t("settings-appearance-density"), density),
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
                    &t("settings-appearance-reduce-transparency"),
                    &t("settings-appearance-reduce-transparency-desc"),
                    Box::new(
                        Toggle::new(
                            "settings.appearance.reduce_transparency",
                            "appearance.reduce_transparency.edit",
                        )
                        .with_id("appearance.reduce_transparency"),
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

        let mut sections = vec![theme_card];
        if self.prefs.mode == "automatic" {
            sections.push(self.schedule_card());
        }
        sections.extend([colour_card, size_card, access_card, self.wallpaper_card()]);
        crate::widgets::cards(sections)
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
                    .push(Box::new(Button::new(
                        "appearance.wallpaper.choose",
                        &t("settings-appearance-choose-picture"),
                        Some(WALLPAPER_CHOOSE_KEY),
                    )))
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
        if schedule.config.follow_sun && schedule.location.is_none() {
            body.push(caption(&t("settings-appearance-schedule-no-location")));
        }
        card(&t("settings-appearance-day-night-card"), rows(body))
    }

    /// Open the chooser when the button asked for it, and carry a chosen
    /// picture through the Wallpaper portal.
    fn sync_wallpaper(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, WALLPAPER_CHOOSE_KEY).unwrap_or(false) {
            self.dispatch(state, &set_bool_action(WALLPAPER_CHOOSE_KEY, false));
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

    /// Write the loaded preferences into the document so every control rests
    /// on the person's current choice.
    fn seed(&mut self, state: &mut AutoCommit) {
        let prefs = self.prefs.clone();
        self.dispatch(state, &set_text_action(MODE_KEY, mode_label(&prefs.mode)));
        self.dispatch(
            state,
            &set_text_action(DARK_THEME_KEY, prefs.theme_name(&prefs.dark_theme)),
        );
        self.dispatch(
            state,
            &set_text_action(LIGHT_THEME_KEY, prefs.theme_name(&prefs.light_theme)),
        );
        self.dispatch(
            state,
            &set_text_action(DENSITY_KEY, density_label(prefs.density)),
        );
        self.dispatch(
            state,
            &set_text_action(
                COLOUR_KEY,
                self.colour.map_or("", PresenceHue::label).to_string(),
            ),
        );
        self.dispatch(
            state,
            &set_text_action(TEXT_SIZE_KEY, text_size_label(prefs.text_scale)),
        );
        self.dispatch(
            state,
            &set_text_action(UI_SCALE_KEY, ui_scale_label(prefs.ui_scale)),
        );
        self.dispatch(
            state,
            &set_bool_action(HIGH_CONTRAST_KEY, prefs.high_contrast),
        );
        self.dispatch(
            state,
            &set_bool_action(REDUCE_TRANSPARENCY_KEY, prefs.reduce_transparency),
        );
        self.dispatch(
            state,
            &set_bool_action(REDUCE_MOTION_KEY, prefs.reduce_motion),
        );
        self.dispatch(
            state,
            &set_text_action(SCHEDULE_KEY, schedule_label(&prefs.schedule)),
        );
        self.dispatch(state, &set_text_action(SUNRISE_KEY, prefs.sunrise.clone()));
        self.dispatch(state, &set_text_action(SUNSET_KEY, prefs.sunset.clone()));
        self.dispatch(
            state,
            &set_text_action(WALLPAPER_SCOPE_KEY, scope_label(&prefs.wallpaper_scope)),
        );
        self.dispatch(
            state,
            &set_text_action(WALLPAPER_TARGET_KEY, target_label(&prefs.wallpaper_target)),
        );
        self.dispatch(state, &set_bool_action(WALLPAPER_CHOOSE_KEY, false));
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
        p.set_identity_colour(self.colour);
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

    pub(crate) fn bind_dispatcher(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Settings>,
    ) {
        self.actions.attach(dispatcher);
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

    #[expect(
        clippy::too_many_lines,
        reason = "each appearance control reconciles its draft in turn"
    )]
    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        if self.needs_seed {
            self.seed(state);
            self.needs_seed = false;
        }
        self.sync_wallpaper(state, root);

        // A segmented control or the swatch grid chose something: write it
        // into the draft, and the reconcile below dispatches the action.
        if let Some((key, label)) = self.segment_request.with(Clone::clone) {
            self.segment_request.set(None);
            let _ = state.put(root, Self::draft_key(key), label.as_str());
        }
        if let Some(index) = self.colour_request.with(|request| *request) {
            self.colour_request.set(None);
            let _ = state.put(
                root,
                Self::draft_key(COLOUR_KEY),
                PresenceHue::from_slot(index).label(),
            );
        }

        let mut changed = false;

        let current = mode_label(&self.prefs.mode);
        let (label, mode_changed) = self.reconcile_text(state, root, MODE_KEY, &current);
        changed |= mode_changed;
        if let Some(mode) = mode_value(&label)
            && mode != self.prefs.mode
        {
            self.prefs.mode = mode.to_string();
            self.structure_dirty = true;
        }

        for (key, dark) in [(DARK_THEME_KEY, true), (LIGHT_THEME_KEY, false)] {
            let stored = if dark {
                self.prefs.dark_theme.clone()
            } else {
                self.prefs.light_theme.clone()
            };
            let current = self.prefs.theme_name(&stored);
            let (name, theme_changed) = self.reconcile_text(state, root, key, &current);
            changed |= theme_changed;
            if let Some(choice) = self
                .prefs
                .theme_choices(dark)
                .into_iter()
                .find(|choice| choice.name == name)
                && choice.reference != stored
            {
                if dark {
                    self.prefs.dark_theme = choice.reference;
                } else {
                    self.prefs.light_theme = choice.reference;
                }
            }
        }

        let current = density_label(self.prefs.density);
        let (label, density_changed) = self.reconcile_text(state, root, DENSITY_KEY, &current);
        changed |= density_changed;
        if let Some(factor) = density_factor(&label)
            && (factor - self.prefs.density).abs() > f64::EPSILON
        {
            self.prefs.density = factor;
        }

        let current = self.colour.map_or("", PresenceHue::label).to_string();
        let (label, colour_changed) = self.reconcile_text(state, root, COLOUR_KEY, &current);
        changed |= colour_changed;
        if let Some(hue) = PresenceHue::ALL.iter().find(|hue| hue.label() == label)
            && self.colour != Some(*hue)
        {
            self.colour = Some(*hue);
            self.structure_dirty = true;
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
        let (reduce_transparency, transparency_changed) = self.reconcile_bool(
            state,
            root,
            REDUCE_TRANSPARENCY_KEY,
            self.prefs.reduce_transparency,
        );
        changed |= transparency_changed;
        if reduce_transparency != self.prefs.reduce_transparency {
            self.prefs.reduce_transparency = reduce_transparency;
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
        let mut composed = self.build_composition(theme);
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

    /// The pane seeds its prefs into the document, resolves them to the real
    /// theme, and builds a layout.
    #[test]
    fn appearance_pane_seeds_prefs_and_resolves_its_theme() {
        let mut pane = AppearancePane::with_prefs(AppearancePrefs {
            mode: "light".into(),
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
            read_str_or_text(&doc, &root, MODE_KEY).as_deref(),
            Some("Light")
        );
        assert_eq!(
            read_str_or_text(&doc, &root, DARK_THEME_KEY).as_deref(),
            Some("Dark")
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
            mode: "light".into(),
            high_contrast: false,
            reduce_motion: true,
            text_scale: 1.15,
            ui_scale: 1.25,
            ..AppearancePrefs::default()
        });
        let resolved = pane.prefs().resolve_theme();
        assert_eq!(resolved.name, "Light");
        assert_eq!(resolved.ui_scale, 1.25);
        assert_eq!(resolved.text_scale, 1.15);
        assert!(resolved.reduce_motion);

        let mut pane = build();
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }

    /// The theme dropdowns show the theme the stored reference names, not a
    /// fixed default, whichever mode is active.
    #[test]
    fn theme_choices_reflect_the_stored_themes() {
        let prefs = AppearancePrefs {
            mode: "light".into(),
            dark_theme: "dark-soft".into(),
            ..AppearancePrefs::default()
        };
        assert_eq!(prefs.theme_name(&prefs.dark_theme), "Dark soft");
        assert_eq!(prefs.theme_name(&prefs.light_theme), "Light");
        assert_eq!(prefs.resolve_theme().name, "Light");
        let dark: Vec<String> = prefs
            .theme_choices(true)
            .into_iter()
            .map(|choice| choice.name)
            .collect();
        assert_eq!(dark, DARK_THEME_NAMES);
        let light: Vec<String> = prefs
            .theme_choices(false)
            .into_iter()
            .map(|choice| choice.name)
            .collect();
        assert_eq!(light, LIGHT_THEME_NAMES);
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

        let _ = doc.put(&root, MODE_KEY, "Automatic");
        let _ = doc.put(&root, SCHEDULE_KEY, "Custom times");
        let _ = doc.put(&root, SUNRISE_KEY, "06:30");
        let _ = doc.put(&root, SUNSET_KEY, "21:00");
        let _ = doc.put(&root, WALLPAPER_SCOPE_KEY, "Every workspace");
        pane.sync(&mut doc, &root);
        assert_eq!(pane.prefs().mode, "automatic");
        assert_eq!(pane.prefs().schedule, "custom");

        let saved = SystemPrefs::load().appearance;
        assert_eq!(saved.mode, "automatic");
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
        assert_eq!(
            section.get("mode").and_then(|v| v.as_str()),
            Some("automatic")
        );

        // A fresh pane shows the saved choice, not the defaults.
        let mut reopened = AppearancePane::new();
        let mut doc = AutoCommit::new();
        reopened.sync(&mut doc, &root);
        assert_eq!(
            read_str_or_text(&doc, &root, SCHEDULE_KEY).as_deref(),
            Some("Custom times")
        );
        assert_eq!(
            read_str_or_text(&doc, &root, MODE_KEY).as_deref(),
            Some("Automatic")
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

    /// Dropdown, segmented and toggle edits in the document update the
    /// prefs, which resolve to the real theme; unknown values are ignored.
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
        let pane = pane_after_edits(&[
            (DARK_THEME_KEY, Str("Dark graphite".into())),
            (DENSITY_KEY, Str("Compact".into())),
        ]);
        assert_eq!(pane.prefs().dark_theme, "dark-graphite");
        assert_eq!(pane.prefs().density, 0.85);
        assert_eq!(pane.prefs().resolve_theme().name, "Dark graphite");

        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let pane = pane_after_edits(&[
            (MODE_KEY, Str("Light".into())),
            (TEXT_SIZE_KEY, Str("Larger".into())),
            (HIGH_CONTRAST_KEY, Boolean(true)),
            (REDUCE_MOTION_KEY, Boolean(true)),
            (REDUCE_TRANSPARENCY_KEY, Boolean(true)),
        ]);
        assert_eq!(pane.prefs().mode, "light");
        assert_eq!(pane.prefs().text_scale, 1.30);
        assert!(pane.prefs().high_contrast);
        assert!(pane.prefs().reduce_motion);
        assert!(pane.prefs().reduce_transparency);
        // high_contrast wins the resolved palette, keeping the mode's polarity.
        assert_eq!(pane.prefs().resolve_theme().name, "High contrast light");

        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let pane = pane_after_edits(&[(DARK_THEME_KEY, Str("not-a-theme".into()))]);
        assert_eq!(pane.prefs().dark_theme, "dark", "stayed on the default");
    }

    #[test]
    fn the_chosen_presence_colour_is_written_to_the_identity_section() {
        use automerge::ScalarValue::Str;
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let pane = pane_after_edits(&[(COLOUR_KEY, Str("Violet".into()))]);
        assert_eq!(pane.colour(), Some(PresenceHue::Violet));
        let path = settings_store::settings_path().expect("test config dir");
        let identity = settings_store::load_section_at(&path, "identity");
        assert_eq!(
            identity.get("colour").and_then(|value| value.as_str()),
            Some("violet")
        );
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
        assert_eq!(density_factor("Roomy"), Some(1.2));
        assert_eq!(density_label(0.86), "Compact");
        assert_eq!(mode_value("Automatic"), Some("automatic"));
        assert_eq!(mode_label("light"), "Light");
    }
}
