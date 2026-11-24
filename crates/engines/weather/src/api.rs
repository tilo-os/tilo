//! Open-Meteo's addresses, and what its answers say read into the flat shape the app keeps.
//!
//! The forecast is asked in metric and stored so: the app converts to the units the person
//! chose in Settings as it draws, so changing them never costs a fetch. A place's forecast and
//! its air quality are two services; one failing leaves the other's answer standing.

// The readings are the service's numbers, added, divided and counted as a calendar does; a
// value that is not one is reported by the decoder, not here.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "the service's own numbers, read into readings"
)]

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use serde::{Deserialize, Serialize};

pub const FORECAST_BASE: &str = "https://api.open-meteo.com/v1/forecast";
pub const GEOCODE_BASE: &str = "https://geocoding-api.open-meteo.com/v1/search";
pub const AIR_QUALITY_BASE: &str = "https://air-quality-api.open-meteo.com/v1/air-quality";

/// What is escaped in a place name typed into a search: everything outside the unreserved set,
/// which is what a query string carries.
const QUERY: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// The forecast for a place: its current reading, the next day of hours and a week of days.
pub fn forecast_url(latitude: f64, longitude: f64) -> String {
    format!(
        "{FORECAST_BASE}?latitude={latitude:.4}&longitude={longitude:.4}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,precipitation,weather_code,wind_speed_10m,wind_direction_10m,pressure_msl,is_day\
         &hourly=temperature_2m,precipitation_probability,weather_code,is_day\
         &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,sunrise,sunset,wind_speed_10m_max,uv_index_max\
         &timezone=auto&forecast_days=7&temperature_unit=celsius&wind_speed_unit=kmh"
    )
}

/// The air quality over a place: the two AQI scales and the pollutants that matter most.
pub fn air_quality_url(latitude: f64, longitude: f64) -> String {
    format!(
        "{AIR_QUALITY_BASE}?latitude={latitude:.4}&longitude={longitude:.4}\
         &current=european_aqi,us_aqi,pm2_5,pm10,nitrogen_dioxide,ozone&timezone=auto"
    )
}

/// A place search: up to eight answers, in the service's own order.
pub fn geocode_url(query: &str) -> String {
    let encoded = utf8_percent_encode(query.trim(), QUERY);
    format!("{GEOCODE_BASE}?name={encoded}&count=8&language=en&format=json")
}

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

/// The hours a forecast's strip shows: a day from the current hour.
const HOURS: usize = 24;

/// What one forecast says, as the app reads it: flat, because a record's field holds no record
/// in the app's language, so each list of readings travels beside the others and the app builds
/// its own out of them.
#[derive(Debug, Default, Serialize)]
pub struct Readings {
    pub time: String,
    pub temperature: f64,
    pub apparent: f64,
    pub humidity: f64,
    pub precipitation: f64,
    pub code: i64,
    pub wind: f64,
    pub wind_from: f64,
    pub pressure: f64,
    pub uv: f64,
    pub is_day: bool,
    pub hour_at: Vec<String>,
    pub hour_temp: Vec<f64>,
    pub hour_chance: Vec<f64>,
    pub hour_code: Vec<i64>,
    pub hour_day: Vec<bool>,
    pub day_date: Vec<String>,
    /// Each day's weekday, counted from Monday as 0: what names it once it is neither today nor
    /// tomorrow.
    pub day_weekday: Vec<i64>,
    pub day_high: Vec<f64>,
    pub day_low: Vec<f64>,
    pub day_chance: Vec<f64>,
    pub day_code: Vec<i64>,
    pub day_wind: Vec<f64>,
    pub day_uv: Vec<f64>,
    pub day_rise: Vec<String>,
    pub day_set: Vec<String>,
}

/// A forecast read, or why it could not be.
///
/// # Errors
///
/// The body is not a forecast the service could have sent.
#[allow(
    clippy::too_many_lines,
    reason = "the reading's fields are read in one pass, in the order the service sends them"
)]
pub fn read_forecast(body: &str) -> Result<Readings, String> {
    let raw: ForecastResponse =
        serde_json::from_str(body).map_err(|e| format!("forecast decode: {e}"))?;
    // The reading's own uv is today's highest: the service gives no current one.
    let uv = raw
        .daily
        .uv_index_max
        .first()
        .and_then(|o| *o)
        .unwrap_or(0.0);
    let start = raw
        .hourly
        .time
        .iter()
        .position(|t| t.as_str() >= raw.current.time.as_str())
        .unwrap_or(0);
    let mut readings = Readings {
        time: raw.current.time.clone(),
        temperature: raw.current.temperature_2m,
        apparent: raw.current.apparent_temperature,
        humidity: raw.current.relative_humidity_2m,
        precipitation: raw.current.precipitation,
        code: raw.current.weather_code,
        wind: raw.current.wind_speed_10m,
        wind_from: raw.current.wind_direction_10m,
        pressure: raw.current.pressure_msl,
        uv,
        is_day: raw.current.is_day != 0,
        ..Readings::default()
    };
    for i in start..raw.hourly.time.len().min(start + HOURS) {
        readings
            .hour_at
            .push(raw.hourly.time.get(i).cloned().unwrap_or_default());
        readings.hour_temp.push(
            raw.hourly
                .temperature_2m
                .get(i)
                .copied()
                .unwrap_or_default(),
        );
        readings.hour_chance.push(
            raw.hourly
                .precipitation_probability
                .get(i)
                .and_then(|o| *o)
                .unwrap_or(0.0),
        );
        readings
            .hour_code
            .push(raw.hourly.weather_code.get(i).copied().unwrap_or_default());
        readings
            .hour_day
            .push(raw.hourly.is_day.get(i).copied().unwrap_or(1) != 0);
    }
    for i in 0..raw.daily.time.len() {
        let date = raw.daily.time.get(i).cloned().unwrap_or_default();
        readings.day_weekday.push(weekday(&date));
        readings.day_date.push(date);
        readings.day_high.push(
            raw.daily
                .temperature_2m_max
                .get(i)
                .copied()
                .unwrap_or_default(),
        );
        readings.day_low.push(
            raw.daily
                .temperature_2m_min
                .get(i)
                .copied()
                .unwrap_or_default(),
        );
        readings.day_chance.push(
            raw.daily
                .precipitation_probability_max
                .get(i)
                .and_then(|o| *o)
                .unwrap_or(0.0),
        );
        readings
            .day_code
            .push(raw.daily.weather_code.get(i).copied().unwrap_or_default());
        readings.day_wind.push(
            raw.daily
                .wind_speed_10m_max
                .get(i)
                .copied()
                .unwrap_or_default(),
        );
        readings.day_uv.push(
            raw.daily
                .uv_index_max
                .get(i)
                .and_then(|o| *o)
                .unwrap_or(0.0),
        );
        readings
            .day_rise
            .push(raw.daily.sunrise.get(i).cloned().unwrap_or_default());
        readings
            .day_set
            .push(raw.daily.sunset.get(i).cloned().unwrap_or_default());
    }
    Ok(readings)
}

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

/// An air-quality reading: unitless indices and pollutants in µg/m³. A value the service omits
/// stays absent, never a fabricated zero.
#[derive(Debug, Default, Serialize)]
pub struct Air {
    pub at: String,
    pub european: Option<f64>,
    pub us: Option<f64>,
    pub pm2_5: Option<f64>,
    pub pm10: Option<f64>,
    pub no2: Option<f64>,
    pub ozone: Option<f64>,
}

/// An air-quality reading, or why it could not be read.
///
/// # Errors
///
/// The body is not a reading the service could have sent.
pub fn read_air(body: &str) -> Result<Air, String> {
    let raw: AirQualityResponse =
        serde_json::from_str(body).map_err(|e| format!("air quality decode: {e}"))?;
    Ok(Air {
        at: raw.current.time,
        european: raw.current.european_aqi,
        us: raw.current.us_aqi,
        pm2_5: raw.current.pm2_5,
        pm10: raw.current.pm10,
        no2: raw.current.nitrogen_dioxide,
        ozone: raw.current.ozone,
    })
}

/// One place a search found, as the app keeps it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Place {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub country: String,
    pub country_code: String,
    pub region: String,
    /// The place's IANA zone, or `auto` when the service names none.
    pub timezone: String,
}

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

/// What a place search found, in the service's order.
///
/// # Errors
///
/// The body is not an answer the service could have sent.
pub fn read_geocode(body: &str) -> Result<Vec<Place>, String> {
    let raw: GeocodeResponse =
        serde_json::from_str(body).map_err(|e| format!("place search decode: {e}"))?;
    Ok(raw
        .results
        .into_iter()
        .map(|r| Place {
            name: r.name,
            latitude: r.latitude,
            longitude: r.longitude,
            country: r.country,
            country_code: r.country_code,
            region: r.admin1,
            timezone: if r.timezone.is_empty() {
                "auto".to_owned()
            } else {
                r.timezone
            },
        })
        .collect())
}

/// The weekday a `YYYY-MM-DD` date falls on, counted from Monday as 0; 0 when the text is no
/// date. Sakamoto's algorithm: a month is shifted so March starts the year, which makes the
/// leap day the last day of a year of thirteen months.
pub(crate) fn weekday(date: &str) -> i64 {
    let parts: Vec<&str> = date.split('-').collect();
    let (Some(year), Some(month), Some(day)) = (
        parts.first().and_then(|y| y.parse::<i64>().ok()),
        parts.get(1).and_then(|m| m.parse::<i64>().ok()),
        parts.get(2).and_then(|d| d.parse::<i64>().ok()),
    ) else {
        return 0;
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return 0;
    }
    let shifts = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let year = if month < 3 { year - 1 } else { year };
    let sunday_first = (year + year / 4 - year / 100
        + year / 400
        + shifts[usize::try_from(month - 1).unwrap_or(0)]
        + day)
        % 7;
    // Sunday first becomes Monday first.
    (sunday_first + 6) % 7
}

/// The places worth offering, in the service's order: Open-Meteo also matches a place's other
/// names ("Amsterdam" brings back a town now called "eMvelo"), and results that would read the
/// same show once.
#[must_use]
pub fn refine_geocode(query: &str, results: Vec<Place>) -> Vec<Place> {
    let needle = query.trim().to_lowercase();
    let named = |place: &Place| place.name.to_lowercase().contains(&needle);
    let any_named = !needle.is_empty() && results.iter().any(named);
    let mut seen = std::collections::HashSet::new();
    results
        .into_iter()
        .filter(|place| !any_named || named(place))
        .filter(|place| {
            seen.insert((
                place.name.to_lowercase(),
                place.region.to_lowercase(),
                place.country.to_lowercase(),
            ))
        })
        .collect()
}
