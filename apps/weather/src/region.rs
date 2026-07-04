//! Read-only consumer of the shared qOS region & formats store.
//!
//! Measurement units are a **system preference** owned by the Settings app's
//! "Region & Formats" panel — not something a weather app should host its own
//! toggle for. Settings writes the canonical value; every consumer (Sheets,
//! Weather, …) reads the same file:
//!
//! ```text
//! $XDG_CONFIG_HOME/qos/settings.json   (falls back to ~/.config/qos/settings.json)
//! ```
//!
//! The relevant field is `locale.measurement`, a string that is either
//! `"Metric"` or `"Imperial"`. Weather derives its temperature/wind display
//! units from it and converts snapshots on the fly (a snapshot always records
//! the units it was fetched in). Reads are forgiving: a missing file, missing
//! key, or unknown value falls back to metric.
//!
//! This is a file/format contract, not a crate dependency — the string values
//! match the Settings app's `Measurement` enum reprs byte-for-byte.

use std::path::PathBuf;

use crate::units::Units;

/// The per-user qOS config root: `$XDG_CONFIG_HOME/qos` or `~/.config/qos`.
fn config_root() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("qos"))
}

/// Absolute path of the shared settings document.
fn store_path() -> Option<PathBuf> {
    config_root().map(|r| r.join("settings.json"))
}

/// Map a `locale.measurement` string to unit preferences. `"Imperial"` →
/// °F + mph; anything else (including `"Metric"` and unknown) → °C + km/h.
pub fn units_from_measurement(measurement: &str) -> Units {
    match measurement {
        "Imperial" => Units::IMPERIAL,
        _ => Units::METRIC,
    }
}

/// Parse the display units out of a `settings.json` body. Forgiving — a
/// malformed document or a missing `locale.measurement` yields metric.
pub fn units_from_settings_json(text: &str) -> Units {
    let measurement = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .as_ref()
        .and_then(|v| v.get("locale"))
        .and_then(|l| l.get("measurement"))
        .and_then(|m| m.as_str())
        .unwrap_or("Metric")
        .to_string();
    units_from_measurement(&measurement)
}

/// Load the user's measurement preference from the shared store. Returns
/// metric when the file is absent or unreadable — never fails, never panics.
pub fn load_units() -> Units {
    let Some(path) = store_path() else {
        return Units::METRIC;
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => units_from_settings_json(&text),
        Err(_) => Units::METRIC,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::{TempUnit, WindUnit};

    #[test]
    fn imperial_measurement_maps_to_fahrenheit_and_mph() {
        let u = units_from_measurement("Imperial");
        assert_eq!(u.temp, TempUnit::Fahrenheit);
        assert_eq!(u.wind, WindUnit::Mph);
    }

    #[test]
    fn metric_and_unknown_map_to_celsius_and_kmh() {
        assert_eq!(units_from_measurement("Metric"), Units::METRIC);
        assert_eq!(units_from_measurement("nonsense"), Units::METRIC);
        assert_eq!(units_from_measurement(""), Units::METRIC);
    }

    #[test]
    fn reads_the_shared_settings_shape() {
        // The exact document Settings writes for an imperial user.
        let doc = r#"{
            "version": 1,
            "locale": {
                "currency": "USD",
                "date_format": "US",
                "time_format": "H12",
                "measurement": "Imperial"
            }
        }"#;
        assert_eq!(units_from_settings_json(doc), Units::IMPERIAL);

        let metric = r#"{"locale":{"measurement":"Metric"}}"#;
        assert_eq!(units_from_settings_json(metric), Units::METRIC);
    }

    #[test]
    fn missing_or_malformed_falls_back_to_metric() {
        assert_eq!(units_from_settings_json(""), Units::METRIC);
        assert_eq!(units_from_settings_json("not json {"), Units::METRIC);
        assert_eq!(units_from_settings_json("{}"), Units::METRIC);
        assert_eq!(units_from_settings_json(r#"{"locale":{}}"#), Units::METRIC);
    }
}
