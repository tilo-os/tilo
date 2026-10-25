//! Bundled, fully-populated sample data.
//!
//! A screenshot cannot wait on a live network fetch. To keep it honest — a
//! real, populated UI rather than an empty shell — a capture stages these
//! places through the resource's actions and shows the representative
//! snapshot synthesized here for each, as its fixture
//! ([`crate::resource::SampleForecasts`]). A live window never shows them: it
//! displays real Open-Meteo fetches, never invented readings. Everything is
//! stored in **metric** (the snapshot's `source_units`); the UI converts to
//! the user's preference at draw time.

use crate::model::{
    AirQuality, CurrentConditions, DayForecast, HourPoint, SavedLocation, WeatherModel,
    WeatherSnapshot,
};
use crate::units::Units;

/// Parameters describing a plausible day so we can synthesize a smooth
/// 24h curve + 7-day outlook without hand-typing every number.
struct DaySpec {
    low: f64,
    high: f64,
    code_day: i64,
    code_night: i64,
    humidity: f64,
    wind: f64,
    wind_dir: f64,
    pressure: f64,
    uv: f64,
    /// Hour-of-day (0..24) the snapshot's "now" sits at.
    now_hour: usize,
    is_day_now: bool,
    sunrise_h: u32,
    sunset_h: u32,
    /// Per-hour precip probability profile peaks around this hour (or
    /// `None` for a dry day).
    precip_peak: Option<(usize, f64)>,
    /// Representative European AQI for the bundled air-quality reading.
    aqi: f64,
}

/// Synthesize a 24h hourly strip starting at midnight, plus the current
/// observation, from a [`DaySpec`].
fn synth_snapshot(loc: &SavedLocation, spec: &DaySpec) -> WeatherSnapshot {
    let date = "2026-06-30";
    let mid = f64::midpoint(spec.low, spec.high);
    let amp = (spec.high - spec.low) / 2.0;

    // Temperature follows a cosine that bottoms out near 05:00 and peaks
    // near 15:00 — the canonical diurnal shape.
    let temp_at = |h: f64| -> f64 {
        let phase = (h - 15.0) / 24.0 * std::f64::consts::TAU;
        mid + amp * phase.cos()
    };
    let precip_at = |h: usize| -> f64 {
        match spec.precip_peak {
            Some((peak, max)) => {
                let d = (h as f64 - peak as f64).abs();
                (max - d * 6.0).clamp(0.0, max)
            }
            None => {
                // A faint, dry baseline so the bars aren't all zero.
                ((h as f64 * 0.7).sin().abs() * 8.0).round()
            }
        }
    };

    let mut hourly = Vec::new();
    // 24 hours starting at the current hour, wrapping past midnight.
    for k in 0..24usize {
        let h = (spec.now_hour + k) % 24;
        let day_idx = (spec.now_hour + k) / 24; // 0 today, 1 tomorrow
        let is_day = h >= spec.sunrise_h as usize && h < spec.sunset_h as usize;
        let code = if is_day {
            spec.code_day
        } else {
            spec.code_night
        };
        hourly.push(HourPoint {
            time: format!("2026-06-{:02}T{:02}:00", 30 + day_idx as u32, h),
            temperature: (temp_at(h as f64) * 10.0).round() / 10.0,
            precip_probability: precip_at(h),
            weather_code: code,
            is_day,
        });
    }

    let now_code = if spec.is_day_now {
        spec.code_day
    } else {
        spec.code_night
    };
    let current = CurrentConditions {
        temperature: (temp_at(spec.now_hour as f64) * 10.0).round() / 10.0,
        apparent: (temp_at(spec.now_hour as f64) - 1.5 + spec.wind * 0.02).round(),
        humidity: spec.humidity,
        precipitation: spec.precip_peak.map_or(0.0, |(_, m)| (m / 100.0) * 1.2),
        weather_code: now_code,
        wind_speed: spec.wind,
        wind_direction: spec.wind_dir,
        pressure: spec.pressure,
        uv_index: spec.uv,
        is_day: spec.is_day_now,
        time: format!("2026-06-30T{:02}:00", spec.now_hour),
    };

    // 7-day outlook: vary code + temps a little each day. Today keeps the
    // day's own range, so it agrees with the hourly curve and the reading
    // now.
    let week_codes = [spec.code_day, 2, 61, 80, 3, 1, spec.code_day];
    let week_drift = [0.0, -0.4, -1.6, -2.2, -0.8, 0.6, 1.2];
    let mut daily = Vec::new();
    for (d, (&code, &drift)) in week_codes.iter().zip(&week_drift).enumerate() {
        let wet = crate::wxcode::Sky::from_code(code).is_wet();
        daily.push(DayForecast {
            // Placeholder; the clean rolling date sequence is assigned in
            // the loop below so we never have to do calendar arithmetic.
            date: String::new(),
            temp_max: ((spec.high + drift) * 10.0).round() / 10.0,
            temp_min: ((spec.low + drift * 0.6) * 10.0).round() / 10.0,
            precip_probability: if wet {
                55.0 + d as f64 * 4.0
            } else {
                5.0 + d as f64 * 3.0
            },
            weather_code: code,
            wind_max: spec.wind + 6.0 + d as f64,
            uv_index_max: (spec.uv - d as f64 * 0.4).max(1.0),
            // format: machine (the ISO 8601 times the forecast service sends)
            sunrise: format!("{date}T{:02}:{:02}", spec.sunrise_h, 12),
            sunset: format!("{date}T{:02}:{:02}", spec.sunset_h, 47),
        });
    }
    // Fix the date strings to a clean rolling sequence Jun 30 → Jul 6.
    let dates = [
        "2026-06-30",
        "2026-07-01",
        "2026-07-02",
        "2026-07-03",
        "2026-07-04",
        "2026-07-05",
        "2026-07-06",
    ];
    for (day, date) in daily.iter_mut().zip(dates) {
        day.date = date.to_string();
    }

    WeatherSnapshot {
        location_id: loc.id,
        current,
        hourly,
        daily,
        source_units: Units::METRIC,
        // A capture fixture, and it says so: no observation time is claimed
        // (the badge reads "Capture fixture" and the page states no age).
        // `fetched` and `stale` stage a real day's age instead — see
        // `seed_fetched`.
        fetched_at: 0,
    }
}

/// Synthesize a plausible air-quality reading from a [`DaySpec`]'s AQI, so the
/// offscreen shot's air-quality section is populated without a network call.
fn synth_air_quality(loc: &SavedLocation, spec: &DaySpec) -> AirQuality {
    let aqi = spec.aqi;
    let r1 = |v: f64| (v * 10.0).round() / 10.0;
    AirQuality {
        location_id: loc.id,
        european_aqi: Some(aqi.round()),
        us_aqi: Some((aqi * 1.4).round()),
        pm2_5: Some(r1(aqi * 0.28)),
        pm10: Some(r1(aqi * 0.46)),
        nitrogen_dioxide: Some(r1(aqi * 0.55)),
        ozone: Some((40.0 + aqi * 0.5).round()),
        time: format!("2026-06-30T{:02}:00", spec.now_hour),
        fetched_at: 0,
    }
}

/// The bundled places and their forecasts that captures show.
pub fn sample_model() -> WeatherModel {
    let mut model = WeatherModel {
        units: Units::METRIC,
        ..Default::default()
    };

    let specs: Vec<(SavedLocation, DaySpec)> = vec![
        san_francisco_spec(),
        tokyo_spec(),
        reykjavik_spec(),
        cairo_spec(),
    ];

    for (index, (mut loc, spec)) in specs.into_iter().enumerate() {
        // Stable ids, so a capture's staged places find their bundled
        // forecasts.
        loc.id = crate::model::LocationId::from_u128(index as u128 + 1);
        loc.order_rank = (index + 1) as f64 * 1024.0;
        let snap = synth_snapshot(&loc, &spec);
        let aq = synth_air_quality(&loc, &spec);
        model.snapshots.insert(loc.id, snap);
        model.air_quality.insert(loc.id, aq);
        model.locations.push(loc);
    }
    model.selected = 0;
    model
}

fn san_francisco_spec() -> (SavedLocation, DaySpec) {
    (
        SavedLocation::new(
            "San Francisco",
            "California",
            "United States",
            "US",
            37.7749,
            -122.4194,
            "America/Los_Angeles",
        ),
        DaySpec {
            low: 13.0,
            high: 21.0,
            code_day: 2,
            code_night: 1,
            humidity: 68.0,
            wind: 18.0,
            wind_dir: 250.0,
            pressure: 1016.0,
            uv: 6.0,
            now_hour: 14,
            is_day_now: true,
            sunrise_h: 6,
            sunset_h: 20,
            precip_peak: None,
            aqi: 24.0,
        },
    )
}

fn tokyo_spec() -> (SavedLocation, DaySpec) {
    (
        SavedLocation::new(
            "Tokyo",
            "Tokyo",
            "Japan",
            "JP",
            35.6762,
            139.6503,
            "Asia/Tokyo",
        ),
        DaySpec {
            low: 24.0,
            high: 31.0,
            code_day: 63,
            code_night: 61,
            humidity: 84.0,
            wind: 14.0,
            wind_dir: 160.0,
            pressure: 1004.0,
            uv: 8.0,
            now_hour: 18,
            is_day_now: true,
            sunrise_h: 4,
            sunset_h: 19,
            precip_peak: Some((19, 85.0)),
            aqi: 58.0,
        },
    )
}

fn reykjavik_spec() -> (SavedLocation, DaySpec) {
    (
        SavedLocation::new(
            "Reykjavík",
            "Capital Region",
            "Iceland",
            "IS",
            64.1466,
            -21.9426,
            "Atlantic/Reykjavik",
        ),
        DaySpec {
            low: 4.0,
            high: 9.0,
            code_day: 85,
            code_night: 71,
            humidity: 90.0,
            wind: 34.0,
            wind_dir: 300.0,
            pressure: 995.0,
            uv: 2.0,
            now_hour: 23,
            is_day_now: false,
            sunrise_h: 3,
            sunset_h: 23,
            precip_peak: Some((22, 70.0)),
            aqi: 12.0,
        },
    )
}

fn cairo_spec() -> (SavedLocation, DaySpec) {
    (
        SavedLocation::new(
            "Cairo",
            "Cairo Governorate",
            "Egypt",
            "EG",
            30.0444,
            31.2357,
            "Africa/Cairo",
        ),
        DaySpec {
            low: 26.0,
            high: 39.0,
            code_day: 0,
            code_night: 0,
            humidity: 22.0,
            wind: 11.0,
            wind_dir: 20.0,
            pressure: 1011.0,
            uv: 11.0,
            now_hour: 13,
            is_day_now: true,
            sunrise_h: 5,
            sunset_h: 19,
            precip_peak: None,
            aqi: 96.0,
        },
    )
}

/// A representative geocode result set (a "Par…" query) for the offscreen
/// screenshot + tests of the search dropdown. The live path fetches these
/// from Open-Meteo's geocoder; this bundled set keeps the headless shot of
/// the dropdown honest without a network round-trip.
pub fn sample_geocode_results() -> Vec<SavedLocation> {
    vec![
        SavedLocation::new(
            "Paris",
            "Île-de-France",
            "France",
            "FR",
            48.8566,
            2.3522,
            "Europe/Paris",
        ),
        SavedLocation::new(
            "Paris",
            "Texas",
            "United States",
            "US",
            33.6609,
            -95.5555,
            "America/Chicago",
        ),
        SavedLocation::new(
            "Parma",
            "Emilia-Romagna",
            "Italy",
            "IT",
            44.8015,
            10.3279,
            "Europe/Rome",
        ),
        SavedLocation::new(
            "Parramatta",
            "New South Wales",
            "Australia",
            "AU",
            -33.815,
            151.0,
            "Australia/Sydney",
        ),
        SavedLocation::new(
            "Pardubice",
            "Pardubický",
            "Czechia",
            "CZ",
            50.0343,
            15.7812,
            "Europe/Prague",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_is_fully_populated_with_clean_hourly_and_daily_series() {
        {
            let m = sample_model();
            assert!(m.locations.len() >= 3);
            for loc in &m.locations {
                let snap = m
                    .snapshot_for(loc.id)
                    .expect("every location has a snapshot");
                assert_eq!(snap.hourly.len(), 24);
                assert_eq!(snap.daily.len(), 7);
            }
        }
        {
            let m = sample_model();
            for loc in &m.locations {
                let aq = m
                    .air_quality_for(loc.id)
                    .expect("every location has air quality");
                assert!(
                    aq.has_any(),
                    "the reading carries real index/pollutant values"
                );
                assert!(aq.european_aqi.is_some());
            }
        }
        {
            let m = sample_model();
            let snap = m.selected_snapshot().unwrap();
            // San Francisco now_hour = 14.
            assert_eq!(
                snap.hourly[0].hour_label(),
                ui::format::time(&chrono::NaiveTime::from_hms_opt(14, 0, 0).unwrap())
            );
        }
        {
            let m = sample_model();
            let snap = m.selected_snapshot().unwrap();
            assert_eq!(snap.daily[0].date, "2026-06-30");
            assert_eq!(snap.daily[6].date, "2026-07-06");
        }
    }
}
