//! The weather binary: the app, or `--probe` for the live network stack.

#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]
// An app runs as a sandboxed process, so a wrapped integer here costs one app,
// not the session. The lints that crash (indexing, string slices, unwrap, panic)
// stay denied: `shell`, `ui` and `crates/` keep the full set.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
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

fn run_probe() -> Result<(), Box<dyn std::error::Error>> {
    use ui::time::Duration;
    use weather::model::SavedLocation;
    use weather::net::{Request, Response, WeatherService};
    use weather::units::Units;

    let svc = WeatherService::spawn();
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
