//! The host's fetch proxy for its members' pages ([`protocol::FETCH_PATH`]):
//! `tilo_net::proxy::host` behind the browser sessions this listener
//! authenticates.
//!
//! A page reads a site that sends no CORS headers (a feed, a video
//! directory's API, a calendar subscription) through the workspace host it
//! is admitted to. Only a member's or guest's device with an open session
//! may: the session token arrives in [`SESSION_HEADER`], as on every other
//! route, and its person must be admitted to a workspace here. A room's
//! spectators only watch, so they may not. The proxy itself enforces
//! destinations, bounds, rates and budgets (see `tilo_net::proxy::host`);
//! this module adds who may call it, which rooms' budgets a request counts
//! against (every workspace here its person is admitted to), and the CORS
//! headers a page on the web entry's origin needs to read the answer.

use std::path::PathBuf;
use std::time::Duration;

use hyper::{Method, Request, Response, header};
use tilo_net::proxy::ProxyFailure;
use tilo_net::proxy::host::{Caller, Config, FetchProxy, Room};

use super::{Body, Web, protocol, reply, token};

/// The fetch proxy a listener serves.
#[derive(Clone, Debug, Default)]
pub struct FetchProxySetup {
    /// Its bounds, destinations and rates.
    pub config: Config,
    /// Where its counters are written as Prometheus text every few seconds,
    /// for a room host's metrics page.
    pub metrics_file: Option<PathBuf>,
}

/// How often the counters are written.
const METRICS_EVERY: Duration = Duration::from_secs(5);
/// The counters' name prefix, as a room host's metrics page names its own.
const METRICS_PREFIX: &str = "tilo_rooms_fetch_proxy";

/// The fetch proxy a listener serves, if any, and the task writing its
/// counters.
pub(super) struct Serving {
    pub(super) proxy: Option<FetchProxy>,
    pub(super) metrics: Option<tokio::task::JoinHandle<()>>,
}

impl Serving {
    /// Start what `setup` names.
    pub(super) fn start(setup: Option<&FetchProxySetup>) -> Result<Self, String> {
        let Some(setup) = setup else {
            return Ok(Self {
                proxy: None,
                metrics: None,
            });
        };
        let proxy = FetchProxy::new(setup.config.clone())
            .map_err(|error| format!("fetch proxy: {error}"))?;
        let metrics = setup
            .metrics_file
            .clone()
            .map(|path| tokio::spawn(write_metrics(proxy.clone(), path)));
        Ok(Self {
            proxy: Some(proxy),
            metrics,
        })
    }

    /// The session grant's feature bit for it.
    pub(super) fn features(&self) -> u64 {
        if self.proxy.is_some() {
            protocol::FEATURE_FETCH_PROXY
        } else {
            0
        }
    }
}

/// Write `proxy`'s counters to `path` until the listener stops.
async fn write_metrics(proxy: FetchProxy, path: PathBuf) {
    let mut every = tokio::time::interval(METRICS_EVERY);
    loop {
        every.tick().await;
        let text = proxy.prometheus(METRICS_PREFIX);
        let path = path.clone();
        let written = tokio::task::spawn_blocking(move || {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            tilo_files::native::atomic::write(&path, text.as_bytes())
        })
        .await;
        if let Ok(Err(error)) = written {
            tracing::debug!(target: "fetch_proxy", %error, "could not write the fetch proxy's counters");
        }
    }
}

/// Answer one request to the fetch route, or its CORS preflight.
pub(super) async fn handle(web: &Web, request: Request<hyper::body::Incoming>) -> Response<Body> {
    let origin = request.headers().get(header::ORIGIN).cloned();
    let asked_headers = request
        .headers()
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .cloned();
    let preflight = request.method() == Method::OPTIONS
        && request
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);
    let mut response = match &web.fetch {
        None => super::status(
            hyper::StatusCode::NOT_FOUND,
            "this host serves no fetch proxy",
        ),
        Some(_) if preflight => reply(hyper::StatusCode::NO_CONTENT, Vec::new()),
        Some(proxy) => match caller(web, &request).await {
            Ok((caller, rooms)) => proxy
                .serve_in(caller, &rooms, request)
                .await
                .map(Body::Proxied),
            Err(why) => proxy
                .refuse(ProxyFailure::Unauthorized, why)
                .map(Body::Proxied),
        },
    };
    cors(
        &web.allowed_origins,
        origin.as_ref(),
        asked_headers.as_ref(),
        &mut response,
    );
    response
}

/// The device a request is made for, and the rooms its person is admitted
/// to here, when its session belongs to a person admitted here.
async fn caller(
    web: &Web,
    request: &Request<hyper::body::Incoming>,
) -> Result<(Caller, Vec<Room>), &'static str> {
    // A media element cannot send the session header; its GET carries the
    // session in the query (`tilo_net::proxy::media_url`).
    let token = token(request)
        .or_else(|| {
            let session = tilo_net::proxy::host::query_session(request.method(), request.uri())?;
            hex::decode(session).ok()?.try_into().ok()
        })
        .ok_or("the fetch proxy is for pages signed in to this host")?;
    let session = web
        .sessions
        .get(&token)
        .map_err(|_| "the session ended; sign in again")?;
    let rooms = member_of(web, session.peer.user_public_key).await;
    if rooms.is_empty() {
        return Err("the fetch proxy is for members of a workspace on this host");
    }
    Ok((Caller(session.peer.device_public_key), rooms))
}

/// The workspaces here `user` is admitted to as a person of its own: a
/// member or a guest. None for the spectator person every watching page of
/// a public room shares.
async fn member_of(web: &Web, user: [u8; 32]) -> Vec<Room> {
    let workspaces: Vec<_> = web
        .sessions
        .context()
        .relay
        .workspaces
        .lock()
        .await
        .iter()
        .map(|(id, workspace)| (*id, workspace.clone()))
        .collect();
    let mut rooms = Vec::new();
    for (id, workspace) in workspaces {
        let workspace = workspace.lock().await;
        if workspace
            .spectators
            .is_some_and(|spectators| spectators.root == user)
        {
            return Vec::new();
        }
        if workspace.admitted_users.contains(&user) || workspace.governed_user_roots.contains(&user)
        {
            rooms.push(Room(id));
        }
    }
    rooms
}

/// The methods a page may send through the proxy, as a preflight answers.
const METHODS: &str = "GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS, PROPFIND, PROPPATCH, REPORT, MKCOL, MKCALENDAR, COPY, MOVE, LOCK, UNLOCK, SEARCH";

/// CORS for a page on an allowed origin: any of the proxy's methods, the
/// headers the page asks to send (an app sets its own), and every header of
/// the answer readable. The page sends no cookies here, only its session
/// header, so nothing is answered with credentials.
fn cors(
    allowed_origins: &[String],
    origin: Option<&header::HeaderValue>,
    asked_headers: Option<&header::HeaderValue>,
    response: &mut Response<Body>,
) {
    let Some(origin) = origin.and_then(|value| value.to_str().ok()) else {
        return;
    };
    if !allowed_origins
        .iter()
        .any(|allowed| allowed == origin || allowed == "*")
    {
        return;
    }
    let exposed = response
        .headers()
        .keys()
        .map(header::HeaderName::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let exposed = header::HeaderValue::from_str(&exposed).ok();
    let headers = response.headers_mut();
    if let Ok(origin) = header::HeaderValue::from_str(origin) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    }
    headers.append(header::VARY, header::HeaderValue::from_static("origin"));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        header::HeaderValue::from_static(METHODS),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        asked_headers
            .cloned()
            .unwrap_or_else(|| header::HeaderValue::from_static("x-tilo-proxy, x-tilo-session")),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        header::HeaderValue::from_static("600"),
    );
    if let Some(exposed) = exposed {
        headers.insert(header::ACCESS_CONTROL_EXPOSE_HEADERS, exposed);
    }
}
