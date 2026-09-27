//! WMO weather-code interpretation.
//!
//! Open-Meteo reports conditions as WMO 4677 weather codes (`weather_code`).
//! We collapse the ~28 distinct codes into a small [`Sky`] enum that drives
//! both the human-readable label and the vector icon we draw in `icon.rs`.
//! Kept deliberately code-table-driven so the mapping is auditable against
//! the published WMO table.

/// Coarse sky/condition family used for iconography + theming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sky {
    Clear,
    MainlyClear,
    PartlyCloudy,
    Overcast,
    Fog,
    Drizzle,
    Rain,
    FreezingRain,
    Snow,
    Showers,
    SnowShowers,
    Thunder,
    ThunderHail,
}

impl Sky {
    /// Map a raw WMO code to a [`Sky`] family.
    pub fn from_code(code: i64) -> Sky {
        match code {
            0 => Sky::Clear,
            1 => Sky::MainlyClear,
            3 => Sky::Overcast,
            45 | 48 => Sky::Fog,
            51 | 53 | 55 => Sky::Drizzle,
            56 | 57 | 66 | 67 => Sky::FreezingRain,
            61 | 63 | 65 => Sky::Rain,
            71 | 73 | 75 | 77 => Sky::Snow,
            80..=82 => Sky::Showers,
            85 | 86 => Sky::SnowShowers,
            95 => Sky::Thunder,
            96 | 99 => Sky::ThunderHail,
            // 2 ("partly cloudy") and any unrecognised code both default here.
            _ => Sky::PartlyCloudy,
        }
    }

    /// Full human-readable label for the exact code (more specific than
    /// the family — e.g. "Heavy rain" vs "Slight rain").
    pub fn label(code: i64) -> &'static str {
        match code {
            0 => "Clear sky",
            1 => "Mainly clear",
            2 => "Partly cloudy",
            3 => "Overcast",
            45 => "Fog",
            48 => "Rime fog",
            51 => "Light drizzle",
            53 => "Drizzle",
            55 => "Dense drizzle",
            56 => "Freezing drizzle",
            57 => "Dense freezing drizzle",
            61 => "Slight rain",
            63 => "Rain",
            65 => "Heavy rain",
            66 => "Freezing rain",
            67 => "Heavy freezing rain",
            71 => "Slight snow",
            73 => "Snow",
            75 => "Heavy snow",
            77 => "Snow grains",
            80 => "Light showers",
            81 => "Showers",
            82 => "Violent showers",
            85 => "Slight snow showers",
            86 => "Snow showers",
            95 => "Thunderstorm",
            96 => "Thunderstorm, hail",
            99 => "Severe thunderstorm",
            _ => "Unknown",
        }
    }

    /// Whether this sky family involves falling precipitation — used to
    /// decide whether the icon should draw rain/snow strokes.
    pub fn is_wet(&self) -> bool {
        matches!(
            self,
            Sky::Drizzle
                | Sky::Rain
                | Sky::FreezingRain
                | Sky::Snow
                | Sky::Showers
                | Sky::SnowShowers
                | Sky::Thunder
                | Sky::ThunderHail
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_codes_map_to_families_and_wetness() {
        // Case `known_codes_map_to_expected_families`.
        {
            assert_eq!(Sky::from_code(0), Sky::Clear);
            assert_eq!(Sky::from_code(3), Sky::Overcast);
            assert_eq!(Sky::from_code(63), Sky::Rain);
            assert_eq!(Sky::from_code(75), Sky::Snow);
            assert_eq!(Sky::from_code(95), Sky::Thunder);
            assert_eq!(Sky::from_code(96), Sky::ThunderHail);
        }
        // Case `unknown_code_is_safe_default`.
        {
            assert_eq!(Sky::from_code(12345), Sky::PartlyCloudy);
            assert_eq!(Sky::label(12345), "Unknown");
        }
        // Case `wet_classification`.
        {
            assert!(Sky::from_code(65).is_wet());
            assert!(!Sky::from_code(0).is_wet());
            assert!(!Sky::from_code(2).is_wet());
        }
    }
}
