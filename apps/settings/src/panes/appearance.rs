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
//! - **Wallpaper** — one of the pictures the system ships, shown the moment
//!   it is chosen, or a picture of the person's own, chosen through the
//!   file-chooser portal and applied through the Wallpaper portal, on this
//!   workspace or every one.
//! - **Theme** — a named preset ([`crate::prefs::AppearancePrefs::THEMES`]),
//!   or "Automatic": light by day, dark by night.
//! - **Day and night** — when "Automatic" and a scheduled Night Light switch:
//!   at sunset and sunrise, or at custom times.
//!
//! High contrast, reduce motion and text size are Accessibility's: they
//! live in that pane alone, where people look for them, and this page links
//! there with one row instead of a second copy of the controls.
//!
//! A control that sets one field is bound to that field of the section, the
//! others to an [`AppearanceEffect`]; either writes the section, and the pane
//! shows what the section holds, whoever changed it.

use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::tilo_resource;
use ui::widget::{AppModel, ModelPath, Widget};
use vello::kurbo::Rect;

use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::panes::keyboard_shortcuts::DesktopControls;
use crate::prefs::AppearancePrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{Item, SettingsSection, block, note, rows, section, value_row};

mod theme_tiles;
use theme_tiles::ThemeTiles;

crate::effects::effect_family!(AppearanceEffect => answer);

/// The shape of the wallpaper thumbnails: the laptop panel's 16:10.
const WALLPAPER_ASPECT: f64 = 16.0 / 10.0;

/// The shipped wallpapers whose thumbnails this system has installed, with
/// those thumbnails, decoded once per process.
fn installed_thumbnails() -> &'static [(
    &'static tilo_wallpapers::Wallpaper,
    vello::peniko::ImageData,
)] {
    static THUMBNAILS: std::sync::OnceLock<
        Vec<(
            &'static tilo_wallpapers::Wallpaper,
            vello::peniko::ImageData,
        )>,
    > = std::sync::OnceLock::new();
    THUMBNAILS.get_or_init(|| {
        tilo_wallpapers::all()
            .iter()
            .filter_map(|wallpaper| {
                let decoded = wallpaper.decode_thumbnail()?;
                Some((
                    wallpaper,
                    ui::raster::rgba8(decoded.rgba, decoded.width, decoded.height),
                ))
            })
            .collect()
    })
}

/// The persisted values of `table`, each `(label, value)`.
fn values(table: &[(&str, &str)]) -> impl Iterator<Item = String> {
    table.iter().map(|(_, value)| (*value).to_owned())
}

/// `(label, persisted value)` of each day/night schedule.
pub const SCHEDULES: [(&str, &str); 2] = [("Sunset to sunrise", "sun"), ("Custom times", "custom")];
pub use crate::prefs::{WALLPAPER_SCOPES, WALLPAPER_TARGETS};
/// The offered custom sunrise and sunset times.
pub const TIMES: [&str; 48] = [
    "00:00", "00:30", "01:00", "01:30", "02:00", "02:30", "03:00", "03:30", "04:00", "04:30",
    "05:00", "05:30", "06:00", "06:30", "07:00", "07:30", "08:00", "08:30", "09:00", "09:30",
    "10:00", "10:30", "11:00", "11:30", "12:00", "12:30", "13:00", "13:30", "14:00", "14:30",
    "15:00", "15:30", "16:00", "16:30", "17:00", "17:30", "18:00", "18:30", "19:00", "19:30",
    "20:00", "20:30", "21:00", "21:30", "22:00", "22:30", "23:00", "23:30",
];

/// The spacing densities offered, `(ftl key, density)`.
const DENSITIES: [(&str, f32); 3] = [
    (
        "settings-appearance-density-compact",
        ui::theme::ThemeDensity::COMPACT,
    ),
    (
        "settings-appearance-density-regular",
        ui::theme::ThemeDensity::REGULAR,
    ),
    (
        "settings-appearance-density-roomy",
        ui::theme::ThemeDensity::ROOMY,
    ),
];

/// The label of the offered density nearest `density`.
fn density_label(density: f64) -> String {
    DENSITIES
        .iter()
        .min_by(|a, b| {
            (f64::from(a.1) - density)
                .abs()
                .total_cmp(&(f64::from(b.1) - density).abs())
        })
        .map(|(key, _)| t(key))
        .unwrap_or_default()
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

/// The person's current appearance selectors and controls, before the OS
/// resolves Automatic mode. Theme documents and their private content are not
/// part of this read surface.
#[derive(serde::Serialize)]
struct AppearanceInspection<'a> {
    schema: &'static str,
    theme: String,
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
    /// The wallpaper the desktop shows, as `appearance.wallpaper` names it.
    wallpaper: &'a str,
}

impl<'a> From<&'a AppearancePrefs> for AppearanceInspection<'a> {
    fn from(prefs: &'a AppearancePrefs) -> Self {
        Self {
            schema: "tilo.settings.appearance.v1",
            theme: prefs.theme_choice(),
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
            wallpaper: tilo_wallpapers::choice(prefs.wallpaper.as_deref())
                .map_or("", tilo_wallpapers::value_of),
        }
    }
}

/// The look of the desktop and every app: theme, scale, contrast, motion,
/// the day-and-night schedule and the wallpaper.
#[derive(Clone, Copy, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum AppearanceEffect {
    /// Read the person's current selectors and schedule without resolving
    /// Automatic mode or changing the OS-controlled theme.
    #[effect(view, returns, kind = "read", label = "Inspect appearance preferences")]
    InspectAppearance,
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
    match effect {
        AppearanceEffect::InspectAppearance => {
            let prefs = host.settings::<AppearancePrefs>().get();
            serde_json::to_value(AppearanceInspection::from(&prefs))
                .map(Effected::value)
                .map_err(|error| format!("the appearance preferences could not be read: {error}"))
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
    Ok(crate::device_read::job(
        cx,
        move || {
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

pub struct AppearancePane {
    cx: PaneCx,
    prefs: Followed<AppearancePrefs>,
    /// The desktop's chrome preferences (the top bar), kept beside the
    /// shell's other desktop controls.
    desktop: Followed<DesktopControls>,
    /// The desk widgets' choices, edited on this pane too.
    desk: Followed<crate::prefs::DeskWidgetsPrefs>,
    /// Where the last wallpaper request stands, written by its effect.
    wallpaper_status: LocalState<Option<String>>,
    /// What the page was built showing ([`Shape`]).
    shape: Shape,
    composed: ui::widget::Composed,
}

/// What the page shows of the values other than through a control that
/// follows its field: the chosen theme's tile, the chosen wallpaper, the
/// schedule's rows, the desk widgets that are on and where the wallpaper
/// request stands. A change in any of them rebuilds the page; any other
/// change only moves its control.
#[derive(Clone, PartialEq)]
struct Shape {
    theme: String,
    wallpaper: Option<String>,
    schedule: [String; 3],
    desk: [bool; 4],
    status: Option<String>,
}

impl AppearancePane {
    fn shape(&self) -> Shape {
        let prefs = self.prefs();
        let desk = self.desk.get();
        Shape {
            theme: prefs.theme_choice(),
            wallpaper: prefs.wallpaper.clone(),
            schedule: [
                prefs.schedule.clone(),
                prefs.sunrise.clone(),
                prefs.sunset.clone(),
            ],
            desk: [
                desk.music.enabled,
                desk.team.enabled,
                desk.calendar.enabled,
                desk.system_monitor.enabled,
            ],
            status: self.wallpaper_status.get(),
        }
    }
}

impl AppearancePane {
    /// The pane over `cx`'s appearance preferences, showing
    /// `wallpaper_status`.
    pub fn new(cx: PaneCx, wallpaper_status: LocalState<Option<String>>) -> Self {
        let prefs = cx.follow::<AppearancePrefs>();
        let desktop = cx.follow::<DesktopControls>();
        let desk = cx.follow::<crate::prefs::DeskWidgetsPrefs>();
        let mut pane = Self {
            cx,
            prefs,
            desktop,
            desk,
            wallpaper_status,
            shape: Shape {
                theme: String::new(),
                wallpaper: None,
                schedule: Default::default(),
                desk: [false; 4],
                status: None,
            },
            composed: ui::widget::Composed::default(),
        };
        pane.shape = pane.shape();
        pane
    }

    pub fn prefs(&self) -> &AppearancePrefs {
        self.prefs.get()
    }

    /// The theme, the density and the interface scale, and the way to
    /// the accessibility options.
    fn theme_card(&self) -> SettingsSection {
        let cx = &self.cx;
        let paths = AppearancePrefs::paths();
        section(
            &t("settings-appearance-theme-card"),
            rows(vec![
                // Each theme drawn in its own colours, the chosen one ringed.
                block(Box::new(cx.bind_verb(
                    ThemeTiles::new(self.prefs().theme_choice()),
                    AppearancePrefs::PICK_THEME,
                ))),
                cx.row(t("settings-appearance-density"))
                    .described(t("settings-appearance-density-desc"))
                    .id("appearance.density")
                    .of(&self.prefs, paths.density)
                    .among(
                        DENSITIES.iter().map(|(_, density)| f64::from(*density)),
                        |density: &f64| density_label(*density),
                    ),
                cx.row(t("settings-appearance-ui-scale"))
                    .id("appearance.ui_scale")
                    .of(&self.prefs, paths.ui_scale)
                    .among(UI_SCALES.iter().map(|(_, scale)| *scale), |scale: &f64| {
                        ui_scale_label(*scale).to_owned()
                    }),
                // Contrast, motion and text size are Accessibility's alone.
                cx.row(t("settings-appearance-accessibility"))
                    .described(t("settings-appearance-accessibility-desc"))
                    .button(
                        "appearance.accessibility",
                        &t("settings-appearance-accessibility-open"),
                        crate::effects::open_pane(crate::categories::Category::Accessibility),
                    ),
            ]),
        )
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let theme_card = self.theme_card();
        let desktop_card = section(
            &t("settings-appearance-desktop-card"),
            rows(vec![hide_top_bar(&self.cx, &self.desktop)]),
        );

        crate::widgets::page(vec![
            theme_card,
            self.wallpaper_card(),
            self.schedule_card(),
            desktop_card,
            self.desk_widgets_card(),
        ])
    }

    fn wallpaper_card(&self) -> SettingsSection {
        let cx = &self.cx;
        let mut body: Vec<Item> = vec![
            block(Box::new(self.wallpaper_picker())),
            cx.row(t("settings-appearance-use-for"))
                .id("appearance.wallpaper_scope")
                .of(&self.prefs, AppearancePrefs::paths().wallpaper_scope)
                .among(values(&WALLPAPER_SCOPES), |scope: &String| {
                    scope_label(scope)
                }),
            cx.row(t("settings-appearance-show-on"))
                .id("appearance.wallpaper_target")
                .of(&self.prefs, AppearancePrefs::paths().wallpaper_target)
                .among(values(&WALLPAPER_TARGETS), |target: &String| {
                    target_label(target)
                }),
        ];
        if let Some(status) = &self.shape.status {
            body.push(note(status));
        }
        section(&t("settings-appearance-wallpaper-card"), rows(body))
    }

    /// The shipped pictures this system has installed, each choosing itself
    /// the moment it is pressed, then the person's own picture. The chosen
    /// one is marked.
    fn wallpaper_picker(&self) -> ui::widget::MediaGrid {
        self.wallpaper_picker_of(installed_thumbnails())
    }

    fn wallpaper_picker_of(
        &self,
        thumbnails: &[(
            &'static tilo_wallpapers::Wallpaper,
            vello::peniko::ImageData,
        )],
    ) -> ui::widget::MediaGrid {
        let chosen = tilo_wallpapers::choice(self.prefs().wallpaper.as_deref());
        let mut cards: Vec<ui::widget::MediaCard> = thumbnails
            .iter()
            .map(|(wallpaper, thumbnail)| {
                let id = wallpaper.id.as_str();
                let card = ui::widget::MediaCard::new(
                    format!("appearance.wallpaper.{id}"),
                    ui::widget::MediaSource::Image(thumbnail.clone()),
                )
                .aspect(ui::widget::MediaAspect::Ratio(WALLPAPER_ASPECT))
                .title(t(&format!("settings-wallpaper-{id}")))
                .meta(wallpaper.author.as_str())
                .selected(chosen == Some(tilo_wallpapers::Choice::Bundled(wallpaper)));
                self.cx
                    .bind_field_with(card, AppearancePrefs::paths().wallpaper, move |()| {
                        Ok(Some(id.to_owned()))
                    })
            })
            .collect();
        cards.push(
            self.cx.bind_invocation(
                ui::widget::MediaCard::new(
                    "appearance.wallpaper.choose",
                    ui::widget::MediaSource::Kind(ui::icons::Icon::Image),
                )
                .aspect(ui::widget::MediaAspect::Ratio(WALLPAPER_ASPECT))
                .title(t("settings-appearance-your-picture"))
                .selected(chosen == Some(tilo_wallpapers::Choice::Custom)),
                AppearanceEffect::ChooseWallpaper,
            ),
        );
        ui::widget::MediaGrid::new(cards).with_size(ui::widget::MediaSize::Small)
    }

    fn schedule_card(&self) -> SettingsSection {
        let cx = &self.cx;
        let paths = AppearancePrefs::paths();
        let times = || TIMES.iter().map(|time| (*time).to_owned());
        let mut body = vec![
            cx.row(t("settings-appearance-schedule"))
                .id("appearance.schedule")
                .of(&self.prefs, paths.schedule)
                .among(values(&SCHEDULES), |schedule: &String| {
                    schedule_label(schedule)
                }),
        ];
        let schedule = self.prefs().schedule();
        if !schedule.config.follow_sun {
            body.push(
                cx.row(t("settings-appearance-day-starts"))
                    .id("appearance.sunrise")
                    .of(&self.prefs, paths.sunrise)
                    .among(times(), String::clone),
            );
            body.push(
                cx.row(t("settings-appearance-night-starts"))
                    .id("appearance.sunset")
                    .of(&self.prefs, paths.sunset)
                    .among(times(), String::clone),
            );
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
        body.push(note(&t(
            if schedule.config.follow_sun && schedule.location.is_none() {
                "settings-appearance-schedule-no-location"
            } else {
                "settings-appearance-schedule-follow"
            },
        )));
        section(&t("settings-appearance-day-night-card"), rows(body))
    }
}

pub(crate) mod values;

pub(crate) fn hide_top_bar(cx: &PaneCx, desktop: &Followed<DesktopControls>) -> Item {
    cx.row(t("settings-appearance-hide-top-bar"))
        .described(t("settings-appearance-hide-top-bar-desc"))
        .id("appearance.hide_top_bar")
        .of(desktop, DesktopControls::paths().hide_top_bar)
        .toggle()
}

impl Widget for AppearancePane {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        // The controls follow their fields; the page is rebuilt only when
        // what it shows otherwise changes.
        self.prefs.refresh();
        self.desktop.refresh();
        self.desk.refresh();
        let shape = self.shape();
        if shape != self.shape {
            self.shape = shape;
            self.composed.invalidate();
        }
        self.composed.sync(state, root);
    }
    crate::panes::composed_pane!(kept);

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
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::prelude::TestApp;
    use ui::theme::Theme;

    use crate::host::testing::{APPEARANCE, edit_field, invoke_section};

    /// Every appearance control's effect writes the person's `appearance`
    /// section, where the compositor and every app read it; a value outside
    /// the offered ones is refused and changes nothing.
    #[test]
    fn appearance_effects_write_the_shared_section() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        invoke_section(
            &mut app,
            APPEARANCE,
            "choose-theme",
            serde_json::json!({ "theme": "Automatic" }),
        )
        .unwrap();
        crate::host::testing::edit_field(
            &mut app,
            crate::host::testing::APPEARANCE,
            "ui_scale",
            1.5,
        )
        .unwrap();
        crate::host::testing::edit_field(
            &mut app,
            crate::host::testing::APPEARANCE,
            "text_scale",
            1.15,
        )
        .unwrap();
        edit_field(&mut app, APPEARANCE, "high_contrast", true).unwrap();
        crate::host::testing::edit_field(
            &mut app,
            crate::host::testing::APPEARANCE,
            "schedule",
            "custom",
        )
        .unwrap();
        edit_field(&mut app, APPEARANCE, "sunrise", "06:30").unwrap();
        edit_field(&mut app, APPEARANCE, "sunset", "21:00").unwrap();
        crate::host::testing::edit_field(
            &mut app,
            crate::host::testing::APPEARANCE,
            "wallpaper_scope",
            "all",
        )
        .unwrap();

        let settings = app.settings::<AppearancePrefs>();
        let prefs = settings.get();
        assert_eq!(prefs.theme_choice(), "Automatic");
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
            invoke_section(
                &mut app,
                APPEARANCE,
                "choose-theme",
                serde_json::json!({ "theme": "Neon" })
            )
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
                mode: "dark".into(),
                dark_theme: "dark-cool".into(),
                text_scale: 1.15,
                ui_scale: 1.25,
                ..AppearancePrefs::default()
            })
            .unwrap();
        let mut pane = AppearancePane::new(cx.clone(), LocalState::new(None));
        let paths = AppearancePrefs::paths();
        assert_eq!(pane.prefs().theme_choice(), "Dark cool");
        let text_size = pane.prefs.shown(&paths.text_scale, |scale| {
            crate::panes::accessibility::text_size_label(*scale)
        });
        assert_eq!(
            text_size.get(),
            crate::panes::accessibility::text_size_label(1.15)
        );
        let ui_scale = pane
            .prefs
            .shown(&paths.ui_scale, |scale| ui_scale_label(*scale).to_owned());
        assert_eq!(ui_scale.get(), "125%");
        let reduce_motion = pane.prefs.shown(&paths.reduce_motion, bool::clone);

        cx.settings::<AppearancePrefs>()
            .update(|prefs| {
                prefs.choose_theme("Dark graphite");
                prefs.reduce_motion = true;
            })
            .unwrap();
        let doc = ui::automerge::AutoCommit::new();
        pane.sync(&doc, &ui::automerge::ROOT);
        assert_eq!(pane.prefs().theme_choice(), "Dark graphite");
        assert!(reduce_motion.get());
        assert!(pane.take_structure_dirty());

        let intent = pane
            .appearance_intent()
            .expect("the appearance pane always expresses an intent");
        assert_eq!(intent.base_theme.name, "Dark graphite");
        assert_eq!(intent.ui_scale, 1.25);
        assert_eq!(intent.text_scale, 1.15);
        assert!(intent.reduce_motion);
    }

    /// "Hide the top bar" is a desktop control: the toggle writes the
    /// `desktop_controls` section the shell reloads live, leaves the section's
    /// other keys alone, and the pane follows a change made elsewhere.
    #[test]
    fn the_top_bar_toggle_writes_the_desktop_controls_and_follows_them() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        edit_field(&mut app, "settings.desktop-controls", "vim_mode", true).unwrap();
        edit_field(&mut app, "settings.desktop-controls", "hide_top_bar", true).unwrap();
        let controls = app.settings::<DesktopControls>().get();
        assert!(controls.hide_top_bar);
        assert!(controls.vim_mode, "the other desktop control is kept");
        let section = app
            .settings::<DesktopControls>()
            .document()
            .section("desktop_controls");
        let stored: settings_store::hardware::DesktopControlsConfig =
            serde_json::from_value(serde_json::Value::Object(section)).unwrap();
        assert!(
            stored.hide_top_bar,
            "the shell's schema reads the same leaf"
        );

        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let mut pane = AppearancePane::new(cx.clone(), LocalState::new(None));
        let hidden = pane
            .desktop
            .shown(&DesktopControls::paths().hide_top_bar, bool::clone);
        assert!(!hidden.get(), "visible by default");
        cx.settings::<DesktopControls>()
            .update(|controls| controls.hide_top_bar = true)
            .unwrap();
        let doc = ui::automerge::AutoCommit::new();
        pane.sync(&doc, &ui::automerge::ROOT);
        assert!(hidden.get());

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        assert!(
            node_map
                .values()
                .filter_map(|&widget| unsafe { (*widget).test_id() })
                .any(|id| id == "appearance.hide_top_bar"),
            "the pane offers the toggle"
        );
        assert!(
            crate::categories::Category::Appearance.matches("top bar"),
            "searching for the top bar lands here"
        );
    }

    /// The ids of the controls the open pane of `app` shows.
    fn shown_ids(app: &mut TestApp<SettingsShell>) -> Vec<String> {
        fn walk(widget: &dyn Widget, ids: &mut Vec<String>) {
            if let Some(id) = widget.test_id() {
                ids.push(id.to_owned());
            }
            for child in widget.children() {
                walk(child, ids);
            }
        }
        let mut ids = Vec::new();
        walk(app.harness().root_widget(), &mut ids);
        ids
    }

    fn settle(app: &mut TestApp<SettingsShell>) {
        for _ in 0..10 {
            app.settle();
            let _ = app.harness().poll_frame();
        }
    }

    #[test]
    fn contrast_motion_and_text_size_live_only_in_accessibility_and_appearance_links_there() {
        use crate::categories::Category;
        use crate::effects::open_pane;
        ui::i18n::I18nManager::init("en-US");
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.harness().resize((1200, 2000)).unwrap();
        app.effect(&open_pane(Category::Appearance)).unwrap();
        settle(&mut app);
        let appearance = shown_ids(&mut app);
        for duplicate in [
            "appearance.high_contrast",
            "appearance.reduce_motion",
            "appearance.text_size",
            "a11y.high_contrast",
            "a11y.reduce_motion",
            "a11y.text_size",
        ] {
            assert!(
                !appearance.iter().any(|id| id == duplicate),
                "Appearance shows {duplicate}: {appearance:?}"
            );
        }
        assert_eq!(
            appearance
                .iter()
                .filter(|id| id.as_str() == "appearance.accessibility")
                .count(),
            1,
            "one row links to Accessibility"
        );
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains("Accessibility options")
        );

        let link =
            ui::widget::find_test_id(app.harness().root_widget(), "appearance.accessibility")
                .expect("the link is shown")
                .get_rect()
                .center();
        app.harness()
            .send(&ui::synthetic_input::cursor_moved(link.x, link.y));
        app.harness().send(&ui::synthetic_input::mouse_press());
        app.harness().send(&ui::synthetic_input::mouse_release());
        settle(&mut app);
        assert_eq!(
            app.with_widget::<SettingsApp, _>(|settings| settings.selected()),
            Some(Category::Accessibility)
        );
        let accessibility = shown_ids(&mut app);
        for control in ["a11y.high_contrast", "a11y.reduce_motion", "a11y.text_size"] {
            assert_eq!(
                accessibility
                    .iter()
                    .filter(|id| id.as_str() == control)
                    .count(),
                1,
                "Accessibility shows {control} once: {accessibility:?}"
            );
        }
        for setting in [
            "appearance.high_contrast",
            "appearance.reduce_motion",
            "appearance.text_size",
        ] {
            assert_eq!(
                settings_store::index::setting(setting).map(|setting| setting.page),
                Some("accessibility"),
                "{setting}"
            );
        }
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

    /// Choosing a shipped wallpaper writes `appearance.wallpaper`, where the
    /// compositor reads it; a picture the system does not ship is refused
    /// and changes nothing.
    #[test]
    fn choosing_a_shipped_wallpaper_writes_the_shared_section() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        assert_eq!(app.settings::<AppearancePrefs>().get().wallpaper, None);
        edit_field(&mut app, APPEARANCE, "wallpaper", "dunes").unwrap();
        let settings = app.settings::<AppearancePrefs>();
        assert_eq!(settings.get().wallpaper.as_deref(), Some("dunes"));
        let section = settings.document().section("appearance");
        assert_eq!(
            settings_store::appearance::WALLPAPER.from_section(&section),
            Some("dunes".to_owned())
        );
        assert!(edit_field(&mut app, APPEARANCE, "wallpaper", "no-such-picture").is_err());
        edit_field(&mut app, APPEARANCE, "wallpaper", tilo_wallpapers::CUSTOM).unwrap();
        assert_eq!(
            app.settings::<AppearancePrefs>().get().wallpaper.as_deref(),
            Some(tilo_wallpapers::CUSTOM)
        );
    }

    /// The picker offers every shipped picture and the person's own, and
    /// marks the one the desktop shows: the default before anything is
    /// chosen, then whatever the section names.
    #[test]
    fn the_wallpaper_picker_marks_the_chosen_picture() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let thumbnails: Vec<_> = tilo_wallpapers::all()
            .iter()
            .map(|wallpaper| {
                (
                    wallpaper,
                    ui::raster::rgba8(vec![128u8; 16 * 10 * 4], 16, 10),
                )
            })
            .collect();
        let selected = |pane: &AppearancePane| -> Vec<(String, bool)> {
            use ui::widget::Widget as _;
            pane.wallpaper_picker_of(&thumbnails)
                .children()
                .iter()
                .map(|card| {
                    let id = card.test_id().unwrap_or_default().to_owned();
                    let chosen = card.accessibility().is_some_and(|node| node.state.selected);
                    (id, chosen)
                })
                .collect()
        };
        let pane = AppearancePane::new(cx.clone(), LocalState::new(None));
        let cards = selected(&pane);
        let default = tilo_wallpapers::default_wallpaper().unwrap();
        assert_eq!(cards.len(), tilo_wallpapers::all().len() + 1);
        assert_eq!(
            cards.last().map(|(id, _)| id.as_str()),
            Some("appearance.wallpaper.choose")
        );
        let chosen: Vec<&str> = cards
            .iter()
            .filter(|(_, chosen)| *chosen)
            .map(|(id, _)| id.as_str())
            .collect();
        assert_eq!(chosen, vec![format!("appearance.wallpaper.{}", default.id)]);

        cx.settings::<AppearancePrefs>()
            .update(|prefs| prefs.wallpaper = Some(tilo_wallpapers::CUSTOM.into()))
            .unwrap();
        let pane = AppearancePane::new(cx, LocalState::new(None));
        let chosen: Vec<String> = selected(&pane)
            .into_iter()
            .filter(|(_, chosen)| *chosen)
            .map(|(id, _)| id)
            .collect();
        assert_eq!(chosen, vec!["appearance.wallpaper.choose".to_owned()]);
    }

    /// The picker names every shipped picture in every language Settings
    /// speaks.
    #[test]
    fn every_shipped_wallpaper_has_a_name_in_every_language() {
        for (locale, text) in [
            (
                "en-US",
                include_str!("../../../settings-core/locales/en-US.ftl"),
            ),
            (
                "de-DE",
                include_str!("../../../settings-core/locales/de-DE.ftl"),
            ),
        ] {
            for wallpaper in tilo_wallpapers::all() {
                let key = format!("settings-wallpaper-{} = ", wallpaper.id);
                assert!(
                    text.lines().any(|line| line.starts_with(&key)),
                    "{locale} has no name for {}",
                    wallpaper.id
                );
            }
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

#[path = "appearance/desk_widgets.rs"]
mod desk_widgets;

#[cfg(test)]
#[path = "appearance/inspection_tests.rs"]
mod inspection_tests;
