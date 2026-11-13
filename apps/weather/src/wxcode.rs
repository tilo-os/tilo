//! Open-Meteo's WMO 4677 weather codes, collapsed into a small [`Sky`] family
//! that drives the label and the glyph; the table can be read against the
//! published one.

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
    pub fn rail_icon(self, is_day: bool) -> ui::icons::Icon {
        use ui::icons::Icon;
        match self {
            Sky::Clear | Sky::MainlyClear if is_day => Icon::Sun,
            Sky::Clear | Sky::MainlyClear => Icon::Moon,
            Sky::PartlyCloudy | Sky::Overcast | Sky::Fog => Icon::Cloud,
            Sky::Snow | Sky::SnowShowers => Icon::CloudSnow,
            Sky::Drizzle
            | Sky::Rain
            | Sky::FreezingRain
            | Sky::Showers
            | Sky::Thunder
            | Sky::ThunderHail => Icon::CloudRain,
        }
    }

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

    /// The message for this exact code, finer than the family: "Heavy rain", not
    /// "rain".
    pub fn label_key(code: i64) -> &'static str {
        match code {
            0 => "weather-sky-clear",
            1 => "weather-sky-mainly-clear",
            2 => "weather-sky-partly-cloudy",
            3 => "weather-sky-overcast",
            45 => "weather-sky-fog",
            48 => "weather-sky-rime-fog",
            51 => "weather-sky-light-drizzle",
            53 => "weather-sky-drizzle",
            55 => "weather-sky-dense-drizzle",
            56 => "weather-sky-freezing-drizzle",
            57 => "weather-sky-dense-freezing-drizzle",
            61 => "weather-sky-slight-rain",
            63 => "weather-sky-rain",
            65 => "weather-sky-heavy-rain",
            66 => "weather-sky-freezing-rain",
            67 => "weather-sky-heavy-freezing-rain",
            71 => "weather-sky-slight-snow",
            73 => "weather-sky-snow",
            75 => "weather-sky-heavy-snow",
            77 => "weather-sky-snow-grains",
            80 => "weather-sky-light-showers",
            81 => "weather-sky-showers",
            82 => "weather-sky-violent-showers",
            85 => "weather-sky-slight-snow-showers",
            86 => "weather-sky-snow-showers",
            95 => "weather-sky-thunderstorm",
            96 => "weather-sky-thunderstorm-hail",
            99 => "weather-sky-severe-thunderstorm",
            _ => "weather-sky-unknown",
        }
    }

    pub fn label(code: i64) -> String {
        crate::i18n::t(Self::label_key(code))
    }

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
        {
            assert_eq!(Sky::from_code(0), Sky::Clear);
            assert_eq!(Sky::from_code(3), Sky::Overcast);
            assert_eq!(Sky::from_code(63), Sky::Rain);
            assert_eq!(Sky::from_code(75), Sky::Snow);
            assert_eq!(Sky::from_code(95), Sky::Thunder);
            assert_eq!(Sky::from_code(96), Sky::ThunderHail);
        }
        {
            assert_eq!(Sky::from_code(12345), Sky::PartlyCloudy);
            assert_eq!(Sky::label_key(12345), "weather-sky-unknown");
        }
        {
            assert!(Sky::from_code(65).is_wet());
            assert!(!Sky::from_code(0).is_wet());
            assert!(!Sky::from_code(2).is_wet());
        }
    }
}
