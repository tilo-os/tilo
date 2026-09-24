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
    Tracked {
        ticket: u64,
        generation: u64,
        request: Box<Request>,
    },
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

impl Request {
    pub(crate) fn kind(&self) -> RequestKind {
        match self {
            Self::Tracked { request, .. } => request.kind(),
            Self::Forecast { .. } => RequestKind::Forecast,
            Self::AirQuality { .. } => RequestKind::AirQuality,
            Self::Geocode { .. } => RequestKind::Geocode,
        }
    }
    fn failure(self, message: String) -> Response {
        let kind = match self {
            Self::Tracked {
                ticket,
                generation,
                request,
            } => {
                return Response::Tracked {
                    ticket,
                    generation,
                    response: Box::new(request.failure(message)),
                };
            }
            Self::Forecast { .. } => RequestKind::Forecast,
            Self::AirQuality { .. } => RequestKind::AirQuality,
            Self::Geocode { .. } => RequestKind::Geocode,
        };
        Response::Error { kind, message }
    }
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
    Tracked {
        ticket: u64,
        generation: u64,
        response: Box<Response>,
    },
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

impl Response {
    pub(crate) fn is_weather(&self) -> bool {
        match self {
            Self::Tracked { response, .. } => response.is_weather(),
            Self::Forecast(_) | Self::AirQuality(_) => true,
            Self::Error { kind, .. } => *kind != RequestKind::Geocode,
            Self::Geocode { .. } => false,
        }
    }
    pub(crate) fn weather_error(&self) -> Option<&str> {
        match self {
            Self::Tracked { response, .. } => response.weather_error(),
            Self::Error { kind, message } if *kind != RequestKind::Geocode => Some(message),
            _ => None,
        }
    }
}
fn send_response(tx: &ResponseSender, context: Option<(u64, u64)>, response: Response) {
    let _ = tx.send(if let Some((ticket, generation)) = context {
        Response::Tracked {
            ticket,
            generation,
            response: Box::new(response),
        }
    } else {
        response
    });
}

/// Handle to the background worker. Dropping it closes the request channel
/// and the worker thread exits after its current job.
pub struct WeatherService {
    tx: Sender<Request>,
    rx: Receiver<Response>,
}

struct ResponseSender {
    sender: Sender<Response>,
    wake: ui::wake::WakeHandle,
}

impl ResponseSender {
    fn send(&self, response: Response) -> Result<(), std::sync::mpsc::SendError<Response>> {
        self.sender.send(response)?;
        self.wake.wake();
        Ok(())
    }
}

impl WeatherService {
    /// Spawn the worker thread + its Tokio runtime and reqwest client.
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = channel::<Request>();
        let (res_tx, res_rx) = channel::<Response>();
        let res_tx = ResponseSender {
            sender: res_tx,
            wake: ui::wake::WakeHandle::new(),
        };

        let _ = thread::Builder::new()
            .name("weather-net".to_string())
            .spawn(move || worker(&req_rx, &res_tx));

        Self {
            tx: req_tx,
            rx: res_rx,
        }
    }

    /// Queue a job, returning an exact failed completion if the worker is gone.
    pub fn request(&self, req: Request) -> Option<Response> {
        self.tx
            .send(req)
            .err()
            .map(|e| e.0.failure("Weather worker disconnected".into()))
    }

    /// Drain all ready responses without blocking (used by the standalone probe).
    pub fn drain(&self) -> Vec<Response> {
        self.drain_with_status().0
    }

    /// An empty live queue and a permanently closed response channel are
    /// different states. Drain buffered real outcomes before reporting closure.
    pub(crate) fn drain_with_status(&self) -> (Vec<Response>, bool) {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(response) => out.push(response),
                Err(std::sync::mpsc::TryRecvError::Empty) => return (out, false),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return (out, true),
            }
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "keep the three existing provider branches together with their exact request completion envelopes"
)]
fn worker(req_rx: &Receiver<Request>, res_tx: &ResponseSender) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            while let Ok(req) = req_rx.recv() {
                let _ = res_tx.send(req.failure(format!("tokio init: {e}")));
            }
            return;
        }
    };
    let client = tilo_net::http::client()
        .user_agent("tilo-weather/0.1 (+https://open-meteo.com)")
        .build()
        .ok();

    while let Ok(req) = req_rx.recv() {
        let (req, context) = match req {
            Request::Tracked {
                ticket,
                generation,
                request,
            } => (*request, Some((ticket, generation))),
            request => (request, None),
        };
        match req {
            Request::Tracked { .. } => {}
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
                            send_response(res_tx, context, Response::Forecast(snap));
                        }
                        Err(e) => {
                            send_response(
                                res_tx,
                                context,
                                Response::Error {
                                    kind: RequestKind::Forecast,
                                    message: e,
                                },
                            );
                        }
                    },
                    Err(e) => {
                        send_response(
                            res_tx,
                            context,
                            Response::Error {
                                kind: RequestKind::Forecast,
                                message: e,
                            },
                        );
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
                            send_response(res_tx, context, Response::AirQuality(aq));
                        }
                        Err(e) => {
                            send_response(
                                res_tx,
                                context,
                                Response::Error {
                                    kind: RequestKind::AirQuality,
                                    message: e,
                                },
                            );
                        }
                    },
                    Err(e) => {
                        send_response(
                            res_tx,
                            context,
                            Response::Error {
                                kind: RequestKind::AirQuality,
                                message: e,
                            },
                        );
                    }
                }
            }
            Request::Geocode { seq, query } => {
                let url = api::geocode_url(&query);
                match rt.block_on(fetch(client.as_ref(), &url)) {
                    Ok(body) => match api::parse_geocode(&body) {
                        Ok(results) => {
                            send_response(res_tx, context, Response::Geocode { seq, results });
                        }
                        Err(e) => {
                            send_response(
                                res_tx,
                                context,
                                Response::Error {
                                    kind: RequestKind::Geocode,
                                    message: e,
                                },
                            );
                        }
                    },
                    Err(e) => {
                        send_response(
                            res_tx,
                            context,
                            Response::Error {
                                kind: RequestKind::Geocode,
                                message: e,
                            },
                        );
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

#[cfg(test)]
pub(crate) fn test_service() -> (WeatherService, Receiver<Request>, Sender<Response>) {
    let (tx, requests) = channel();
    let (responses, rx) = channel();
    (WeatherService { tx, rx }, requests, responses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worker_response_wakes_its_owning_app_without_animation() {
        let (events, received) = channel();
        let mut scope = ui::wake::WakeScope::new();
        scope
            .bind(move |event| {
                events.send(event).unwrap();
            })
            .unwrap();
        let _entry = scope.enter();
        let (sender, responses) = channel();
        let sender = ResponseSender {
            sender,
            wake: ui::wake::WakeHandle::new(),
        };
        std::thread::spawn(move || {
            sender
                .send(Response::Geocode {
                    seq: 7,
                    results: Vec::new(),
                })
                .unwrap()
        })
        .join()
        .unwrap();
        received
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("worker must wake the app");
        assert!(matches!(
            responses.try_recv().unwrap(),
            Response::Geocode { seq: 7, .. }
        ));
    }
}
