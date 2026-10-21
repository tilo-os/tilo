//! Open-Meteo request construction + response parsing.
//!
//! This module is **pure**: it builds URLs and turns JSON strings into
//! model types. The actual HTTP (`tilo_net`) lives in `net.rs` so
//! the parsing can be unit-tested without a network. The `--shots`
//! offscreen path never calls into here at all — it renders bundled
//! sample data (`sample.rs`).
//!
//! Endpoints (no API key required):
//!   * Forecast:  <https://api.open-meteo.com/v1/forecast>
//!   * Geocoding: <https://geocoding-api.open-meteo.com/v1/search>

use serde::Deserialize;

use crate::model::{
    AirQuality, CurrentConditions, DayForecast, HourPoint, LocationId, SavedLocation,
    WeatherSnapshot,
};
use crate::units::Units;

pub const FORECAST_BASE: &str = "https://api.open-meteo.com/v1/forecast";
pub const GEOCODE_BASE: &str = "https://geocoding-api.open-meteo.com/v1/search";
pub const AIR_QUALITY_BASE: &str = "https://air-quality-api.open-meteo.com/v1/air-quality";

/// Build the forecast request URL for a location + unit preference.
pub fn forecast_url(lat: f64, lon: f64, units: Units) -> String {
    format!(
        "{base}?latitude={lat:.4}&longitude={lon:.4}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,precipitation,weather_code,wind_speed_10m,wind_direction_10m,pressure_msl,is_day\
         &hourly=temperature_2m,precipitation_probability,weather_code,is_day\
         &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,sunrise,sunset,wind_speed_10m_max,uv_index_max\
         &timezone=auto&forecast_days=7\
         &temperature_unit={tu}&wind_speed_unit={wu}",
        base = FORECAST_BASE,
        lat = lat,
        lon = lon,
        tu = units.temp_api(),
        wu = units.wind_api(),
    )
}

/// Build the air-quality request URL for a location. AQI + the key pollutants
/// are all unitless indices or µg/m³, so no unit preference is threaded here.
pub fn air_quality_url(lat: f64, lon: f64) -> String {
    format!(
        "{AIR_QUALITY_BASE}?latitude={lat:.4}&longitude={lon:.4}\
         &current=european_aqi,us_aqi,pm2_5,pm10,nitrogen_dioxide,ozone\
         &timezone=auto",
    )
}

/// Build the geocoding search URL.
pub fn geocode_url(query: &str) -> String {
    let encoded = ui::uri::encode_component(query.trim());
    format!("{GEOCODE_BASE}?name={encoded}&count=8&language=en&format=json")
}

// ---------------------------------------------------------------------
// Forecast response
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ForecastResponse {
    current: CurrentRaw,
    hourly: HourlyRaw,
    daily: DailyRaw,
}

#[derive(Debug, Deserialize)]
struct CurrentRaw {
    time: String,
    temperature_2m: f64,
    apparent_temperature: f64,
    relative_humidity_2m: f64,
    precipitation: f64,
    weather_code: i64,
    wind_speed_10m: f64,
    wind_direction_10m: f64,
    #[serde(default)]
    pressure_msl: f64,
    is_day: i64,
}

#[derive(Debug, Deserialize)]
struct HourlyRaw {
    time: Vec<String>,
    temperature_2m: Vec<f64>,
    #[serde(default)]
    precipitation_probability: Vec<Option<f64>>,
    weather_code: Vec<i64>,
    is_day: Vec<i64>,
}

#[derive(Debug, Deserialize)]
struct DailyRaw {
    time: Vec<String>,
    weather_code: Vec<i64>,
    temperature_2m_max: Vec<f64>,
    temperature_2m_min: Vec<f64>,
    #[serde(default)]
    precipitation_probability_max: Vec<Option<f64>>,
    sunrise: Vec<String>,
    sunset: Vec<String>,
    #[serde(default)]
    wind_speed_10m_max: Vec<f64>,
    #[serde(default)]
    uv_index_max: Vec<Option<f64>>,
}

/// Parse a forecast JSON body into a [`WeatherSnapshot`]. The hourly strip
/// is sliced to the 24 hours starting at the current observation time.
///
/// # Errors
/// Returns an error message if `body` doesn't decode as the expected
/// Open-Meteo forecast JSON shape.
pub fn parse_forecast(
    body: &str,
    location_id: LocationId,
    units: Units,
    fetched_at: u64,
) -> Result<WeatherSnapshot, String> {
    let raw: ForecastResponse =
        serde_json::from_str(body).map_err(|e| format!("forecast decode: {e}"))?;

    let uv_today = raw
        .daily
        .uv_index_max
        .first()
        .and_then(|o| *o)
        .unwrap_or(0.0);

    let current = CurrentConditions {
        temperature: raw.current.temperature_2m,
        apparent: raw.current.apparent_temperature,
        humidity: raw.current.relative_humidity_2m,
        precipitation: raw.current.precipitation,
        weather_code: raw.current.weather_code,
        wind_speed: raw.current.wind_speed_10m,
        wind_direction: raw.current.wind_direction_10m,
        pressure: raw.current.pressure_msl,
        uv_index: uv_today,
        is_day: raw.current.is_day != 0,
        time: raw.current.time.clone(),
    };

    // Find the hourly index at/after the current time, take 24 forward.
    let start = raw
        .hourly
        .time
        .iter()
        .position(|t| t.as_str() >= raw.current.time.as_str())
        .unwrap_or(0);
    let mut hourly = Vec::new();
    for i in start..raw.hourly.time.len().min(start + 24) {
        hourly.push(HourPoint {
            time: raw.hourly.time.get(i).cloned().unwrap_or_default(),
            temperature: *raw.hourly.temperature_2m.get(i).unwrap_or(&0.0),
            precip_probability: raw
                .hourly
                .precipitation_probability
                .get(i)
                .and_then(|o| *o)
                .unwrap_or(0.0),
            weather_code: *raw.hourly.weather_code.get(i).unwrap_or(&0),
            is_day: raw.hourly.is_day.get(i).copied().unwrap_or(1) != 0,
        });
    }

    let mut daily = Vec::new();
    for i in 0..raw.daily.time.len() {
        daily.push(DayForecast {
            date: raw.daily.time.get(i).cloned().unwrap_or_default(),
            temp_max: *raw.daily.temperature_2m_max.get(i).unwrap_or(&0.0),
            temp_min: *raw.daily.temperature_2m_min.get(i).unwrap_or(&0.0),
            precip_probability: raw
                .daily
                .precipitation_probability_max
                .get(i)
                .and_then(|o| *o)
                .unwrap_or(0.0),
            weather_code: *raw.daily.weather_code.get(i).unwrap_or(&0),
            wind_max: raw.daily.wind_speed_10m_max.get(i).copied().unwrap_or(0.0),
            uv_index_max: raw
                .daily
                .uv_index_max
                .get(i)
                .and_then(|o| *o)
                .unwrap_or(0.0),
            sunrise: raw.daily.sunrise.get(i).cloned().unwrap_or_default(),
            sunset: raw.daily.sunset.get(i).cloned().unwrap_or_default(),
        });
    }

    Ok(WeatherSnapshot {
        location_id,
        current,
        hourly,
        daily,
        source_units: units,
        fetched_at,
    })
}

// ---------------------------------------------------------------------
// Air-quality response
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AirQualityResponse {
    current: AirQualityCurrentRaw,
}

#[derive(Debug, Deserialize)]
struct AirQualityCurrentRaw {
    #[serde(default)]
    time: String,
    #[serde(default)]
    european_aqi: Option<f64>,
    #[serde(default)]
    us_aqi: Option<f64>,
    #[serde(default)]
    pm2_5: Option<f64>,
    #[serde(default)]
    pm10: Option<f64>,
    #[serde(default)]
    nitrogen_dioxide: Option<f64>,
    #[serde(default)]
    ozone: Option<f64>,
}

/// Parse an air-quality JSON body into an [`AirQuality`] reading. Missing
/// fields decode to `None` (honest "unavailable"), never a fabricated zero.
///
/// # Errors
/// Returns an error message if `body` doesn't decode as the expected
/// Open-Meteo air-quality JSON shape.
pub fn parse_air_quality(
    body: &str,
    location_id: LocationId,
    fetched_at: u64,
) -> Result<AirQuality, String> {
    let raw: AirQualityResponse =
        serde_json::from_str(body).map_err(|e| format!("air-quality decode: {e}"))?;
    Ok(AirQuality {
        location_id,
        european_aqi: raw.current.european_aqi,
        us_aqi: raw.current.us_aqi,
        pm2_5: raw.current.pm2_5,
        pm10: raw.current.pm10,
        nitrogen_dioxide: raw.current.nitrogen_dioxide,
        ozone: raw.current.ozone,
        time: raw.current.time,
        fetched_at,
    })
}

/// The European-AQI qualitative band label for an index value.
pub fn european_aqi_band(aqi: f64) -> &'static str {
    match aqi.round() as i64 {
        i64::MIN..=20 => "Good",
        21..=40 => "Fair",
        41..=60 => "Moderate",
        61..=80 => "Poor",
        81..=100 => "Very poor",
        _ => "Extremely poor",
    }
}

// ---------------------------------------------------------------------
// Geocoding response
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GeocodeResponse {
    #[serde(default)]
    results: Vec<GeocodeResult>,
}

#[derive(Debug, Deserialize)]
struct GeocodeResult {
    name: String,
    latitude: f64,
    longitude: f64,
    #[serde(default)]
    country: String,
    #[serde(default)]
    country_code: String,
    #[serde(default)]
    admin1: String,
    #[serde(default)]
    timezone: String,
}

/// Parse a geocoding JSON body into a list of candidate locations.
///
/// # Errors
/// Returns an error message if `body` doesn't decode as the expected
/// Open-Meteo geocoding JSON shape.
pub fn parse_geocode(body: &str) -> Result<Vec<SavedLocation>, String> {
    let raw: GeocodeResponse =
        serde_json::from_str(body).map_err(|e| format!("geocode decode: {e}"))?;
    Ok(raw
        .results
        .into_iter()
        .map(|r| {
            SavedLocation::new(
                r.name,
                r.admin1,
                r.country,
                r.country_code,
                r.latitude,
                r.longitude,
                if r.timezone.is_empty() {
                    "auto".to_string()
                } else {
                    r.timezone
                },
            )
        })
        .collect())
}

/// The places worth offering for `query`, in the provider's order.
///
/// Open-Meteo also matches a place's other names, so "Amsterdam" brings back
/// a town now called "eMvelo"; when some results carry the query in their
/// own name, the others are dropped. Places that would read the same in the
/// list ("Amsterdam, Ohio, United States" twice) are shown once.
#[must_use]
pub fn refine_geocode(query: &str, results: Vec<SavedLocation>) -> Vec<SavedLocation> {
    let needle = query.trim().to_lowercase();
    let named = |r: &SavedLocation| r.name.to_lowercase().contains(&needle);
    let any_named = !needle.is_empty() && results.iter().any(named);
    let mut seen = std::collections::HashSet::new();
    results
        .into_iter()
        .filter(|r| !any_named || named(r))
        .filter(|r| {
            seen.insert((
                r.name.to_lowercase(),
                r.region.to_lowercase(),
                r.country.to_lowercase(),
            ))
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn open_meteo_urls_carry_params_and_encoding() {
        // Case `forecast_url_has_required_params`.
        {
            let u = forecast_url(52.52, 13.41, Units::IMPERIAL);
            assert!(u.contains("latitude=52.5200"));
            assert!(u.contains("current=temperature_2m"));
            assert!(u.contains("daily=weather_code"));
            assert!(u.contains("temperature_unit=fahrenheit"));
            assert!(u.contains("wind_speed_unit=mph"));
            assert!(u.contains("timezone=auto"));
        }
        // Case `geocode_url_encodes_spaces`.
        {
            let u = geocode_url("New York");
            assert!(u.contains("name=New%20York"), "{u}");
        }
    }

    #[test]
    fn place_search_drops_other_names_and_repeats() {
        let place = |name: &str, region: &str, country: &str, lat: f64| {
            SavedLocation::new(name, region, country, "", lat, 0.0, "auto")
        };
        let results = vec![
            place("Amsterdam", "North Holland", "The Netherlands", 52.4),
            place("Amsterdam", "Ohio", "United States", 40.5),
            place("eMvelo", "Mpumalanga", "South Africa", -26.5),
            place("Amsterdam", "Ohio", "United States", 41.0),
            place("New Amsterdam", "Indiana", "United States", 38.1),
        ];
        let shown: Vec<_> = refine_geocode(" amsterdam", results)
            .into_iter()
            .map(|r| format!("{}, {}", r.name, r.region))
            .collect();
        assert_eq!(
            shown,
            [
                "Amsterdam, North Holland",
                "Amsterdam, Ohio",
                "New Amsterdam, Indiana"
            ]
        );
        // A query in another language matches no own name: keep them all.
        let munich = vec![place("Munich", "Bavaria", "Germany", 48.1)];
        assert_eq!(refine_geocode("München", munich).len(), 1);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn open_meteo_responses_parse_forecast_air_quality_and_geocode() {
        // Case `parse_forecast_slices_24_hours_from_current`.
        {
            // 26-hour hourly array; current time is hour index 2.
            let mut times = Vec::new();
            for h in 0..26 {
                times.push(format!("2026-06-30T{h:02}:00"));
            }
            let temps: Vec<f64> = (0..26).map(f64::from).collect();
            let codes: Vec<i64> = vec![0; 26];
            let isday: Vec<i64> = vec![1; 26];
            let body = serde_json::json!({
                "current": {
                    "time": "2026-06-30T02:00",
                    "temperature_2m": 2.0,
                    "apparent_temperature": 1.0,
                    "relative_humidity_2m": 50.0,
                    "precipitation": 0.0,
                    "weather_code": 0,
                    "wind_speed_10m": 5.0,
                    "wind_direction_10m": 180.0,
                    "pressure_msl": 1012.0,
                    "is_day": 1
                },
                "hourly": {
                    "time": times,
                    "temperature_2m": temps,
                    "precipitation_probability": vec![10.0; 26],
                    "weather_code": codes,
                    "is_day": isday
                },
                "daily": {
                    "time": ["2026-06-30"],
                    "weather_code": [3],
                    "temperature_2m_max": [10.0],
                    "temperature_2m_min": [2.0],
                    "precipitation_probability_max": [40.0],
                    "sunrise": ["2026-06-30T05:00"],
                    "sunset": ["2026-06-30T21:00"],
                    "wind_speed_10m_max": [20.0],
                    "uv_index_max": [5.0]
                }
            })
            .to_string();
            let snap = parse_forecast(&body, Uuid::new_v4(), Units::METRIC, 0).unwrap();
            assert_eq!(snap.hourly.len(), 24);
            assert_eq!(snap.hourly[0].time, "2026-06-30T02:00");
            assert_eq!(
                snap.current.uv_index, 5.0,
                "current uv comes from today's max"
            );
            assert_eq!(snap.daily.len(), 1);
        }
        // Case `parse_air_quality_maps_fields_and_nulls`.
        {
            let body = serde_json::json!({
                "current": {
                    "time": "2026-06-30T14:00",
                    "european_aqi": 32.0,
                    "us_aqi": 45.0,
                    "pm2_5": 8.4,
                    "pm10": 14.2,
                    "nitrogen_dioxide": 18.0,
                    "ozone": null
                }
            })
            .to_string();
            let aq = parse_air_quality(&body, Uuid::new_v4(), 1234).unwrap();
            assert_eq!(aq.european_aqi, Some(32.0));
            assert_eq!(aq.pm2_5, Some(8.4));
            assert_eq!(aq.ozone, None, "a null pollutant stays honestly absent");
            assert!(aq.has_any());
            assert_eq!(aq.fetched_at, 1234);
        }
        // Case `european_aqi_bands_cover_the_scale`.
        {
            assert_eq!(european_aqi_band(10.0), "Good");
            assert_eq!(european_aqi_band(30.0), "Fair");
            assert_eq!(european_aqi_band(50.0), "Moderate");
            assert_eq!(european_aqi_band(70.0), "Poor");
            assert_eq!(european_aqi_band(90.0), "Very poor");
            assert_eq!(european_aqi_band(140.0), "Extremely poor");
        }
        // Case `parse_geocode_maps_fields`.
        {
            let body = serde_json::json!({
                "results": [{
                    "name": "Berlin",
                    "latitude": 52.52,
                    "longitude": 13.41,
                    "country": "Germany",
                    "country_code": "DE",
                    "admin1": "Berlin",
                    "timezone": "Europe/Berlin"
                }]
            })
            .to_string();
            let locs = parse_geocode(&body).unwrap();
            assert_eq!(locs.len(), 1);
            assert_eq!(locs[0].name, "Berlin");
            assert_eq!(locs[0].timezone, "Europe/Berlin");
        }
        // Case `parse_geocode_empty_results_ok`.
        {
            let body = serde_json::json!({}).to_string();
            let locs = parse_geocode(&body).unwrap();
            assert!(locs.is_empty());
        }
    }
}
