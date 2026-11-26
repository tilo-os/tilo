//! The person's **region & formats**: the `locale` section of their settings, which
//! every app follows and the Settings app's "Region & Formats" panel edits.
//!
//! ```json
//! "locale": {
//!   "currency": "USD",
//!   "date_format": "Language",
//!   "time_format": "Language",
//!   "measurement": "Metric"
//! }
//! ```
//!
//! The strings are the serde variant names of the enums below and of [`TimeStyle`],
//! which the toolkit owns so every clock display follows it. A missing section or an
//! unknown value falls back to the default, key by key.

use serde::{Deserialize, Serialize};
use ui::clock::TimeStyle;
/// The date, number and week formats are the toolkit's
/// ([`ui::format::region`]), so formatters and this section share them.
pub use ui::format::{DateStyle, FirstDayOfWeek, NumberStyle};
use ui::tilo_resource;

/// The world's currencies, shared with every formatter: [`tilo_formats::Currency`].
pub use tilo_formats::Currency;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default, ui::tilo_resource::Model,
)]
pub enum Measurement {
    #[default]
    Metric,
    Imperial,
}

impl Measurement {
    pub const ALL: [Measurement; 2] = [Measurement::Metric, Measurement::Imperial];

    pub fn label(self) -> &'static str {
        match self {
            Measurement::Metric => "Metric (\u{00B0}C, km, kg)",
            Measurement::Imperial => "Imperial (\u{00B0}F, mi, lb)",
        }
    }
}

/// The `locale` section of the shared store: every field has a default, so a
/// new install needs no file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(default)]
#[model(defaults)]
pub struct Locale {
    /// The currency amounts are shown in.
    pub currency: Currency,
    /// How dates are written, or as the language writes them.
    pub date_format: DateStyle,
    /// A 12- or 24-hour clock, or as the language writes times.
    pub time_format: TimeStyle,
    /// Metric or imperial units.
    pub measurement: Measurement,
    /// Which marks group thousands and separate decimals.
    pub number_style: NumberStyle,
    /// The day a week starts on, in calendars and date pickers.
    pub first_day_of_week: FirstDayOfWeek,
}

impl Default for Locale {
    fn default() -> Self {
        Self {
            currency: Currency::USD,
            date_format: DateStyle::Language,
            time_format: TimeStyle::Language,
            measurement: Measurement::Metric,
            number_style: NumberStyle::DecimalPoint,
            first_day_of_week: FirstDayOfWeek::Monday,
        }
    }
}

impl settings_model::SettingsSchema for Locale {
    const SCOPE: settings_model::Scope = settings_model::Scope::Shared("locale");
}

#[ui::tilo_resource::resource(kind = "settings.locale")]
impl Locale {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_locale_section_keeps_the_cross_app_shape() {
        let locale = Locale::default();
        assert_eq!(locale.currency, Currency::USD);
        assert_eq!(
            locale.date_format,
            DateStyle::Language,
            "dates follow the desktop's language until a format is chosen"
        );
        assert_eq!(
            serde_json::to_value(locale).unwrap()["date_format"],
            "Language"
        );
        assert_eq!(
            locale.time_format,
            TimeStyle::Language,
            "the clock follows the desktop's language until one is chosen"
        );
        assert_eq!(
            serde_json::to_value(locale).unwrap()["time_format"],
            "Language"
        );
        assert_eq!(locale.measurement, Measurement::Metric);

        let locale = Locale {
            currency: Currency::EUR,
            date_format: DateStyle::Eu,
            time_format: TimeStyle::H12,
            measurement: Measurement::Imperial,
            ..Locale::default()
        };
        let section = serde_json::to_value(locale).unwrap();
        assert_eq!(
            section,
            serde_json::json!({
                "currency": "EUR",
                "date_format": "EU",
                "time_format": "H12",
                "measurement": "Imperial",
                "number_style": "DecimalPoint",
                "first_day_of_week": "Monday",
            })
        );
        assert_eq!(serde_json::from_value::<Locale>(section).unwrap(), locale);
    }

    #[test]
    fn an_unreadable_key_reads_as_its_default() {
        let profile = tempfile::tempdir().unwrap();
        ui::storage::SettingsSection::named("locale")
            .in_profile(profile.path())
            .update(
                serde_json::json!({"currency":"XXX","date_format":"US","time_format":"H24","measurement":"Metric"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let locale = ui::storage::Settings::<Locale>::shared()
            .in_profile(profile.path())
            .get();
        assert_eq!(locale.currency, Currency::USD);
        assert_eq!(locale.date_format, DateStyle::Us);
    }
}
