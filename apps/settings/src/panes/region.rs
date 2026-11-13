//! Region & Formats: the editor for the person's locale.
//!
//! This panel edits [`tilo_locale::Locale`], the `locale` section every app's
//! toolkit follows for number, date and time formatting and for units.

use std::rc::Rc;

use provider_settings_ui::bound::SettingRows as _;
use ui::framework::{Actions, ViewCx};
use ui::widget::Element;

use crate::effects::effect;
use crate::i18n::t;
use crate::prefs::AppearancePrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;

pub mod language;
use provider_settings_ui::widgets::{Item, SettingsSection, note, page_of, rows, section};
use tilo_locale::{Currency, DateStyle, FirstDayOfWeek, Locale, Measurement, NumberStyle};
use ui::clock::TimeStyle;

fn date_label(style: DateStyle) -> String {
    t(match style {
        DateStyle::Language => "settings-region-date-language",
        DateStyle::Iso => "settings-region-date-iso",
        DateStyle::Us => "settings-region-date-us",
        DateStyle::Eu => "settings-region-date-eu",
        DateStyle::Long => "settings-region-date-long",
    })
}

fn time_label(style: TimeStyle) -> String {
    t(match style {
        TimeStyle::Language => "settings-region-time-language",
        TimeStyle::H24 => "settings-region-time-24",
        TimeStyle::H12 => "settings-region-time-12",
    })
}

fn measure_label(measure: Measurement) -> String {
    t(match measure {
        Measurement::Metric => "settings-region-measure-metric",
        Measurement::Imperial => "settings-region-measure-imperial",
    })
}

fn weekday_label(day: FirstDayOfWeek) -> String {
    t(match day {
        FirstDayOfWeek::Monday => "settings-region-weekday-monday",
        FirstDayOfWeek::Sunday => "settings-region-weekday-sunday",
        FirstDayOfWeek::Saturday => "settings-region-weekday-saturday",
    })
}

pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    if let Some(area) = language::area(app) {
        area.read();
    }
    let actions = cx.actions();
    let languages = Rc::clone(app);
    let appearance = cx.settings::<AppearancePrefs>();
    let language = cx.record(&appearance, move |cx| {
        let chosen = cx.value(|prefs: &AppearancePrefs| prefs.language.clone());
        let (actions, languages) = (actions.clone(), Rc::clone(&languages));
        cx.switch(&chosen, move |cx, chosen| {
            let (actions, languages, chosen) =
                (actions.clone(), Rc::clone(&languages), chosen.clone());
            let shown = language::area(&languages).map(|area| area.shown.clone());
            match shown {
                Some(shown) => cx.switch(&shown, move |cx, _| {
                    language_card(cx, &actions, chosen.as_deref(), language::area(&languages))
                        .into_element()
                }),
                None => language_card(cx, &actions, chosen.as_deref(), None).into_element(),
            }
        })
    })?;
    let locale = cx.settings::<Locale>();
    let formats = cx.record(&locale, |cx| {
        cx.switch(&cx.value(|locale: &Locale| *locale), |cx, locale| {
            page_of(vec![
                formats_card(cx).into_element(),
                preview_card(*locale).into_element(),
            ])
        })
    })?;
    let explainer = section(
        &t("settings-region-shared-card"),
        rows(vec![note(&t("settings-region-shared-body"))]),
    );
    Some(page_of(vec![language, formats, explainer.into_element()]))
}

fn language_card<R: ui::tilo_resource::Resource<State = R> + ui::tilo_resource::Model>(
    cx: &ViewCx<R>,
    actions: &Actions<Settings>,
    chosen: Option<&str>,
    languages: Option<&language::LanguageArea>,
) -> SettingsSection {
    let offered = languages
        .and_then(|languages| languages.offered.get())
        .unwrap_or_default();
    let names: Vec<String> = std::iter::once(language::name(None))
        .chain(
            language::choices(&crate::i18n::CATALOG.languages(), &offered)
                .iter()
                .map(|choice| language::name(Some(choice))),
        )
        .collect();
    let current = ui::LocalState::new(language::name(chosen));
    let mut items = vec![
        cx.setting(t("settings-region-language"))
            .id("region.language")
            .choice_effect(
                names,
                &current,
                actions,
                effect(language::LanguageEffect::CHOOSE_LANGUAGE),
            ),
    ];
    if let Some(languages) = languages {
        if let Some(installing) = languages.installing.with(Clone::clone) {
            items.push(note(&crate::i18n::t_args(
                "settings-region-language-installing",
                &[("language", language::name(Some(&installing)).into())],
            )));
        } else if let Some(failed) = languages.failed.with(Clone::clone) {
            items.push(provider_settings_ui::widgets::error_row(
                &t("settings-region-language"),
                &crate::i18n::t_args(
                    "settings-region-language-failed",
                    &[("error", failed.into())],
                ),
            ));
        }
    }
    items.push(note(&t("settings-region-language-body")));
    section(&t("settings-region-language-card"), rows(items))
}

fn formats_card(cx: &ViewCx<Locale>) -> SettingsSection {
    section(
        &t("settings-region-formats-card"),
        rows(vec![
            currency(cx),
            date_format(cx),
            time_format(cx),
            measurement(cx),
            number_style(cx),
            first_day(cx),
        ]),
    )
}

fn preview_card(loc: Locale) -> SettingsSection {
    let amount = loc.number_style.amount(loc.currency.symbol());
    let temp = t(match loc.measurement {
        Measurement::Metric => "settings-region-preview-c",
        Measurement::Imperial => "settings-region-preview-f",
    });
    let week_starts = weekday_label(loc.first_day_of_week);
    section(
        &t("settings-region-preview-card"),
        rows(vec![
            preview_line(&t("settings-region-preview-amount"), &amount),
            preview_line(
                &t("settings-region-preview-date"),
                &loc.date_format.example(ui::format::words()),
            ),
            preview_line(
                &t("settings-region-preview-time"),
                &loc.time_format.example(ui::format::words()),
            ),
            preview_line(&t("settings-region-preview-temp"), &temp),
            preview_line(&t("settings-region-preview-week"), &week_starts),
        ]),
    )
}

pub(crate) fn currency(cx: &ViewCx<Locale>) -> Item {
    cx.setting(t("settings-region-currency"))
        .id("region.currency")
        .menu(
            Locale::paths().currency,
            Currency::ALL.iter().copied(),
            |currency: &Currency| currency.code().to_owned(),
        )
}

pub(crate) fn date_format(cx: &ViewCx<Locale>) -> Item {
    cx.setting(t("settings-region-date-format"))
        .id("region.date")
        .menu(
            Locale::paths().date_format,
            DateStyle::ALL.iter().copied(),
            |style: &DateStyle| date_label(*style),
        )
}

pub(crate) fn time_format(cx: &ViewCx<Locale>) -> Item {
    cx.setting(t("settings-region-time-format"))
        .id("region.time")
        .menu(
            Locale::paths().time_format,
            TimeStyle::ALL.iter().copied(),
            |style: &TimeStyle| time_label(*style),
        )
}

pub(crate) fn measurement(cx: &ViewCx<Locale>) -> Item {
    cx.setting(t("settings-region-measurement"))
        .id("region.measurement")
        .menu(
            Locale::paths().measurement,
            Measurement::ALL.iter().copied(),
            |measure: &Measurement| measure_label(*measure),
        )
}

pub(crate) fn number_style(cx: &ViewCx<Locale>) -> Item {
    cx.setting(t("settings-region-number-format"))
        .id("region.number_style")
        .menu(
            Locale::paths().number_style,
            NumberStyle::ALL,
            |style: &NumberStyle| style.label().to_owned(),
        )
}

pub(crate) fn first_day(cx: &ViewCx<Locale>) -> Item {
    cx.setting(t("settings-region-first-day"))
        .id("region.first_day")
        .menu(
            Locale::paths().first_day_of_week,
            FirstDayOfWeek::ALL.iter().copied(),
            |day: &FirstDayOfWeek| weekday_label(*day),
        )
}

fn preview_line(label: &str, value: &str) -> Item {
    provider_settings_ui::widgets::value_row(label, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    #[test]
    fn region_effects_write_the_locale() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let locale_field = |app: &mut TestApp<SettingsShell>, field: &str, value: &str| {
            crate::testing::edit_field(app, "settings.locale", field, value)
        };
        locale_field(&mut app, "currency", "JPY").unwrap();
        locale_field(&mut app, "date_format", "EU").unwrap();
        locale_field(&mut app, "time_format", "H12").unwrap();
        locale_field(&mut app, "measurement", "imperial").unwrap();
        locale_field(&mut app, "first_day_of_week", "sunday").unwrap();
        let settings = app.settings::<Locale>();
        let locale = settings.get();
        assert_eq!(locale.currency, Currency::JPY);
        assert_eq!(locale.date_format, DateStyle::Eu);
        assert_eq!(locale.time_format, TimeStyle::H12);
        assert_eq!(locale.measurement, Measurement::Imperial);
        assert_eq!(locale.first_day_of_week, FirstDayOfWeek::Sunday);
        let section = settings.document().section("locale");
        assert_eq!(section["currency"], "JPY");
        assert_eq!(section["date_format"], "EU");
        assert_eq!(section["time_format"], "H12");

        assert!(locale_field(&mut app, "currency", "not-a-currency").is_err());
        assert_eq!(app.settings::<Locale>().get(), locale);
    }

    #[test]
    fn the_language_is_chosen_among_the_installed_ones() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::RegionFormats);
        app.settle();
        let language = crate::testing::caption("settings-region-language");
        let device = crate::testing::caption("settings-region-language-device");
        assert!(
            crate::testing::shown_values(&mut app, &language).contains(&device),
            "unset, the device's language is shown as the choice"
        );
        crate::testing::edit_field(&mut app, "settings.appearance", "language", "de-DE").unwrap();
        let settings = app.settings::<AppearancePrefs>();
        assert_eq!(settings.get().language.as_deref(), Some("de-DE"));
        assert_eq!(
            settings.document().section("appearance")["language"],
            "de-DE"
        );
    }

    #[test]
    fn choosing_an_installed_language_writes_it_at_once() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let choose = |language: &str| {
            crate::effects::SettingsEffect::from(language::LanguageEffect::ChooseLanguage {
                language: language.into(),
            })
        };
        app.effect(&choose("Deutsch")).unwrap();
        assert_eq!(
            app.settings::<AppearancePrefs>().get().language.as_deref(),
            Some("de")
        );
        assert!(app.effect(&choose("xx-XX")).is_err());
        assert_eq!(
            app.settings::<AppearancePrefs>().get().language.as_deref(),
            Some("de")
        );
        app.effect(&choose(language::DEVICE)).unwrap();
        assert_eq!(app.settings::<AppearancePrefs>().get().language, None);
    }

    #[test]
    fn the_date_format_and_clock_default_to_following_the_language() {
        assert_eq!(Locale::default().date_format, DateStyle::Language);
        assert_eq!(DateStyle::ALL[0], DateStyle::Language);
        assert_eq!(Locale::default().time_format, TimeStyle::Language);
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::RegionFormats);
        app.settle();
        let follows = crate::testing::caption("settings-region-date-language");
        assert_eq!(date_label(DateStyle::Language), follows);
        let clock_follows = crate::testing::caption("settings-region-time-language");
        assert_eq!(time_label(TimeStyle::Language), clock_follows);
        for (caption_key, shown) in [
            ("settings-region-date-format", follows),
            ("settings-region-time-format", clock_follows),
        ] {
            let caption = crate::testing::caption(caption_key);
            assert!(
                crate::testing::shown_values(&mut app, &caption).contains(&shown),
                "the default is shown as following the language: {caption} {shown}"
            );
        }
        assert_eq!(TimeStyle::ALL[0], TimeStyle::Language);
    }

    #[test]
    fn the_page_follows_the_locale() {
        let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
        app.settings::<Locale>()
            .set(&Locale {
                currency: Currency::EUR,
                ..Locale::default()
            })
            .unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::RegionFormats);
        app.settle();
        let currency = crate::testing::caption("settings-region-currency");
        assert!(
            crate::testing::shown_values(&mut app, &currency).contains(&"EUR".to_owned()),
            "the currency the section holds is shown"
        );
        app.settings::<Locale>()
            .update(|locale| locale.measurement = Measurement::Imperial)
            .unwrap();
        app.settle();
        let measurement = crate::testing::caption("settings-region-measurement");
        let imperial = measure_label(Measurement::Imperial);
        assert_eq!(
            imperial,
            crate::testing::caption("settings-region-measure-imperial")
        );
        assert!(
            crate::testing::shown_values(&mut app, &measurement).contains(&imperial),
            "a change made elsewhere is followed"
        );
    }
}
