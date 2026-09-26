//! Unit preferences and conversions.
//!
//! Open-Meteo can return data directly in the requested units (we pass
//! `temperature_unit` / `wind_speed_unit` on every request), but the
//! bundled sample snapshot is stored in **metric** (°C, km/h) and the
//! CRDT caches whatever the last fetch produced. To keep rendering
//! honest regardless of how a snapshot was sourced, snapshots always
//! carry a `source_units` tag and the UI converts on the fly via the
//! helpers here. That makes the °C/°F and km/h vs mph toggle instant —
//! no refetch required.

use serde::{Deserialize, Serialize};

/// Temperature display unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TempUnit {
    Celsius,
    Fahrenheit,
}

/// Wind-speed display unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindUnit {
    Kmh,
    Mph,
}

/// The combined unit preference, persisted in the CRDT.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Units {
    pub temp: TempUnit,
    pub wind: WindUnit,
}

impl Default for Units {
    fn default() -> Self {
        Self {
            temp: TempUnit::Celsius,
            wind: WindUnit::Kmh,
        }
    }
}

impl Units {
    /// Metric preset (°C, km/h) — what the sample data is stored in.
    pub const METRIC: Units = Units {
        temp: TempUnit::Celsius,
        wind: WindUnit::Kmh,
    };
    /// Imperial preset (°F, mph).
    pub const IMPERIAL: Units = Units {
        temp: TempUnit::Fahrenheit,
        wind: WindUnit::Mph,
    };

    /// Open-Meteo `temperature_unit` query value.
    pub fn temp_api(&self) -> &'static str {
        match self.temp {
            TempUnit::Celsius => "celsius",
            TempUnit::Fahrenheit => "fahrenheit",
        }
    }

    /// Open-Meteo `wind_speed_unit` query value.
    pub fn wind_api(&self) -> &'static str {
        match self.wind {
            WindUnit::Kmh => "kmh",
            WindUnit::Mph => "mph",
        }
    }

    pub fn temp_suffix(&self) -> &'static str {
        match self.temp {
            TempUnit::Celsius => "°C",
            TempUnit::Fahrenheit => "°F",
        }
    }

    /// Compact degree glyph for the big readout (no unit letter).
    pub fn degree_glyph(&self) -> &'static str {
        "°"
    }

    pub fn wind_suffix(&self) -> &'static str {
        match self.wind {
            WindUnit::Kmh => "km/h",
            WindUnit::Mph => "mph",
        }
    }
}

/// Convert a temperature between display units. Stored values are
/// tagged with their source unit on the snapshot.
pub fn convert_temp(value: f64, from: TempUnit, to: TempUnit) -> f64 {
    if from == to {
        return value;
    }
    match (from, to) {
        (TempUnit::Celsius, TempUnit::Fahrenheit) => value * 9.0 / 5.0 + 32.0,
        (TempUnit::Fahrenheit, TempUnit::Celsius) => (value - 32.0) * 5.0 / 9.0,
        _ => value,
    }
}

/// Convert a wind speed between display units.
pub fn convert_wind(value: f64, from: WindUnit, to: WindUnit) -> f64 {
    if from == to {
        return value;
    }
    match (from, to) {
        (WindUnit::Kmh, WindUnit::Mph) => value * 0.621_371,
        (WindUnit::Mph, WindUnit::Kmh) => value / 0.621_371,
        _ => value,
    }
}

/// Map a wind bearing in degrees to a compass abbreviation.
pub fn compass(deg: f64) -> &'static str {
    const DIRS: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    let idx = (((deg % 360.0) + 360.0) % 360.0 / 22.5).round() as usize % 16;
    DIRS.get(idx).copied().unwrap_or("N")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_conversions_and_compass_cardinals() {
        // Case `celsius_fahrenheit_round_trips`.
        {
            let f = convert_temp(20.0, TempUnit::Celsius, TempUnit::Fahrenheit);
            assert!((f - 68.0).abs() < 1e-6);
            let c = convert_temp(f, TempUnit::Fahrenheit, TempUnit::Celsius);
            assert!((c - 20.0).abs() < 1e-6);
        }
        // Case `wind_kmh_to_mph`.
        {
            let mph = convert_wind(100.0, WindUnit::Kmh, WindUnit::Mph);
            assert!((mph - 62.1371).abs() < 1e-3);
        }
        // Case `compass_cardinals`.
        {
            assert_eq!(compass(0.0), "N");
            assert_eq!(compass(90.0), "E");
            assert_eq!(compass(180.0), "S");
            assert_eq!(compass(270.0), "W");
            assert_eq!(compass(360.0), "N");
        }
    }
}
