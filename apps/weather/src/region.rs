//! Read-only consumer of the shared Tilo region & formats store.
//!
//! Measurement units are a **system preference** owned by the Settings app's
//! "Region & Formats" panel — not something a weather app should host its own
//! toggle for. Settings writes the canonical value; every consumer (Sheets,
//! Weather, …) reads the same file:
//!
//! ```text
//! $XDG_CONFIG_HOME/tilo/settings.json   (falls back to ~/.config/tilo/settings.json)
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

use crate::units::Units;

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
    let locale = ui::storage::Settings::section("locale");
    units_from_measurement(locale.string("measurement").as_deref().unwrap_or("Metric"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::{TempUnit, WindUnit};

    #[test]
    fn region_settings_map_measurement_system_to_units() {
        // Case `imperial_measurement_maps_to_fahrenheit_and_mph`.
        {
            let u = units_from_measurement("Imperial");
            assert_eq!(u.temp, TempUnit::Fahrenheit);
            assert_eq!(u.wind, WindUnit::Mph);
        }
        // Case `metric_and_unknown_map_to_celsius_and_kmh`.
        {
            assert_eq!(units_from_measurement("Metric"), Units::METRIC);
            assert_eq!(units_from_measurement("nonsense"), Units::METRIC);
            assert_eq!(units_from_measurement(""), Units::METRIC);
        }
        // Case `reads_the_shared_settings_shape`.
        {
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
        // Case `missing_or_malformed_falls_back_to_metric`.
        {
            assert_eq!(units_from_settings_json(""), Units::METRIC);
            assert_eq!(units_from_settings_json("not json {"), Units::METRIC);
            assert_eq!(units_from_settings_json("{}"), Units::METRIC);
            assert_eq!(units_from_settings_json(r#"{"locale":{}}"#), Units::METRIC);
        }
    }
}
