//! The shared **region & formats store** — Tilo's global per-user locale
//! preferences.
//!
//! # Why Settings owns this
//!
//! Many apps need the same answer to "how should I show money / dates /
//! times / units for this user?". Rather than each app inventing its own
//! preference file, they all read and write **one** JSON document that the
//! Settings app's "Region & Formats" panel edits:
//!
//! ```text
//! $XDG_CONFIG_HOME/tilo/settings.json   (falls back to ~/.config/tilo/settings.json)
//! ```
//!
//! The file is a plain JSON object, namespaced by domain so unrelated apps
//! can grow their own sections without colliding. The section this store
//! owns is `locale`:
//!
//! ```json
//! {
//!   "version": 1,
//!   "locale": {
//!     "currency": "USD",
//!     "date_format": "Iso",
//!     "time_format": "H24",
//!     "measurement": "Metric"
//!   }
//! }
//! ```
//!
//! The string values are the serde-derived variant names of the enums
//! below (`"Iso" | "Us" | "Eu" | "Long"`, `"H24" | "H12"`,
//! `"Metric" | "Imperial"`, ISO-4217 currency codes). This is the same
//! shape the Sheets app's `settings::Locale` and the Weather app's unit
//! toggle read — Settings is the canonical editor, the others are
//! consumers.
//!
//! # Non-destructive writes
//!
//! Reads are forgiving: a missing file, missing section, or unknown enum
//! string falls back to the built-in default. Writes are **merge** writes:
//! [`RegionSettings::to_json_merged`] preserves every key an app doesn't
//! understand, so two apps with different schemas never clobber each
//! other.
//!
//! This is a file/format contract, not a crate dependency — Settings,
//! Sheets, and Weather read the *same* file with the *same* keys. The
//! structs here are the reference implementation; the enum reprs are
//! pinned by the tests so they can never silently drift from Sheets.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// World currency, by ISO-4217 code. The serde repr is the bare variant
/// name, which equals the ISO code — matching Sheets' `Currency`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

    pub fn label(self) -> &'static str {
        match self {
            TimeStyle::H24 => "24-hour (14:05)",
            TimeStyle::H12 => "12-hour (2:05 PM)",
        }
    }
}

/// Measurement system. serde repr matches Sheets' / Weather's `Measurement`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegionSettings {
    pub locale: Locale,
}

impl RegionSettings {
    /// Resolve the shared store path:
    /// `$XDG_CONFIG_HOME/tilo/settings.json`, falling back to
    /// `~/.config/tilo/settings.json`. `None` only if no home dir exists.
    pub fn store_path() -> Option<PathBuf> {
        settings_store::settings_path()
    }

    /// Parse from a JSON string, falling back to defaults for anything
    /// missing or malformed. Never fails.
    pub fn from_json(text: &str) -> Self {
        let root: serde_json::Value = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
        let locale = root
            .get("locale")
            .cloned()
            .and_then(|v| serde_json::from_value::<Locale>(v).ok())
            .unwrap_or_default();
        Self { locale }
    }

    /// Serialize to pretty JSON, **merging** into any existing on-disk
    /// document so foreign keys survive. `existing` is the current file
    /// contents (or empty).
    pub fn to_json_merged(&self, existing: &str) -> String {
        let mut obj = match serde_json::from_str::<serde_json::Value>(existing) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        obj.insert("version".into(), serde_json::json!(1));
        obj.insert(
            "locale".into(),
            serde_json::to_value(self.locale).unwrap_or(serde_json::Value::Null),
        );
        serde_json::to_string_pretty(&obj).unwrap_or_else(|_| "{}".into())
    }

    /// Load from the shared path. Returns defaults if the file is absent.
    pub fn load() -> Self {
        let Some(path) = Self::store_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_json(&text),
            Err(_) => Self::default(),
        }
    }

    /// Persist to the shared path, preserving foreign keys. Creates the
    /// `tilo` config directory if needed. Returns the path written.
    ///
    /// # Errors
    ///
    /// Returns an error if no config directory can be resolved (`no config
    /// dir`), or if creating the parent directory or writing the JSON file
    /// fails (permissions, missing/read-only filesystem, etc).
    pub fn save(&self) -> std::io::Result<PathBuf> {
        let path = Self::store_path().ok_or_else(|| std::io::Error::other("no config dir"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        std::fs::write(&path, self.to_json_merged(&existing))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The region section's defaults, its cross-app JSON contract, and its
    /// round-trip through the shared settings file.
    #[test]
    fn region_settings_default_and_round_trip_the_cross_app_contract() {
        let r = RegionSettings::default();
        assert_eq!(r.locale.currency, Currency::USD);
        assert_eq!(r.locale.date_format, DateStyle::Iso);
        assert_eq!(r.locale.time_format, TimeStyle::H24);
        assert_eq!(r.locale.measurement, Measurement::Metric);

        let region = |currency, date_format, time_format, measurement| RegionSettings {
            locale: Locale {
                currency,
                date_format,
                time_format,
                measurement,
                ..Default::default()
            },
        };
        let json = region(
            Currency::EUR,
            DateStyle::Eu,
            TimeStyle::H12,
            Measurement::Imperial,
        )
        .to_json_merged("");
        // These exact strings are the cross-app contract Sheets reads. If serde
        // ever renamed a variant this test would catch the drift.
        for want in [
            "\"currency\": \"EUR\"",
            "\"date_format\": \"EU\"",
            "\"time_format\": \"H12\"",
            "\"measurement\": \"Imperial\"",
            "\"version\": 1",
        ] {
            assert!(json.contains(want), "{want} missing from {json}");
        }

        let r = region(
            Currency::JPY,
            DateStyle::Long,
            TimeStyle::H24,
            Measurement::Metric,
        );
        assert_eq!(RegionSettings::from_json(&r.to_json_merged("")), r);

        let existing = r#"{"version":1,"theme":{"name":"dracula"},"locale":{"currency":"USD","date_format":"Iso","time_format":"H24","measurement":"Metric"}}"#;
        let r = RegionSettings {
            locale: Locale {
                currency: Currency::GBP,
                ..Default::default()
            },
        };
        let out = r.to_json_merged(existing);
        assert!(
            out.contains("\"currency\": \"GBP\""),
            "our edit landed: {out}"
        );
        assert!(
            out.contains("dracula"),
            "the unrelated `theme` section survived: {out}"
        );
    }

    /// Missing, unparsable or partially invalid documents degrade to defaults
    /// rather than erroring.
    #[test]
    fn region_settings_degrade_to_defaults() {
        for input in ["", "not json at all {"] {
            assert_eq!(
                RegionSettings::from_json(input),
                RegionSettings::default(),
                "{input:?}"
            );
        }
        // The whole locale section fails to parse (XXX isn't a Currency).
        let r = RegionSettings::from_json(
            r#"{"locale":{"currency":"XXX","date_format":"Iso","time_format":"H24","measurement":"Metric"}}"#,
        );
        assert_eq!(r.locale, Locale::default());
    }
}
