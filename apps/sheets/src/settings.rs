//! Shared per-user global settings + per-document overrides.
//!
//! # The shared store
//!
//! Tilo apps need a single place to read user-wide locale defaults — the
//! currency to assume for money, how to render dates and times, and which
//! measurement system to use. Rather than each app inventing its own
//! preference file, they all read and write **one** JSON document:
//!
//! ```text
//! $XDG_CONFIG_HOME/tilo/settings.json   (falls back to ~/.config/tilo/settings.json)
//! ```
//!
//! The file is a plain JSON object, namespaced by domain so that unrelated
//! apps can grow their own sections without colliding. Today the only
//! section is `locale`:
//!
//! ```json
//! {
//!   "version": 1,
//!   "locale": {
//!     "currency": "USD",
//!     "date_format": "ISO",
//!     "time_format": "H24",
//!     "measurement": "Metric"
//!   }
//! }
//! ```
//!
//! Reads are forgiving: a missing file, missing section, or unknown enum
//! string all fall back to the built-in default rather than erroring.
//! Writes are **non-destructive** — any keys an app doesn't understand are
//! preserved verbatim across a save, so two apps with different schemas
//! never clobber each other.
//!
//! This is deliberately a file/format contract, not a Rust crate
//! dependency: the settings app and the weather app (whose unit toggle
//! should follow `locale.measurement`) read the *same* file with the
//! *same* keys. The structs below are the reference implementation of that
//! contract; a future shared crate can host an identical copy.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::model::{Currency, DateStyle, TimeStyle};

fn locale() -> ui::storage::SettingsSection {
    ui::storage::SettingsSection::named("locale")
}

/// Where the shared per-user settings live.
pub fn settings_path() -> Option<PathBuf> {
    locale().path()
}

/// Measurement system. Sheets doesn't use this directly, but it is part of
/// the shared `locale` contract (weather's °C/°F + km/h vs mph toggle reads
/// it), so it round-trips through this struct untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Measurement {
    #[default]
    Metric,
    Imperial,
}

/// The `locale` section of the shared store. Every field has a sensible
/// default so a brand-new install needs no file at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Locale {
    pub currency: Currency,
    pub date_format: DateStyle,
    pub time_format: TimeStyle,
    pub measurement: Measurement,
}

impl Default for Locale {
    fn default() -> Self {
        Self {
            currency: Currency::USD,
            date_format: DateStyle::Iso,
            time_format: TimeStyle::H24,
            measurement: Measurement::Metric,
        }
    }
}

/// The shared global-settings document. `extra` captures any JSON keys
/// outside our schema so we can write them back unchanged.
#[derive(Clone, Debug, Default)]
pub struct GlobalSettings {
    pub locale: Locale,
}

impl GlobalSettings {
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
        let mut root: serde_json::Value =
            serde_json::from_str(existing).unwrap_or(serde_json::Value::Null);
        if !root.is_object() {
            root = serde_json::json!({});
        }
        if let Some(obj) = root.as_object_mut() {
            obj.insert("version".into(), serde_json::json!(1));
            obj.insert(
                "locale".into(),
                serde_json::to_value(self.locale).unwrap_or(serde_json::Value::Null),
            );
        }
        serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".into())
    }

    /// Load from the shared settings document (on the web, the page's
    /// profile). Defaults for anything absent.
    pub fn load() -> Self {
        Self::from_json(&serde_json::Value::Object(locale().document()).to_string())
    }

    /// Persist the `locale` section to the shared settings document,
    /// preserving every other section.
    ///
    /// # Errors
    ///
    /// Returns an error if there is no settings document, or the write
    /// fails.
    pub fn save(&self) -> std::io::Result<()> {
        match serde_json::to_value(self.locale).map_err(std::io::Error::other)? {
            serde_json::Value::Object(values) => locale().update(values),
            _ => Err(std::io::Error::other(
                "the locale is not a settings section",
            )),
        }
    }
}

/// Per-document locale overrides. `None` on a field means "inherit the
/// global default". Stored in the workbook's CRDT so collaborators share
/// the document's conventions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocSettings {
    pub currency: Option<Currency>,
    pub date_format: Option<DateStyle>,
    pub time_format: Option<TimeStyle>,
}

/// The effective locale a document renders with: per-document overrides
/// layered on top of the global defaults. This is the single value the
/// grid and inference consult — neither layer is read in isolation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectiveLocale {
    pub currency: Currency,
    pub date_format: DateStyle,
    pub time_format: TimeStyle,
}

impl EffectiveLocale {
    /// Resolve overrides over globals. A `Some` in `doc` wins; otherwise
    /// the global value is used.
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

    /// Settings defaults, locale parsing, unknown keys, malformed fallback and document layering.
    #[test]
    fn settings_load_layer_and_round_trip() {
        // Case `defaults_when_file_missing_or_empty`.
        {
            let g = GlobalSettings::from_json("");
            assert_eq!(g.locale.currency, Currency::USD);
            assert_eq!(g.locale.date_format, DateStyle::Iso);
            assert_eq!(g.locale.measurement, Measurement::Metric);
        }

        // Case `parses_locale_section`.
        {
            let json = r#"{"version":1,"locale":{"currency":"EUR","date_format":"EU","time_format":"H12","measurement":"Imperial"}}"#;
            let g = GlobalSettings::from_json(json);
            assert_eq!(g.locale.currency, Currency::EUR);
            assert_eq!(g.locale.date_format, DateStyle::Eu);
            assert_eq!(g.locale.time_format, TimeStyle::H12);
            assert_eq!(g.locale.measurement, Measurement::Imperial);
        }

        // Case `unknown_keys_round_trip_intact`.
        {
            // A foreign app stored a "weather" section; saving sheets settings
            // must not drop it.
            let existing =
                r#"{"version":1,"weather":{"city":"Berlin"},"locale":{"currency":"GBP"}}"#;
            let mut g = GlobalSettings::from_json(existing);
            g.locale.currency = Currency::JPY;
            let out = g.to_json_merged(existing);
            let v: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert_eq!(v["weather"]["city"], "Berlin");
            assert_eq!(v["locale"]["currency"], "JPY");
        }

        // Case `malformed_json_falls_back`.
        {
            let g = GlobalSettings::from_json("{not json");
            assert_eq!(g.locale, Locale::default());
        }

        // Case `doc_override_layers_over_global`.
        {
            let global = Locale {
                currency: Currency::USD,
                date_format: DateStyle::Iso,
                time_format: TimeStyle::H24,
                measurement: Measurement::Metric,
            };
            // Document overrides only the currency.
            let doc = DocSettings {
                currency: Some(Currency::EUR),
                ..Default::default()
            };
            let eff = EffectiveLocale::resolve(&global, &doc);
            assert_eq!(eff.currency, Currency::EUR); // override wins
            assert_eq!(eff.date_format, DateStyle::Iso); // inherited from global
            assert_eq!(eff.time_format, TimeStyle::H24); // inherited from global
        }

        // Case `empty_doc_settings_inherit_everything`.
        {
            let global = Locale {
                currency: Currency::GBP,
                date_format: DateStyle::Long,
                time_format: TimeStyle::H12,
                measurement: Measurement::Imperial,
            };
            let eff = EffectiveLocale::resolve(&global, &DocSettings::default());
            assert_eq!(eff.currency, Currency::GBP);
            assert_eq!(eff.date_format, DateStyle::Long);
            assert_eq!(eff.time_format, TimeStyle::H12);
        }
    }
}
