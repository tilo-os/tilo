//! Background networking worker (live binary only).
//!
//! `--shots` never touches this module — the offscreen frame renders the
//! bundled [`crate::sample`] data. In the live app the [`WeatherApp`] owns
//! one [`WeatherService`]: a detached worker thread running a Tokio runtime
//! and a `reqwest` client. The widget posts [`Request`]s down an mpsc
//! channel and drains [`Response`]s (non-blocking) from another, applying
//! them to the model on the next frame. While a request is in flight the
//! widget reports `needs_animation_frame()`, so the event loop keeps
//! pumping frames and the result surfaces within a few hundred ms of
//! arrival.
//!
//! [`WeatherApp`]: crate::app::WeatherApp

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use crate::api;
use crate::model::{AirQuality, LocationId, SavedLocation, WeatherSnapshot};
use crate::units::Units;

/// A job for the network worker.
pub enum Request {
    Forecast {
        location_id: LocationId,
        lat: f64,
        lon: f64,
        units: Units,
    },
    AirQuality {
        location_id: LocationId,
        lat: f64,
        lon: f64,
    },
    Geocode {
        seq: u64,
        query: String,
    },
}

/// Identifies which request produced a response. Keeping this on failures is
/// important: a geocoding failure must not complete an unrelated forecast
/// refresh that happens to be running at the same time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Forecast,
    AirQuality,
    Geocode,
}

/// A completed job (or a classified error for the status line).
pub enum Response {
    Forecast(WeatherSnapshot),
    AirQuality(AirQuality),
    Geocode {
        seq: u64,
        results: Vec<SavedLocation>,
    },
    Error {
        kind: RequestKind,
        message: String,
    },
}

/// Handle to the background worker. Dropping it closes the request channel
/// and the worker thread exits after its current job.
pub struct WeatherService {
    tx: Sender<Request>,
    rx: Receiver<Response>,
}

impl WeatherService {
    /// Spawn the worker thread + its Tokio runtime and reqwest client.
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = channel::<Request>();
        let (res_tx, res_rx) = channel::<Response>();

        let _ = thread::Builder::new()
            .name("weather-net".to_string())
            .spawn(move || worker(&req_rx, &res_tx));

        Self {
            tx: req_tx,
            rx: res_rx,
        }
    }

    /// Queue a job. Errors (worker gone) are swallowed — the UI keeps its
    /// last good data.
    pub fn request(&self, req: Request) {
        let _ = self.tx.send(req);
    }

    /// Drain all ready responses without blocking.
    pub fn drain(&self) -> Vec<Response> {
        let mut out = Vec::new();
        while let Ok(r) = self.rx.try_recv() {
            out.push(r);
        }
        out
    }
}

fn worker(req_rx: &Receiver<Request>, res_tx: &Sender<Response>) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = res_tx.send(Response::Error {
                kind: RequestKind::Forecast,
                message: format!("tokio init: {e}"),
            });
            return;
        }
    };
    let client = reqwest::Client::builder()
        .user_agent("qos-weather/0.1 (+https://open-meteo.com)")
        .build()
        .ok();

    while let Ok(req) = req_rx.recv() {
        match req {
            Request::Forecast {
                location_id,
                lat,
                lon,
                units,
            } => {
                let url = api::forecast_url(lat, lon, units);
                match rt.block_on(fetch(client.as_ref(), &url)) {
                    Ok(body) => match api::parse_forecast(&body, location_id, units, now_secs()) {
                        Ok(snap) => {
                            let _ = res_tx.send(Response::Forecast(snap));
                        }
                        Err(e) => {
                            let _ = res_tx.send(Response::Error {
                                kind: RequestKind::Forecast,
                                message: e,
                            });
                        }
                    },
                    Err(e) => {
                        let _ = res_tx.send(Response::Error {
                            kind: RequestKind::Forecast,
                            message: e,
                        });
                    }
                }
            }
            Request::AirQuality {
                location_id,
                lat,
                lon,
            } => {
                let url = api::air_quality_url(lat, lon);
                match rt.block_on(fetch(client.as_ref(), &url)) {
                    Ok(body) => match api::parse_air_quality(&body, location_id, now_secs()) {
                        Ok(aq) => {
                            let _ = res_tx.send(Response::AirQuality(aq));
                        }
                        Err(e) => {
                            let _ = res_tx.send(Response::Error {
                                kind: RequestKind::AirQuality,
                                message: e,
                            });
                        }
                    },
                    Err(e) => {
                        let _ = res_tx.send(Response::Error {
                            kind: RequestKind::AirQuality,
                            message: e,
                        });
                    }
                }
            }
            Request::Geocode { seq, query } => {
                let url = api::geocode_url(&query);
                match rt.block_on(fetch(client.as_ref(), &url)) {
                    Ok(body) => match api::parse_geocode(&body) {
                        Ok(results) => {
                            let _ = res_tx.send(Response::Geocode { seq, results });
                        }
                        Err(e) => {
                            let _ = res_tx.send(Response::Error {
                                kind: RequestKind::Geocode,
                                message: e,
                            });
                        }
                    },
                    Err(e) => {
                        let _ = res_tx.send(Response::Error {
                            kind: RequestKind::Geocode,
                            message: e,
                        });
                    }
                }
            }
        }
    }
}

async fn fetch(client: Option<&reqwest::Client>, url: &str) -> Result<String, String> {
    let client = client.ok_or_else(|| "no http client".to_string())?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("http {}", resp.status()));
    }
    resp.text().await.map_err(|e| format!("read body: {e}"))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
