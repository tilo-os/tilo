//! Region & Formats pane — the editor for the person's **locale**.
//!
//! This panel edits [`tilo_locale::Locale`], the `locale` section of the
//! person's settings, which every app's toolkit follows. The Sheets app reads
//! `locale.currency` / `locale.date_format` / `locale.time_format` for its
//! cell formatting, and the Weather app reads `locale.measurement` for its
//! °C/°F + km/h vs mph toggle. Editing here is the single source of truth
//! those apps follow.
//!
//! Each control is a `Dropdown` bound to its field of the section, mapping the
//! shown label to the stored value; the pane shows what the section holds,
//! whoever changed it.

use ui::widget::{AppModel, ModelPath, Widget};

use crate::effects::effect;
use crate::host::{Followed, PaneCx};
use crate::i18n::t;
use crate::prefs::AppearancePrefs;

pub mod language;
use crate::widgets::{Item, note, rows, section};
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

pub struct RegionPane {
    cx: PaneCx,
    locale: Followed<Locale>,
    /// The interface language is the appearance section's.
    appearance: Followed<AppearancePrefs>,
    /// The languages offered and the install under way, when the window
    /// holds them.
    languages: Option<Languages>,
    composed: ui::widget::Composed,
}

/// What the pane shows of [`language::LanguageArea`].
struct Languages {
    offered: crate::device_read::Reading<language::Offered>,
    offered_seen: u64,
    installing: ui::LocalState<Option<String>>,
    failed: ui::LocalState<Option<String>>,
    /// The revisions of `installing` and `failed` the page shows.
    install_seen: (u64, u64),
}

impl RegionPane {
    /// The pane over `cx`'s locale, and the languages `languages` knows of.
    pub fn new(cx: PaneCx, languages: Option<&language::LanguageArea>) -> Self {
        let languages = languages.map(|area| {
            // Ask the repositories what they offer as the page opens.
            area.read();
            Languages {
                offered: area.offered.clone(),
                offered_seen: area.offered.revision(),
                installing: area.installing.clone(),
                failed: area.failed.clone(),
                install_seen: (area.installing.revision(), area.failed.revision()),
            }
        });
        Self {
            locale: cx.follow::<Locale>(),
            appearance: cx.follow::<AppearancePrefs>(),
            languages,
            cx,
            composed: ui::widget::Composed::default(),
        }
    }

    /// The Language card: the language chosen, among the installed ones and
    /// those offered, and an install under way or the reason one stopped.
    fn language_card(&self) -> crate::widgets::SettingsSection {
        let chosen = self.appearance.get().language.clone();
        let offered = self
            .languages
            .as_ref()
            .and_then(|languages| languages.offered.get())
            .unwrap_or_default();
        let names: Vec<String> = std::iter::once(language::name(None))
            .chain(
                language::choices(&offered)
                    .iter()
                    .map(|choice| language::name(Some(choice))),
            )
            .collect();
        let current = ui::LocalState::new(language::name(chosen.as_deref()));
        let mut items = vec![
            self.cx
                .row(t("settings-region-language"))
                .id("region.language")
                .choice_effect(
                    names,
                    &current,
                    effect(language::LanguageEffect::CHOOSE_LANGUAGE),
                ),
        ];
        if let Some(languages) = &self.languages {
            if let Some(installing) = languages.installing.with(Clone::clone) {
                items.push(note(&crate::i18n::t_args(
                    "settings-region-language-installing",
                    &[("language", language::name(Some(&installing)).into())],
                )));
            } else if let Some(failed) = languages.failed.with(Clone::clone) {
                items.push(crate::widgets::error_row(
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

    pub fn locale(&self) -> Locale {
        *self.locale.get()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one declarative card list mirroring the visible Region & Formats pane"
    )]
    fn build_composition(&self) -> Box<dyn Widget> {
        let loc = *self.locale.get();
        let paths = Locale::paths();
        let row = |label: &str, id: &str| self.cx.row(t(label)).id(id);
        let language = self.language_card();
        let formats = section(
            &t("settings-region-formats-card"),
            rows(vec![
                // Currencies by their ISO code, the stored form.
                row("settings-region-currency", "region.currency")
                    .of(&self.locale, paths.currency)
                    .menu(Currency::ALL.iter().copied(), |currency: &Currency| {
                        currency.code().to_owned()
                    }),
                row("settings-region-date-format", "region.date")
                    .of(&self.locale, paths.date_format)
                    .menu(DateStyle::ALL.iter().copied(), |style: &DateStyle| {
                        date_label(*style)
                    }),
                row("settings-region-time-format", "region.time")
                    .of(&self.locale, paths.time_format)
                    .menu(TimeStyle::ALL.iter().copied(), |style: &TimeStyle| {
                        time_label(*style)
                    }),
                row("settings-region-measurement", "region.measurement")
                    .of(&self.locale, paths.measurement)
                    .menu(Measurement::ALL.iter().copied(), |measure: &Measurement| {
                        measure_label(*measure)
                    }),
                row("settings-region-number-format", "region.number_style")
                    .of(&self.locale, paths.number_style)
                    .menu(NumberStyle::ALL, |style: &NumberStyle| {
                        style.label().to_owned()
                    }),
                row("settings-region-first-day", "region.first_day")
                    .of(&self.locale, paths.first_day_of_week)
                    .menu(
                        FirstDayOfWeek::ALL.iter().copied(),
                        |day: &FirstDayOfWeek| weekday_label(*day),
                    ),
            ]),
        );

        let amount = loc.number_style.amount(loc.currency.symbol());
        let temp = t(match loc.measurement {
            Measurement::Metric => "settings-region-preview-c",
            Measurement::Imperial => "settings-region-preview-f",
        });
        let week_starts = weekday_label(loc.first_day_of_week);
        let preview = section(
            &t("settings-region-preview-card"),
            rows(vec![
                preview_line(&t("settings-region-preview-amount"), &amount),
                preview_line(
                    &t("settings-region-preview-date"),
                    &loc.date_format.example(),
                ),
                preview_line(
                    &t("settings-region-preview-time"),
                    &loc.time_format.example(),
                ),
                preview_line(&t("settings-region-preview-temp"), &temp),
                preview_line(&t("settings-region-preview-week"), &week_starts),
            ]),
        );

        let explainer = section(
            &t("settings-region-shared-card"),
            rows(vec![note(&t("settings-region-shared-body"))]),
        );

        crate::widgets::page(vec![language, formats, preview, explainer])
    }
}

impl Widget for RegionPane {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        // What the repositories offer landed, or an install started or
        // stopped.
        let languages_moved = self.languages.as_mut().is_some_and(|languages| {
            let seen = (
                languages.offered.revision(),
                (languages.installing.revision(), languages.failed.revision()),
            );
            let moved = seen != (languages.offered_seen, languages.install_seen);
            (languages.offered_seen, languages.install_seen) = seen;
            moved
        });
        if self.locale.refresh() | self.appearance.refresh() | languages_moved {
            // The preview card follows the values.
            self.composed.invalidate();
        }
        self.composed.sync(state, root);
    }

    crate::panes::composed_pane!();

    fn name(&self) -> &'static str {
        "RegionPane"
    }
}

fn preview_line(label: &str, value: &str) -> Item {
    crate::widgets::value_row(label, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    /// Each format is the person's `locale` section, written in the
    /// cross-app shape Sheets and Weather read; a value outside the offered
    /// ones is refused and changes nothing.
    #[test]
    fn region_effects_write_the_locale() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let locale_field = |app: &mut TestApp<SettingsShell>, field: &str, value: &str| {
            crate::host::testing::edit_field(app, "settings.locale", field, value)
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

    /// The interface language is chosen here, among the languages the
    /// desktop has installed, and written where every app and the shell
    /// read it (`appearance.language`); unset, the pane shows the device's
    /// language as the choice.
    #[test]
    fn the_language_is_chosen_among_the_installed_ones() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let pane = RegionPane::new(cx, None);
        let language = AppearancePrefs::paths().language;
        let label = |language: &Option<String>| {
            language.as_deref().map_or_else(
                || t("settings-region-language-device"),
                ui::i18n::language_name,
            )
        };
        assert_eq!(
            pane.appearance.shown(&language, label).get(),
            t("settings-region-language-device")
        );
        assert!(crate::i18n::languages().contains(&"en-US"));
        assert!(crate::i18n::languages().contains(&"de-DE"));

        let mut app = TestApp::<SettingsShell>::new().unwrap();
        crate::host::testing::edit_field(&mut app, "settings.appearance", "language", "de-DE")
            .unwrap();
        let settings = app.settings::<AppearancePrefs>();
        assert_eq!(settings.get().language.as_deref(), Some("de-DE"));
        assert_eq!(
            settings.document().section("appearance")["language"],
            "de-DE"
        );
    }

    /// Choosing an installed language takes effect at once; a language the
    /// desktop neither has nor is offered is refused and changes nothing.
    #[test]
    fn choosing_an_installed_language_writes_it_at_once() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let choose = |language: &str| {
            crate::effects::SettingsEffect::from(language::LanguageEffect::ChooseLanguage {
                language: language.into(),
            })
        };
        app.effect(&choose("Deutsch (Deutschland)")).unwrap();
        assert_eq!(
            app.settings::<AppearancePrefs>().get().language.as_deref(),
            Some("de-DE")
        );
        assert!(app.effect(&choose("xx-XX")).is_err());
        assert_eq!(
            app.settings::<AppearancePrefs>().get().language.as_deref(),
            Some("de-DE")
        );
        app.effect(&choose(language::DEVICE)).unwrap();
        assert_eq!(app.settings::<AppearancePrefs>().get().language, None);
    }

    /// With no date format or clock chosen the pane shows "Follow language"
    /// as each value and offers it first; ISO, 12-hour and 24-hour are
    /// explicit choices among the rest.
    #[test]
    fn the_date_format_and_clock_default_to_following_the_language() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let pane = RegionPane::new(cx, None);
        assert_eq!(pane.locale().date_format, DateStyle::Language);
        let paths = Locale::paths();
        assert_eq!(
            pane.locale
                .shown(&paths.date_format, |style| date_label(*style))
                .get(),
            date_label(DateStyle::Language),
            "the default is shown as following the language"
        );
        assert_eq!(DateStyle::ALL[0], DateStyle::Language);
        assert_eq!(pane.locale().time_format, TimeStyle::Language);
        assert_eq!(
            pane.locale
                .shown(&paths.time_format, |style| time_label(*style))
                .get(),
            time_label(TimeStyle::Language)
        );
        assert_eq!(TimeStyle::ALL[0], TimeStyle::Language);
    }

    /// The pane shows the stored locale and follows a change made elsewhere.
    #[test]
    fn the_pane_follows_the_locale() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        cx.settings::<Locale>()
            .set(&Locale {
                currency: Currency::EUR,
                ..Locale::default()
            })
            .unwrap();
        let mut pane = RegionPane::new(cx.clone(), None);
        let paths = Locale::paths();
        let currency = pane
            .locale
            .shown(&paths.currency, |currency| currency.code().to_owned());
        assert_eq!(currency.get(), "EUR");
        let measurement = pane
            .locale
            .shown(&paths.measurement, |measure| measure_label(*measure));
        cx.settings::<Locale>()
            .update(|locale| locale.measurement = Measurement::Imperial)
            .unwrap();
        let doc = ui::automerge::AutoCommit::new();
        pane.sync(&doc, &ui::automerge::ROOT);
        assert_eq!(pane.locale().measurement, Measurement::Imperial);
        assert_eq!(measurement.get(), measure_label(Measurement::Imperial));
    }
}
