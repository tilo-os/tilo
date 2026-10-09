//! Appearance pane — the editor for the **shared appearance preferences**.
//!
//! This panel edits [`crate::prefs::AppearancePrefs`], the `appearance`
//! section of the person's settings. The OS theming layer resolves those
//! preferences into the concrete [`ui::Theme`] every app draws against (see
//! [`crate::prefs::AppearancePrefs::resolve_theme`]) — so choosing a theme
//! here is the single source of truth the rest of the system follows,
//! exactly like Region & Formats owns the locale.
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
//! A control that sets one field is bound to that field of the section, the
//! others to an [`AppearanceEffect`]; either writes the section, and the pane
//! shows what the section holds, whoever changed it.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::theme::Theme;
use ui::tilo_resource;
use ui::widget::{AppModel, Button, Dropdown, ModelPath, Row, Spacer, Toggle, Widget};
use vello::kurbo::Rect;

use crate::effects::effect;
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::prefs::AppearancePrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{caption, card, choice_row, rows, setting_row_described, value_row};

/// `(label, persisted value)` of each day/night schedule.
pub const SCHEDULES: [(&str, &str); 2] = [("Sunset to sunrise", "sun"), ("Custom times", "custom")];
/// `(label, persisted value)` of which workspaces a new wallpaper covers.
pub const WALLPAPER_SCOPES: [(&str, &str); 2] =
    [("This workspace", "workspace"), ("Every workspace", "all")];
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

pub fn target_label(value: &str) -> String {
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
pub(crate) fn ui_scale_label(scale: f64) -> &'static str {
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

/// The persisted values of a `(label, value)` table, as an effect's choices.
fn values(table: &'static [(&'static str, &'static str)]) -> impl Iterator<Item = &'static str> {
    table.iter().map(|(_, value)| *value)
}

/// The English text-size names an effect takes.
pub const TEXT_SIZE_NAMES: [&str; 4] = ["Small", "Default", "Large", "Larger"];

/// The UI-scale steps an effect takes.
pub const UI_SCALE_NAMES: [&str; 5] = ["90%", "100%", "110%", "125%", "150%"];

/// The English name of a text size shown in either language.
pub fn text_size_name(shown: &str) -> String {
    TEXT_SIZES
        .iter()
        .find(|(english, _)| *english == shown || t(text_size_ftl(english)) == shown)
        .map_or_else(|| shown.to_owned(), |(english, _)| (*english).to_owned())
}

/// The person's current appearance selectors and controls, before the OS
/// resolves Automatic mode. Theme documents and their private content are not
/// part of this read surface.
#[derive(serde::Serialize)]
struct AppearanceInspection<'a> {
    schema: &'static str,
    theme: &'a str,
    mode: &'a str,
    dark_theme: &'a str,
    light_theme: &'a str,
    density: f64,
    ui_scale: f64,
    text_scale: f64,
    high_contrast: bool,
    reduce_motion: bool,
    schedule: &'a str,
    sunrise: &'a str,
    sunset: &'a str,
    wallpaper_scope: &'a str,
    wallpaper_target: &'a str,
}

impl<'a> From<&'a AppearancePrefs> for AppearanceInspection<'a> {
    fn from(prefs: &'a AppearancePrefs) -> Self {
        Self {
            schema: "tilo.settings.appearance.v1",
            theme: &prefs.theme,
            mode: &prefs.mode,
            dark_theme: &prefs.dark_theme,
            light_theme: &prefs.light_theme,
            density: prefs.density,
            ui_scale: prefs.ui_scale,
            text_scale: prefs.text_scale,
            high_contrast: prefs.high_contrast,
            reduce_motion: prefs.reduce_motion,
            schedule: &prefs.schedule,
            sunrise: &prefs.sunrise,
            sunset: &prefs.sunset,
            wallpaper_scope: &prefs.wallpaper_scope,
            wallpaper_target: &prefs.wallpaper_target,
        }
    }
}

/// The look of the desktop and every app: theme, scale, contrast, motion,
/// the day-and-night schedule and the wallpaper.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum AppearanceEffect {
    /// Read the person's current selectors and schedule without resolving
    /// Automatic mode or changing the OS-controlled theme.
    #[effect(
        view,
        returns,
        device_only,
        kind = "read",
        label = "Inspect appearance preferences"
    )]
    InspectAppearance,
    /// Set the theme every app draws with.
    #[effect(idempotent)]
    SetTheme {
        #[param("The theme; Automatic is light by day and dark by night", choices = AppearancePrefs::THEMES.iter().copied())]
        theme: String,
    },
    /// Set the interface scale.
    #[effect(idempotent)]
    SetUiScale {
        #[param("How large the whole interface is drawn", choices = UI_SCALE_NAMES)]
        scale: String,
    },
    /// Set the text size.
    #[effect(idempotent)]
    SetTextSize {
        #[param("How large text is drawn", choices = TEXT_SIZE_NAMES)]
        size: String,
    },
    /// Set when day and night begin.
    #[effect(idempotent)]
    SetDayNightSchedule {
        #[param("`sun`: at sunrise and sunset; `custom`: at the day and night times", choices = values(&SCHEDULES))]
        schedule: String,
    },
    /// Set which workspaces a new wallpaper covers.
    #[effect(idempotent)]
    SetWallpaperScope {
        #[param("`workspace`: this workspace; `all`: every workspace", choices = values(&WALLPAPER_SCOPES))]
        scope: String,
    },
    /// Set where a new wallpaper shows.
    #[effect(idempotent)]
    SetWallpaperTarget {
        #[param("The desktop, the lock screen or both", choices = values(&WALLPAPER_TARGETS))]
        target: String,
    },
    /// Choose a picture and make it the wallpaper.
    #[effect(job)]
    ChooseWallpaper,
}

/// Answer `effect`.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: AppearanceEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let host = app.host();
    let pick = |table: &[(&str, &'static str)], value: &str, what: &str| {
        table
            .iter()
            .find(|(_, known)| *known == value)
            .map(|(_, known)| (*known).to_owned())
            .ok_or_else(|| format!("`{value}` is not a {what}"))
    };
    match effect {
        AppearanceEffect::InspectAppearance => {
            let prefs = host.settings::<AppearancePrefs>().get();
            serde_json::to_value(AppearanceInspection::from(&prefs))
                .map(Effected::value)
                .map_err(|error| format!("the appearance preferences could not be read: {error}"))
        }
        AppearanceEffect::SetTheme { theme } => {
            if !AppearancePrefs::THEMES.contains(&theme.as_str()) {
                return Err(format!("`{theme}` is not a theme"));
            }
            host.update(|prefs: &mut AppearancePrefs| prefs.choose_legacy_theme(theme))
        }
        AppearanceEffect::SetUiScale { scale } => {
            let factor =
                ui_scale_factor(&scale).ok_or_else(|| format!("`{scale}` is not a UI scale"))?;
            host.update(|prefs: &mut AppearancePrefs| prefs.ui_scale = factor)
        }
        AppearanceEffect::SetTextSize { size } => {
            let scale =
                text_size_scale(&size).ok_or_else(|| format!("`{size}` is not a text size"))?;
            host.update(|prefs: &mut AppearancePrefs| prefs.text_scale = scale)
        }
        AppearanceEffect::SetDayNightSchedule { schedule } => {
            let schedule = pick(&SCHEDULES, &schedule, "day and night schedule")?;
            host.update(|prefs: &mut AppearancePrefs| prefs.schedule = schedule)
        }
        AppearanceEffect::SetWallpaperScope { scope } => {
            let scope = pick(&WALLPAPER_SCOPES, &scope, "wallpaper scope")?;
            host.update(|prefs: &mut AppearancePrefs| prefs.wallpaper_scope = scope)
        }
        AppearanceEffect::SetWallpaperTarget { target } => {
            let target = pick(&WALLPAPER_TARGETS, &target, "wallpaper target")?;
            host.update(|prefs: &mut AppearancePrefs| prefs.wallpaper_target = target)
        }
        AppearanceEffect::ChooseWallpaper => choose_wallpaper(app, cx),
    }
}

/// Ask for a picture through the file-chooser portal, then hand it to the
/// Wallpaper portal, which previews it and asks before applying it. The
/// pane shows where the request stands.
fn choose_wallpaper(app: &SettingsApp, cx: &mut EffectCx<Settings, SettingsApp>) -> EffectAnswer {
    if !app.host().reaches_devices() {
        return Err("only a window someone uses opens the file chooser".into());
    }
    let target = match app
        .host()
        .settings::<AppearancePrefs>()
        .get()
        .wallpaper_target
        .as_str()
    {
        "lockscreen" => ui::portal::WallpaperTarget::Lockscreen,
        "both" => ui::portal::WallpaperTarget::Both,
        _ => ui::portal::WallpaperTarget::Background,
    };
    let status = app.wallpaper_status().writer();
    let title = t("settings-appearance-choose-wallpaper");
    let accept = t("settings-appearance-choose");
    let pictures = t("settings-appearance-pictures");
    let confirm = t("settings-appearance-confirm-preview");
    Ok(cx.thread(
        move |_| {
            let pick = ui::portal::PickRequest::open(&title, &accept)
                .filter(
                    ui::portal::FileFilter::new(&pictures)
                        .mimetype("image/png")
                        .mimetype("image/jpeg")
                        .mimetype("image/webp"),
                )
                .thread_name("settings-wallpaper")
                .send_open_file();
            let file = match pick.recv() {
                Ok(ui::portal::OpenFileOutcome::Opened(file)) => file,
                Ok(ui::portal::OpenFileOutcome::Cancelled) | Err(_) => return Ok(None),
                Ok(ui::portal::OpenFileOutcome::Failed(error)) => {
                    return Err(t_args(
                        "settings-appearance-open-failed",
                        &[("error", error.into())],
                    ));
                }
            };
            status.set(Some(confirm));
            match ui::portal::send_set_wallpaper(file.into_file(), target).recv() {
                Ok(Ok(())) => Ok(Some(t("settings-appearance-updated"))),
                Ok(Err(error)) => Err(t_args(
                    "settings-appearance-unchanged",
                    &[("error", error.into())],
                )),
                Err(_) => Err(t_args(
                    "settings-appearance-unchanged",
                    &[("error", "the portal went away".into())],
                )),
            }
        },
        |app: &mut SettingsApp, outcome: Result<Option<String>, String>, _| match outcome {
            Ok(None) => {
                app.wallpaper_status().set(None);
                Ok(Effected::done())
            }
            Ok(Some(done)) => {
                app.wallpaper_status().set(Some(done));
                Ok(Effected::done())
            }
            Err(error) => {
                app.wallpaper_status().set(Some(error.clone()));
                Err(error)
            }
        },
    ))
}

/// What the pane's controls show: the stored preferences, as labels.
struct Shown {
    theme: LocalState<String>,
    ui_scale: LocalState<String>,
    text_size: LocalState<String>,
    high_contrast: LocalState<bool>,
    reduce_motion: LocalState<bool>,
    schedule: LocalState<String>,
    sunrise: LocalState<String>,
    sunset: LocalState<String>,
    wallpaper_scope: LocalState<String>,
    wallpaper_target: LocalState<String>,
}

impl Shown {
    fn of(prefs: &AppearancePrefs) -> Self {
        let shown = Self {
            theme: LocalState::new(String::new()),
            ui_scale: LocalState::new(String::new()),
            text_size: LocalState::new(String::new()),
            high_contrast: LocalState::new(false),
            reduce_motion: LocalState::new(false),
            schedule: LocalState::new(String::new()),
            sunrise: LocalState::new(String::new()),
            sunset: LocalState::new(String::new()),
            wallpaper_scope: LocalState::new(String::new()),
            wallpaper_target: LocalState::new(String::new()),
        };
        shown.show(prefs);
        shown
    }

    fn show(&self, prefs: &AppearancePrefs) {
        self.theme.set(prefs.theme.clone());
        self.ui_scale.set(ui_scale_label(prefs.ui_scale).to_owned());
        self.text_size.set(text_size_label(prefs.text_scale));
        self.high_contrast.set(prefs.high_contrast);
        self.reduce_motion.set(prefs.reduce_motion);
        self.schedule.set(schedule_label(&prefs.schedule));
        self.sunrise.set(prefs.sunrise.clone());
        self.sunset.set(prefs.sunset.clone());
        self.wallpaper_scope
            .set(scope_label(&prefs.wallpaper_scope));
        self.wallpaper_target
            .set(target_label(&prefs.wallpaper_target));
    }
}

pub struct AppearancePane {
    cx: PaneCx,
    prefs: Followed<AppearancePrefs>,
    shown: Shown,
    /// Where the last wallpaper request stands, written by its effect.
    wallpaper_status: LocalState<Option<String>>,
    status_shown: Option<String>,
    structure_dirty: bool,
    composed: ui::widget::Composed,
}

impl AppearancePane {
    /// The pane over `cx`'s appearance preferences, showing
    /// `wallpaper_status`.
    pub fn new(cx: PaneCx, wallpaper_status: LocalState<Option<String>>) -> Self {
        let prefs = cx.follow::<AppearancePrefs>();
        let shown = Shown::of(prefs.get());
        let status_shown = wallpaper_status.get();
        Self {
            cx,
            prefs,
            shown,
            wallpaper_status,
            status_shown,
            structure_dirty: false,
            composed: ui::widget::Composed::default(),
        }
    }

    pub fn prefs(&self) -> &AppearancePrefs {
        self.prefs.get()
    }

    fn dropdown(
        options: Vec<String>,
        state: &LocalState<String>,
        id: &str,
        bind: impl FnOnce(Dropdown) -> Dropdown,
    ) -> Box<dyn Widget> {
        let placeholder = state.get();
        Box::new(bind(
            Dropdown::from_state(options, state)
                .with_id(id)
                .with_placeholder(&placeholder),
        ))
    }

    fn toggle(
        &self,
        state: &LocalState<bool>,
        id: &str,
        field: ui::tilo_resource::Path<AppearancePrefs, bool>,
    ) -> Box<dyn Widget> {
        Box::new(
            self.cx
                .bind_field(Toggle::from_state(state).with_id(id), field),
        )
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let cx = &self.cx;
        let theme = Self::dropdown(
            AppearancePrefs::THEMES
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
            &self.shown.theme,
            "appearance.theme",
            |control| cx.bind(control, effect(AppearanceEffect::SET_THEME)),
        );
        let ui_scale = Self::dropdown(
            UI_SCALE_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            &self.shown.ui_scale,
            "appearance.ui_scale",
            |control| cx.bind(control, effect(AppearanceEffect::SET_UI_SCALE)),
        );
        let size = Self::dropdown(
            text_size_options(),
            &self.shown.text_size,
            "appearance.text_size",
            |control| {
                cx.bind(
                    control,
                    effect(AppearanceEffect::SET_TEXT_SIZE)
                        .map(|shown: String| text_size_name(&shown)),
                )
            },
        );

        let theme_card = card(
            &t("settings-appearance-theme-card"),
            rows(vec![
                choice_row(&t("settings-appearance-theme"), theme),
                choice_row(&t("settings-appearance-ui-scale"), ui_scale),
                choice_row(&t("settings-appearance-text-size"), size),
            ]),
        );

        let access_card = card(
            &t("settings-appearance-access-card"),
            rows(vec![
                setting_row_described(
                    &t("settings-appearance-high-contrast"),
                    &t("settings-appearance-high-contrast-desc"),
                    self.toggle(
                        &self.shown.high_contrast,
                        "appearance.high_contrast",
                        AppearancePrefs::paths().high_contrast,
                    ),
                ),
                setting_row_described(
                    &t("settings-appearance-reduce-motion"),
                    &t("settings-appearance-reduce-motion-desc"),
                    self.toggle(
                        &self.shown.reduce_motion,
                        "appearance.reduce_motion",
                        AppearancePrefs::paths().reduce_motion,
                    ),
                ),
            ]),
        );

        let resolved = self.prefs().resolve_theme();
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

    fn wallpaper_card(&self) -> Box<dyn Widget> {
        let cx = &self.cx;
        let mut body = vec![
            caption(&t("settings-appearance-wallpaper-body")),
            choice_row(
                &t("settings-appearance-use-for"),
                Self::dropdown(
                    WALLPAPER_SCOPES
                        .iter()
                        .map(|(_, value)| scope_label(value))
                        .collect(),
                    &self.shown.wallpaper_scope,
                    "appearance.wallpaper_scope",
                    |control| {
                        cx.bind(
                            control,
                            effect(AppearanceEffect::SET_WALLPAPER_SCOPE).map(|shown: String| {
                                value_for_shown(&WALLPAPER_SCOPES, &shown, scope_label)
                                    .unwrap_or_default()
                                    .to_owned()
                            }),
                        )
                    },
                ),
            ),
            choice_row(
                &t("settings-appearance-show-on"),
                Self::dropdown(
                    WALLPAPER_TARGETS
                        .iter()
                        .map(|(_, value)| target_label(value))
                        .collect(),
                    &self.shown.wallpaper_target,
                    "appearance.wallpaper_target",
                    |control| {
                        cx.bind(
                            control,
                            effect(AppearanceEffect::SET_WALLPAPER_TARGET).map(|shown: String| {
                                value_for_shown(&WALLPAPER_TARGETS, &shown, target_label)
                                    .unwrap_or_default()
                                    .to_owned()
                            }),
                        )
                    },
                ),
            ),
            Box::new(
                Row::new()
                    .push(Box::new(cx.bind_invocation(
                        Button::new(
                            "appearance.wallpaper.choose",
                            &t("settings-appearance-choose-picture"),
                        ),
                        AppearanceEffect::ChooseWallpaper,
                    )))
                    .push(Box::new(Spacer::new())),
            ),
        ];
        if let Some(status) = &self.status_shown {
            body.push(caption(status));
        }
        card(&t("settings-appearance-wallpaper-card"), rows(body))
    }

    fn schedule_card(&self) -> Box<dyn Widget> {
        let cx = &self.cx;
        let times = || {
            TIMES
                .iter()
                .map(|time| (*time).to_string())
                .collect::<Vec<_>>()
        };
        let mut body = vec![choice_row(
            &t("settings-appearance-schedule"),
            Self::dropdown(
                SCHEDULES
                    .iter()
                    .map(|(_, value)| schedule_label(value))
                    .collect(),
                &self.shown.schedule,
                "appearance.schedule",
                |control| {
                    cx.bind(
                        control,
                        effect(AppearanceEffect::SET_DAY_NIGHT_SCHEDULE).map(|shown: String| {
                            value_for_shown(&SCHEDULES, &shown, schedule_label)
                                .unwrap_or_default()
                                .to_owned()
                        }),
                    )
                },
            ),
        )];
        let schedule = self.prefs().schedule();
        if !schedule.config.follow_sun {
            body.push(choice_row(
                &t("settings-appearance-day-starts"),
                Self::dropdown(
                    times(),
                    &self.shown.sunrise,
                    "appearance.sunrise",
                    |control| cx.bind_field(control, AppearancePrefs::paths().sunrise),
                ),
            ));
            body.push(choice_row(
                &t("settings-appearance-night-starts"),
                Self::dropdown(
                    times(),
                    &self.shown.sunset,
                    "appearance.sunset",
                    |control| cx.bind_field(control, AppearancePrefs::paths().sunset),
                ),
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
}

impl Widget for AppearancePane {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if self.prefs.refresh() {
            self.shown.show(self.prefs.get());
            // The schedule's rows and the applied theme follow the values.
            self.structure_dirty = true;
        }
        let status = self.wallpaper_status.get();
        if status != self.status_shown {
            self.status_shown = status;
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

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        // This pane paints only its composition. Keep a child's local visual
        // change local instead of expanding it to the whole form.
        if let Some(composed) = self.composed.element() {
            composed.collect_damage(regions);
        }
    }

    fn name(&self) -> &'static str {
        "AppearancePane"
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self.composed.take_structure_dirty();
        own || child
    }

    /// Surface the user's current appearance choice so the running Settings
    /// window re-scales and re-themes live the instant a control changes. The
    /// host [`ui::App`] (with `appearance_reconcile` enabled by `main`) polls
    /// this each frame and applies any field that differs. We hand back the
    /// *base* colour theme unscaled — the App folds in high-contrast, the UI
    /// scale, and the text scale itself, keeping that bookkeeping in one place.
    fn appearance_intent(&self) -> Option<ui::theme::AppearanceIntent> {
        let prefs = self.prefs.get();
        Some(ui::theme::AppearanceIntent {
            base_theme: prefs.base_theme(),
            high_contrast: prefs.high_contrast,
            reduce_motion: prefs.reduce_motion,
            text_scale: prefs.text_scale as f32,
            ui_scale: prefs.ui_scale as f32,
        })
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    use crate::host::testing::{APPEARANCE, edit_field};

    fn set(app: &mut TestApp<SettingsShell>, effect: AppearanceEffect) {
        app.effect(&SettingsEffect::from(effect))
            .expect("the appearance effect is answered");
    }

    /// Every appearance control's effect writes the person's `appearance`
    /// section, where the compositor and every app read it; a value outside
    /// the offered ones is refused and changes nothing.
    #[test]
    fn appearance_effects_write_the_shared_section() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        set(
            &mut app,
            AppearanceEffect::SetTheme {
                theme: "Automatic".into(),
            },
        );
        set(
            &mut app,
            AppearanceEffect::SetUiScale {
                scale: "150%".into(),
            },
        );
        set(
            &mut app,
            AppearanceEffect::SetTextSize {
                size: "Large".into(),
            },
        );
        edit_field(&mut app, APPEARANCE, "high_contrast", true).unwrap();
        set(
            &mut app,
            AppearanceEffect::SetDayNightSchedule {
                schedule: "custom".into(),
            },
        );
        edit_field(&mut app, APPEARANCE, "sunrise", "06:30").unwrap();
        edit_field(&mut app, APPEARANCE, "sunset", "21:00").unwrap();
        set(
            &mut app,
            AppearanceEffect::SetWallpaperScope {
                scope: "all".into(),
            },
        );

        let settings = app.settings::<AppearancePrefs>();
        let prefs = settings.get();
        assert_eq!(prefs.theme, "Automatic");
        assert_eq!(prefs.ui_scale, 1.50);
        assert_eq!(prefs.text_scale, 1.15);
        assert!(prefs.high_contrast);
        assert_eq!(prefs.wallpaper_scope, "all");
        let resolved = prefs.resolve_theme();
        assert_eq!(resolved.ui_scale, 1.50);

        let section = settings.document().section("appearance");
        let config = settings_store::day_schedule::ScheduleConfig::from_section(&section);
        assert!(!config.follow_sun);
        assert_eq!(config.fixed.sunrise, 6 * 60 + 30);
        assert_eq!(config.fixed.sunset, 21 * 60);

        assert!(
            app.effect(&SettingsEffect::from(AppearanceEffect::SetTheme {
                theme: "Neon".into(),
            }))
            .is_err()
        );
        assert!(edit_field(&mut app, APPEARANCE, "sunrise", "25:00").is_err());
        assert_eq!(app.settings::<AppearancePrefs>().get(), prefs);
    }

    /// The pane shows the stored section, follows a change made elsewhere,
    /// and hands the toolkit the look the person chose.
    #[test]
    fn the_pane_follows_the_section_and_expresses_its_intent() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        cx.settings::<AppearancePrefs>()
            .set(&AppearancePrefs {
                theme: "Dark cool".into(),
                text_scale: 1.15,
                ui_scale: 1.25,
                ..AppearancePrefs::default()
            })
            .unwrap();
        let mut pane = AppearancePane::new(cx.clone(), LocalState::new(None));
        assert_eq!(pane.shown.theme.get(), "Dark cool");
        assert_eq!(pane.shown.text_size.get(), text_size_label(1.15));
        assert_eq!(pane.shown.ui_scale.get(), "125%");

        cx.settings::<AppearancePrefs>()
            .update(|prefs| {
                prefs.theme = "Dark graphite".into();
                prefs.reduce_motion = true;
            })
            .unwrap();
        let doc = ui::automerge::AutoCommit::new();
        pane.sync(&doc, &ui::automerge::ROOT);
        assert_eq!(pane.shown.theme.get(), "Dark graphite");
        assert!(pane.shown.reduce_motion.get());
        assert!(pane.take_structure_dirty());

        let intent = pane
            .appearance_intent()
            .expect("the appearance pane always expresses an intent");
        assert_eq!(intent.base_theme.name, "Dark graphite");
        assert_eq!(intent.ui_scale, 1.25);
        assert_eq!(intent.text_scale, 1.15);
        assert!(intent.reduce_motion);
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

        let profile = tempfile::tempdir().unwrap();
        let mut pane = AppearancePane::new(
            PaneCx::in_file(profile.path().join("settings.json")),
            LocalState::new(None),
        );
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        let ids: Vec<String> = node_map
            .values()
            .filter_map(|&widget| unsafe { (*widget).test_id() }.map(str::to_string))
            .collect();
        for id in ["appearance.wallpaper.choose", "appearance.wallpaper_scope"] {
            assert!(
                ids.iter().any(|candidate| candidate == id),
                "{id} missing from {ids:?}"
            );
        }
    }

    /// Without a person at the window nothing opens the file chooser.
    #[test]
    fn only_a_window_someone_uses_chooses_a_wallpaper() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        assert!(
            app.effect(&SettingsEffect::from(AppearanceEffect::ChooseWallpaper))
                .is_err()
        );
    }
}

#[cfg(test)]
#[path = "appearance/inspection_tests.rs"]
mod inspection_tests;
