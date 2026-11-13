//! Appearance: the editor for the person's `appearance` settings.

use std::rc::Rc;

use provider_settings_ui::bound::{SettingRows as _, record_item};
use ui::LocalState;
use ui::framework::{ActionControl as _, Actions, EffectAnswer, EffectCx, Effected, ViewCx};
use ui::tilo_resource;
use ui::widget::Element;

use crate::effects::SettingsEffect;
use crate::i18n::{t, t_args};
use crate::prefs::AppearancePrefs;
use crate::prefs::{AppearanceDevicePrefs, DesktopControlsDevice};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    Item, SettingsSection, block, note, page_of, rows, section, value_row,
};

mod theme_tiles;
use theme_tiles::ThemeTiles;

crate::effects::effect_family!(AppearanceEffect => answer);

const WALLPAPER_ASPECT: f64 = 16.0 / 10.0;

type Thumbnail = (
    &'static tilo_wallpapers::Wallpaper,
    vello::peniko::ImageData,
);

static THUMBNAILS: std::sync::OnceLock<Vec<Thumbnail>> = std::sync::OnceLock::new();

fn installed_thumbnails() -> &'static [Thumbnail] {
    THUMBNAILS.get().map_or(&[], Vec::as_slice)
}

fn read_thumbnails() {
    THUMBNAILS.get_or_init(|| {
        crate::device_read::off_ui_thread();
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
    });
}

fn values(table: &[(&str, &str)]) -> impl Iterator<Item = String> {
    table.iter().map(|(_, value)| (*value).to_owned())
}

pub const SCHEDULES: [(&str, &str); 2] = [("Sunset to sunrise", "sun"), ("Custom times", "custom")];
pub use crate::prefs::{WALLPAPER_SCOPES, WALLPAPER_TARGETS};
pub const TIMES: [&str; 48] = [
    "00:00", "00:30", "01:00", "01:30", "02:00", "02:30", "03:00", "03:30", "04:00", "04:30",
    "05:00", "05:30", "06:00", "06:30", "07:00", "07:30", "08:00", "08:30", "09:00", "09:30",
    "10:00", "10:30", "11:00", "11:30", "12:00", "12:30", "13:00", "13:30", "14:00", "14:30",
    "15:00", "15:30", "16:00", "16:30", "17:00", "17:30", "18:00", "18:30", "19:00", "19:30",
    "20:00", "20:30", "21:00", "21:30", "22:00", "22:30", "23:00", "23:30",
];

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

pub fn schedule_label(value: &str) -> String {
    t(match value {
        "custom" => "settings-appearance-schedule-custom",
        _ => "settings-appearance-schedule-sun",
    })
}

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

pub const UI_SCALES: &[(&str, f64)] = &[
    ("90%", 0.90),
    ("100%", 1.0),
    ("110%", 1.10),
    ("125%", 1.25),
    ("150%", 1.50),
];

pub(crate) fn ui_scale_label(scale: f64) -> &'static str {
    // `total_cmp`, not `partial_cmp().unwrap()`: a hand-edited setting
    // could hold a non-finite scale, and this must degrade to "100%" rather
    // than panic the pane.
    UI_SCALES
        .iter()
        .min_by(|(_, a), (_, b)| (a - scale).abs().total_cmp(&(b - scale).abs()))
        .map_or("100%", |(label, _)| *label)
}

const THEME_CHOICE: &str = "theme";

const UI_FACES: [(&str, &str); 2] = [
    ("Default", "settings-appearance-face-default"),
    ("Hyperlegible", "settings-appearance-face-hyperlegible"),
];

const TRANSLUCENCIES: [(&str, &str, f64); 3] = [
    ("full", "settings-appearance-translucency-full", 1.0),
    ("subtle", "settings-appearance-translucency-subtle", 0.4),
    ("opaque", "settings-appearance-translucency-opaque", 0.0),
];

fn face_choices() -> Vec<(String, String)> {
    let mut choices = vec![(THEME_CHOICE.to_owned(), t("settings-appearance-face-theme"))];
    choices.extend(
        UI_FACES
            .iter()
            .map(|(face, label)| ((*face).to_owned(), t(label))),
    );
    choices
}

fn translucency_choices() -> Vec<(String, String)> {
    let mut choices = vec![(
        THEME_CHOICE.to_owned(),
        t("settings-appearance-translucency-theme"),
    )];
    choices.extend(
        TRANSLUCENCIES
            .iter()
            .map(|(choice, label, _)| ((*choice).to_owned(), t(label))),
    );
    choices
}

fn accent_choices() -> Vec<(String, String)> {
    let mut choices = vec![(
        THEME_CHOICE.to_owned(),
        t("settings-appearance-accent-theme"),
    )];
    choices.extend(
        ui::tokens::PresenceHue::ALL
            .iter()
            .map(|hue| (hue.id().to_owned(), hue.label().to_owned())),
    );
    choices
}

fn accent_of(key: &str) -> Result<Option<String>, String> {
    if key == THEME_CHOICE {
        return Ok(None);
    }
    ui::tokens::PresenceHue::from_id(key)
        .map(|hue| Some(hue.id().to_owned()))
        .ok_or_else(|| format!("`{key}` is not an accent colour"))
}

fn ui_face_of(choice: String) -> Result<Option<String>, String> {
    if choice == THEME_CHOICE {
        return Ok(None);
    }
    if UI_FACES.iter().any(|(face, _)| *face == choice) {
        return Ok(Some(choice));
    }
    Err(format!("`{choice}` is not an interface face"))
}

fn translucency_key(prefs: &AppearancePrefs) -> String {
    match prefs.translucency {
        None => THEME_CHOICE.to_owned(),
        Some(value) => TRANSLUCENCIES
            .iter()
            .min_by(|(_, _, a), (_, _, b)| (a - value).abs().total_cmp(&(b - value).abs()))
            .map_or_else(
                || THEME_CHOICE.to_owned(),
                |(choice, _, _)| (*choice).to_owned(),
            ),
    }
}

fn translucency_of(key: &str) -> Result<Option<f64>, String> {
    if key == THEME_CHOICE {
        return Ok(None);
    }
    TRANSLUCENCIES
        .iter()
        .find(|(choice, _, _)| *choice == key)
        .map(|(_, _, value)| Some(*value))
        .ok_or_else(|| format!("`{key}` is not a translucency"))
}

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

#[derive(Clone, Copy, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum AppearanceEffect {
    /// Read the person's current selectors and schedule without resolving
    /// Automatic mode or changing the OS-controlled theme.
    #[effect(view, returns, kind = "read")]
    InspectAppearance,
    /// Choose a picture and make it the wallpaper.
    #[effect(job)]
    #[effect(keywords = "wallpaper, background, desktop picture, desktop background")]
    ChooseWallpaper,
}

pub(crate) fn answer(
    app: &SettingsApp,
    effect: AppearanceEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    match effect {
        AppearanceEffect::InspectAppearance => {
            let prefs = app.storage().settings::<AppearancePrefs>().get();
            serde_json::to_value(AppearanceInspection::from(&prefs))
                .map(Effected::value)
                .map_err(|error| {
                    crate::i18n::t_args(
                        "appearance-inspect-failed",
                        &[("detail", ui::i18n::ArgValue::Text(error.to_string()))],
                    )
                })
        }
        AppearanceEffect::ChooseWallpaper => choose_wallpaper(app, cx),
    }
}

fn choose_wallpaper(app: &SettingsApp, cx: &mut EffectCx<Settings>) -> EffectAnswer {
    if !app.reaches_devices() {
        return Err("only a window someone uses opens the file chooser".into());
    }
    let target = match app
        .storage()
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
    Ok(app.job(
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
                    &[("error", t("settings-error-portal-gone").into())],
                )),
            }
        },
        |app: &SettingsApp, outcome: Result<Option<String>, String>, _| match outcome {
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

pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let decoded = LocalState::new(THUMBNAILS.get().is_some());
    if !decoded.get() {
        let writer = decoded.writer();
        crate::device_read::Reading::new(move || {
            read_thumbnails();
            writer.set(true);
        })
        .refresh();
    }
    let actions = cx.actions();
    let status = app.wallpaper_status().clone();
    let prefs = cx.settings::<AppearancePrefs>();
    cx.record(&prefs, move |cx| {
        let mut groups = vec![
            theme_card(cx, &actions),
            {
                let (actions, decoded) = (actions.clone(), decoded.clone());
                cx.switch(&status, move |cx, status| {
                    wallpaper_card(cx, &actions, &decoded, status.as_deref())
                })
            },
            cx.switch(&schedule_shown(cx), |cx, _| {
                schedule_card(cx).into_element()
            }),
        ];
        let desktop = cx.settings::<DesktopControlsDevice>();
        if let Some(top_bar) = record_item(cx, &desktop, hide_top_bar) {
            groups.push(
                section(&t("settings-appearance-desktop-card"), rows(vec![top_bar])).into_element(),
            );
        }
        let desk = cx.settings::<crate::prefs::DeskWidgetsPrefs>();
        if let Some(card) = cx.record(&desk, desk_widgets::card) {
            groups.push(card);
        }
        let colour = cx.settings::<crate::prefs::IdentityPrefs>();
        if let Some(card) = cx.record(&colour, |cx| colour_card(cx)) {
            groups.push(card);
        }
        page_of(groups)
    })
}

fn colour_card(cx: &ViewCx<crate::prefs::IdentityPrefs>) -> Element {
    let mut choices = vec![(String::new(), t("settings-appearance-colour-identity"))];
    choices.extend(
        ui::tokens::PresenceHue::ALL
            .iter()
            .map(|hue| (hue.id().to_owned(), hue.label().to_owned())),
    );
    let row = cx
        .setting(t("settings-appearance-colour"))
        .described(t("settings-appearance-colour-desc"))
        .id("appearance.colour")
        .keyed(
            crate::prefs::IdentityPrefs::paths().colour,
            choices,
            |prefs| prefs.colour.clone().unwrap_or_default(),
            |key| Ok(if key.is_empty() { None } else { Some(key) }),
        );
    section(&t("settings-appearance-colour-card"), rows(vec![row])).into_element()
}

fn theme_card(cx: &ViewCx<AppearancePrefs>, actions: &Actions<Settings>) -> Element {
    let paths = AppearancePrefs::paths();
    let tiles = cx.switch(&cx.value(AppearancePrefs::theme_choice), |cx, theme| {
        Box::new(
            cx.actions()
                .bind(ThemeTiles::new(theme.clone()), AppearancePrefs::PICK_THEME),
        )
    });
    let open = crate::effects::open_pane(crate::categories::Category::Accessibility);
    section(
        &t("settings-appearance-theme-card"),
        rows(vec![
            block(tiles),
            cx.setting(t("settings-appearance-density"))
                .described(t("settings-appearance-density-desc"))
                .id("appearance.density")
                .pictures(
                    paths.density,
                    DENSITIES.iter().map(|(_, density)| f64::from(*density)),
                    |density: &f64| density_label(*density),
                    |density: &f64| ui::widget::Picture::Density(*density as f32),
                ),
            cx.setting(t("settings-appearance-ui-scale"))
                .id("appearance.ui_scale")
                .among(
                    paths.ui_scale,
                    UI_SCALES.iter().map(|(_, scale)| *scale),
                    |scale: &f64| ui_scale_label(*scale).to_owned(),
                ),
            cx.setting(t("settings-appearance-ui-face"))
                .described(t("settings-appearance-ui-face-desc"))
                .id("appearance.ui_face")
                .keyed(
                    paths.ui_face,
                    face_choices(),
                    |prefs| {
                        prefs
                            .ui_face
                            .clone()
                            .unwrap_or_else(|| THEME_CHOICE.to_owned())
                    },
                    ui_face_of,
                ),
            cx.setting(t("settings-appearance-accent"))
                .described(t("settings-appearance-accent-desc"))
                .id("appearance.accent")
                .keyed(
                    paths.accent,
                    accent_choices(),
                    |prefs| {
                        prefs
                            .accent
                            .clone()
                            .unwrap_or_else(|| THEME_CHOICE.to_owned())
                    },
                    |key: String| accent_of(&key),
                ),
            cx.setting(t("settings-appearance-translucency"))
                .described(t("settings-appearance-translucency-desc"))
                .id("appearance.translucency")
                .keyed(
                    paths.translucency,
                    translucency_choices(),
                    translucency_key,
                    |key: String| translucency_of(&key),
                ),
            cx.setting(t("settings-appearance-accessibility"))
                .described(t("settings-appearance-accessibility-desc"))
                .id("appearance.accessibility")
                .button(&t("settings-appearance-accessibility-open"), |button| {
                    actions.bind_effect_invocation(button, &open)
                }),
        ]),
    )
    .into_element()
}

fn wallpaper_card(
    cx: &ViewCx<AppearancePrefs>,
    actions: &Actions<Settings>,
    decoded: &LocalState<bool>,
    status: Option<&str>,
) -> Element {
    let paths = AppearancePrefs::paths();
    let picker = {
        let actions = actions.clone();
        cx.switch(decoded, move |cx, _| {
            let actions = actions.clone();
            let chosen = cx.value(|prefs: &AppearancePrefs| prefs.wallpaper.clone());
            cx.switch(&chosen, move |cx, chosen| {
                Box::new(wallpaper_picker(
                    cx,
                    &actions,
                    chosen.as_deref(),
                    installed_thumbnails(),
                ))
            })
        })
    };
    let mut body = vec![
        block(picker),
        cx.setting(t("settings-appearance-use-for"))
            .id("appearance.wallpaper_scope")
            .among(
                paths.wallpaper_scope,
                values(&WALLPAPER_SCOPES),
                |scope: &String| scope_label(scope),
            ),
        cx.setting(t("settings-appearance-show-on"))
            .id("appearance.wallpaper_target")
            .among(
                paths.wallpaper_target,
                values(&WALLPAPER_TARGETS),
                |target: &String| target_label(target),
            ),
    ];
    if let Some(status) = status {
        body.push(note(status));
    }
    section(&t("settings-appearance-wallpaper-card"), rows(body)).into_element()
}

fn wallpaper_picker(
    cx: &ViewCx<AppearancePrefs>,
    actions: &Actions<Settings>,
    chosen: Option<&str>,
    thumbnails: &'static [Thumbnail],
) -> ui::widget::MediaGrid {
    let mut cards = wallpaper_cards(chosen, thumbnails);
    let custom = cards.pop();
    let mut bound: Vec<ui::widget::MediaCard> = cards
        .into_iter()
        .zip(thumbnails)
        .map(|(card, (wallpaper, _))| {
            let id = wallpaper.id.as_str();
            match cx
                .actions()
                .set_binding(AppearancePrefs::paths().wallpaper, move |()| {
                    Ok(Some(id.to_owned()))
                }) {
                Some(binding) => card.bind_action(binding),
                None => card.unavailable(),
            }
        })
        .collect();
    bound.extend(custom.map(|card| {
        actions.bind_effect_invocation(
            card,
            &SettingsEffect::from(AppearanceEffect::ChooseWallpaper),
        )
    }));
    ui::widget::MediaGrid::new(bound).with_size(ui::widget::MediaSize::Small)
}

fn wallpaper_cards(chosen: Option<&str>, thumbnails: &[Thumbnail]) -> Vec<ui::widget::MediaCard> {
    let chosen = tilo_wallpapers::choice(chosen);
    let mut cards: Vec<ui::widget::MediaCard> = thumbnails
        .iter()
        .map(|(wallpaper, thumbnail)| {
            let id = wallpaper.id.as_str();
            ui::widget::MediaCard::new(
                format!("appearance.wallpaper.{id}"),
                ui::widget::MediaSource::Image(thumbnail.clone()),
            )
            .aspect(ui::widget::MediaAspect::Ratio(WALLPAPER_ASPECT))
            .title(t(&format!("settings-wallpaper-{id}")))
            .meta(wallpaper.author.as_str())
            .selected(chosen == Some(tilo_wallpapers::Choice::Bundled(wallpaper)))
        })
        .collect();
    cards.push(
        ui::widget::MediaCard::new(
            "appearance.wallpaper.choose",
            ui::widget::MediaSource::Kind(ui::icons::Icon::Image),
        )
        .aspect(ui::widget::MediaAspect::Ratio(WALLPAPER_ASPECT))
        .title(t("settings-appearance-your-picture"))
        .selected(chosen == Some(tilo_wallpapers::Choice::Custom)),
    );
    cards
}

fn schedule_shown(cx: &ViewCx<AppearancePrefs>) -> LocalState<[String; 3]> {
    cx.value(|prefs: &AppearancePrefs| {
        [
            prefs.schedule.clone(),
            prefs.sunrise.clone(),
            prefs.sunset.clone(),
        ]
    })
}

fn schedule_card(cx: &ViewCx<AppearancePrefs>) -> SettingsSection {
    let paths = AppearancePrefs::paths();
    let times = || TIMES.iter().map(|time| (*time).to_owned());
    let mut body = vec![
        cx.setting(t("settings-appearance-schedule"))
            .id("appearance.schedule")
            .among(paths.schedule, values(&SCHEDULES), |schedule: &String| {
                schedule_label(schedule)
            }),
    ];
    let prefs = cx.read(|prefs| prefs.cloned().unwrap_or_default());
    let schedule = prefs.schedule();
    if !schedule.config.follow_sun {
        body.push(
            cx.setting(t("settings-appearance-day-starts"))
                .id("appearance.sunrise")
                .among(paths.sunrise, times(), String::clone),
        );
        body.push(
            cx.setting(t("settings-appearance-night-starts"))
                .id("appearance.sunset")
                .among(paths.sunset, times(), String::clone),
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

pub(crate) mod values;

pub(crate) fn hide_top_bar(cx: &ViewCx<DesktopControlsDevice>) -> Item {
    cx.setting(t("settings-appearance-hide-top-bar"))
        .described(t("settings-appearance-hide-top-bar-desc"))
        .id("appearance.hide_top_bar")
        .toggle(DesktopControlsDevice::paths().hide_top_bar)
}

pub(crate) fn intent(
    prefs: &AppearancePrefs,
    device: &AppearanceDevicePrefs,
) -> ui::theme::AppearanceIntent {
    ui::theme::AppearanceIntent {
        base_theme: prefs.base_theme(device),
        high_contrast: prefs.high_contrast,
        reduce_motion: prefs.reduce_motion,
        text_scale: prefs.text_scale as f32,
        ui_scale: prefs.ui_scale as f32,
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;
    use ui::theme::Theme;
    use ui::widget::Widget;

    use crate::prefs::IdentityPrefs;
    use crate::testing::{APPEARANCE, IDENTITY, edit_field, invoke_section};

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
        crate::testing::edit_field(&mut app, crate::testing::APPEARANCE, "ui_scale", 1.5).unwrap();
        crate::testing::edit_field(&mut app, crate::testing::APPEARANCE, "text_scale", 1.15)
            .unwrap();
        edit_field(&mut app, APPEARANCE, "high_contrast", true).unwrap();
        crate::testing::edit_field(&mut app, crate::testing::APPEARANCE, "schedule", "custom")
            .unwrap();
        edit_field(&mut app, APPEARANCE, "sunrise", "06:30").unwrap();
        edit_field(&mut app, APPEARANCE, "sunset", "21:00").unwrap();
        crate::testing::edit_field(
            &mut app,
            crate::testing::APPEARANCE,
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
        let resolved = prefs.resolve_theme(&crate::prefs::AppearanceDevicePrefs::default());
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

    #[test]
    fn the_interface_face_and_translucency_are_the_persons_over_the_theme() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let device = crate::prefs::AppearanceDevicePrefs::default();
        let stock = app
            .settings::<AppearancePrefs>()
            .get()
            .resolve_theme(&device);
        assert!(
            stock.tokens.colors.surfaces.material_blur_px > 1.0,
            "the stock theme blurs its floating chrome"
        );

        edit_field(&mut app, APPEARANCE, "ui_face", "Hyperlegible").unwrap();
        edit_field(&mut app, APPEARANCE, "translucency", 0.0).unwrap();

        let prefs = app.settings::<AppearancePrefs>().get();
        assert_eq!(prefs.ui_face.as_deref(), Some("Hyperlegible"));
        assert_eq!(prefs.translucency, Some(0.0));
        let chosen = prefs.resolve_theme(&device);
        assert_eq!(chosen.document.type_.face, "Hyperlegible");
        assert_eq!(
            chosen.tokens.typography.sans_family,
            ui::text_render::UI_HYPERLEGIBLE_FAMILY
        );
        assert!(chosen.tokens.colors.surfaces.material_blur_px.abs() < f32::EPSILON);
        assert!((chosen.tokens.colors.surfaces.material_opacity - 1.0).abs() < 1e-6);

        edit_field(&mut app, APPEARANCE, "ui_face", serde_json::Value::Null).unwrap();
        edit_field(
            &mut app,
            APPEARANCE,
            "translucency",
            serde_json::Value::Null,
        )
        .unwrap();
        let following = app.settings::<AppearancePrefs>().get();
        assert_eq!(following.ui_face, None);
        assert_eq!(following.translucency, None);
        let theme = following.resolve_theme(&device);
        assert_eq!(theme.document.type_.face, stock.document.type_.face);
        assert!(
            (theme.tokens.colors.surfaces.material_blur_px
                - stock.tokens.colors.surfaces.material_blur_px)
                .abs()
                < 1e-4
        );

        assert!(edit_field(&mut app, APPEARANCE, "ui_face", "   ").is_err());
        assert!(edit_field(&mut app, APPEARANCE, "translucency", 2.0).is_err());
        assert_eq!(app.settings::<AppearancePrefs>().get(), following);
    }

    #[test]
    fn an_accent_colour_recolours_the_theme_and_survives_a_restart() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let device = crate::prefs::AppearanceDevicePrefs::default();
        let stock = app
            .settings::<AppearancePrefs>()
            .get()
            .resolve_theme(&device);

        edit_field(&mut app, APPEARANCE, "accent", "violet").unwrap();

        let prefs = app.settings::<AppearancePrefs>().get();
        assert_eq!(prefs.accent.as_deref(), Some("violet"));
        let chosen = prefs.resolve_theme(&device);
        assert_eq!(
            chosen.document.emphasis,
            ui::tokens::Emphasis::Tinted {
                hue: ui::tokens::PresenceHue::Violet.degrees()
            }
        );
        let accent = chosen.tokens.colors.intent.accent;
        assert_ne!(accent, stock.tokens.colors.intent.accent);
        let hue = ui::color::Oklch::from_color(accent).h;
        let wanted = ui::tokens::PresenceHue::Violet.degrees();
        let off = (hue - wanted).rem_euclid(360.0);
        assert!(off.min(360.0 - off) <= 12.0, "the accent is violet: {hue}°");
        assert_eq!(chosen.border_focus, chosen.tokens.colors.intent.accent_ink);
        assert!(
            ui::tokens::contrast::audit(&chosen.tokens.colors).is_empty(),
            "the accent keeps every contrast floor"
        );

        let document = app.settings::<AppearancePrefs>().document().load();
        assert_eq!(
            settings_store::appearance::ACCENT
                .from_document(&document)
                .as_deref(),
            Some("violet")
        );
        let text = serde_json::to_string(&document).unwrap();
        let restarted = Theme::resolved_from_settings(&text);
        assert_eq!(restarted.tokens.colors.intent.accent, accent);
        assert_eq!(
            Theme::configured_from(&document)
                .tokens
                .colors
                .intent
                .accent,
            accent
        );

        assert!(edit_field(&mut app, APPEARANCE, "accent", "chartreuse").is_err());
        assert_eq!(
            app.settings::<AppearancePrefs>().get().accent.as_deref(),
            Some("violet")
        );
        edit_field(&mut app, APPEARANCE, "accent", serde_json::Value::Null).unwrap();
        let following = app.settings::<AppearancePrefs>().get();
        assert_eq!(following.accent, None);
        assert_eq!(
            following.resolve_theme(&device).tokens.colors.intent.accent,
            stock.tokens.colors.intent.accent
        );
    }

    #[test]
    fn the_colour_the_person_wears_is_stored_where_the_theme_reads_it() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let chosen = |app: &TestApp<SettingsShell>| {
            let section = app
                .settings::<IdentityPrefs>()
                .document()
                .section(settings_store::identity::SECTION);
            ui::tokens::PresenceHue::chosen_in(&serde_json::json!({
                settings_store::identity::SECTION: section,
            }))
        };
        assert_eq!(chosen(&app), None, "no choice follows the identity");

        edit_field(&mut app, IDENTITY, "colour", "violet").unwrap();
        assert_eq!(
            app.settings::<IdentityPrefs>().get().colour.as_deref(),
            Some("violet")
        );
        assert_eq!(chosen(&app), Some(ui::tokens::PresenceHue::Violet));

        edit_field(&mut app, IDENTITY, "colour", serde_json::Value::Null).unwrap();
        assert_eq!(chosen(&app), None);

        assert!(edit_field(&mut app, IDENTITY, "colour", "chartreuse").is_err());
        assert_eq!(app.settings::<IdentityPrefs>().get().colour, None);
    }

    #[test]
    fn the_page_follows_the_section_and_the_window_takes_its_look() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.settings::<AppearancePrefs>()
            .set(&AppearancePrefs {
                mode: "dark".into(),
                dark_theme: "dark-cool".into(),
                text_scale: 1.15,
                ui_scale: 1.25,
                ..AppearancePrefs::default()
            })
            .unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::Appearance);
        app.settle();
        let intent = || {
            |window: &mut crate::window::Inspected| ui::widget::Widget::appearance_intent(window)
        };
        let shown = app
            .with_widget::<crate::window::Inspected, _>(intent())
            .flatten()
            .expect("the Appearance page expresses the look");
        assert_eq!(shown.base_theme.name, "Dark cool");
        assert_eq!(shown.ui_scale, 1.25);
        assert_eq!(shown.text_scale, 1.15);

        app.settings::<AppearancePrefs>()
            .update(|prefs| {
                prefs.choose_theme("Dark graphite");
                prefs.reduce_motion = true;
            })
            .unwrap();
        app.settle();
        let shown = app
            .with_widget::<crate::window::Inspected, _>(intent())
            .flatten()
            .expect("the Appearance page expresses the look");
        assert_eq!(shown.base_theme.name, "Dark graphite");
        assert!(shown.reduce_motion);
        let size = crate::testing::caption("settings-appearance-ui-scale");
        assert!(
            crate::testing::shown_values(&mut app, &size).contains(&"125%".to_owned()),
            "the interface scale shows what the section holds"
        );
    }

    #[test]
    fn the_top_bar_toggle_writes_the_desktop_controls_and_follows_them() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        edit_field(&mut app, "settings.desktop-controls", "vim_mode", true).unwrap();
        edit_field(
            &mut app,
            "settings.desktop-controls-device",
            "hide_top_bar",
            true,
        )
        .unwrap();
        assert!(app.settings::<DesktopControlsDevice>().get().hide_top_bar);
        assert!(
            app.settings::<crate::prefs::DesktopControls>()
                .get()
                .vim_mode,
            "the person's desktop control is kept"
        );
        let stored = settings_store::desktop_controls::DesktopControlsConfig::from_document(
            &app.settings::<DesktopControlsDevice>().document().load(),
        );
        assert!(
            stored.hide_top_bar && stored.vim_mode,
            "the shell's schema reads the same leaves"
        );

        crate::effects::show_pane(&mut app, crate::categories::Category::Appearance);
        settle(&mut app);
        assert!(
            shown_ids(&mut app)
                .iter()
                .any(|id| id == "appearance.hide_top_bar"),
            "the page offers the toggle"
        );
        assert!(
            crate::categories::Category::Appearance.matches("top bar"),
            "searching for the top bar lands here"
        );
    }

    #[test]
    fn a_reflow_keeps_the_focused_control_and_it_still_works() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.effect(&crate::effects::open_pane(
            crate::categories::Category::Appearance,
        ))
        .unwrap();
        settle(&mut app);
        let control = "appearance.hide_top_bar";
        app.harness()
            .perform_semantic_named(control, None, &ui::a11y::Action::Focus)
            .unwrap();
        let owner = app.harness().inspection().keyboard_focus_owner;
        let focused = app.harness().focused_access_node().expect("focused");
        assert!(owner.is_some() && focused.state.focused);
        let widget = |app: &mut TestApp<SettingsShell>| {
            std::ptr::from_ref(
                ui::widget::find_test_id(app.harness().root_widget(), control).expect("shown"),
            )
            .cast::<()>()
        };
        let before = widget(&mut app);
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            for viewport in [(1472, 920), (440, 820)] {
                app.harness()
                    .relayout_themed(viewport, theme.clone())
                    .unwrap();
                app.harness().verify_retained_geometry().unwrap();
                assert_eq!(widget(&mut app), before, "the control is kept");
                assert_eq!(app.harness().inspection().keyboard_focus_owner, owner);
                let now = app.harness().focused_access_node().expect("still focused");
                assert_eq!(now.semantic_key, focused.semantic_key);
                let wanted = !app.settings::<DesktopControlsDevice>().get().hide_top_bar;
                app.harness()
                    .perform_semantic_named(control, None, &ui::a11y::Action::Activate)
                    .unwrap();
                settle(&mut app);
                assert_eq!(
                    app.settings::<DesktopControlsDevice>().get().hide_top_bar,
                    wanted
                );
            }
        }
    }

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
            app.with_widget::<crate::window::Inspected, _>(|settings| settings.selected()),
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

        let mut app = TestApp::<SettingsShell>::new().unwrap();
        crate::effects::show_pane(&mut app, Category::Appearance);
        settle(&mut app);
        let ids = shown_ids(&mut app);
        for id in ["appearance.wallpaper.choose", "appearance.wallpaper_scope"] {
            assert!(
                ids.iter().any(|candidate| candidate == id),
                "{id} missing from {ids:?}"
            );
        }
    }

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

    #[test]
    fn the_wallpaper_picker_marks_the_chosen_picture() {
        let thumbnails: Vec<_> = tilo_wallpapers::all()
            .iter()
            .map(|wallpaper| {
                (
                    wallpaper,
                    ui::raster::rgba8(vec![128u8; 16 * 10 * 4], 16, 10),
                )
            })
            .collect();
        let selected = |chosen: Option<&str>| -> Vec<(String, bool)> {
            use ui::widget::Widget as _;
            wallpaper_cards(chosen, &thumbnails)
                .iter()
                .map(|card| {
                    let id = card.test_id().unwrap_or_default().to_owned();
                    let chosen = card.accessibility().is_some_and(|node| node.state.selected);
                    (id, chosen)
                })
                .collect()
        };
        let cards = selected(None);
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

        let chosen: Vec<String> = selected(Some(tilo_wallpapers::CUSTOM))
            .into_iter()
            .filter(|(_, chosen)| *chosen)
            .map(|(id, _)| id)
            .collect();
        assert_eq!(chosen, vec!["appearance.wallpaper.choose".to_owned()]);
    }

    #[test]
    fn every_shipped_wallpaper_has_a_name_in_every_language() {
        for (locale, text) in [
            ("en-US", include_str!("../../locales/en-US.ftl")),
            ("de-DE", include_str!("../../locales/de-DE.ftl")),
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
