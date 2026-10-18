//! The person's **region & formats**: the `locale` section of their
//! settings, which every app follows and the Settings app's "Region &
//! Formats" panel edits.
//!
//! ```json
//! "locale": {
//!   "currency": "USD",
//!   "date_format": "ISO",
//!   "time_format": "H24",
//!   "measurement": "Metric"
//! }
//! ```
//!
//! The string values are the serde variant names of the enums below and of
//! [`TimeStyle`], which the toolkit owns so every clock display follows it. Reads
//! are forgiving: a missing section or an unknown value falls back to the
//! built-in default, key by key.

use serde::{Deserialize, Serialize};
use ui::clock::TimeStyle;
/// The formats every app shows dates and numbers in are the toolkit's
/// ([`ui::format::region`]), so the formatter and this section share them.
pub use ui::format::{DateStyle, FirstDayOfWeek, NumberStyle};
use ui::tilo_resource;

/// World currency, by ISO-4217 code. Each carries its own symbol,
/// placement, and conventional number of decimals — `$1,234.50` vs
/// `1.234,50 €` vs `¥1,235` (yen has no minor unit). The serde repr is the
/// bare variant name, which equals the ISO code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
pub enum Currency {
    #[model(rename = "USD")]
    USD,
    #[model(rename = "EUR")]
    EUR,
    #[model(rename = "GBP")]
    GBP,
    #[model(rename = "JPY")]
    JPY,
    #[model(rename = "CHF")]
    CHF,
    #[model(rename = "CAD")]
    CAD,
    #[model(rename = "AUD")]
    AUD,
    #[model(rename = "CNY")]
    CNY,
    #[model(rename = "INR")]
    INR,
}

impl Currency {
    /// Display symbol (or ISO code where there's no common glyph).
    pub fn symbol(self) -> &'static str {
        match self {
            Currency::USD | Currency::CAD | Currency::AUD => "$",
            Currency::EUR => "\u{20AC}",
            Currency::GBP => "\u{00A3}",
            Currency::JPY | Currency::CNY => "\u{00A5}",
            Currency::CHF => "CHF",
            Currency::INR => "\u{20B9}",
        }
    }

    /// ISO-4217 code — the stable serialised form.
    pub fn code(self) -> &'static str {
        match self {
            Currency::USD => "USD",
            Currency::EUR => "EUR",
            Currency::GBP => "GBP",
            Currency::JPY => "JPY",
            Currency::CHF => "CHF",
            Currency::CAD => "CAD",
            Currency::AUD => "AUD",
            Currency::CNY => "CNY",
            Currency::INR => "INR",
        }
    }

    pub fn from_code(s: &str) -> Option<Currency> {
        Some(match s.to_ascii_uppercase().as_str() {
            "USD" => Currency::USD,
            "EUR" => Currency::EUR,
            "GBP" => Currency::GBP,
            "JPY" => Currency::JPY,
            "CHF" => Currency::CHF,
            "CAD" => Currency::CAD,
            "AUD" => Currency::AUD,
            "CNY" => Currency::CNY,
            "INR" => Currency::INR,
            _ => return None,
        })
    }

    /// Conventional minor-unit digits: 2 for most, 0 for yen.
    pub fn decimals(self) -> usize {
        match self {
            Currency::JPY => 0,
            _ => 2,
        }
    }

    /// Whether the symbol leads the amount (`$5`) or trails it (`5 €`).
    pub fn symbol_leads(self) -> bool {
        !matches!(self, Currency::EUR | Currency::CHF)
    }

    /// Map a bare currency *symbol* (as typed) to a currency. Ambiguous
    /// symbols (`$`, `¥`) resolve to the most common owner.
    pub fn from_symbol(sym: &str) -> Option<Currency> {
        Some(match sym {
            "$" => Currency::USD,
            "\u{20AC}" | "EUR" => Currency::EUR,
            "\u{00A3}" => Currency::GBP,
            "\u{00A5}" => Currency::JPY,
            "CHF" => Currency::CHF,
            "\u{20B9}" => Currency::INR,
            _ => return None,
        })
    }

    /// Every currency the picker offers, in display order.
    pub const ALL: [Currency; 9] = [
        Currency::USD,
        Currency::EUR,
        Currency::GBP,
        Currency::JPY,
        Currency::CHF,
        Currency::CAD,
        Currency::AUD,
        Currency::CNY,
        Currency::INR,
    ];
}

/// Measurement system.
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

/// The `locale` section of the shared store. Every field has a sensible
/// default so a brand-new install needs no file at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(default)]
pub struct Locale {
    pub currency: Currency,
    pub date_format: DateStyle,
    pub time_format: TimeStyle,
    pub measurement: Measurement,
    pub number_style: NumberStyle,
    pub first_day_of_week: FirstDayOfWeek,
}

impl Default for Locale {
    fn default() -> Self {
        Self {
            currency: Currency::USD,
            date_format: DateStyle::Iso,
            time_format: TimeStyle::H24,
            measurement: Measurement::Metric,
            number_style: NumberStyle::DecimalPoint,
            first_day_of_week: FirstDayOfWeek::Monday,
        }
    }
}

impl ui::storage::SettingsSchema for Locale {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("locale");
}

#[ui::tilo_resource::resource(kind = "settings.locale", label = "Region and language")]
impl Locale {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The section's exact strings are what every reader parses; renaming a
    /// variant would silently reset their setting.
    #[test]
    fn the_locale_section_keeps_the_cross_app_shape() {
        let locale = Locale::default();
        assert_eq!(locale.currency, Currency::USD);
        assert_eq!(locale.date_format, DateStyle::Iso);
        assert_eq!(locale.time_format, TimeStyle::H24);
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

    /// A key that does not read keeps its default while the others keep
    /// their stored values.
    #[test]
    fn an_unreadable_key_reads_as_its_default() {
        let profile = tempfile::tempdir().unwrap();
        let path = profile.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"locale":{"currency":"XXX","date_format":"US","time_format":"H24","measurement":"Metric"}}"#,
        )
        .unwrap();
        let locale = ui::storage::Settings::<Locale>::shared()
            .in_file(path)
            .get();
        assert_eq!(locale.currency, Currency::USD);
        assert_eq!(locale.date_format, DateStyle::Us);
    }
}
