//! Reaching Open-Meteo: the bytes a service answers a forecast, an air-quality reading or a
//! place search with, over HTTP.
//!
//! Nothing here draws or waits on a frame: a job calls these on worker threads, and the app
//! takes what landed when it asks.

use std::io::Read;
use std::time::Duration;

use crate::api;
use crate::{Failure, Found};

/// How long one request may take: a service that has not answered by then is unreachable for
/// this read.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The most of an answer read: a forecast or a search larger than this is not one.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// The body `url` answers with.
///
/// # Errors
///
/// Why the service could not be reached, or refused the read.
fn body(url: &str) -> Result<String, Failure> {
    let client = tilo_net::http::blocking_client()
        .user_agent(concat!(
            "Tilo-Weather/",
            env!("CARGO_PKG_VERSION"),
            " (+https://tilo.so)"
        ))
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| Failure::unavailable(format!("no http client: {e}")))?;
    let response = client
        .get(url)
        .send()
        .map_err(|e| Failure::unreachable(format!("request: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(Failure::refused(
            status.as_u16(),
            format!("HTTP {}", status.as_u16()),
        ));
    }
    let mut body = Vec::new();
    response
        .take(MAX_BYTES)
        .read_to_end(&mut body)
        .map_err(|e| Failure::unreachable(format!("body: {e}")))?;
    String::from_utf8(body).map_err(|e| Failure::not_a_reading(format!("body is not text: {e}")))
}

/// A place's forecast and air quality. The air reading is the second half of one answer: a
/// service that does not answer it leaves the forecast standing.
///
/// # Errors
///
/// Why the forecast could not be read.
pub fn place(latitude: f64, longitude: f64) -> Result<Found, Failure> {
    let forecast = body(&api::forecast_url(latitude, longitude))?;
    let readings = api::read_forecast(&forecast).map_err(Failure::not_a_reading)?;
    let air = body(&api::air_quality_url(latitude, longitude))
        .and_then(|text| api::read_air(&text).map_err(Failure::not_a_reading))
        .ok();
    Ok(Found { readings, air })
}

/// The places a query names.
///
/// # Errors
///
/// Why the search could not be read.
pub fn search(query: &str) -> Result<Vec<api::Place>, Failure> {
    let body = body(&api::geocode_url(query))?;
    let places = api::read_geocode(&body).map_err(Failure::not_a_reading)?;
    Ok(api::refine_geocode(query, places))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_name_is_escaped_for_the_query_string() {
        let url = api::geocode_url("New York");
        assert!(url.contains("name=New%20York"), "{url}");
        let url = api::geocode_url("München");
        assert!(url.contains("name=M%C3%BCnchen"), "{url}");
        let url = api::geocode_url("São Paulo & co");
        assert!(url.contains("name=S%C3%A3o%20Paulo%20%26%20co"), "{url}");
    }
}
