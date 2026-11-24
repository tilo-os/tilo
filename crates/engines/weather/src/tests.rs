//! The engine's own tests: what a service's answer reads as, and the job surface the app asks.
#![allow(
    clippy::cast_precision_loss,
    clippy::float_cmp,
    reason = "a fixture's small hour counts, and the readings it asserts exactly"
)]

use serde_json::{Value as Json, json};

use crate::api;
use crate::{Reason, call, cancel, start, status};

/// A forecast body with `hours` hourly points, the current reading at hour `current`.
fn forecast_body(hours: usize, current: usize) -> String {
    let times: Vec<String> = (0..hours)
        .map(|h| format!("2026-06-30T{h:02}:00"))
        .collect();
    let temps: Vec<f64> = (0..hours).map(|h| (h as f64) + 2.0).collect();
    json!({
        "current": {
            "time": format!("2026-06-30T{current:02}:00"),
            "temperature_2m": 21.4,
            "apparent_temperature": 20.1,
            "relative_humidity_2m": 58.0,
            "precipitation": 0.0,
            "weather_code": 3,
            "wind_speed_10m": 12.0,
            "wind_direction_10m": 210.0,
            "pressure_msl": 1012.0,
            "is_day": 1
        },
        "hourly": {
            "time": times,
            "temperature_2m": temps,
            "precipitation_probability": vec![Some(10.0); hours],
            "weather_code": vec![61; hours],
            "is_day": vec![1; hours]
        },
        "daily": {
            "time": ["2026-06-30", "2026-07-01"],
            "weather_code": [3, 61],
            "temperature_2m_max": [24.0, 19.0],
            "temperature_2m_min": [13.0, 11.0],
            "precipitation_probability_max": [40.0, null],
            "sunrise": ["2026-06-30T05:01", "2026-07-01T05:02"],
            "sunset": ["2026-06-30T21:34", "2026-07-01T21:33"],
            "wind_speed_10m_max": [20.0, 30.0],
            "uv_index_max": [5.0, null]
        }
    })
    .to_string()
}

#[test]
fn a_forecast_reads_from_the_current_hour_with_the_days_beside_it() {
    let readings = api::read_forecast(&forecast_body(30, 2)).expect("a forecast reads");
    assert_eq!(readings.time, "2026-06-30T02:00");
    assert_eq!(readings.temperature, 21.4);
    assert_eq!(readings.apparent, 20.1);
    assert_eq!(readings.code, 3);
    assert!(readings.is_day);
    assert_eq!(readings.uv, 5.0, "the current uv is today's highest");
    assert_eq!(readings.hour_at.len(), 24, "a day of hours from now");
    assert_eq!(readings.hour_at[0], "2026-06-30T02:00");
    assert_eq!(readings.hour_temp[0], 4.0);
    assert_eq!(readings.hour_code[0], 61);
    assert_eq!(readings.day_date.len(), 2);
    assert_eq!(readings.day_high, [24.0, 19.0]);
    assert_eq!(
        readings.day_chance[1], 0.0,
        "a missing chance reads as none"
    );
    assert_eq!(readings.day_uv[1], 0.0);
    assert_eq!(readings.day_set[0], "2026-06-30T21:34");
    assert_eq!(
        readings.day_weekday,
        [1, 2],
        "2026-06-30 is a Tuesday, and the day after a Wednesday"
    );
}

#[test]
fn a_date_names_the_weekday_it_falls_on() {
    assert_eq!(api::weekday("2026-06-29"), 0, "a Monday");
    assert_eq!(api::weekday("2026-11-24"), 1, "a Tuesday");
    assert_eq!(api::weekday("2026-11-28"), 5, "a Saturday");
    assert_eq!(api::weekday("2026-11-29"), 6, "a Sunday");
    assert_eq!(api::weekday("2028-02-29"), 1, "a leap day");
    assert_eq!(api::weekday("no date"), 0);
}

#[test]
fn a_body_that_is_no_forecast_says_so_rather_than_inventing_one() {
    let error = api::read_forecast("{\"current\": 3}").expect_err("not a forecast");
    assert!(error.starts_with("forecast decode"), "{error}");
}

#[test]
fn an_air_reading_keeps_what_the_service_omitted_absent() {
    let body = json!({
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
    let air = api::read_air(&body).expect("a reading reads");
    assert_eq!(air.european, Some(32.0));
    assert_eq!(air.pm2_5, Some(8.4));
    assert_eq!(air.ozone, None, "a null pollutant stays honestly absent");
    assert_eq!(air.no2, Some(18.0));
}

#[test]
fn a_place_search_drops_other_names_and_repeats() {
    let place = |name: &str, region: &str, country: &str, at: f64| api::Place {
        name: name.to_owned(),
        region: region.to_owned(),
        country: country.to_owned(),
        latitude: at,
        longitude: 0.0,
        ..api::Place::default()
    };
    let results = vec![
        place("Amsterdam", "North Holland", "The Netherlands", 52.4),
        place("Amsterdam", "Ohio", "United States", 40.5),
        place("eMvelo", "Mpumalanga", "South Africa", -26.5),
        place("Amsterdam", "Ohio", "United States", 41.0),
        place("New Amsterdam", "Indiana", "United States", 38.1),
    ];
    let shown: Vec<String> = api::refine_geocode(" amsterdam", results)
        .into_iter()
        .map(|place| format!("{}, {}", place.name, place.region))
        .collect();
    assert_eq!(
        shown,
        [
            "Amsterdam, North Holland",
            "Amsterdam, Ohio",
            "New Amsterdam, Indiana"
        ]
    );
    let munich = vec![place("Munich", "Bavaria", "Germany", 48.1)];
    assert_eq!(
        api::refine_geocode("München", munich).len(),
        1,
        "a query the service matched by another name keeps its answer"
    );
    assert!(api::refine_geocode("Zzqx", Vec::new()).is_empty());
}

#[test]
fn the_addresses_ask_metric_and_carry_the_place() {
    let url = api::forecast_url(52.52, 13.41);
    assert!(url.contains("latitude=52.5200"), "{url}");
    assert!(url.contains("current=temperature_2m"));
    assert!(url.contains("timezone=auto"));
    assert!(url.contains("temperature_unit=celsius"));
    assert!(url.contains("wind_speed_unit=kmh"));
    let url = api::air_quality_url(52.52, 13.41);
    assert!(url.contains("european_aqi"), "{url}");
}

#[test]
fn a_job_reports_its_state_and_leaves_when_it_is_drained() {
    let job = start("refresh", &json!({ "places": [] }).to_string()).expect("a refresh starts");
    let state: Json =
        serde_json::from_str(&status(&job).expect("the job answers")).expect("a state");
    assert_eq!(state["state"], json!("done"));
    assert_eq!(state["total"], json!(0));
    let taken: Json =
        serde_json::from_str(&call("take", &json!({ "job": job }).to_string()).unwrap())
            .expect("a take");
    assert_eq!(taken["answers"], json!([]));
    assert_eq!(taken["places"], json!([]));
    assert_eq!(
        status(&job).unwrap_err(),
        format!("no job `{job}` runs on this device"),
        "a drained job is gone"
    );
    assert!(!cancel(&job), "a job that does not run is not stopped");
}

#[test]
fn a_bad_ask_is_refused_by_name() {
    let error = start("tornado", "{}").unwrap_err();
    assert!(error.contains("`tornado` is no op"), "{error}");
    let error = call("refresh", "{}").unwrap_err();
    assert!(error.contains("`refresh` is no op"), "{error}");
    let error = call("take", "{}").unwrap_err();
    assert!(error.starts_with("take:"), "{error}");
    assert_eq!(
        status("nope").unwrap_err(),
        "no job `nope` runs on this device"
    );
}

#[test]
fn a_refusal_is_worded_as_the_app_says_it() {
    let failure = crate::Failure::refused(403, "HTTP 403");
    assert_eq!(failure.reason, Reason::Refused);
    assert_eq!(failure.status, 403);
}
