//! Background fetches: each [`Request`] runs as its own task on the app runtime
//! through `tilo_net`, and its [`Response`] wakes the UI on a channel. A capture
//! never touches this.

use std::sync::mpsc::{Receiver, Sender, channel};

use tilo_clock::unix_now;

use crate::api;
use crate::model::{AirQuality, LocationId, SavedLocation, WeatherSnapshot};
use crate::units::Units;

pub(crate) const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

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

/// Which request produced a response: a geocode failure must not settle a forecast
/// refresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Forecast,
    AirQuality,
    Geocode,
}

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

/// Dropping it discards the responses still in flight.
pub struct WeatherService {
    dispatch: Dispatch,
    rx: Receiver<Response>,
}

enum Dispatch {
    Live {
        client: Option<tilo_net::Client>,
        responses: ResponseSender,
    },
    #[cfg(test)]
    Recorded(Sender<Request>),
}

#[derive(Clone)]
struct ResponseSender {
    sender: Sender<Response>,
    wake: ui::wake::WakeHandle,
}

impl ResponseSender {
    fn send(&self, response: Response) -> Result<(), std::sync::mpsc::SendError<()>> {
        self.sender
            .send(response)
            .map_err(|_| std::sync::mpsc::SendError(()))?;
        self.wake.wake();
        Ok(())
    }
}

impl WeatherService {
    pub fn spawn() -> Self {
        let (sender, rx) = channel::<Response>();
        let client = tilo_net::Client::builder()
            .user_agent("tilo-weather/0.1 (+https://open-meteo.com)")
            // A reply that never comes ends as an error the forecast shows with Retry.
            .timeout(REQUEST_TIMEOUT)
            .build()
            .ok();
        Self {
            dispatch: Dispatch::Live {
                client,
                responses: ResponseSender {
                    sender,
                    wake: ui::wake::WakeHandle::new(),
                },
            },
            rx,
        }
    }

    pub fn request(&self, req: Request) -> Option<Response> {
        match &self.dispatch {
            Dispatch::Live { client, responses } => match ui::rt::try_app() {
                Ok(runtime) => {
                    drop(runtime.spawn(serve(client.clone(), req, responses.clone())));
                    None
                }
                Err(e) => Some(req.failure(format!("app runtime: {e}"))),
            },
            #[cfg(test)]
            Dispatch::Recorded(requests) => requests
                .send(req)
                .err()
                .map(|e| e.0.failure("Weather worker disconnected".into())),
        }
    }

    pub fn wait(&self, timeout: std::time::Duration) -> Option<Response> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Buffered real outcomes drain before closure is reported.
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

async fn serve(client: Option<tilo_net::Client>, req: Request, responses: ResponseSender) {
    let (req, context) = match req {
        Request::Tracked {
            ticket,
            generation,
            request,
        } => (*request, Some((ticket, generation))),
        request => (request, None),
    };
    let response = match req {
        Request::Tracked { .. } => return,
        Request::Forecast {
            location_id,
            lat,
            lon,
            units,
        } => fetch(client.as_ref(), &api::forecast_url(lat, lon, units))
            .await
            .and_then(|body| api::parse_forecast(&body, location_id, units, unix_now()))
            .map_or_else(
                |message| Response::Error {
                    kind: RequestKind::Forecast,
                    message,
                },
                Response::Forecast,
            ),
        Request::AirQuality {
            location_id,
            lat,
            lon,
        } => fetch(client.as_ref(), &api::air_quality_url(lat, lon))
            .await
            .and_then(|body| api::parse_air_quality(&body, location_id, unix_now()))
            .map_or_else(
                |message| Response::Error {
                    kind: RequestKind::AirQuality,
                    message,
                },
                Response::AirQuality,
            ),
        Request::Geocode { seq, query } => fetch(client.as_ref(), &api::geocode_url(&query))
            .await
            .and_then(|body| api::parse_geocode(&body))
            .map(|results| api::refine_geocode(&query, results))
            .map_or_else(
                |message| Response::Error {
                    kind: RequestKind::Geocode,
                    message,
                },
                |results| Response::Geocode { seq, results },
            ),
    };
    send_response(&responses, context, response);
}

async fn fetch(client: Option<&tilo_net::Client>, url: &str) -> Result<String, String> {
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

#[cfg(test)]
pub(crate) fn test_service() -> (WeatherService, Receiver<Request>, Sender<Response>) {
    let (tx, requests) = channel();
    let (responses, rx) = channel();
    (
        WeatherService {
            dispatch: Dispatch::Recorded(tx),
            rx,
        },
        requests,
        responses,
    )
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
                .unwrap();
        })
        .join()
        .unwrap();
        received
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("worker must wake the app");
        assert!(matches!(
            responses.try_recv().unwrap(),
            Response::Geocode { seq: 7, .. }
        ));
    }
}
