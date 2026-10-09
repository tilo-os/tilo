//! The person's **region & formats** — Tilo's per-user locale, the `locale`
//! section of their settings.
//!
//! Many apps need the same answer to "how should I show money / dates /
//! times / units for this user?". Rather than each app inventing its own
//! preference file, they all read **one** section of the person's settings,
//! which the Settings app's "Region & Formats" panel edits:
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
//! The string values are the serde-derived variant names of the enums
//! below (`"ISO" | "US" | "EU" | "Long"`, `"H24" | "H12"`,
//! `"Metric" | "Imperial"`, ISO-4217 currency codes). This is the same
//! shape the Sheets app's `settings::Locale` and the Weather app's unit
//! toggle read — Settings is the canonical editor, the others are
//! consumers. Reads are forgiving: a missing section or an unknown value
//! falls back to the built-in default, key by key, and a write keeps every
//! key of the section it does not name.
//!
//! This is a format contract, not a crate dependency — Settings, Sheets, and
//! Weather read the *same* section with the *same* keys. The structs here
//! are the reference implementation; the enum reprs are pinned by the tests
//! so they can never silently drift from Sheets.

use serde::{Deserialize, Serialize};
use ui::tilo_resource;

/// World currency, by ISO-4217 code. The serde repr is the bare variant
/// name, which equals the ISO code — matching Sheets' `Currency`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
pub enum Currency {
    USD,
    EUR,
    GBP,
    JPY,
    CHF,
    CAD,
    AUD,
    CNY,
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

/// How dates render. The serde names are the exact strings the shared
/// settings file uses (`ISO`/`US`/`EU`/`Long`), matching Sheets'
/// `DateStyle` so the two apps read one another's writes byte-for-byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
pub enum DateStyle {
    /// `2023-03-15`
    #[serde(rename = "ISO")]
    Iso,
    /// `03/15/2023`
    #[serde(rename = "US")]
    Us,
    /// `15/03/2023`
    #[serde(rename = "EU")]
    Eu,
    /// `15 Mar 2023`
    #[serde(rename = "Long")]
    Long,
}

impl DateStyle {
    pub const ALL: [DateStyle; 4] = [
        DateStyle::Iso,
        DateStyle::Us,
        DateStyle::Eu,
        DateStyle::Long,
    ];

    /// A worked example so the picker shows what each style produces.
    pub fn example(self) -> &'static str {
        match self {
            DateStyle::Iso => "2023-03-15",
            DateStyle::Us => "03/15/2023",
            DateStyle::Eu => "15/03/2023",
            DateStyle::Long => "15 Mar 2023",
        }
    }

    /// Today's date in this style, read through the toolkit clock.
    pub fn today(self) -> String {
        ui::clock::today()
            .format(match self {
                DateStyle::Iso => "%Y-%m-%d",
                DateStyle::Us => "%m/%d/%Y",
                DateStyle::Eu => "%d/%m/%Y",
                DateStyle::Long => "%-d %b %Y",
            })
            .to_string()
    }

    /// User-facing name for the dropdown.
    pub fn label(self) -> &'static str {
        match self {
            DateStyle::Iso => "ISO (2023-03-15)",
            DateStyle::Us => "US (03/15/2023)",
            DateStyle::Eu => "European (15/03/2023)",
            DateStyle::Long => "Long (15 Mar 2023)",
        }
    }
}

/// How times render. serde repr matches Sheets' `TimeStyle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
pub enum TimeStyle {
    /// `14:05`
    H24,
    /// `2:05 PM`
    H12,
}

impl TimeStyle {
    pub const ALL: [TimeStyle; 2] = [TimeStyle::H24, TimeStyle::H12];

    pub fn example(self) -> &'static str {
        match self {
            TimeStyle::H24 => "14:05",
            TimeStyle::H12 => "2:05 PM",
        }
    }

    /// The time now in this style, read through the toolkit clock.
    pub fn now(self) -> String {
        ui::clock::local_now()
            .format(match self {
                TimeStyle::H24 => "%H:%M",
                TimeStyle::H12 => "%-I:%M %p",
            })
            .to_string()
    }

    pub fn label(self) -> &'static str {
        match self {
            TimeStyle::H24 => "24-hour (14:05)",
            TimeStyle::H12 => "12-hour (2:05 PM)",
        }
    }
}

/// Measurement system. serde repr matches Sheets' / Weather's `Measurement`.
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

/// Decimal/grouping convention used by locale-aware consumers. Keeping this
/// separate from currency lets someone use EUR with either continental or
/// English-language formatting.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
pub enum NumberStyle {
    #[default]
    DecimalPoint,
    DecimalComma,
}

impl NumberStyle {
    pub const ALL: [Self; 2] = [Self::DecimalPoint, Self::DecimalComma];

    pub fn label(self) -> &'static str {
        match self {
            Self::DecimalPoint => "1,234,567.89",
            Self::DecimalComma => "1.234.567,89",
        }
    }

    pub fn amount(self, symbol: &str) -> String {
        format!("{symbol}{}", self.label())
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
pub enum FirstDayOfWeek {
    #[default]
    Monday,
    Sunday,
    Saturday,
}

impl FirstDayOfWeek {
    pub const ALL: [Self; 3] = [Self::Monday, Self::Sunday, Self::Saturday];

    pub fn label(self) -> &'static str {
        match self {
            Self::Monday => "Monday",
            Self::Sunday => "Sunday",
            Self::Saturday => "Saturday",
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

/// The shared global-settings document, locale section.
/// The person's locale, which every app's toolkit follows.
impl ui::storage::SettingsSchema for Locale {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("locale");
}

#[ui::tilo_resource::resource(kind = "settings.locale", label = "Region and language")]
impl Locale {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The section's exact strings are the cross-app contract Sheets reads;
    /// if serde ever renamed a variant this test would catch the drift.
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
