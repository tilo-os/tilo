//! Region & Formats pane — the editor for the person's **locale**.
//!
//! This panel edits [`tilo_locale::Locale`], the `locale` section of the
//! person's settings, which every app's toolkit follows. The Sheets app reads
//! `locale.currency` / `locale.date_format` / `locale.time_format` for its
//! cell formatting, and the Weather app reads `locale.measurement` for its
//! °C/°F + km/h vs mph toggle. Editing here is the single source of truth
//! those apps follow.
//!
//! Each control is a `Dropdown` bound to a [`RegionEffect`] that writes the
//! section; the pane shows what the section holds, whoever changed it.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx};
use ui::theme::Theme;
use ui::tilo_resource;
use ui::widget::{AppModel, Dropdown, ModelPath, Widget};

use crate::effects::effect;
use crate::host::{Followed, PaneCx};
use crate::i18n::t;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{caption, card, rows};
use tilo_locale::{
    Currency, DateStyle, FirstDayOfWeek, Locale, Measurement, NumberStyle, TimeStyle,
};

fn date_label(style: DateStyle) -> String {
    t(match style {
        DateStyle::Iso => "settings-region-date-iso",
        DateStyle::Us => "settings-region-date-us",
        DateStyle::Eu => "settings-region-date-eu",
        DateStyle::Long => "settings-region-date-long",
    })
}

fn date_from_shown(label: &str) -> Option<DateStyle> {
    DateStyle::ALL
        .iter()
        .copied()
        .find(|style| style.label() == label || date_label(*style) == label)
}

fn time_label(style: TimeStyle) -> String {
    t(match style {
        TimeStyle::H24 => "settings-region-time-24",
        TimeStyle::H12 => "settings-region-time-12",
    })
}

fn time_from_shown(label: &str) -> Option<TimeStyle> {
    TimeStyle::ALL
        .iter()
        .copied()
        .find(|style| style.label() == label || time_label(*style) == label)
}

fn measure_label(measure: Measurement) -> String {
    t(match measure {
        Measurement::Metric => "settings-region-measure-metric",
        Measurement::Imperial => "settings-region-measure-imperial",
    })
}

fn measure_from_shown(label: &str) -> Option<Measurement> {
    Measurement::ALL
        .iter()
        .copied()
        .find(|measure| measure.label() == label || measure_label(*measure) == label)
}

fn weekday_label(day: FirstDayOfWeek) -> String {
    t(match day {
        FirstDayOfWeek::Monday => "settings-region-weekday-monday",
        FirstDayOfWeek::Sunday => "settings-region-weekday-sunday",
        FirstDayOfWeek::Saturday => "settings-region-weekday-saturday",
    })
}

fn weekday_from_shown(label: &str) -> Option<FirstDayOfWeek> {
    FirstDayOfWeek::ALL
        .iter()
        .copied()
        .find(|day| day.label() == label || weekday_label(*day) == label)
}

/// How amounts, dates, times, units and weeks read in every app.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum RegionEffect {
    /// Set the currency amounts are shown in.
    #[effect(idempotent)]
    SetCurrency {
        #[param("The ISO-4217 code", choices = Currency::ALL.map(Currency::code))]
        currency: String,
    },
    /// Set how dates are written.
    #[effect(idempotent)]
    SetDateFormat {
        #[param("The date format", choices = DateStyle::ALL.map(DateStyle::label))]
        format: String,
    },
    /// Set how times are written.
    #[effect(idempotent)]
    SetTimeFormat {
        #[param("The time format", choices = TimeStyle::ALL.map(TimeStyle::label))]
        format: String,
    },
    /// Set the units of measurement.
    #[effect(idempotent)]
    SetMeasurement {
        #[param("The units", choices = Measurement::ALL.map(Measurement::label))]
        units: String,
    },
    /// Set how numbers are written.
    #[effect(idempotent)]
    SetNumberFormat {
        #[param("The number format", choices = NumberStyle::ALL.map(NumberStyle::label))]
        format: String,
    },
    /// Set the day weeks start on.
    #[effect(idempotent)]
    SetFirstDayOfWeek {
        #[param("The day", choices = FirstDayOfWeek::ALL.map(FirstDayOfWeek::label))]
        day: String,
    },
}

/// The variant of `all` whose label is `value`.
fn named<T: Copy>(all: &[T], label: fn(T) -> &'static str, value: &str) -> Result<T, String> {
    all.iter()
        .copied()
        .find(|item| label(*item) == value)
        .ok_or_else(|| format!("`{value}` is not offered"))
}

/// Answer `effect`.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: RegionEffect,
    _cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let host = app.host();
    match effect {
        RegionEffect::SetCurrency { currency } => {
            let currency = Currency::from_code(&currency)
                .ok_or_else(|| format!("`{currency}` is not offered"))?;
            host.update(|locale: &mut Locale| locale.currency = currency)
        }
        RegionEffect::SetDateFormat { format } => {
            let format = named(&DateStyle::ALL, DateStyle::label, &format)?;
            host.update(|locale: &mut Locale| locale.date_format = format)
        }
        RegionEffect::SetTimeFormat { format } => {
            let format = named(&TimeStyle::ALL, TimeStyle::label, &format)?;
            host.update(|locale: &mut Locale| locale.time_format = format)
        }
        RegionEffect::SetMeasurement { units } => {
            let units = named(&Measurement::ALL, Measurement::label, &units)?;
            host.update(|locale: &mut Locale| locale.measurement = units)
        }
        RegionEffect::SetNumberFormat { format } => {
            let format = named(&NumberStyle::ALL, NumberStyle::label, &format)?;
            host.update(|locale: &mut Locale| locale.number_style = format)
        }
        RegionEffect::SetFirstDayOfWeek { day } => {
            let day = named(&FirstDayOfWeek::ALL, FirstDayOfWeek::label, &day)?;
            host.update(|locale: &mut Locale| locale.first_day_of_week = day)
        }
    }
}

/// What the pane's choosers show.
struct Shown {
    currency: LocalState<String>,
    date: LocalState<String>,
    time: LocalState<String>,
    measurement: LocalState<String>,
    number_style: LocalState<String>,
    first_day: LocalState<String>,
}

impl Shown {
    fn of(locale: Locale) -> Self {
        let text = || LocalState::new(String::new());
        let shown = Self {
            currency: text(),
            date: text(),
            time: text(),
            measurement: text(),
            number_style: text(),
            first_day: text(),
        };
        shown.show(locale);
        shown
    }

    fn show(&self, locale: Locale) {
        self.currency.set(locale.currency.code().to_owned());
        self.date.set(date_label(locale.date_format));
        self.time.set(time_label(locale.time_format));
        self.measurement.set(measure_label(locale.measurement));
        self.number_style
            .set(locale.number_style.label().to_owned());
        self.first_day.set(weekday_label(locale.first_day_of_week));
    }
}

pub struct RegionPane {
    cx: PaneCx,
    locale: Followed<Locale>,
    shown: Shown,
    composed: ui::widget::Composed,
}

impl RegionPane {
    /// The pane over `cx`'s locale.
    pub fn new(cx: PaneCx) -> Self {
        let locale = cx.follow::<Locale>();
        let shown = Shown::of(*locale.get());
        Self {
            cx,
            locale,
            shown,
            composed: ui::widget::Composed::default(),
        }
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
        let cx = &self.cx;
        let shown = &self.shown;
        let dropdown = |options: Vec<String>, state: &LocalState<String>, id: &str| {
            let placeholder = state.get();
            Dropdown::from_state(options, state)
                .with_id(id)
                .with_placeholder(&placeholder)
                .fill()
        };

        // Currency dropdown — options are ISO codes (the stored form).
        let currency = cx.bind(
            dropdown(
                Currency::ALL.iter().map(|c| c.code().to_string()).collect(),
                &shown.currency,
                "region.currency",
            ),
            effect(RegionEffect::SET_CURRENCY),
        );
        let date = cx.bind(
            dropdown(
                DateStyle::ALL.iter().copied().map(date_label).collect(),
                &shown.date,
                "region.date",
            ),
            effect(RegionEffect::SET_DATE_FORMAT).map(|shown: String| {
                date_from_shown(&shown).map_or(shown, |style| style.label().to_owned())
            }),
        );
        let time = cx.bind(
            dropdown(
                TimeStyle::ALL.iter().copied().map(time_label).collect(),
                &shown.time,
                "region.time",
            ),
            effect(RegionEffect::SET_TIME_FORMAT).map(|shown: String| {
                time_from_shown(&shown).map_or(shown, |style| style.label().to_owned())
            }),
        );
        let units = cx.bind(
            dropdown(
                Measurement::ALL
                    .iter()
                    .copied()
                    .map(measure_label)
                    .collect(),
                &shown.measurement,
                "region.measurement",
            ),
            effect(RegionEffect::SET_MEASUREMENT).map(|shown: String| {
                measure_from_shown(&shown).map_or(shown, |measure| measure.label().to_owned())
            }),
        );
        let number_style = cx.bind(
            dropdown(
                NumberStyle::ALL
                    .iter()
                    .map(|style| style.label().to_string())
                    .collect(),
                &shown.number_style,
                "region.number_style",
            ),
            effect(RegionEffect::SET_NUMBER_FORMAT),
        );
        let first_day = cx.bind(
            dropdown(
                FirstDayOfWeek::ALL
                    .iter()
                    .copied()
                    .map(weekday_label)
                    .collect(),
                &shown.first_day,
                "region.first_day",
            ),
            effect(RegionEffect::SET_FIRST_DAY_OF_WEEK).map(|shown: String| {
                weekday_from_shown(&shown).map_or(shown, |day| day.label().to_owned())
            }),
        );

        let currency_title = t("settings-region-currency");
        let date_title = t("settings-region-date-format");
        let time_title = t("settings-region-time-format");
        let measure_title = t("settings-region-measurement");
        let number_title = t("settings-region-number-format");
        let first_title = t("settings-region-first-day");
        let formats = card(
            &t("settings-region-formats-card"),
            rows(vec![
                Box::new(ui::widget::Field::new(&currency_title, currency).with_trailing_control()),
                Box::new(ui::widget::Field::new(&date_title, date).with_trailing_control()),
                Box::new(ui::widget::Field::new(&time_title, time).with_trailing_control()),
                Box::new(ui::widget::Field::new(&measure_title, units).with_trailing_control()),
                Box::new(
                    ui::widget::Field::new(&number_title, number_style).with_trailing_control(),
                ),
                Box::new(ui::widget::Field::new(&first_title, first_day).with_trailing_control()),
            ]),
        );

        let amount = loc.number_style.amount(loc.currency.symbol());
        let temp = t(match loc.measurement {
            Measurement::Metric => "settings-region-preview-c",
            Measurement::Imperial => "settings-region-preview-f",
        });
        let week_starts = weekday_label(loc.first_day_of_week);
        let preview = card(
            &t("settings-region-preview-card"),
            rows(vec![
                preview_line(&t("settings-region-preview-amount"), &amount),
                preview_line(
                    &t("settings-region-preview-date"),
                    loc.date_format.example(),
                ),
                preview_line(
                    &t("settings-region-preview-time"),
                    loc.time_format.example(),
                ),
                preview_line(&t("settings-region-preview-temp"), &temp),
                preview_line(&t("settings-region-preview-week"), &week_starts),
            ]),
        );

        let explainer = card(
            &t("settings-region-shared-card"),
            rows(vec![caption(&t("settings-region-shared-body"))]),
        );

        crate::widgets::cards(vec![formats, preview, explainer])
    }
}

impl Widget for RegionPane {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if self.locale.refresh() {
            self.shown.show(*self.locale.get());
            // The preview card follows the values.
            self.composed.invalidate();
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
        "RegionPane"
    }
}

fn preview_line(label: &str, value: &str) -> crate::widgets::El {
    crate::widgets::value_row(label, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    fn set(app: &mut TestApp<SettingsShell>, effect: RegionEffect) {
        app.effect(&SettingsEffect::from(effect))
            .expect("the region effect is answered");
    }

    /// Each format is the person's `locale` section, written in the
    /// cross-app shape Sheets and Weather read; a value outside the offered
    /// ones is refused and changes nothing.
    #[test]
    fn region_effects_write_the_locale() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        set(
            &mut app,
            RegionEffect::SetCurrency {
                currency: "JPY".into(),
            },
        );
        set(
            &mut app,
            RegionEffect::SetDateFormat {
                format: DateStyle::Eu.label().into(),
            },
        );
        set(
            &mut app,
            RegionEffect::SetTimeFormat {
                format: TimeStyle::H12.label().into(),
            },
        );
        set(
            &mut app,
            RegionEffect::SetMeasurement {
                units: Measurement::Imperial.label().into(),
            },
        );
        set(
            &mut app,
            RegionEffect::SetFirstDayOfWeek {
                day: "Sunday".into(),
            },
        );
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

        let refused = RegionEffect::SetCurrency {
            currency: "not-a-currency".into(),
        };
        assert!(app.effect(&SettingsEffect::from(refused)).is_err());
        assert_eq!(app.settings::<Locale>().get(), locale);
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
        let mut pane = RegionPane::new(cx.clone());
        assert_eq!(pane.shown.currency.get(), "EUR");
        cx.settings::<Locale>()
            .update(|locale| locale.measurement = Measurement::Imperial)
            .unwrap();
        let doc = ui::automerge::AutoCommit::new();
        pane.sync(&doc, &ui::automerge::ROOT);
        assert_eq!(pane.locale().measurement, Measurement::Imperial);
        assert_eq!(
            pane.shown.measurement.get(),
            measure_label(Measurement::Imperial)
        );
    }
}
