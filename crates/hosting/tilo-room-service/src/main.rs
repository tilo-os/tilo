//! Shared Tilo rooms next to a website: short-lived rooms on request, or
//! one persistent public room. See README.md.
mod config;
mod discord_sign_in;
mod guests;
mod limits;
mod mail;
mod ops;
mod outbox;
mod persistent;
mod room;
mod tls;

use anyhow::{Context, Result};
use bytes::Bytes;
use config::Config;
use http_body_util::{BodyExt, Full, Limited, StreamBody, combinators::BoxBody};
use hyper::body::Frame;
use hyper::{Method, Request, Response, StatusCode, header};
use ops::{JoinFailure, Mode};
use room::{Event, Member, Room};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;

type Body = BoxBody<Bytes, Infallible>;

struct Service {
    config: Arc<Config>,
    identity: tls::HostIdentity,
    persistent: Option<Arc<persistent::Persistent>>,
    rooms: Mutex<BTreeMap<String, Arc<Room>>>,
    /// Rooms being created count against the limit before they exist.
    starting: Mutex<usize>,
}

fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into()).boxed()
}

/// A guest's link into a room, in the web link grammar: the Writer full tab
/// (on `record` when given) under `entry_base`, with the guest `ticket` as
/// the fragment's secret. The page learns the room's root key from the
/// room's host and redeems the ticket at the door `entry.json` names, so
/// neither rides in the link.
fn guest_link(
    entry_base: &str,
    origin: &str,
    room: &str,
    ticket: &str,
    record: Option<&str>,
) -> Result<String> {
    use tilo_reference::link::{
        AppId, HostOrigin, Link, LinkSecrets, LinkTarget, Mode, RecordSegment, Scope, Secret,
        WebEntry,
    };
    let mut target = LinkTarget::new(Scope::Room {
        host: HostOrigin::new(origin)?,
        room: room.parse().context("room uuid")?,
        slug: None,
    });
    target.app = Some(AppId::new(WRITER)?);
    target.record = record.map(RecordSegment::new).transpose()?;
    let link = Link {
        mode: Mode::App,
        target,
        secrets: LinkSecrets {
            ticket: Some(Secret::new(ticket)?),
            ..LinkSecrets::default()
        },
    };
    Ok(link.to_url(&WebEntry::new(entry_base)?)?)
}

/// The body of `POST /rooms/<id>/invitations`, which may be empty.
#[derive(serde::Deserialize, Default)]
struct InvitationRequest {
    /// The Writer record the guest's link opens; absent, the room's default.
    #[serde(default)]
    record: Option<String>,
}

/// The app a guest's link opens full tab.
const WRITER: &str = "so.tilo.word-processor";

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
        // `ftl`: the entry pages' arrival lines (`locales/`).
        "txt" | "md" | "ftl" => "text/plain; charset=utf-8",
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

    /// A room by the service's id, or by its room uuid as links name it.
    /// Only for what anyone holding a guest link may do (redeem a ticket);
    /// the owner's routes go through [`Self::owned_room`].
    fn room(&self, id: &str) -> Option<Arc<Room>> {
        let rooms = self.rooms.lock().ok()?;
        rooms
            .get(id)
            .or_else(|| rooms.values().find(|room| room.uuid == id))
            .cloned()
    }

    /// The room with service id `id`, when `query` carries its owner key:
    /// the capability the `POST /rooms` answer hands the room's creator for
    /// minting invitations and watching members. Both names that route to
    /// a room are public (the id is its hostname, the uuid is in every
    /// guest link), so neither alone opens these routes, and the uuid not
    /// at all. A missing or wrong key reads as no room, so the routes are
    /// no oracle for which rooms are live.
    fn owned_room(&self, id: &str, query: Option<&str>) -> Option<Arc<Room>> {
        let presented = query?
            .split('&')
            .find_map(|pair| pair.strip_prefix("key="))?;
        let room = self.rooms.lock().ok()?.get(id).cloned()?;
        room.owned_by(presented).then_some(room)
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

    /// The web entry guest links name: the bundle again, under `/open/`. Not
    /// `/app/`, whose own `app/` segment is how the link grammar says "full
    /// tab", so an entry cannot end in it.
    fn entry_base(&self) -> String {
        format!("{}/open/", self.config.public_api)
    }

    /// A guest's link into `room`; see [`guest_link`].
    fn join_url(&self, room: &Room, ticket: &str, record: Option<&str>) -> Result<String> {
        guest_link(&self.entry_base(), &room.origin, &room.uuid, ticket, record)
    }

    /// `entry.json` for the bundle's pages: where they redeem guest tickets.
    fn entry_config(&self) -> Response<Body> {
        Self::json(
            StatusCode::OK,
            &serde_json::json!({"tickets": format!("{}/rooms/", self.config.public_api)}),
        )
    }

    /// `GET /` of a persistent room. The service is the room's API, not a
    /// page: the Tilo app lives at its web entry, so a visitor goes there.
    fn front_door(web_entry: Option<&str>) -> Response<Body> {
        let Some(location) = web_entry.and_then(|entry| header::HeaderValue::from_str(entry).ok())
        else {
            return Self::error(StatusCode::NOT_FOUND, "not found");
        };
        let mut response = Response::new(full(Bytes::new()));
        *response.status_mut() = StatusCode::FOUND;
        response.headers_mut().insert(header::LOCATION, location);
        response
    }

    /// `GET /discord/callback`: back from Discord's consent page, on to the
    /// web entry, or a page saying what went wrong.
    async fn discord_callback(
        &self,
        persistent: &persistent::Persistent,
        query: &str,
    ) -> Response<Body> {
        let web_entry = self
            .config
            .persistent
            .as_ref()
            .and_then(|persistent| persistent.web_entry.clone())
            .unwrap_or_default();
        match persistent.discord_callback(query, &web_entry).await {
            None => Self::error(StatusCode::NOT_FOUND, "not found"),
            Some(Ok(onward)) => Self::front_door(Some(&onward)),
            Some(Err(discord_sign_in::Failed::Declined)) => Self::front_door(Some(&web_entry)),
            Some(Err(failed)) => Self::sign_in_failed(failed.said(), &web_entry),
        }
    }

    /// A sign-in with Discord that linked nothing: a plain page saying why
    /// and what to do, with the way back into Tilo.
    fn sign_in_failed(said: &str, web_entry: &str) -> Response<Body> {
        let escape = |text: &str| {
            text.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
        };
        let page = format!(
            "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\">\
             <title>Linking Discord didn't work</title>\
             <body style=\"font:16px/1.5 system-ui,sans-serif;max-width:32rem;margin:15vh auto;padding:0 1.5rem;color:#1c1c1e\">\
             <h1 style=\"font-size:1.4rem\">Linking Discord didn't work</h1><p>{}</p>\
             <p><a href=\"{}\" style=\"color:#5865f2\">Back to Tilo</a></p>",
            escape(said),
            escape(web_entry)
        );
        let mut response = Response::new(full(Bytes::from(page)));
        *response.status_mut() = StatusCode::BAD_REQUEST;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/html; charset=utf-8"),
        );
        response
    }

    /// One more guest's link into `room`.
    fn invitation(&self, room: &Room, record: Option<&str>) -> Response<Body> {
        let Some(ticket) = room.ticket(self.config.max_guests.saturating_add(1)) else {
            return Self::error(
                StatusCode::TOO_MANY_REQUESTS,
                "this room has all the guests it takes",
            );
        };
        match self.join_url(room, &ticket, record) {
            Ok(url) => Self::json(
                StatusCode::CREATED,
                &serde_json::json!({"joinUrl": url, "ticket": ticket}),
            ),
            Err(error) => Self::error(StatusCode::BAD_REQUEST, &format!("{error:#}")),
        }
    }

    /// Refuses new members while the operator's kill switch is on.
    fn paused() -> Option<Response<Body>> {
        let ops = ops::get()?;
        let mode = ops.mode();
        if mode.allows_joins() {
            return None;
        }
        ops.join_failed(JoinFailure::Paused);
        let mut response = Self::error(
            StatusCode::SERVICE_UNAVAILABLE,
            if mode == Mode::BackSoon {
                "the room is paused; back soon"
            } else {
                "the room is watch-only right now"
            },
        );
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, header::HeaderValue::from_static("60"));
        Some(response)
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
        self.created(&room)
    }

    /// The `POST /rooms` answer for a room just started: the page's ticket,
    /// the first guest's link, and the owner's URLs, which carry the room's
    /// owner key (a query rather than a header because an `EventSource`
    /// cannot send headers). Pages use them as given.
    fn created(&self, room: &Room) -> Result<serde_json::Value> {
        let limit = self.config.max_guests.saturating_add(1);
        let page = room.ticket(limit).context("no ticket for the page")?;
        let guest = room.ticket(limit).context("no ticket for a guest")?;
        let api = &self.config.public_api;
        let key = room.owner_key();
        Ok(serde_json::json!({
            "roomId": room.id,
            "origin": room.origin,
            "room": room.uuid,
            "userPublicKey": room.user_public_key,
            "expiresAt": room.expires_at_ms,
            "pageTicket": page,
            "joinUrl": self.join_url(room, &guest, None)?,
            "admit": format!("{api}/rooms/{}/admit", room.id),
            "events": format!("{api}/rooms/{}/events?key={key}", room.id),
            "invitations": format!("{api}/rooms/{}/invitations?key={key}", room.id),
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
                let chunk = match tokio::time::timeout(Duration::from_secs(15), receiver.recv())
                    .await
                {
                    Err(_) => ": keep-alive\n\n".to_owned(),
                    Ok(Ok(Event::Member(member))) => member_event(&member),
                    Ok(
                        Ok(Event::Expired) | Err(tokio::sync::broadcast::error::RecvError::Closed),
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
        // A directory is its index page: `/open/` is the desktop entry and
        // `/open/app/` the full-tab one.
        let relative = path.trim_start_matches('/');
        let relative = if relative.is_empty() || relative.ends_with('/') {
            format!("{relative}index.html")
        } else {
            relative.to_owned()
        };
        let relative = relative.as_str();
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

    /// The Prometheus page. Requests that came through the ingress carry
    /// `X-Forwarded-For`; those are refused so the page is reachable from
    /// inside the cluster only.
    async fn metrics(&self, headers: &hyper::HeaderMap) -> Response<Body> {
        let Some(ops) = ops::get() else {
            return Self::error(StatusCode::NOT_FOUND, "not found");
        };
        if headers.contains_key("x-forwarded-for") || headers.contains_key(header::FORWARDED) {
            return Self::error(StatusCode::NOT_FOUND, "not found");
        }
        let rooms = self.rooms.lock().map_or(0, |rooms| rooms.len());
        let page = tokio::task::spawn_blocking(move || ops.render(rooms))
            .await
            .unwrap_or_default();
        let mut response = Response::new(full(page));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
        );
        response
    }

    /// The body of a persistent room's request, decoded.
    async fn decode<T: serde::de::DeserializeOwned>(
        body: hyper::body::Incoming,
    ) -> std::result::Result<T, Response<Body>> {
        let body = Limited::new(body, 4096)
            .collect()
            .await
            .map(http_body_util::Collected::to_bytes)
            .map_err(|_| Self::error(StatusCode::BAD_REQUEST, "invalid request"))?;
        serde_json::from_slice(&body)
            .map_err(|_| Self::error(StatusCode::BAD_REQUEST, "invalid request"))
    }

    /// [`Self::decode`], with an empty body read as the default.
    async fn decode_or_default<T: serde::de::DeserializeOwned + Default>(
        body: hyper::body::Incoming,
    ) -> std::result::Result<T, Response<Body>> {
        let body = Limited::new(body, 4096)
            .collect()
            .await
            .map(http_body_util::Collected::to_bytes)
            .map_err(|_| Self::error(StatusCode::BAD_REQUEST, "invalid request"))?;
        if body.is_empty() {
            return Ok(T::default());
        }
        serde_json::from_slice(&body)
            .map_err(|_| Self::error(StatusCode::BAD_REQUEST, "invalid request"))
    }

    /// What the door answers a visitor it turned away. A room taking people
    /// by invitation says whether the visitor may leave an address to be
    /// invited at (`waitList`), and where they ask to be taken off that
    /// list (`contact`), so its door page offers that.
    fn refusal(
        refusal: persistent::JoinRefusal,
        wait_list: Option<persistent::WaitListDoor<'_>>,
    ) -> Response<Body> {
        use persistent::JoinRefusal;
        if let Some(ops) = ops::get() {
            ops.join_failed(match refusal {
                JoinRefusal::Invalid(_) => JoinFailure::Invalid,
                JoinRefusal::CoolingDown | JoinRefusal::Removed => JoinFailure::CoolingDown,
                JoinRefusal::TooMany { .. } => JoinFailure::RateLimited,
                JoinRefusal::Full { .. } => JoinFailure::Full,
                JoinRefusal::Unavailable => JoinFailure::RelayUnavailable,
                JoinRefusal::InviteOnly => JoinFailure::Refused,
            });
        }
        let (status, message, retry_after) = match refusal {
            JoinRefusal::Invalid(message) => (StatusCode::BAD_REQUEST, message, None),
            JoinRefusal::CoolingDown => (
                StatusCode::FORBIDDEN,
                "someone who joined from your address was removed from the room; try again tomorrow",
                None,
            ),
            JoinRefusal::Removed => (
                StatusCode::FORBIDDEN,
                "you were removed from this room; you can join again tomorrow",
                None,
            ),
            JoinRefusal::InviteOnly => (
                StatusCode::FORBIDDEN,
                "this room takes people by invitation for now; ask someone in it for an invite",
                None,
            ),
            JoinRefusal::TooMany { retry_after_secs } => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many joins from your address; try again later",
                Some(retry_after_secs),
            ),
            JoinRefusal::Full { retry_after_secs } => (
                StatusCode::SERVICE_UNAVAILABLE,
                "the room is full",
                Some(retry_after_secs),
            ),
            JoinRefusal::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "the room is restarting; try again in a moment",
                None,
            ),
        };
        // The person themselves, rather than their address, is named apart,
        // so the page says they were removed instead of blaming their network;
        // and a room taking people by invitation, so it says they need one.
        let code = match refusal {
            JoinRefusal::Removed => Some("removed"),
            JoinRefusal::InviteOnly => Some("invitation-only"),
            _ => None,
        };
        let mut response = if let Some(code) = code {
            let mut body = serde_json::json!({"error": message, "code": code});
            if let (JoinRefusal::InviteOnly, Some(door), Some(fields)) =
                (refusal, wait_list, body.as_object_mut())
            {
                fields.insert("waitList".into(), true.into());
                fields.insert("contact".into(), door.contact.into());
                fields.insert("mail".into(), door.mail.into());
            }
            Self::json(status, &body)
        } else {
            Self::error(status, message)
        };
        if let Some(seconds) = retry_after
            && let Ok(value) = header::HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }

    /// What `POST /wait-list` answers an address it did not keep: `code`
    /// names the case for the door page, `error` says it.
    fn wait_refusal(refusal: persistent::WaitRefusal) -> Response<Body> {
        use persistent::WaitRefusal;
        let (status, code, message, retry_after) = match refusal {
            WaitRefusal::NoWaitList => (
                StatusCode::NOT_FOUND,
                "no-wait-list",
                "this room keeps no wait list",
                None,
            ),
            WaitRefusal::Invalid(message) => {
                (StatusCode::BAD_REQUEST, "invalid-email", message, None)
            }
            WaitRefusal::NotASecret => (
                StatusCode::BAD_REQUEST,
                "invalid-secret",
                "not a secret this door handed out",
                None,
            ),
            WaitRefusal::TooMany { retry_after_secs } => (
                StatusCode::TOO_MANY_REQUESTS,
                "too-many",
                "too many addresses from your network; try again later",
                Some(retry_after_secs),
            ),
            WaitRefusal::Full => (
                StatusCode::SERVICE_UNAVAILABLE,
                "wait-list-full",
                "the wait list is full for now",
                None,
            ),
            WaitRefusal::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "the room is restarting; try again in a moment",
                None,
            ),
        };
        let mut response = Self::json(status, &serde_json::json!({"error": message, "code": code}));
        if let Some(seconds) = retry_after
            && let Ok(value) = header::HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }

    /// `GET /mode`: the mode, which build answers and, for a persistent
    /// room, whether its relay serves.
    async fn mode(&self) -> Response<Body> {
        let mut mode = serde_json::json!({
            "mode": ops::get().map_or(Mode::Normal, ops::Ops::mode).as_str(),
            // Public like the web entry's version.json: which build answers
            // here, for drift reports from outside the cluster.
            "revision": ops::get().map_or(ops::UNKNOWN_REVISION, ops::Ops::revision),
        });
        // A deploy's health check needs the relay: one that cannot start
        // leaves this service answering while nobody gets in.
        if let (Some(persistent), Some(fields)) = (&self.persistent, mode.as_object_mut()) {
            let relay = if persistent.relay_up().await {
                "up"
            } else {
                "restarting"
            };
            fields.insert("relay".into(), relay.into());
            // Why it is down, so a failed deploy is diagnosable from
            // outside the cluster: the relay's error, sanitized.
            if let Some(failure) = persistent.relay_failure().await {
                fields.insert(
                    "relay_error".into(),
                    serde_json::to_value(&failure).unwrap_or_default(),
                );
            }
        }
        Self::json(StatusCode::OK, &mode)
    }

    async fn avatar(persistent: &persistent::Persistent, name: &str) -> Response<Body> {
        use tilo_relay_protocol::public_avatars::{MAX_AVATAR_BYTES, sniff};
        let Some(path) = persistent.avatar_path(name) else {
            return Self::error(StatusCode::NOT_FOUND, "not found");
        };
        let Ok(bytes) = tokio::fs::read(path).await else {
            return Self::error(StatusCode::NOT_FOUND, "not found");
        };
        let Some(kind) = sniff(&bytes).filter(|_| bytes.len() <= MAX_AVATAR_BYTES) else {
            return Self::error(StatusCode::NOT_FOUND, "not found");
        };
        let mut response = Response::new(full(bytes));
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, header::HeaderValue::from_static(kind));
        headers.insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
        headers.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            header::HeaderValue::from_static("nosniff"),
        );
        response
    }

    /// A browser's error report: size-capped, rate-limited, logged as one
    /// structured line (never stored). Always answers 204 so a failing page
    /// does not loop on the answer.
    async fn client_error(&self, body: hyper::body::Incoming) -> Response<Body> {
        let mut response = Response::new(full(Bytes::new()));
        *response.status_mut() = StatusCode::NO_CONTENT;
        let Some(ops) = ops::get() else {
            return response;
        };
        let Ok(body) = Limited::new(body, ops::REPORT_MAX_BYTES)
            .collect()
            .await
            .map(http_body_util::Collected::to_bytes)
        else {
            return response;
        };
        if ops.admit_report() {
            match ops::report_line(&body) {
                Ok(line) => eprintln!("{line}"),
                Err(error) => eprintln!("tilo-room-service: bad error report: {error:#}"),
            }
        }
        response
    }

    /// `POST /wait-list`, `/wait-list/remove`, `/wait-list/confirm` and
    /// `/wait-list/standing`. Taking an entry off with its secret is
    /// answered alike whether or not an entry had it.
    async fn wait_list_route(
        persistent: &persistent::Persistent,
        client: std::net::IpAddr,
        request: Request<hyper::body::Incoming>,
        act: &str,
    ) -> Response<Body> {
        let body = request.into_body();
        let answer = match act {
            "remove" => match Self::decode::<persistent::UnlistRequest>(body).await {
                Ok(asked) => persistent
                    .unlist(client, asked)
                    .await
                    .map(|()| serde_json::json!({})),
                Err(response) => return response,
            },
            "confirm" => match Self::decode::<persistent::ConfirmRequest>(body).await {
                Ok(asked) => persistent
                    .confirm(client, asked)
                    .await
                    .map(|confirmed| serde_json::json!({"confirmed": confirmed})),
                Err(response) => return response,
            },
            "standing" => match Self::decode::<persistent::UnlistRequest>(body).await {
                Ok(asked) => persistent
                    .standing(client, asked)
                    .await
                    .map(|standing| serde_json::json!({"standing": standing})),
                Err(response) => return response,
            },
            _ => match Self::decode::<persistent::WaitListRequest>(body).await {
                Ok(asked) => persistent
                    .wait(client, asked)
                    .await
                    .map(|answer| serde_json::to_value(answer).unwrap_or_default()),
                Err(response) => return response,
            },
        };
        match answer {
            Ok(answer) => Self::json(StatusCode::OK, &answer),
            Err(refusal) => Self::wait_refusal(refusal),
        }
    }

    /// The persistent room's routes; `None` passes to the shared ones.
    async fn persistent_route(
        &self,
        persistent: &Arc<persistent::Persistent>,
        client: std::net::IpAddr,
        request: Request<hyper::body::Incoming>,
    ) -> std::result::Result<Response<Body>, Request<hyper::body::Incoming>> {
        let path = request.uri().path().to_owned();
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        Ok(match (request.method(), parts.as_slice()) {
            (&Method::GET, [""]) => Self::front_door(
                self.config
                    .persistent
                    .as_ref()
                    .and_then(|persistent| persistent.web_entry.as_deref()),
            ),
            (&Method::GET, ["discord", "callback"]) => {
                let query = request.uri().query().unwrap_or_default().to_owned();
                self.discord_callback(persistent, &query).await
            }
            (&Method::POST, ["join"]) => match Self::paused() {
                Some(refused) => refused,
                None => match Self::decode::<persistent::JoinRequest>(request.into_body()).await {
                    Ok(join) => match persistent.join(client, join).await {
                        Ok(answer) => {
                            if let Some(ops) = ops::get() {
                                ops.joined();
                            }
                            Self::json(StatusCode::OK, &answer)
                        }
                        Err(refusal) => Self::refusal(refusal, persistent.wait_list()),
                    },
                    Err(response) => response,
                },
            },
            (&Method::POST, ["wait-list"]) => {
                Self::wait_list_route(persistent, client, request, "").await
            }
            (&Method::POST, ["wait-list", act @ ("remove" | "confirm" | "standing")]) => {
                let act = *act;
                Self::wait_list_route(persistent, client, request, act).await
            }
            (&Method::POST, ["spectate"])
                if ops::get().is_some_and(|ops| !ops.mode().allows_spectators()) =>
            {
                Self::paused().unwrap_or_else(|| {
                    Self::error(StatusCode::SERVICE_UNAVAILABLE, "the room is paused")
                })
            }
            (&Method::POST, ["spectate"]) => {
                match Self::decode::<persistent::SpectateRequest>(request.into_body()).await {
                    Ok(spectate) => match persistent.spectate(client, spectate).await {
                        Ok(answer) => Self::json(StatusCode::OK, &answer),
                        Err(refusal) => Self::refusal(refusal, None),
                    },
                    Err(response) => response,
                }
            }
            (&Method::GET, ["presence"]) => {
                let joins = ops::get().is_none_or(|ops| ops.mode().allows_joins());
                let mut response = Self::json(
                    StatusCode::OK,
                    // `admission`: whether the door lets anyone in as a
                    // guest (`open`) or only the room's members (`invite`),
                    // so a member's page offers an invitation, not the link.
                    // `joins`: whether anyone joins at all now (not while the
                    // room is watch-only), so a watching page never promises
                    // a join the door will not give.
                    &serde_json::json!({
                        "online": persistent.online(),
                        "watching": persistent.watching(),
                        // Whether a guest would get in now, so a watching
                        // page reloads to join only then.
                        "open": joins && persistent.seat_free(),
                        "joins": joins,
                        "admission": if persistent.invite_only() { "invite" } else { "open" },
                    }),
                );
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static("max-age=5"),
                );
                response
            }
            (method, ["directory"]) if *method == Method::GET || *method == Method::POST => {
                let settings = self.config.persistent.as_ref();
                let name = settings
                    .and_then(|settings| settings.shown_name.clone())
                    .or_else(|| settings.map(|settings| settings.name.clone()))
                    .unwrap_or_default();
                let description = settings
                    .map(|settings| settings.description.clone())
                    .unwrap_or_default();
                match persistent.directory(name, description).await {
                    Some(page) => {
                        let mut response = Self::json(StatusCode::OK, &page);
                        response.headers_mut().insert(
                            header::CACHE_CONTROL,
                            header::HeaderValue::from_static("max-age=5"),
                        );
                        response
                    }
                    None => Self::error(StatusCode::SERVICE_UNAVAILABLE, "the room is restarting"),
                }
            }
            (&Method::GET, ["avatars", name]) => Self::avatar(persistent, name).await,
            (_, ["rooms", ..]) => Self::error(
                StatusCode::NOT_FOUND,
                "this service runs one persistent room",
            ),
            _ => return Err(request),
        })
    }

    async fn handle(
        self: Arc<Self>,
        peer: SocketAddr,
        request: Request<hyper::body::Incoming>,
    ) -> Response<Body> {
        let origin = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let request = match (&self.persistent, &self.config.persistent) {
            (Some(persistent), Some(settings)) if request.method() != Method::OPTIONS => {
                let forwarded_for = request
                    .headers()
                    .get("x-forwarded-for")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let client = limits::client_ip(
                    peer.ip(),
                    forwarded_for.as_deref(),
                    &settings.trusted_proxies,
                );
                match self.persistent_route(persistent, client, request).await {
                    Ok(response) => return self.finish(origin.as_deref(), response),
                    Err(request) => request,
                }
            }
            _ => request,
        };
        let path = request.uri().path().to_owned();
        let query = request.uri().query().map(str::to_owned);
        let method = request.method().clone();
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        let response = match (&method, parts.as_slice()) {
            (&Method::OPTIONS, _) => Response::new(full(Bytes::new())),
            (&Method::GET, ["healthz"]) => {
                Self::json(StatusCode::OK, &serde_json::json!({"ok": true}))
            }
            (&Method::GET, ["open", "entry.json"]) => self.entry_config(),
            (&Method::GET, ["app", ..]) => {
                self.asset(path.strip_prefix("/app").unwrap_or_default())
                    .await
            }
            (&Method::GET, ["open", ..]) => {
                self.asset(path.strip_prefix("/open").unwrap_or_default())
                    .await
            }
            (&Method::GET, ["metrics"]) => self.metrics(request.headers()).await,
            (&Method::GET, ["mode"]) => self.mode().await,
            (&Method::POST, ["errors"]) => self.client_error(request.into_body()).await,
            (&Method::POST, ["rooms"]) => match Self::paused() {
                Some(refused) => refused,
                None => match self.create().await {
                    Ok(value) => {
                        if let Some(ops) = ops::get() {
                            ops.joined();
                        }
                        Self::json(StatusCode::CREATED, &value)
                    }
                    Err((status, message)) => {
                        if let Some(ops) = ops::get() {
                            ops.join_failed(if status == StatusCode::SERVICE_UNAVAILABLE {
                                JoinFailure::Full
                            } else {
                                JoinFailure::Invalid
                            });
                        }
                        Self::error(status, &message)
                    }
                },
            },
            (&Method::GET, ["rooms", id, "events"]) => {
                self.owner_route(id, query.as_deref(), None).await
            }
            (&Method::POST, ["rooms", id, "admit"]) => {
                let id = (*id).to_owned();
                match Self::paused() {
                    Some(refused) => refused,
                    None => self.admit(&id, request.into_body()).await,
                }
            }
            (&Method::POST, ["rooms", id, "invitations"]) => {
                let id = (*id).to_owned();
                self.owner_route(&id, query.as_deref(), Some(request.into_body()))
                    .await
            }
            _ => Self::error(StatusCode::NOT_FOUND, "not found"),
        };
        self.finish(origin.as_deref(), response)
    }

    /// A room's owner routes, open only with its owner key (see
    /// [`Self::owned_room`]): with an `invitation` body, one more guest's
    /// link; without, the member stream.
    async fn owner_route(
        &self,
        id: &str,
        query: Option<&str>,
        invitation: Option<hyper::body::Incoming>,
    ) -> Response<Body> {
        let Some(room) = self.owned_room(id, query) else {
            return Self::error(StatusCode::NOT_FOUND, "no such room");
        };
        let Some(body) = invitation else {
            return Self::events(&room);
        };
        if let Some(refused) = Self::paused() {
            return refused;
        }
        match Self::decode_or_default::<InvitationRequest>(body).await {
            Err(response) => response,
            Ok(asked) => self.invitation(&room, asked.record.as_deref()),
        }
    }

    /// CORS, and for the persistent room the pages allowed to frame it.
    fn finish(&self, origin: Option<&str>, response: Response<Body>) -> Response<Body> {
        let mut response = self.cors(origin, response);
        if self.persistent.is_some()
            && let Ok(value) = header::HeaderValue::from_str(&format!(
                "frame-ancestors {}",
                self.config.frame_ancestors()
            ))
        {
            response
                .headers_mut()
                .insert(header::CONTENT_SECURITY_POLICY, value);
        }
        response
    }
}

/// Serves one API connection.
async fn serve(service: Arc<Service>, socket: tokio::net::TcpStream, peer: SocketAddr) {
    let handler = hyper::service::service_fn(move |request| {
        let service = Arc::clone(&service);
        async move { Ok::<_, Infallible>(service.handle(peer, request).await) }
    });
    let _ = hyper::server::conn::http1::Builder::new()
        .serve_connection(hyper_util::rt::TokioIo::new(socket), handler)
        .await;
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .first()
        .is_some_and(|command| command == "mail-preview")
    {
        let dir = args
            .get(1)
            .context("usage: tilo-room-service mail-preview <dir>")?;
        // The same words the door would send: a configured directory is
        // read and checked here too, never quietly replaced by the built-in
        // ones.
        let templates = match std::env::var_os("TILO_ROOMS_MAIL_TEMPLATES") {
            Some(templates) => mail::Templates::load(std::path::Path::new(&templates))
                .context("TILO_ROOMS_MAIL_TEMPLATES")?,
            None => mail::Templates::baked(),
        };
        for path in mail::preview(std::path::Path::new(dir), &templates)? {
            println!("{}", path.display());
        }
        return Ok(());
    }
    if args.first().is_some_and(|command| command == "mode") {
        let state = std::env::var("TILO_ROOMS_STATE").unwrap_or_else(|_| "/run/tilo-rooms".into());
        return ops::mode_command(
            std::path::Path::new(&state),
            args.get(1..).unwrap_or_default(),
        );
    }
    let config = Arc::new(Config::from_env()?);
    std::fs::create_dir_all(&config.state).context("create TILO_ROOMS_STATE")?;
    let initial = match std::env::var("TILO_ROOMS_MODE")
        .ok()
        .filter(|mode| !mode.is_empty())
    {
        Some(text) => {
            Mode::parse(&text).context("TILO_ROOMS_MODE is normal, spectator-only or back-soon")?
        }
        None => Mode::Normal,
    };
    let ops = ops::install(config.state.clone(), initial);
    if let Some(settings) = &config.persistent {
        ops.set_caps(
            u64::try_from(config.max_guests).unwrap_or(u64::MAX),
            u64::try_from(settings.max_spectators).unwrap_or(u64::MAX),
        );
    }
    let identity = tls::HostIdentity::generate()?;
    let api = TcpListener::bind(config.api).await?;
    let persistent = match &config.persistent {
        Some(settings) => {
            eprintln!(
                "tilo-room-service: API {} (listening {}), persistent room {:?} at {}",
                config.public_api,
                config.api,
                settings.name,
                config.persistent_origin()
            );
            Some(persistent::Persistent::start(Arc::clone(&config))?)
        }
        None => None,
    };
    let service = Arc::new(Service {
        config: Arc::clone(&config),
        identity,
        persistent,
        rooms: Mutex::default(),
        starting: Mutex::new(0),
    });
    // Short-lived rooms are reached through the TLS front; the persistent
    // room has its own route to the relay and never uses it.
    if config.persistent.is_none() {
        let public = tls::public_config(config.certificate.as_ref(), &config.domain)?;
        let front = TcpListener::bind(config.tls).await?;
        eprintln!(
            "tilo-room-service: API {} (listening {}), rooms on {} (TLS {})",
            config.public_api,
            config.api,
            config.origin("<room>", config.port_base),
            config.tls
        );
        let lookup = Arc::clone(&service);
        tokio::spawn(tls::serve(
            front,
            public,
            config.domain.clone(),
            service.identity.certificate.clone(),
            move |id| lookup.room(id).map(|room| room.port),
        ));
    }
    // As a container's first process nothing stops it but a handled signal;
    // the persistent room's relay is then told to close its store cleanly.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("listen for SIGTERM")?;
    loop {
        let (socket, peer) = tokio::select! {
            accepted = api.accept() => accepted?,
            _ = terminate.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
        };
        tokio::spawn(serve(Arc::clone(&service), socket, peer));
    }
    eprintln!("tilo-room-service: stopping");
    if let Some(persistent) = &service.persistent {
        persistent.shutdown().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Service, content_type, guest_link};
    use tilo_reference::link::{Link, Mode, WebEntries, WebEntry};

    mod owner_routes;

    const ROOM: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90";
    const ENTRY: &str = "https://rooms.example/open/";

    #[test]
    fn the_arrival_screens_lines_are_served_as_text() {
        // The entry pages fetch `locales/<locale>.ftl`; an unknown type is a 404.
        for locale in ["en-US", "de-DE"] {
            assert_eq!(
                content_type(&format!("locales/{locale}.ftl")),
                Some("text/plain; charset=utf-8")
            );
        }
    }

    #[test]
    fn a_guest_link_is_a_route_and_a_ticket() {
        let url = guest_link(
            ENTRY,
            "http://127.0.0.1:9700",
            ROOM,
            "t0ken",
            Some("uuid:3e0d5a1c-7b2f-4e8a-9c6d-1a2b3c4d5e6f"),
        )
        .unwrap();
        assert_eq!(
            url,
            format!(
                "{ENTRY}app/#/@127.0.0.1:9700/w/{ROOM}/so.tilo.word-processor/uuid:3e0d5a1c-7b2f-4e8a-9c6d-1a2b3c4d5e6f?ticket=t0ken"
            )
        );
        let entry = WebEntry::new(ENTRY).unwrap();
        let link = Link::parse(&url, &WebEntries::new(vec![entry.clone()])).unwrap();
        assert_eq!(link.mode, Mode::App);
        assert_eq!(
            link.secrets
                .ticket
                .as_ref()
                .map(tilo_reference::link::Secret::expose),
            Some("t0ken")
        );
        assert!(
            !link.public().to_url(&entry).unwrap().contains("t0ken"),
            "the public form drops the ticket"
        );
    }

    #[test]
    fn a_guest_link_without_a_record_opens_the_rooms_writer() {
        let url = guest_link(ENTRY, "https://h.example", ROOM, "t", None).unwrap();
        assert!(
            url.ends_with(&format!("/w/{ROOM}/so.tilo.word-processor?ticket=t")),
            "{url}"
        );
    }

    #[test]
    fn a_persistent_rooms_front_door_leads_to_the_app_or_nowhere() {
        let door = Service::front_door(Some("https://app.example/"));
        assert_eq!(door.status(), hyper::StatusCode::FOUND);
        assert_eq!(
            door.headers()[hyper::header::LOCATION],
            "https://app.example/"
        );
        assert_eq!(
            Service::front_door(None).status(),
            hyper::StatusCode::NOT_FOUND
        );
    }
}
