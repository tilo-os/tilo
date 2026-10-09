//! Short-lived shared Tilo rooms next to a website. See README.md.
mod config;
mod room;
mod tls;

use anyhow::{Context, Result};
use bytes::Bytes;
use config::Config;
use http_body_util::{BodyExt, Full, Limited, StreamBody, combinators::BoxBody};
use hyper::body::Frame;
use hyper::{Method, Request, Response, StatusCode, header};
use room::{Event, Member, Room};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;

type Body = BoxBody<Bytes, Infallible>;

struct Service {
    config: Config,
    identity: tls::HostIdentity,
    rooms: Mutex<BTreeMap<String, Arc<Room>>>,
    /// Rooms being created count against the limit before they exist.
    starting: Mutex<usize>,
}

fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into()).boxed()
}

/// Percent-encodes everything but RFC 3986 unreserved characters.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The body of `POST /rooms/<id>/admit`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdmitRequest {
    ticket: String,
    pairing_code: String,
}

/// One `member` server-sent event.
fn member_event(member: &Member) -> String {
    format!(
        "event: member\ndata: {}\n\n",
        serde_json::to_string(member).unwrap_or_default()
    )
}

fn content_type(path: &str) -> Option<&'static str> {
    Some(match path.rsplit_once('.')?.1 {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "json" => "application/json",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "ttc" => "font/collection",
        "woff2" => "font/woff2",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => return None,
    })
}

impl Service {
    fn cors(&self, origin: Option<&str>, mut response: Response<Body>) -> Response<Body> {
        let headers = response.headers_mut();
        if let Some(value) = origin
            .filter(|origin| self.config.allows(origin))
            .and_then(|origin| header::HeaderValue::from_str(origin).ok())
        {
            headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
            headers.insert(header::VARY, header::HeaderValue::from_static("origin"));
        }
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            header::HeaderValue::from_static("GET, POST, OPTIONS"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            header::HeaderValue::from_static("content-type"),
        );
        response
    }

    fn json(status: StatusCode, value: &impl serde::Serialize) -> Response<Body> {
        let mut response = Response::new(full(serde_json::to_string(value).unwrap_or_default()));
        *response.status_mut() = status;
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
        response
    }

    fn error(status: StatusCode, message: &str) -> Response<Body> {
        Self::json(status, &serde_json::json!({"error": message}))
    }

    fn room(&self, id: &str) -> Option<Arc<Room>> {
        self.rooms.lock().ok()?.get(id).cloned()
    }

    fn free_port(&self) -> Option<u16> {
        let rooms = self.rooms.lock().ok()?;
        (0..64u16)
            .filter_map(|offset| self.config.port_base.checked_add(offset))
            .find(|port| {
                rooms.values().all(|room| room.port != *port)
                    && std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok()
                    && std::net::UdpSocket::bind(("127.0.0.1", *port)).is_ok()
            })
    }

    fn join_url(&self, room: &Room, ticket: &str) -> String {
        format!(
            "{}/app/join.html#origin={}&user={}&room={}&ticket={}&admit={}",
            self.config.public_api,
            encode(&room.origin),
            room.user_public_key,
            room.uuid,
            ticket,
            encode(&format!(
                "{}/rooms/{}/admit",
                self.config.public_api, room.id
            )),
        )
    }

    async fn create(self: &Arc<Self>) -> Result<serde_json::Value, (StatusCode, String)> {
        {
            let poisoned = || {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "room registry poisoned".to_owned(),
                )
            };
            let open = self.rooms.lock().map_err(|_| poisoned())?.len();
            let mut starting = self.starting.lock().map_err(|_| poisoned())?;
            if open.saturating_add(*starting) >= self.config.max_rooms {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    "all rooms are in use; try again later".into(),
                ));
            }
            *starting = starting.saturating_add(1);
            drop(starting);
        }
        let result = self.start_room().await;
        if let Ok(mut starting) = self.starting.lock() {
            *starting = starting.saturating_sub(1);
            drop(starting);
        }
        result.map_err(|error| {
            eprintln!("tilo-room-service: room start failed: {error:#}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "the room could not be started".into(),
            )
        })
    }

    async fn start_room(self: &Arc<Self>) -> Result<serde_json::Value> {
        let id = hex::encode(rand::random::<[u8; 6]>());
        let port = self.free_port().context("no free room port")?;
        let (room, mut child) = Room::start(&self.config, &self.identity, id.clone(), port).await?;
        self.rooms
            .lock()
            .map_err(|_| anyhow::anyhow!("room registry poisoned"))?
            .insert(id.clone(), Arc::clone(&room));
        let (registry, expiring, ttl) = (Arc::clone(self), Arc::clone(&room), self.config.ttl);
        tokio::spawn(async move {
            tokio::select! {
                () = tokio::time::sleep(ttl) => {}
                _ = child.wait() => {
                    eprintln!("tilo-room-service: room {} relay exited", expiring.id);
                }
                () = expiring.broken() => {
                    eprintln!("tilo-room-service: room {} relay broke the operator protocol", expiring.id);
                }
            }
            if let Ok(mut rooms) = registry.rooms.lock() {
                rooms.remove(&expiring.id);
            }
            expiring.stop(child).await;
        });
        // The page's ticket, then the first guest's.
        let limit = self.config.max_guests.saturating_add(1);
        let page = room.ticket(limit).context("no ticket for the page")?;
        let guest = room.ticket(limit).context("no ticket for a guest")?;
        let api = &self.config.public_api;
        Ok(serde_json::json!({
            "roomId": room.id,
            "origin": room.origin,
            "room": room.uuid,
            "userPublicKey": room.user_public_key,
            "expiresAt": room.expires_at_ms,
            "pageTicket": page,
            "joinUrl": self.join_url(&room, &guest),
            "admit": format!("{api}/rooms/{}/admit", room.id),
            "events": format!("{api}/rooms/{}/events", room.id),
            "invitations": format!("{api}/rooms/{}/invitations", room.id),
            "app": format!("{api}/app/"),
        }))
    }

    fn events(room: &Arc<Room>) -> Response<Body> {
        let (history, receiver) = room.subscribe();
        let mut backlog: std::collections::VecDeque<String> =
            std::iter::once(": connected\n\n".to_owned())
                .chain(history.iter().map(member_event))
                .collect();
        let stream = futures::stream::unfold((receiver, false), move |(mut receiver, done)| {
            let next = backlog.pop_front();
            async move {
                if let Some(chunk) = next {
                    return Some((Ok(Frame::data(Bytes::from(chunk))), (receiver, done)));
                }
                if done {
                    return None;
                }
                let chunk = loop {
                    break match tokio::time::timeout(Duration::from_secs(15), receiver.recv()).await
                    {
                        Err(_) => ": keep-alive\n\n".to_owned(),
                        Ok(Ok(Event::Member(member))) => member_event(&member),
                        // A refused admission concerns only the guest who asked.
                        Ok(Ok(Event::Refused { .. })) => continue,
                        Ok(
                            Ok(Event::Expired)
                            | Err(tokio::sync::broadcast::error::RecvError::Closed),
                        ) => {
                            return Some((
                                Ok(Frame::data(Bytes::from_static(
                                    b"event: expired\ndata: {}\n\n",
                                ))),
                                (receiver, true),
                            ));
                        }
                        Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                            ": lagged\n\n".to_owned()
                        }
                    };
                };
                Some((Ok(Frame::data(Bytes::from(chunk))), (receiver, false)))
            }
        });
        let mut response = Response::new(StreamBody::new(stream).boxed());
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/event-stream"),
        );
        headers.insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
        response
    }

    async fn asset(&self, path: &str) -> Response<Body> {
        let relative = path.trim_start_matches('/');
        let relative = if relative.is_empty() {
            "join.html"
        } else {
            relative
        };
        let Some(kind) = content_type(relative) else {
            return Self::error(StatusCode::NOT_FOUND, "not found");
        };
        if relative
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.'))
            || relative.contains('\\')
        {
            return Self::error(StatusCode::NOT_FOUND, "not found");
        }
        match tokio::fs::read(self.config.assets.join(relative)).await {
            Ok(bytes) => {
                let mut response = Response::new(full(bytes));
                let headers = response.headers_mut();
                headers.insert(header::CONTENT_TYPE, header::HeaderValue::from_static(kind));
                headers.insert(
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static("no-cache"),
                );
                response
            }
            Err(_) => Self::error(StatusCode::NOT_FOUND, "not found"),
        }
    }

    async fn admit(&self, id: &str, body: hyper::body::Incoming) -> Response<Body> {
        let Some(room) = self.room(id) else {
            return Self::error(StatusCode::NOT_FOUND, "no such room");
        };
        let Ok(body) = Limited::new(body, 4096)
            .collect()
            .await
            .map(http_body_util::Collected::to_bytes)
        else {
            return Self::error(StatusCode::BAD_REQUEST, "invalid request");
        };
        let request = match serde_json::from_slice::<AdmitRequest>(&body) {
            Ok(request) => request,
            Err(error) if error.is_data() => {
                return Self::error(
                    StatusCode::BAD_REQUEST,
                    "ticket and pairingCode are required",
                );
            }
            Err(_) => return Self::error(StatusCode::BAD_REQUEST, "invalid request"),
        };
        match room.admit(&request.ticket, &request.pairing_code).await {
            Ok(member) => Self::json(StatusCode::OK, &member),
            Err(error) => Self::error(StatusCode::FORBIDDEN, &format!("{error:#}")),
        }
    }

    async fn handle(self: Arc<Self>, request: Request<hyper::body::Incoming>) -> Response<Body> {
        let origin = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let path = request.uri().path().to_owned();
        let method = request.method().clone();
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        let response = match (&method, parts.as_slice()) {
            (&Method::OPTIONS, _) => Response::new(full(Bytes::new())),
            (&Method::GET, ["healthz"]) => {
                Self::json(StatusCode::OK, &serde_json::json!({"ok": true}))
            }
            (&Method::GET, ["app", ..]) => {
                self.asset(path.strip_prefix("/app").unwrap_or_default())
                    .await
            }
            (&Method::POST, ["rooms"]) => match self.create().await {
                Ok(value) => Self::json(StatusCode::CREATED, &value),
                Err((status, message)) => Self::error(status, &message),
            },
            (&Method::GET, ["rooms", id, "events"]) => match self.room(id) {
                Some(room) => Self::events(&room),
                None => Self::error(StatusCode::NOT_FOUND, "no such room"),
            },
            (&Method::POST, ["rooms", id, "admit"]) => {
                let id = (*id).to_owned();
                self.admit(&id, request.into_body()).await
            }
            (&Method::POST, ["rooms", id, "invitations"]) => match self.room(id) {
                None => Self::error(StatusCode::NOT_FOUND, "no such room"),
                Some(room) => match room.ticket(self.config.max_guests.saturating_add(1)) {
                    Some(ticket) => Self::json(
                        StatusCode::CREATED,
                        &serde_json::json!({"joinUrl": self.join_url(&room, &ticket), "ticket": ticket}),
                    ),
                    None => Self::error(
                        StatusCode::TOO_MANY_REQUESTS,
                        "this room has all the guests it takes",
                    ),
                },
            },
            _ => Self::error(StatusCode::NOT_FOUND, "not found"),
        };
        self.cors(origin.as_deref(), response)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    std::fs::create_dir_all(&config.state).context("create TILO_ROOMS_STATE")?;
    let public = tls::public_config(config.certificate.as_ref(), &config.domain)?;
    let identity = tls::HostIdentity::generate()?;
    let host_certificate = identity.certificate.clone();
    let (api, front) = (
        TcpListener::bind(config.api).await?,
        TcpListener::bind(config.tls).await?,
    );
    eprintln!(
        "tilo-room-service: API {} (listening {}), rooms on {} (TLS {})",
        config.public_api,
        config.api,
        config.origin("<room>", config.port_base),
        config.tls
    );
    let domain = config.domain.clone();
    let service = Arc::new(Service {
        config,
        identity,
        rooms: Mutex::default(),
        starting: Mutex::new(0),
    });
    let lookup = Arc::clone(&service);
    tokio::spawn(tls::serve(
        front,
        public,
        domain,
        host_certificate,
        move |id| lookup.room(id).map(|room| room.port),
    ));
    loop {
        let (socket, _) = api.accept().await?;
        let service = Arc::clone(&service);
        tokio::spawn(async move {
            let handler = hyper::service::service_fn(move |request| {
                let service = Arc::clone(&service);
                async move { Ok::<_, Infallible>(service.handle(request).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(socket), handler)
                .await;
        });
    }
}
