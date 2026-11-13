//! The person's locale defaults and the per-document overrides layered on them: money, dates,
//! times and measurement. A workbook may override them for everyone editing it;
//! [`EffectiveLocale`] is the two layered.

// `#[derive(Model)]` names the crate as `tilo_resource`.
use ui::tilo_resource::{self, Model};

use tilo_locale::{Currency, DateStyle, Locale};
use ui::clock::TimeStyle;

/// Per-document locale overrides, stored in the workbook so collaborators share them; `None`
/// inherits the global default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Model)]
pub struct DocSettings {
    /// Currency
    pub currency: Option<Currency>,
    /// Date format
    pub date_format: Option<DateStyle>,
    /// Time format
    pub time_format: Option<TimeStyle>,
}

/// The locale a document renders with, the one value the grid and inference consult.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectiveLocale {
    pub currency: Currency,
    pub date_format: DateStyle,
    pub time_format: TimeStyle,
}

impl EffectiveLocale {
    /// The overrides over the globals: a `Some` in `doc` wins.
    pub fn resolve(global: &Locale, doc: &DocSettings) -> Self {
        Self {
            currency: doc.currency.unwrap_or(global.currency),
            date_format: doc.date_format.unwrap_or(global.date_format),
            time_format: doc.time_format.unwrap_or(global.time_format),
        }
    }
}

impl Default for EffectiveLocale {
    fn default() -> Self {
        Self::resolve(&Locale::default(), &DocSettings::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_locale::Measurement;

    #[test]
    fn locale_reads_and_layers_under_the_document() {
        {
            let json = r#"{"currency":"EUR","date_format":"EU","time_format":"H12","measurement":"Imperial"}"#;
            let locale: Locale = serde_json::from_str(json).unwrap();
            assert_eq!(locale.currency, Currency::EUR);
            assert_eq!(locale.date_format, DateStyle::Eu);
            assert_eq!(locale.time_format, TimeStyle::H12);
            assert_eq!(locale.measurement, Measurement::Imperial);
        }

        {
            let locale: Locale = serde_json::from_str(r#"{"currency":"GBP"}"#).unwrap();
            assert_eq!(locale.currency, Currency::GBP);
            assert_eq!(locale.date_format, DateStyle::Language);
            assert_eq!(locale.time_format, TimeStyle::Language);
            assert_eq!(locale.measurement, Measurement::Metric);
        }

        {
            let global = Locale {
                currency: Currency::USD,
                date_format: DateStyle::Iso,
                time_format: TimeStyle::H24,
                measurement: Measurement::Metric,
                ..Locale::default()
            };
            let doc = DocSettings {
                currency: Some(Currency::EUR),
                ..Default::default()
            };
            let eff = EffectiveLocale::resolve(&global, &doc);
            assert_eq!(eff.currency, Currency::EUR); // override wins
            assert_eq!(eff.date_format, DateStyle::Iso); // inherited from global
            assert_eq!(eff.time_format, TimeStyle::H24); // inherited from global
        }

        {
            let global = Locale {
                currency: Currency::GBP,
                date_format: DateStyle::Long,
                time_format: TimeStyle::H12,
                measurement: Measurement::Imperial,
                ..Locale::default()
            };
            let eff = EffectiveLocale::resolve(&global, &DocSettings::default());
            assert_eq!(eff.currency, Currency::GBP);
            assert_eq!(eff.date_format, DateStyle::Long);
            assert_eq!(eff.time_format, TimeStyle::H12);
        }
    }
}
