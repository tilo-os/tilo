//! Weather — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default — the framework app ([`weather::WeatherShell`]), including its
//!     `--shots --scenario NAME` captures.
//!   * `--probe` — exercise the live networking stack and print what came
//!     back.

#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]
// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// # Errors
///
/// Returns an error if `--probe` can't complete its live fetch within its
/// deadline.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|a| a == "--probe") {
        let _ = tracing_subscriber::fmt::try_init();
        return run_probe();
    }
    ui::framework::run::<weather::WeatherShell>();
    Ok(())
}

/// Exercise the live networking stack the app uses (`net` + `api`) against the
/// keyless Open-Meteo endpoints and print what came back. This is the same
/// [`weather::net::WeatherService`] the event loop drives. Invoked with
/// `--probe`; it touches no UI and no window.
fn run_probe() -> Result<(), Box<dyn std::error::Error>> {
    use ui::time::Duration;
    use weather::model::SavedLocation;
    use weather::net::{Request, Response, WeatherService};
    use weather::units::Units;

    let svc = WeatherService::spawn();
    // A saved place (real coordinates) + a free-text geocode — the two live
    // request kinds the app issues.
    let berlin = SavedLocation::new("Berlin", "Berlin", "Germany", "DE", 52.52, 13.41, "auto");
    let _ = svc.request(Request::Forecast {
        location_id: berlin.id,
        lat: berlin.latitude,
        lon: berlin.longitude,
        units: Units::METRIC,
    });
    let _ = svc.request(Request::AirQuality {
        location_id: berlin.id,
        lat: berlin.latitude,
        lon: berlin.longitude,
    });
    let _ = svc.request(Request::Geocode {
        seq: 1,
        query: "Tokyo".to_string(),
    });

    let deadline = ui::time::now() + Duration::from_secs(10);
    let (mut got_forecast, mut got_geocode, mut got_aq) = (false, false, false);
    while ui::time::now() < deadline && !(got_forecast && got_geocode && got_aq) {
        let Some(resp) = svc.wait(Duration::from_millis(250)) else {
            continue;
        };
        match resp {
            Response::Tracked { .. } => {}
            Response::Forecast(snap) => {
                got_forecast = true;
                println!(
                    "probe: forecast Berlin — {:.1}°C, code {}, {} hourly pts, {} days (fetched_at={})",
                    snap.current.temperature,
                    snap.current.weather_code,
                    snap.hourly.len(),
                    snap.daily.len(),
                    snap.fetched_at,
                );
            }
            Response::AirQuality(aq) => {
                got_aq = true;
                println!(
                    "probe: air quality Berlin — EU AQI {:?}, PM2.5 {:?}, NO2 {:?} µg/m³",
                    aq.european_aqi, aq.pm2_5, aq.nitrogen_dioxide,
                );
            }
            Response::Geocode { results, .. } => {
                got_geocode = true;
                let names: Vec<String> = results
                    .iter()
                    .take(3)
                    .map(|r| format!("{} ({})", r.name, r.country))
                    .collect();
                println!("probe: geocode 'Tokyo' → {}", names.join(", "));
            }
            Response::Error { message, .. } => println!("probe: error — {message}"),
        }
    }

    if got_forecast && got_geocode && got_aq {
        println!("probe: OK — live Open-Meteo forecast + air-quality + geocode fetch succeeded");
        Ok(())
    } else {
        Err(format!(
            "probe: incomplete (forecast={got_forecast}, air_quality={got_aq}, geocode={got_geocode}) — network unreachable?"
        )
        .into())
    }
}
