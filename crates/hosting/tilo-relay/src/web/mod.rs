//! The host's browser listener (feature `web`): HTTPS (TCP, HTTP/2 and
//! HTTP/1.1), served by the relay itself.
//!
//! What a page needs before it holds a session rides it: the host's
//! certificate and address (`/host`), joining a workspace, the public
//! directory, root envelopes and profiles, the fetch proxy and the static
//! entry. Everything after that — certification, presence, membership,
//! records, previews, calls — goes over iroh, as every device's does. Routes are listed in [`tilo_relay_protocol::web`].
//!
//! No owner loop runs here: every handler is async and awaits the relay's
//! own locks; workspace I/O already runs on the blocking pool.

mod envelopes;
mod fetch;
pub mod install;
mod login_codes;
mod page_errors;
pub(crate) mod plane;
mod shelf;
mod sign_in;
mod stand_in;
#[cfg(test)]
mod tests;
mod tls;

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
pub(crate) use tilo_relay_protocol::web as protocol;
use tilo_relay_protocol::web::{API_PREFIX, HostAddress, HostInfo, MAX_REQUEST_BYTES};

use crate::daemon::{Relay, RelayError, WebContext};

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
    /// The fetch proxy this host serves its members' pages at
    /// [`protocol::FETCH_PATH`], if any.
    pub fetch_proxy: Option<FetchProxySetup>,
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
    /// The live plane: the one account of who is here.
    presence: Arc<plane::Authority>,
}

impl WebHandle {
    /// Open browser sessions per person (user root) now, for a public
    /// host's presence count and idle-guest pruning. One person on two
    /// devices is one entry; the workspace's visitor person is one entry whose
    /// count is its open pages.
    #[must_use]
    pub fn online_users(&self) -> std::collections::BTreeMap<[u8; 32], usize> {
        self.presence
            .participants()
            .into_iter()
            .map(|(user, devices)| (user, devices.len()))
            .collect()
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
    context: WebContext,
    host_info: HostInfo,
    allowed_origins: Vec<String>,
    /// The workspaces this relay governs, for `/join`.
    workspaces: Option<crate::workspaces::Workspaces>,
    /// Members' root envelopes, for `/envelope`.
    envelopes: envelopes::Envelopes,
    /// Members' profiles, for `/profile`.
    profiles: Arc<envelopes::Envelopes>,
    /// The fetch proxy, when [`WebConfig::fetch_proxy`] asked for one, and the
    /// tokens its pages' sessions take for it.
    fetch: Option<tilo_net::proxy::host::FetchProxy>,
    grants: Arc<fetch::Grants>,
    /// The live plane: the one account of who is here.
    presence: Arc<plane::Authority>,
    /// The page errors the door logs.
    page_errors: page_errors::PageErrors,
    /// The sealed invitations this site keeps in place of their clerk: what a
    /// short link names, in bytes the site has no key for.
    shelf: shelf::Shelf,
    /// The login codes tabs hand their agents.
    login_codes: Arc<login_codes::LoginCodes>,
    /// The stand-in approvers this host keeps for people.
    stand_ins: Arc<stand_in::StandIns>,
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
    /// certificate cannot be loaded or an address cannot be bound.
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
        if let Some(workspaces) = self.workspaces() {
            let _ = context.relay.carried_calls.set(workspaces.downgrade());
        }
        let host_cert = context.relay.identity.cert.clone();
        // Where a page dials this host: its endpoint's key and addresses, and
        // the relay it was told to reach it through. A page has no UDP socket,
        // so without that relay an endpoint it binds carries no packet at all —
        // and an endpoint names its relay only once it has connected to it,
        // which a start that restarts on kept state opens its listener before.
        let mut endpoint_addr = self.node_addr();
        if let Some(relay) = &self.pages_reach {
            endpoint_addr = endpoint_addr.with_relay_url(relay.clone());
        }
        let relay_url = endpoint_addr.relay_urls().next().map(ToString::to_string);
        let web = Arc::new(Web {
            context: context.clone(),
            host_info: HostInfo {
                host_cert,
                address: Some(HostAddress {
                    endpoint_addr,
                    relay_url,
                }),
            },
            allowed_origins: config.allowed_origins,
            workspaces: self.workspaces().cloned(),
            envelopes: envelopes::Envelopes::new(self.state_root().join("envelopes")),
            profiles: Arc::new(envelopes::Envelopes::of(
                envelopes::Record::Profile,
                self.state_root().join("profiles"),
            )),
            fetch: fetch.proxy,
            grants: Arc::default(),
            presence: plane.authority(),
            page_errors: page_errors::PageErrors::default(),
            // A link is read days after it was made, so what it names waits on
            // this host's disk: a restart, an upgrade or a deploy over this
            // relay leaves it there.
            shelf: shelf::Shelf::opening(
                self.state_root().join("invitations"),
                tilo_clock::unix_now_ns(),
            ),
            login_codes: Arc::new(login_codes::LoginCodes::new(public_origin.clone())),
            stand_ins: Arc::new(stand_in::StandIns::new(self.state_root().join("stand-ins"))),
        });
        web.serve_stand_ins();
        // A page's fetch proxy token is minted on the session it fetched from.
        if web.fetch.is_some() {
            let grants: Arc<dyn crate::peer::FetchGrants> = web.grants.clone();
            let _ = context.relay.fetch.set(grants);
        }
        let mut tasks = Vec::new();
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
            presence: plane.authority(),
        })
    }
}

impl Relay {}

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
            serve_connection(tls, peer, web).await;
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
        connections.spawn(serve_connection(stream, peer, web.clone()));
    }
}

/// The address a request's connection came from.
#[derive(Clone, Copy, Debug)]
struct Peer(std::net::IpAddr);

async fn serve_connection<Io>(io: Io, peer: SocketAddr, web: Arc<Web>)
where
    Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = hyper::service::service_fn(move |mut request: Request<hyper::body::Incoming>| {
        let web = web.clone();
        request.extensions_mut().insert(Peer(peer.ip()));
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
            | crate::workspaces::DoorRefusal::Banned
            | crate::workspaces::DoorRefusal::Removed
            | crate::workspaces::DoorRefusal::InvitationOnly => StatusCode::FORBIDDEN,
        };
        let mut response = json_status(full, &error.answer());
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

#[allow(
    clippy::result_large_err,
    reason = "the error is the response the handler sends as it is"
)]
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Response<Body>> {
    postcard::from_bytes(bytes).map_err(|_| status(StatusCode::BAD_REQUEST, "malformed body"))
}

/// Headers a page may send to the browser routes.
const ALLOWED_HEADERS: &str = "content-type, x-tilo-session";

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
    // The person this host runs for is its own before any workspace is bound.
    web.context.relay.admits(user).await
}

/// The client a request came from: its connection's address, or, for a
/// connection from a proxy in front of this host (a loopback or private
/// address), the last address that proxy names in `X-Forwarded-For`.
fn client_address(request: &Request<hyper::body::Incoming>) -> Option<std::net::IpAddr> {
    let Peer(peer) = *request.extensions().get::<Peer>()?;
    let proxied = match peer {
        std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private(),
        std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
    };
    let forwarded = proxied
        .then(|| {
            request
                .headers()
                .get_all("x-forwarded-for")
                .iter()
                .next_back()
        })
        .flatten()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit(',').next())
        .and_then(|last| last.trim().parse().ok());
    Some(forwarded.unwrap_or(peer))
}

/// A JSON refusal the page reads: `{error}` with `code`.
fn refused(code: StatusCode, message: &str) -> Response<Body> {
    let mut response = json_status(code, &serde_json::json!({ "error": message }));
    if code == StatusCode::TOO_MANY_REQUESTS || code == StatusCode::SERVICE_UNAVAILABLE {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, header::HeaderValue::from_static("60"));
    }
    response
}

fn code_refused(refusal: &login_codes::Refusal) -> Response<Body> {
    let code = match refusal {
        login_codes::Refusal::Malformed(_) => StatusCode::BAD_REQUEST,
        login_codes::Refusal::Unknown => StatusCode::NOT_FOUND,
        login_codes::Refusal::TooMany => StatusCode::TOO_MANY_REQUESTS,
        login_codes::Refusal::Full => StatusCode::SERVICE_UNAVAILABLE,
        login_codes::Refusal::Conflict(_) => StatusCode::CONFLICT,
    };
    refused(code, refusal.message())
}

#[allow(
    clippy::result_large_err,
    reason = "the error is the response the handler sends as it is"
)]
fn json_body<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Response<Body>> {
    serde_json::from_slice(bytes)
        .map_err(|_| refused(StatusCode::BAD_REQUEST, "not a request this route takes"))
}

/// The login code routes ([`tilo_relay_protocol::login_code`]), or `None`
/// for any other route.
fn login_route(
    web: &Web,
    route: &str,
    bytes: &[u8],
    client: Option<std::net::IpAddr>,
) -> Option<Response<Body>> {
    use tilo_relay_protocol::login_code as wire;
    fn answered<T: serde::Serialize>(result: Result<T, login_codes::Refusal>) -> Response<Body> {
        result.map_or_else(|refusal| code_refused(&refusal), |value| json(&value))
    }
    let now_ms = tilo_clock::unix_now_ms();
    let codes = &web.login_codes;
    Some(match route {
        wire::CREATE_ROUTE => match json_body::<wire::CreateCode>(bytes) {
            Ok(create) => answered(codes.create(create.link, client, now_ms)),
            Err(refusal) => refusal,
        },
        wire::RESOLVE_ROUTE => match json_body::<wire::Resolve>(bytes) {
            Ok(resolve) => answered(codes.resolve(&resolve.code, client, now_ms)),
            Err(refusal) => refusal,
        },
        wire::STEP_ROUTE => match json_body::<wire::Report>(bytes) {
            Ok(report) => codes.report(&report, now_ms).map_or_else(
                |refusal| code_refused(&refusal),
                |()| reply(StatusCode::NO_CONTENT, Vec::new()),
            ),
            Err(refusal) => refusal,
        },
        wire::CLAIM_ROUTE => match json_body::<wire::Claim>(bytes) {
            Ok(claim) => web.claim(&claim).unwrap_or_else(|refusal| refusal),
            Err(refusal) => refusal,
        },
        wire::FOLLOW_ROUTE => match json_body::<wire::Follow>(bytes) {
            Ok(follow) => answered(codes.follow(&follow.follow, now_ms)),
            Err(refusal) => refusal,
        },
        wire::STAND_IN_ROUTE => match json_body::<wire::StandInRequest>(bytes) {
            Ok(request) => web
                .stand_ins
                .answer(request, tilo_clock::unix_now_ns())
                .map_or_else(
                    |refusal| {
                        let code = match refusal {
                            stand_in::Refusal::Malformed(_) => StatusCode::BAD_REQUEST,
                            stand_in::Refusal::Forbidden(_) => StatusCode::FORBIDDEN,
                            stand_in::Refusal::Unknown => StatusCode::NOT_FOUND,
                            stand_in::Refusal::Full => StatusCode::SERVICE_UNAVAILABLE,
                            stand_in::Refusal::Storage => StatusCode::INTERNAL_SERVER_ERROR,
                        };
                        refused(code, refusal.message())
                    },
                    |reply| json(&reply),
                ),
            Err(refusal) => refusal,
        },
        _ => return None,
    })
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
    let client = client_address(&request);
    let (_, bytes) = body(request, MAX_REQUEST_BYTES).await?;
    if let Some(answer) = login_route(web, route, &bytes, client) {
        return Ok(answer);
    }
    match route {
        "/host" => Ok(encoded(&web.host_info)),
        tilo_relay_protocol::web::WORKSPACE_DELETED_PATH => {
            let workspace: String = decode(&bytes)?;
            let deleted = web.workspaces.as_ref().is_some_and(|workspaces| {
                tilo_relay_protocol::room_workspace_id(&workspace)
                    .is_ok_and(|workspace_id| workspaces.deleted(workspace_id))
            });
            Ok(encoded(&deleted))
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
            let online = web.presence.participants().into_keys().collect();
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
