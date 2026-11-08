//! The host's browser listener (feature `web`): HTTPS (TCP, HTTP/2 and
//! HTTP/1.1), served by the relay itself.
//!
//! Device certification, the session handshake, the durable plane and
//! presence — `/send` and the long-polled `/events` of `sessions` — and the
//! page's calls all ride it. Routes are listed in
//! [`tilo_relay_protocol::web`].
//!
//! Browser sessions are ordinary relay peers (see `sessions`): their
//! presence, previews and window streams travel with every other device's,
//! on the workspace's own session.
//!
//! No owner loop runs here: every handler is async and awaits the relay's
//! own locks; workspace I/O already runs on the blocking pool. The session
//! janitor runs on a tokio interval.

mod envelopes;
mod fetch;
mod here;
pub mod install;
mod page_errors;
mod sessions;
mod shelf;
#[cfg(test)]
mod tests;
mod tls;
pub(crate) mod wt;

pub use fetch::FetchProxySetup;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
pub use tls::TlsSource;

use bytes::Bytes;
use http_body_util::BodyExt as _;
use hyper::{Method, Request, Response, StatusCode, header};
use tilo_relay_protocol::certify::CertifyReply;
pub(crate) use tilo_relay_protocol::web as protocol;
use tilo_relay_protocol::web::{
    API_PREFIX, EventsRequest, HostAddress, HostInfo, MAX_POLL_MS, MAX_REQUEST_BYTES, NodeInfo,
    SESSION_HEADER, SessionFinish,
};
use tilo_user_identity::CertificationRequest;

use crate::daemon::{Relay, RelayError};
use sessions::{SessionError, Sessions};

/// Configuration of the browser listener.
#[derive(Clone, Debug)]
pub struct WebConfig {
    /// TCP address for HTTPS. Port 0 picks one free.
    pub listen: SocketAddr,
    /// Certificate source.
    pub tls: TlsSource,
    /// Page origins allowed to call the host cross-origin (the Cloud console
    /// origin, a dev server). Empty allows same-origin pages only.
    pub allowed_origins: Vec<String>,
    /// A loopback address that also serves the HTTPS routes as plain HTTP,
    /// for local review in a browser that should not need a certificate
    /// exception.
    pub plain_listen: Option<SocketAddr>,
    /// The `https://` origin a TLS-terminating proxy publishes the plain
    /// listener at. When set, the plain listener may bind a non-loopback
    /// address (the proxy is the way in), and this origin is what pages
    /// and links are told.
    pub public_origin: Option<String>,
    /// How many browsers the listener serves at once.
    pub limits: WebLimits,
    /// Where the proxy in front of the host names a client's country. The
    /// host shows it to a person approving a new device and looks nothing up
    /// itself.
    pub country: Option<CountryHeader>,
    /// The fetch proxy this host serves its members' pages at
    /// [`protocol::FETCH_PATH`], if any.
    pub fetch_proxy: Option<FetchProxySetup>,
    /// The live plane the pages join, and where they dial it.
    pub live: LiveConfig,
}

/// The live plane (WebTransport on UDP): the address its socket binds and the
/// origin pages are told to dial it at. A proxy that publishes the plane
/// somewhere other than the host's own origin, on a port of its choosing,
/// names both.
#[derive(Clone, Debug, Default)]
pub struct LiveConfig {
    /// The UDP address the page carrier binds. An address is the one a proxy
    /// forwards to, so it binds exactly there or the host stops. Default: the
    /// HTTPS listener's address and port, reusing the port where UDP allows
    /// it and letting the kernel pick otherwise.
    pub listen: Option<SocketAddr>,
    /// The `https://host[:port]` origin pages dial the page carrier at. The
    /// port it names is the one pages use, 443 when it names none. Default:
    /// the relay's own origin's host on the port the socket bound.
    pub origin: Option<String>,
}

/// The request header a proxy in front of the host names the client's
/// country in (such as a CDN's `CF-IPCountry`), and the peers it is
/// believed from: that proxy, never the open internet.
#[derive(Clone, Debug)]
pub struct CountryHeader {
    pub name: String,
    /// The peers whose header is believed.
    pub proxies: tilo_relay_protocol::proxies::TrustedProxies,
}

/// Caps on concurrent browser state. A public workspace expects hundreds of
/// visitors, so its host raises them; the defaults suit a personal host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebLimits {
    /// Open browser sessions.
    pub max_sessions: usize,
    /// Session handshakes in flight.
    pub max_handshakes: usize,
}

impl Default for WebLimits {
    fn default() -> Self {
        Self {
            max_sessions: 256,
            max_handshakes: 64,
        }
    }
}

/// A running browser listener. Dropping it stops serving.
pub struct WebHandle {
    /// HTTPS address.
    pub addr: SocketAddr,
    /// `https://host:port` of the listener.
    pub origin: String,
    /// SHA-256 of a development certificate.
    pub certificate_sha256: Option<String>,
    /// `http://localhost:port` of the plain loopback listener, if any.
    pub plain_origin: Option<String>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    locator_origin: Arc<std::sync::RwLock<Option<Arc<String>>>>,
    published_origin: Option<Arc<String>>,
    sessions: Arc<Sessions>,
}

impl WebHandle {
    /// Open browser sessions per person (user root) now, for a public
    /// host's presence count and idle-guest pruning. One person on two
    /// devices is one entry; the workspace's visitor person is one entry whose
    /// count is its open pages.
    #[must_use]
    pub fn online_users(&self) -> std::collections::BTreeMap<[u8; 32], usize> {
        self.sessions.online_users()
    }
}

impl Drop for WebHandle {
    fn drop(&mut self) {
        if let Ok(mut published) = self.locator_origin.write()
            && self.published_origin.as_ref().is_some_and(|ours| {
                published
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(ours, current))
            })
        {
            *published = None;
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}

struct Web {
    sessions: Arc<Sessions>,
    host_info: HostInfo,
    allowed_origins: Vec<String>,
    /// What `/session/finish` advertises.
    features: u64,
    /// The workspaces this relay governs, for `/rooms` and `/join`.
    workspaces: Option<crate::workspaces::Workspaces>,
    /// Members' root envelopes, for `/envelope`.
    envelopes: envelopes::Envelopes,
    /// Members' profiles, for `/profile`.
    profiles: envelopes::Envelopes,
    /// Who is here and what they are doing, for `/here`.
    here: Arc<here::Here>,
    /// [`WebConfig::country`], its header name parsed.
    country: Option<(
        hyper::header::HeaderName,
        tilo_relay_protocol::proxies::TrustedProxies,
    )>,
    /// The fetch proxy, when [`WebConfig::fetch_proxy`] asked for one.
    fetch: Option<tilo_net::proxy::host::FetchProxy>,
    /// The pages' live plane: the WebTransport listener they join, and the
    /// authority that admits their session tokens. `None` in a host that
    /// serves the browser routes without it.
    live: Option<Arc<wt::Live>>,
    /// The page errors the door logs.
    page_errors: page_errors::PageErrors,
    /// The sealed invitations this site keeps in place of their clerk: what a
    /// short link names, in bytes the site has no key for.
    shelf: shelf::Shelf,
}

/// Checks the published origins against the listeners they front.
fn checked_origins(config: &WebConfig) -> Result<(), String> {
    if config.public_origin.is_none()
        && config
            .plain_listen
            .is_some_and(|plain| !plain.ip().is_loopback())
    {
        return Err(
            "the plain HTTP listener must be a loopback address unless a proxy publishes it".into(),
        );
    }
    if config
        .public_origin
        .as_ref()
        .is_some_and(|origin| !origin.starts_with("https://") || config.plain_listen.is_none())
    {
        return Err(
            "a public origin is an https:// origin published for the plain listener".into(),
        );
    }
    Ok(())
}

/// An unspecified listener is not a usable browser destination. Only its
/// configured certificate host can provide one; no caller URL is accepted.
fn browser_locator_origin(tls: &TlsSource, addr: SocketAddr) -> Result<Option<String>, RelayError> {
    let host = match tls {
        TlsSource::Files { host, .. } => host
            .clone()
            .or_else(|| (!addr.ip().is_unspecified()).then(|| addr.ip().to_string())),
        TlsSource::Development { hosts, .. } => {
            let bound = addr.ip().to_string();
            if !addr.ip().is_unspecified() && hosts.contains(&bound) {
                Some(bound)
            } else {
                hosts
                    .iter()
                    .find(|host| {
                        !host
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_unspecified())
                    })
                    .cloned()
            }
        }
        TlsSource::Managed { domain } => Some(domain.clone()),
    };
    let Some(host) = host else {
        return Ok(None);
    };
    if host
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_unspecified())
    {
        return Ok(None);
    }
    let authority = if host.contains(':') {
        format!("[{host}]:{}", addr.port())
    } else {
        format!("{host}:{}", addr.port())
    };
    let origin = format!("https://{authority}");
    let uri: hyper::Uri = origin
        .parse()
        .map_err(|_| RelayError::Identity("invalid configured browser locator host".into()))?;
    if uri
        .authority()
        .is_none_or(|value| value.as_str() != authority || value.as_str().contains('@'))
    {
        return Err(RelayError::Identity(
            "invalid configured browser locator host".into(),
        ));
    }
    Ok(Some(if addr.port() == 443 {
        origin.strip_suffix(":443").unwrap_or(&origin).to_owned()
    } else {
        origin
    }))
}

impl Relay {
    /// Make `origin` the one browser locators name, and return it shared.
    fn publish_browser_origin(
        &self,
        origin: Option<String>,
    ) -> Result<Option<Arc<String>>, RelayError> {
        let published = origin.map(Arc::new);
        self.browser_origin
            .write()
            .map_err(|_| RelayError::Iroh("web listener: browser locator lock poisoned".into()))?
            .clone_from(&published);
        Ok(published)
    }

    /// Serve browsers: HTTPS on `config.listen`.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::Iroh`] (the transport error kind) when the
    /// certificate cannot be loaded or an address cannot be bound (the live
    /// plane's included).
    pub async fn serve_web(&self, config: WebConfig) -> Result<WebHandle, RelayError> {
        let failed = |message: String| RelayError::Iroh(format!("web listener: {message}"));
        checked_origins(&config).map_err(failed)?;
        let identity = tls::Identity::load(&config.tls).map_err(failed)?;
        let tcp = bind_listener(config.listen).map_err(|error| failed(error.to_string()))?;
        let addr = tcp
            .local_addr()
            .map_err(|error| failed(error.to_string()))?;
        let public_origin = match config.public_origin.clone() {
            Some(origin) => Some(origin),
            None => browser_locator_origin(&config.tls, addr)?,
        };
        let acceptor =
            tokio_rustls::TlsAcceptor::from(Arc::new(identity.https_config().map_err(failed)?));
        let fetch = fetch::Serving::start(config.fetch_proxy.as_ref()).map_err(failed)?;
        let context = self.web_context().clone();
        let Some(plane) = self.plane.as_ref() else {
            return Err(failed("the relay runs no live plane".into()));
        };
        let sessions = Sessions::with_limits(context.clone(), config.limits);
        plane.pages.serve_pages(sessions.clone());
        if let Some(workspaces) = self.workspaces() {
            let _ = context.relay.carried_calls.set(workspaces.downgrade());
        }
        let host_cert = context.relay.identity.cert.clone();
        let node_addr = self.node_addr();
        let mut live = wt::Live::start(
            plane,
            &identity,
            &config,
            &public_origin
                .clone()
                .unwrap_or_else(|| format!("https://{addr}")),
        )
        .map_err(failed)?;
        let live_tasks = std::mem::take(&mut live.tasks);
        tracing::info!(
            origin = %live.info.webtransport_origin,
            udp = %live.bound,
            "serving the live plane"
        );
        let web = Arc::new(Web {
            sessions: sessions.clone(),
            host_info: HostInfo {
                host_cert,
                address: Some(HostAddress {
                    endpoint_addr: node_addr.clone(),
                    relay_url: node_addr.relay_urls().next().map(ToString::to_string),
                }),
            },
            allowed_origins: config.allowed_origins,
            features: self.web_features() | fetch.features(),
            workspaces: self.workspaces().cloned(),
            envelopes: envelopes::Envelopes::new(self.state_root().join("envelopes")),
            profiles: envelopes::Envelopes::of(
                envelopes::Kind::Profile,
                self.state_root().join("profiles"),
            ),
            here: Arc::new(here::Here::new(here::from_env())),
            country: config.country.and_then(country_header),
            fetch: fetch.proxy,
            live: Some(Arc::new(live)),
            page_errors: page_errors::PageErrors::default(),
            shelf: shelf::Shelf::default(),
        });
        let mut tasks = live_tasks;
        tasks.extend(fetch.metrics);
        let mut plain_origin = None;
        if let Some(plain) = config.plain_listen {
            let plain = tokio::net::TcpListener::bind(plain)
                .await
                .map_err(|error| failed(error.to_string()))?;
            let port = plain
                .local_addr()
                .map_err(|error| failed(error.to_string()))?
                .port();
            plain_origin = Some(format!("http://localhost:{port}"));
            tasks.push(tokio::spawn(plain_accept_loop(plain, web.clone())));
        }
        tasks.push(tokio::spawn(Arc::clone(&sessions).janitor()));
        tasks.push(tokio::spawn(accept_loop(tcp, acceptor, web)));
        let published_origin = self.publish_browser_origin(public_origin.clone())?;
        Ok(WebHandle {
            addr,
            origin: public_origin.unwrap_or_else(|| format!("https://{addr}")),
            certificate_sha256: identity.fingerprint,
            plain_origin,
            tasks,
            locator_origin: Arc::clone(&self.browser_origin),
            published_origin,
            sessions,
        })
    }
}

impl Relay {
    /// What `/session/finish` advertises: calls and workspace administration
    /// only where workspaces are governed.
    fn web_features(&self) -> u64 {
        let workspaces = if self.workspaces().is_some() {
            protocol::FEATURE_VOICE | protocol::FEATURE_ROOMS
        } else {
            0
        };
        protocol::FEATURE_RESUME | protocol::FEATURE_COMPACTION | workspaces
    }
}

/// The HTTPS listener on `listen`. For port 0 the kernel picks the port.
fn bind_listener(listen: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    let socket = if listen.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    // As `TcpListener::bind`: a restarted host takes its port back while its
    // old connections linger in TIME_WAIT.
    socket.set_reuseaddr(true)?;
    socket.bind(listen)?;
    socket.listen(1024)
}

/// Accepted connections belong to their listener's loop: aborting the loop
/// (dropping the [`WebHandle`]) closes every open connection with it, so a
/// browser sees its host go away instead of a kept-alive orphan serving it.
async fn accept_loop(
    tcp: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    web: Arc<Web>,
) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let Ok((stream, peer)) = tcp.accept().await else {
            continue;
        };
        while connections.try_join_next().is_some() {}
        let acceptor = acceptor.clone();
        let web = web.clone();
        connections.spawn(async move {
            let Ok(Ok(tls)) =
                tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await
            else {
                return;
            };
            serve_connection(tls, web, peer).await;
        });
    }
}

/// The same routes as plain HTTP on a loopback listener.
async fn plain_accept_loop(tcp: tokio::net::TcpListener, web: Arc<Web>) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let Ok((stream, peer)) = tcp.accept().await else {
            continue;
        };
        while connections.try_join_next().is_some() {}
        connections.spawn(serve_connection(stream, web.clone(), peer));
    }
}

/// The TCP peer a request arrived from.
#[derive(Clone, Copy)]
struct Peer(SocketAddr);

async fn serve_connection<Io>(io: Io, web: Arc<Web>, peer: SocketAddr)
where
    Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = hyper::service::service_fn(move |mut request: Request<hyper::body::Incoming>| {
        let web = web.clone();
        request.extensions_mut().insert(Peer(peer));
        async move { Ok::<_, Infallible>(handle(&web, request).await) }
    });
    let _ = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
        .serve_connection(hyper_util::rt::TokioIo::new(io), service)
        .await;
}

/// A response body: whole bytes, or what the fetch proxy relays.
pub(crate) enum Body {
    Full(Option<Bytes>),
    Proxied(tilo_net::proxy::host::ProxyBody),
}

impl http_body::Body for Body {
    type Data = Bytes;
    /// Only a relayed body fails: it breaks off at the proxy's bounds, so
    /// the page sees an error rather than a short body that reads as whole.
    type Error = tilo_net::proxy::host::ProxyBodyError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        match self.get_mut() {
            Self::Full(bytes) => {
                Poll::Ready(bytes.take().map(|bytes| Ok(http_body::Frame::data(bytes))))
            }
            Self::Proxied(body) => Pin::new(body).poll_frame(cx),
        }
    }
}

fn reply(status: StatusCode, body: Vec<u8>) -> Response<Body> {
    let mut response = Response::new(Body::Full(Some(Bytes::from(body))));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    response
}

fn encoded<T: serde::Serialize>(value: &T) -> Response<Body> {
    match postcard::to_allocvec(value) {
        Ok(bytes) => reply(StatusCode::OK, bytes),
        Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
    }
}

fn status(status: StatusCode, message: &str) -> Response<Body> {
    reply(status, message.as_bytes().to_vec())
}

fn json_status<T: serde::Serialize>(code: StatusCode, value: &T) -> Response<Body> {
    match serde_json::to_vec(value) {
        Ok(json) => {
            let mut response = reply(code, json);
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            );
            response
        }
        Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
    }
}

fn json<T: serde::Serialize>(value: &T) -> Response<Body> {
    json_status(StatusCode::OK, value)
}

/// A public workspace's door (`/directory/<workspace>/join`, `…/report` or
/// `…/errors`): JSON in, JSON out, a refusal as `{error}` the person reads.
#[allow(
    clippy::result_large_err,
    reason = "the error is the response the handler sends as it is"
)]
async fn directory_door(
    web: &Web,
    route: &str,
    bytes: &[u8],
) -> Result<Response<Body>, Response<Body>> {
    use tilo_relay_protocol::directory::{DoorAnswer, DoorJoin, DoorReport, door_route};
    let (workspace, step) =
        door_route(route).ok_or_else(|| status(StatusCode::NOT_FOUND, "not found"))?;
    // A relay that governs no workspaces refuses before the request is parsed.
    governed_workspaces(web)?;
    let refused = |error: crate::workspaces::CollaborationError| {
        json_status(
            StatusCode::FORBIDDEN,
            &serde_json::json!({ "error": error.to_string() }),
        )
    };
    let door_refused = |error: crate::workspaces::DoorRefusal| {
        let full = match &error {
            crate::workspaces::DoorRefusal::Full { .. } => StatusCode::SERVICE_UNAVAILABLE,
            crate::workspaces::DoorRefusal::Refused(_)
            | crate::workspaces::DoorRefusal::InvitationOnly => StatusCode::FORBIDDEN,
        };
        let mut response = json_status(full, &serde_json::json!({ "error": error.to_string() }));
        if let crate::workspaces::DoorRefusal::Full { retry_after_secs } = error
            && let Ok(seconds) = header::HeaderValue::from_str(&retry_after_secs.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, seconds);
        }
        response
    };
    match step {
        "join" => {
            let join: DoorJoin = serde_json::from_slice(bytes)
                .map_err(|_| status(StatusCode::BAD_REQUEST, "not a join request"))?;
            let (issued, token) = governed_workspaces(web)?
                .admit_listed(workspace, &join)
                .await
                .map_err(door_refused)?;
            Ok(json(&DoorAnswer {
                invitation: base64::Engine::encode(
                    &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                    issued.signed_invitation,
                ),
                admission_token: token,
                origin: None,
            }))
        }
        "report" => {
            let report: DoorReport = serde_json::from_slice(bytes)
                .map_err(|_| status(StatusCode::BAD_REQUEST, "not a report"))?;
            governed_workspaces(web)?
                .report_listed(workspace, &report)
                .await
                .map_err(refused)?;
            Ok(reply(StatusCode::NO_CONTENT, Vec::new()))
        }
        // A page's own uncaught error: one log line for the site's operator,
        // and the same answer whatever became of it, so a page never retries.
        "errors" => {
            web.page_errors.report(workspace, bytes);
            Ok(reply(StatusCode::NO_CONTENT, Vec::new()))
        }
        _ => Err(status(StatusCode::NOT_FOUND, "not found")),
    }
}

/// The workspaces this relay governs, or the answer that it governs none.
#[allow(
    clippy::result_large_err,
    reason = "the error is the response the handler sends as it is"
)]
fn governed_workspaces(web: &Web) -> Result<&crate::workspaces::Workspaces, Response<Body>> {
    web.workspaces
        .as_ref()
        .ok_or_else(|| status(StatusCode::NOT_FOUND, "this relay governs no workspaces"))
}

fn session_status(error: &SessionError) -> Response<Body> {
    match error {
        SessionError::Gone => status(StatusCode::UNAUTHORIZED, "session ended"),
        // Typed, so the device acts on the host's reason (see
        // `protocol::SessionRefusal`).
        SessionError::Denied(refusal) => match postcard::to_allocvec(refusal) {
            Ok(body) => reply(StatusCode::FORBIDDEN, body),
            Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
        },
        SessionError::Refused(reason) => status(StatusCode::FORBIDDEN, reason),
        SessionError::Busy => status(StatusCode::SERVICE_UNAVAILABLE, "busy"),
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the error is the response the handler sends as it is"
)]
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Response<Body>> {
    postcard::from_bytes(bytes).map_err(|_| status(StatusCode::BAD_REQUEST, "malformed body"))
}

/// The client's country as the proxy in front of the host names it
/// ([`WebConfig::country`]): from a trusted peer only, and only a two-letter
/// code.
fn country(web: &Web, request: &Request<hyper::body::Incoming>) -> Option<String> {
    let (header, proxies) = web.country.as_ref()?;
    let peer = request.extensions().get::<Peer>()?;
    if !proxies.contains(peer.0.ip()) {
        return None;
    }
    let value = request.headers().get(header)?.to_str().ok()?.trim();
    (value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_uppercase()) && value != "XX")
        .then(|| value.to_owned())
}

fn token(request: &Request<hyper::body::Incoming>) -> Option<[u8; 32]> {
    let value = request.headers().get(SESSION_HEADER)?.to_str().ok()?;
    hex::decode(value).ok()?.try_into().ok()
}

/// Headers a page may send to the browser routes.
const ALLOWED_HEADERS: &str = "content-type, x-tilo-session, x-tilo-live-session";

/// Whether `path` is part of the public directory: a listing anyone may read,
/// and a door anyone may ask, from any site.
fn public_directory_route(path: &str) -> bool {
    directory_route(path).is_some()
}

/// The public directory's route a site names, below the listener's own paths.
///
/// A relay's own routes live below [`API_PREFIX`]. A site names its directory
/// in its `entry.json` and hangs each door off that address
/// ([`tilo_relay_protocol::directory`]), so the same routes answer at the
/// site's own paths — `/directory` and `/directory/<workspace>/join` — for a
/// host a site's person reaches without the prefix: the ingress in front of
/// it sends the site's paths to the relay.
fn directory_route(path: &str) -> Option<&str> {
    let route = path.strip_prefix(API_PREFIX).unwrap_or(path);
    (route == tilo_relay_protocol::directory::DIRECTORY_PATH
        || tilo_relay_protocol::directory::door_route(route).is_some())
    .then_some(route)
}

/// CORS for the public directory: any origin, no credentials, and the
/// `Retry-After` a full door answers with. A page on any site reads the
/// listings and joins a workspace without going through its host.
fn public_cors(response: &mut Response<Body>) {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        header::HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        header::HeaderValue::from_static("POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        header::HeaderValue::from_static("content-type"),
    );
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        header::HeaderValue::from_static("retry-after"),
    );
}

fn cors(
    allowed_origins: &[String],
    allowed_headers: &str,
    origin: Option<&header::HeaderValue>,
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
    let headers = response.headers_mut();
    if let Ok(origin) = header::HeaderValue::from_str(origin) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    }
    headers.insert(header::VARY, header::HeaderValue::from_static("origin"));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        header::HeaderValue::from_static("POST, OPTIONS"),
    );
    if let Ok(allowed) = header::HeaderValue::from_str(allowed_headers) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, allowed);
    }
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        header::HeaderValue::from_static("x-tilo-live-session"),
    );
}

async fn handle(web: &Web, request: Request<hyper::body::Incoming>) -> Response<Body> {
    // The fetch proxy takes the method of the request it relays.
    if request
        .uri()
        .path()
        .strip_prefix(API_PREFIX)
        .is_some_and(|route| route == protocol::FETCH_PATH)
    {
        return fetch::handle(web, request).await;
    }
    // A sealed invitation is fetched and taken back with their own methods,
    // so the shelf answers before the POST-only gate.
    if let Some(handle) = tilo_relay_protocol::sealed_invitations::handle_of(request.uri().path()) {
        let handle = handle.to_owned();
        let mut response = shelf::handle(web, request, &handle).await;
        shelf::cors(&mut response);
        return response;
    }
    let origin = request.headers().get(header::ORIGIN).cloned();
    let public = public_directory_route(request.uri().path());
    let mut response = if request.method() == Method::OPTIONS {
        reply(StatusCode::NO_CONTENT, Vec::new())
    } else if request.method() != Method::POST {
        status(StatusCode::METHOD_NOT_ALLOWED, "POST only")
    } else {
        route(web, request)
            .await
            .unwrap_or_else(|response| response)
    };
    if public {
        public_cors(&mut response);
    } else {
        cors(
            &web.allowed_origins,
            ALLOWED_HEADERS,
            origin.as_ref(),
            &mut response,
        );
    }
    response
}

async fn body(
    request: Request<hyper::body::Incoming>,
    limit: usize,
) -> Result<(hyper::http::request::Parts, Bytes), Response<Body>> {
    let (parts, body) = request.into_parts();
    let bytes = http_body_util::Limited::new(body, limit)
        .collect()
        .await
        .map_err(|_| status(StatusCode::PAYLOAD_TOO_LARGE, "body too large"))?
        .to_bytes();
    Ok((parts, bytes))
}

/// Whether `user` is admitted to any workspace this relay serves: whose
/// envelope it keeps.
async fn admitted_anywhere(web: &Web, user: [u8; 32]) -> bool {
    let workspaces: Vec<_> = web
        .sessions
        .context()
        .relay
        .workspaces
        .lock()
        .await
        .values()
        .cloned()
        .collect();
    // The person this host runs for is its own before any workspace is bound.
    let own_user = web.sessions.context().relay.identity.user_public_key;
    if user == own_user {
        return true;
    }
    for workspace in workspaces {
        if workspace.lock().await.admits_person(&user, &own_user) {
            return true;
        }
    }
    false
}

fn wait(ms: u32) -> Duration {
    Duration::from_millis(u64::from(ms.min(MAX_POLL_MS)))
}

#[allow(
    clippy::too_many_lines,
    reason = "one routing table: every browser route and its body type in one place"
)]
async fn route(
    web: &Web,
    request: Request<hyper::body::Incoming>,
) -> Result<Response<Body>, Response<Body>> {
    let path = request.uri().path().to_owned();
    // A site's own paths for the public directory — `/directory` and each
    // door below it — reach the same routes as their versions below the
    // prefix, so a host a site's person dials by the address its `entry.json`
    // names answers without being reached under the prefix.
    let Some(route) = path
        .strip_prefix(API_PREFIX)
        .or_else(|| directory_route(&path))
    else {
        return Err(status(StatusCode::NOT_FOUND, "not found"));
    };
    let session = token(&request);
    let country = country(web, &request);
    let (_, bytes) = body(request, MAX_REQUEST_BYTES).await?;
    let certification = &web.sessions.context().certification;
    #[allow(
        clippy::result_large_err,
        reason = "the error is the response the handler sends as it is"
    )]
    let authed = || {
        session
            .ok_or_else(|| status(StatusCode::UNAUTHORIZED, "no session"))
            .and_then(|token| {
                web.sessions
                    .get(&token)
                    .map_err(|error| session_status(&error))
            })
    };
    match route {
        "/host" => Ok(encoded(&web.host_info)),
        "/node" => {
            authed()?;
            Ok(encoded(&NodeInfo {
                session: web.sessions.context().node_session.get().copied(),
            }))
        }
        "/certify" => {
            let request: CertificationRequest = decode(&bytes)?;
            // A member of a workspace here pairs their devices through it;
            // the member's own devices answer (`/certify/approve`).
            let member = admitted_anywhere(web, request.user_public_key).await;
            Ok(encoded(&match certification.enqueue_member(
                request,
                tilo_clock::unix_now_ns(),
                member,
                country,
            ) {
                Ok((_, pairing_code, _)) => CertifyReply::Pending { pairing_code },
                Err(reason) => CertifyReply::Denied { reason },
            }))
        }
        "/certify/approve" => {
            let approval: crate::certify::MemberApproval = decode(&bytes)?;
            if !admitted_anywhere(web, approval.user_public_key).await {
                return Err(status(
                    StatusCode::FORBIDDEN,
                    "only a member approves devices here",
                ));
            }
            Ok(encoded(
                &certification.member_approve(approval, tilo_clock::unix_now_ns()),
            ))
        }
        "/certify/wait" => {
            let digest: [u8; 32] = decode(&bytes)?;
            Ok(encoded(
                &certification.wait(&digest, wait(MAX_POLL_MS)).await,
            ))
        }
        "/session" => {
            // A device at another peer protocol version is told this host's
            // and reads nothing else.
            let hello = match crate::auth::decode_handshake(&bytes) {
                Ok(hello) => hello,
                Err(crate::auth::HandshakeFrameError::Version { theirs }) => {
                    tracing::info!(
                        theirs,
                        "a browser device at another protocol version was told this host's"
                    );
                    return Ok(match crate::auth::version_answer() {
                        Ok(answer) => reply(StatusCode::CONFLICT, answer),
                        Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
                    });
                }
                Err(crate::auth::HandshakeFrameError::Codec(_)) => {
                    return Err(status(StatusCode::BAD_REQUEST, "malformed body"));
                }
            };
            web.sessions
                .hello(&hello)
                .map(|challenge| encoded(&challenge))
                .map_err(|error| session_status(&error))
        }
        "/live" => {
            let session = authed()?;
            let request: tilo_relay_protocol::web::LiveRequest = decode(&bytes)?;
            let live = web
                .live
                .as_ref()
                .ok_or_else(|| status(StatusCode::NOT_FOUND, "no live plane"))?;
            if !tilo_relay_protocol::room_workspace_id_matches(
                &request.workspace,
                request.workspace_id,
            ) {
                return Err(status(
                    StatusCode::BAD_REQUEST,
                    "not a workspace of this relay",
                ));
            }
            if !web
                .sessions
                .admits(&session.peer, request.workspace_id)
                .await
            {
                return Err(status(
                    StatusCode::FORBIDDEN,
                    "not admitted to this workspace",
                ));
            }
            live.pages.issue(
                tilo_relay_protocol::web::LiveGrant {
                    workspace_id: request.workspace_id,
                    token: session.token(),
                },
                &request.workspace,
                &session.peer,
            );
            Ok(encoded(&live.info))
        }
        "/session/finish" => {
            let finish: SessionFinish = decode(&bytes)?;
            let grant = web
                .sessions
                .finish(&finish.handshake, &finish.ack2, finish.background)
                .map_err(|error| session_status(&error))?;
            Ok(match protocol::encode_grant(&grant, web.features) {
                Ok(bytes) => reply(StatusCode::OK, bytes),
                Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
            })
        }
        // A page leaving says so in a beacon, its token the body (no
        // header rides a beacon): its session ends now, not when its polls
        // stop. Whoever holds a token may end it, as by going quiet.
        tilo_relay_protocol::web::SESSION_END_PATH => {
            if let Some(token) = std::str::from_utf8(&bytes)
                .ok()
                .and_then(|text| hex::decode(text.trim()).ok())
                .and_then(|token| <[u8; 32]>::try_from(token).ok())
            {
                web.sessions.end(&token);
            }
            Ok(reply(StatusCode::NO_CONTENT, Vec::new()))
        }
        "/send" => {
            let session = authed()?;
            let envelopes = decode(&bytes)?;
            web.sessions
                .send(&session, envelopes)
                .await
                .map(|()| reply(StatusCode::NO_CONTENT, Vec::new()))
                .map_err(|error| session_status(&error))
        }
        "/events" => {
            let session = authed()?;
            let request: EventsRequest = decode(&bytes)?;
            web.sessions
                .events(&session, request.ack, wait(request.wait_ms))
                .await
                .map(|page| encoded(&page))
                .map_err(|error| session_status(&error))
        }
        protocol::HERE_PATH => {
            let session = authed()?;
            let request: protocol::HereRequest = decode(&bytes)?;
            request
                .validate()
                .map_err(|why| status(StatusCode::BAD_REQUEST, why))?;
            if !web
                .sessions
                .admits(&session.peer, request.workspace_id)
                .await
            {
                return Err(status(
                    StatusCode::FORBIDDEN,
                    "not admitted to this workspace",
                ));
            }
            let workspace_id = request.workspace_id;
            let (mut reply, job) = web.here.post(
                request,
                session.peer.device_public_key,
                session.peer.user_public_key,
                std::time::Instant::now(),
                tilo_clock::unix_now_ms(),
            );
            here::pair_members(&mut reply, &web.sessions.persons_in(workspace_id));
            for person in &mut reply.people {
                person.profile_ms = web.profiles.issued_at(&person.user).unwrap_or(0);
            }
            if let Some(job) = job {
                tokio::spawn(web.here.clone().summarize(job));
            }
            Ok(encoded(&reply))
        }
        "/rooms" => {
            let session = authed()?;
            let workspaces = web
                .workspaces
                .as_ref()
                .ok_or_else(|| status(StatusCode::NOT_FOUND, "this relay governs no workspaces"))?;
            let request = decode(&bytes)?;
            Ok(encoded(
                &workspaces.page_rooms(&session.peer.cert, request).await,
            ))
        }
        "/join" => {
            let workspaces = web
                .workspaces
                .as_ref()
                .ok_or_else(|| status(StatusCode::NOT_FOUND, "this relay governs no workspaces"))?;
            let step = decode(&bytes)?;
            Ok(encoded(&workspaces.page_join(step).await))
        }
        tilo_relay_protocol::directory::DIRECTORY_PATH => {
            let workspaces = web
                .workspaces
                .as_ref()
                .ok_or_else(|| status(StatusCode::NOT_FOUND, "this relay governs no workspaces"))?;
            let online = web.sessions.online_users().into_keys().collect();
            Ok(json(&tilo_relay_protocol::directory::DirectoryPage {
                workspaces: workspaces.directory(&online).await,
            }))
        }
        _ if tilo_relay_protocol::directory::door_route(route).is_some() => {
            directory_door(web, route, &bytes).await
        }
        "/envelope" | "/profile" => {
            let now_ms = tilo_clock::unix_now_ms();
            let kept = if route == "/profile" {
                &web.profiles
            } else {
                &web.envelopes
            };
            kept.publish(&bytes, now_ms, |root| admitted_anywhere(web, root))
                .await
                .map(|()| reply(StatusCode::NO_CONTENT, Vec::new()))
                .map_err(|refusal| {
                    let code = match refusal {
                        envelopes::Refusal::Malformed(_) => StatusCode::BAD_REQUEST,
                        envelopes::Refusal::BadSignature | envelopes::Refusal::NotMember => {
                            StatusCode::FORBIDDEN
                        }
                        envelopes::Refusal::Stale => StatusCode::CONFLICT,
                        envelopes::Refusal::Storage => StatusCode::INTERNAL_SERVER_ERROR,
                    };
                    status(code, refusal.message())
                })
        }
        "/envelope/fetch" | "/profile/fetch" => match if route == "/profile/fetch" {
            web.profiles.answer(&bytes)
        } else {
            web.envelopes.answer(&bytes)
        } {
            Some(json) => {
                let mut response = reply(StatusCode::OK, json);
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("application/json"),
                );
                Ok(response)
            }
            None => Err(status(StatusCode::BAD_REQUEST, "malformed root public key")),
        },
        _ => Err(status(StatusCode::NOT_FOUND, "not found")),
    }
}

/// The configured country header as the listener reads it; a name that is not
/// a valid header name is no header.
fn country_header(
    country: CountryHeader,
) -> Option<(
    hyper::header::HeaderName,
    tilo_relay_protocol::proxies::TrustedProxies,
)> {
    hyper::header::HeaderName::try_from(country.name.as_str())
        .ok()
        .map(|name| (name, country.proxies))
}
