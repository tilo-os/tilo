//! Display units and conversions.
//!
//! A snapshot records the units it was fetched in and the UI converts at draw
//! time, so the °C/°F and km/h/mph toggle is instant and never refetches. Rain
//! has no request parameter, so it is always converted here.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TempUnit {
    Celsius,
    Fahrenheit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindUnit {
    Kmh,
    Mph,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrecipUnit {
    Millimetres,
    Inches,
}

impl PrecipUnit {
    /// The digits a reading shows: tenths of a millimetre, hundredths of an inch.
    pub fn decimals(self) -> usize {
        match self {
            Self::Millimetres => 1,
            Self::Inches => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Units {
    pub temp: TempUnit,
    pub wind: WindUnit,
    pub precip: PrecipUnit,
}

impl Default for Units {
    fn default() -> Self {
        Self {
            temp: TempUnit::Celsius,
            wind: WindUnit::Kmh,
            precip: PrecipUnit::Millimetres,
        }
    }
}

impl Units {
    /// °C, km/h, mm: what the sample data is stored in.
    pub const METRIC: Units = Units {
        temp: TempUnit::Celsius,
        wind: WindUnit::Kmh,
        precip: PrecipUnit::Millimetres,
    };
    pub const IMPERIAL: Units = Units {
        temp: TempUnit::Fahrenheit,
        wind: WindUnit::Mph,
        precip: PrecipUnit::Inches,
    };

    pub fn temp_api(&self) -> &'static str {
        match self.temp {
            TempUnit::Celsius => "celsius",
            TempUnit::Fahrenheit => "fahrenheit",
        }
    }

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

    pub fn wind_suffix(&self) -> &'static str {
        match self.wind {
            WindUnit::Kmh => "km/h",
            WindUnit::Mph => "mph",
        }
    }

    /// The reading in this unit: `2.4` millimetres, `0.09` inches.
    pub fn precip_reading(&self, value: f64, from: PrecipUnit) -> String {
        let converted = convert_precip(value, from, self.precip);
        format!("{:.*}", self.precip.decimals(), converted)
    }
}

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

pub const MM_PER_INCH: f64 = 25.4;

pub fn convert_precip(value: f64, from: PrecipUnit, to: PrecipUnit) -> f64 {
    if from == to {
        return value;
    }
    match (from, to) {
        (PrecipUnit::Millimetres, PrecipUnit::Inches) => value / MM_PER_INCH,
        (PrecipUnit::Inches, PrecipUnit::Millimetres) => value * MM_PER_INCH,
        _ => value,
    }
}

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
        {
            let f = convert_temp(20.0, TempUnit::Celsius, TempUnit::Fahrenheit);
            assert!((f - 68.0).abs() < 1e-6);
            let c = convert_temp(f, TempUnit::Fahrenheit, TempUnit::Celsius);
            assert!((c - 20.0).abs() < 1e-6);
        }
        {
            let mph = convert_wind(100.0, WindUnit::Kmh, WindUnit::Mph);
            assert!((mph - 62.1371).abs() < 1e-3);
        }
        {
            let inches = convert_precip(25.4, PrecipUnit::Millimetres, PrecipUnit::Inches);
            assert!((inches - 1.0).abs() < 1e-9);
            assert!(
                (convert_precip(1.0, PrecipUnit::Inches, PrecipUnit::Millimetres) - 25.4).abs()
                    < 1e-9
            );
            assert!(
                (convert_precip(3.0, PrecipUnit::Inches, PrecipUnit::Inches) - 3.0).abs() < 1e-12
            );
        }
        {
            assert_eq!(
                Units::METRIC.precip_reading(0.0, PrecipUnit::Millimetres),
                "0.0"
            );
            assert_eq!(
                Units::METRIC.precip_reading(2.44, PrecipUnit::Millimetres),
                "2.4"
            );
            assert_eq!(
                Units::METRIC.precip_reading(187.6, PrecipUnit::Millimetres),
                "187.6"
            );
            assert_eq!(
                Units::IMPERIAL.precip_reading(0.0, PrecipUnit::Millimetres),
                "0.00"
            );
            assert_eq!(
                Units::IMPERIAL.precip_reading(2.44, PrecipUnit::Millimetres),
                "0.10"
            );
            assert_eq!(
                Units::IMPERIAL.precip_reading(187.6, PrecipUnit::Millimetres),
                "7.39"
            );
            assert_eq!(
                Units::IMPERIAL.precip_reading(1.25, PrecipUnit::Inches),
                "1.25"
            );
            assert_eq!(
                Units::METRIC.precip_reading(1.0, PrecipUnit::Inches),
                "25.4"
            );
        }
        {
            assert_eq!(compass(0.0), "N");
            assert_eq!(compass(90.0), "E");
            assert_eq!(compass(180.0), "S");
            assert_eq!(compass(270.0), "W");
            assert_eq!(compass(360.0), "N");
        }
    }
}
