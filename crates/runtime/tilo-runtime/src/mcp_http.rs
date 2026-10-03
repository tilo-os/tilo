//! Streamable HTTP transport for the MCP projection.
//!
//! Every MCP session is one agent run: concurrent clients of one machine are
//! attributed separately in history while acting with the profile actor's
//! authority. Whoever fronts this endpoint (a hosting provider's proxy) owns
//! end-user authentication and pins sessions to the right machine; the
//! optional bearer token only proves that the caller is that proxy.
//!
//! Only JSON responses are produced: no server-initiated stream is offered, so
//! `GET` answers 405 as the specification allows.
//!
//! Beside `/mcp`, `POST /watch {"watching": bool}` holds the compositor's
//! [`WatchLease`] for a dashboard showing this machine, under the same token.

use crate::mcp::McpServer;
use crate::watch::{WATCH_LEASE, WatchLease};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Bytes, Incoming},
    header::{self, HeaderValue},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io,
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tilo_actions::remote::ActionHost;

pub const ADDRESS_ENV: &str = "TILO_RUNTIME_MCP_HTTP";
pub const TOKEN_FILE_ENV: &str = "TILO_RUNTIME_MCP_HTTP_TOKEN_FILE";
const SESSION_HEADER: &str = "mcp-session-id";
const MAX_BODY_BYTES: usize = crate::mcp_process::MAX_REQUEST_BYTES;
const MAX_SESSIONS: usize = 256;
const SESSION_IDLE: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_LABEL_CHARS: usize = 80;
const VIEWER_PREFIX: &str = "/viewer/invitations";

/// Where to listen and which bearer token, if any, callers must present.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub address: SocketAddr,
    pub token: Option<String>,
}

impl Config {
    /// Read the listen address and optional token file named by the
    /// environment. `None` when no address is configured.
    ///
    /// # Errors
    ///
    /// Rejects an unparsable address, an unreadable or empty token file, and a
    /// non-loopback address without a token.
    pub fn from_env() -> io::Result<Option<Self>> {
        let Some(address) = std::env::var_os(ADDRESS_ENV) else {
            return Ok(None);
        };
        let address = address
            .to_str()
            .and_then(|value| value.parse::<SocketAddr>().ok())
            .ok_or_else(|| invalid(format!("{ADDRESS_ENV} must be HOST:PORT")))?;
        let token = std::env::var_os(TOKEN_FILE_ENV)
            .map(|path| read_token(Path::new(&path)))
            .transpose()?;
        let config = Self { address, token };
        config.validate()?;
        Ok(Some(config))
    }

    /// # Errors
    ///
    /// Rejects exposing the endpoint beyond loopback without a token.
    pub fn validate(&self) -> io::Result<()> {
        if !self.address.ip().is_loopback() && self.token.is_none() {
            return Err(invalid(format!(
                "{ADDRESS_ENV} beyond loopback requires {TOKEN_FILE_ENV}"
            )));
        }
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn read_token(path: &Path) -> io::Result<String> {
    let token = std::fs::read_to_string(path)?.trim().to_owned();
    if token.is_empty() {
        return Err(invalid(format!("{} is empty", path.display())));
    }
    Ok(token)
}

struct Session<H> {
    server: Arc<tokio::sync::Mutex<McpServer<H>>>,
    last_used: Instant,
}

/// The per-endpoint state shared by every connection.
pub struct Endpoint<H> {
    host: H,
    room: String,
    actor: String,
    token: Option<String>,
    computer: Option<Arc<crate::computer::Computer>>,
    viewer: Option<Arc<crate::viewer::Viewer>>,
    /// Present exactly when a compositor is driven, which `/watch` then gates.
    watch: Option<Arc<WatchLease>>,
    sessions: Mutex<HashMap<String, Session<H>>>,
    open_files: Arc<crate::mcp::OpenFiles>,
    /// Where sessions read the content actions produced.
    media: std::sync::OnceLock<Arc<dyn crate::MediaReader>>,
}

/// The only body `POST /watch` accepts.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchRequest {
    watching: bool,
}

impl<H: ActionHost + Clone + Send + Sync + 'static> Endpoint<H> {
    /// [`Self::with_viewer`] without a partner-hosted viewer.
    #[must_use]
    pub fn new(
        host: H,
        room: String,
        actor: String,
        token: Option<String>,
        computer: Option<Arc<crate::computer::Computer>>,
    ) -> Arc<Self> {
        Self::with_viewer(host, room, actor, token, computer, None)
    }

    /// Also answer `/viewer/invitations` for a partner-hosted viewer. A
    /// `computer`, when present, is also the compositor `/watch` drives.
    #[must_use]
    pub fn with_viewer(
        host: H,
        room: String,
        actor: String,
        token: Option<String>,
        computer: Option<Arc<crate::computer::Computer>>,
        viewer: Option<Arc<crate::viewer::Viewer>>,
    ) -> Arc<Self> {
        let watch = computer.clone().map(|computer| {
            let sink: Arc<dyn crate::watch::WatchSink> = computer;
            WatchLease::new(sink, WATCH_LEASE)
        });
        Self::build(host, room, actor, token, computer, viewer, watch)
    }

    fn build(
        host: H,
        room: String,
        actor: String,
        token: Option<String>,
        computer: Option<Arc<crate::computer::Computer>>,
        viewer: Option<Arc<crate::viewer::Viewer>>,
        watch: Option<Arc<WatchLease>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            host,
            room,
            actor,
            token,
            computer,
            viewer,
            watch,
            sessions: Mutex::new(HashMap::new()),
            open_files: Arc::default(),
            media: std::sync::OnceLock::new(),
        })
    }

    /// Let sessions answer with the content actions produced, read from
    /// `media`. Set once, before the endpoint serves.
    pub fn serve_media(&self, media: Option<Arc<dyn crate::MediaReader>>) {
        if let Some(media) = media {
            let _ = self.media.set(media);
        }
    }

    /// Number of live sessions.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.lock().map_or(0, |sessions| sessions.len())
    }

    /// Answer one HTTP request.
    pub async fn respond(self: &Arc<Self>, request: Request<Incoming>) -> Response<Full<Bytes>> {
        let (parts, body) = request.into_parts();
        let body = match Limited::new(body, MAX_BODY_BYTES).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(_) => return plain(StatusCode::PAYLOAD_TOO_LARGE, "request body exceeds 1 MiB"),
        };
        self.dispatch(&parts.method, parts.uri.path(), &parts.headers, &body)
            .await
    }

    /// Transport-level routing, separated from hyper's body type for tests.
    pub async fn dispatch(
        self: &Arc<Self>,
        method: &Method,
        path: &str,
        headers: &hyper::HeaderMap,
        body: &[u8],
    ) -> Response<Full<Bytes>> {
        match (method, path) {
            (&Method::GET, "/health") => json_response(StatusCode::OK, &json!({"ok":true}), None),
            (_, "/mcp" | "/watch") if !self.authorized(headers) => {
                plain(StatusCode::UNAUTHORIZED, "missing or wrong bearer token")
            }
            (&Method::POST, "/mcp") => self.post(headers, body).await,
            (&Method::DELETE, "/mcp") => match session_id(headers) {
                Some(id) if self.remove(&id) => plain(StatusCode::NO_CONTENT, ""),
                Some(_) => plain(StatusCode::NOT_FOUND, "unknown session"),
                None => plain(StatusCode::BAD_REQUEST, "missing Mcp-Session-Id"),
            },
            (_, "/mcp") => plain(StatusCode::METHOD_NOT_ALLOWED, "POST or DELETE"),
            (&Method::POST, "/watch") => match &self.watch {
                Some(lease) => watch(lease, body).await,
                None => plain(StatusCode::NOT_FOUND, "no compositor to watch"),
            },
            (_, "/watch") => plain(StatusCode::METHOD_NOT_ALLOWED, "POST"),
            (_, path) if path.starts_with(VIEWER_PREFIX) => {
                self.viewer_route(method, path, headers, body).await
            }
            _ => plain(StatusCode::NOT_FOUND, "not found"),
        }
    }

    /// `POST /viewer/invitations`, `DELETE /viewer/invitations/<id>`,
    /// `POST /viewer/invitations/<id>/role` and `…/<id>/renew`.
    async fn viewer_route(
        &self,
        method: &Method,
        path: &str,
        headers: &hyper::HeaderMap,
        body: &[u8],
    ) -> Response<Full<Bytes>> {
        let Some(viewer) = self.viewer.clone() else {
            return plain(StatusCode::NOT_FOUND, "not found");
        };
        if !self.authorized(headers) {
            return plain(StatusCode::UNAUTHORIZED, "missing or wrong bearer token");
        }
        let rest = path.trim_start_matches(VIEWER_PREFIX);
        let segments: Vec<&str> = rest.split('/').filter(|part| !part.is_empty()).collect();
        let valid_id = |id: &str| id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit());
        let call = match (method, segments.as_slice()) {
            (&Method::POST, []) => ViewerCall::Issue,
            (&Method::DELETE, [id]) if valid_id(id) => ViewerCall::Revoke((*id).to_owned()),
            (&Method::POST, [id, "role"]) if valid_id(id) => ViewerCall::Role((*id).to_owned()),
            (&Method::POST, [id, "renew"]) if valid_id(id) => ViewerCall::Renew((*id).to_owned()),
            (_, [] | [_] | [_, "role" | "renew"]) => {
                return plain(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
            }
            _ => return plain(StatusCode::NOT_FOUND, "not found"),
        };
        let body = body.to_vec();
        let result = tokio::task::spawn_blocking(move || match call {
            ViewerCall::Issue => viewer.issue(&body),
            ViewerCall::Revoke(id) => viewer.revoke(&id),
            ViewerCall::Role(id) => viewer.set_role(&id, &body),
            ViewerCall::Renew(id) => viewer.renew(&id, &body, crate::viewer::now_ms()),
        })
        .await;
        match result {
            Ok(Ok(value)) => json_response(StatusCode::OK, &value, None),
            Ok(Err(error)) => json_response(
                StatusCode::from_u16(error.status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
                &json!({"error": error.message}),
                None,
            ),
            Err(_) => plain(StatusCode::SERVICE_UNAVAILABLE, "viewer request failed"),
        }
    }

    fn authorized(&self, headers: &hyper::HeaderMap) -> bool {
        let Some(expected) = &self.token else {
            return true;
        };
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|presented| constant_time_eq(presented.as_bytes(), expected.as_bytes()))
    }

    async fn post(
        self: &Arc<Self>,
        headers: &hyper::HeaderMap,
        body: &[u8],
    ) -> Response<Full<Bytes>> {
        let Ok(request) = serde_json::from_slice::<Value>(body) else {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
                None,
            );
        };
        let initialize = request.get("method").and_then(Value::as_str) == Some("initialize");
        let (id, server, created) = match (session_id(headers), initialize) {
            (None, true) => match self.create(&request) {
                Some((id, server)) => (id, server, true),
                None => return plain(StatusCode::SERVICE_UNAVAILABLE, "too many MCP sessions"),
            },
            (Some(id), _) => match self.find(&id) {
                Some(server) => (id, server, false),
                None => return plain(StatusCode::NOT_FOUND, "unknown session; initialize again"),
            },
            (None, false) => return plain(StatusCode::BAD_REQUEST, "missing Mcp-Session-Id"),
        };
        let reply = server.lock().await.handle(request).await;
        let header = created.then_some(id.as_str());
        if let Some(reply) = reply {
            return json_response(StatusCode::OK, &reply, header);
        }
        let mut response = plain(StatusCode::ACCEPTED, "");
        if let Some(id) = header
            && let Ok(value) = HeaderValue::from_str(id)
        {
            response.headers_mut().insert(SESSION_HEADER, value);
        }
        response
    }

    fn create(
        &self,
        initialize: &Value,
    ) -> Option<(String, Arc<tokio::sync::Mutex<McpServer<H>>>)> {
        let id = uuid::Uuid::new_v4().to_string();
        let server = Arc::new(tokio::sync::Mutex::new(
            McpServer::new(self.host.clone(), self.room.clone(), self.actor.clone())
                .with_agent_run(agent_run_id(&id), client_label(initialize))
                .with_computer(self.computer.clone())
                .with_open_files(Arc::clone(&self.open_files))
                .with_media(self.media.get().cloned()),
        ));
        let now = Instant::now();
        let session = Session {
            server: Arc::clone(&server),
            last_used: now,
        };
        let mut idle = Vec::new();
        let admitted = {
            let mut sessions = self.sessions.lock().ok()?;
            sessions.retain(|id, session| {
                let live = now.duration_since(session.last_used) < SESSION_IDLE;
                if !live {
                    idle.push(id.clone());
                }
                live
            });
            let admitted = sessions.len() < MAX_SESSIONS;
            if admitted {
                sessions.insert(id.clone(), session);
            }
            admitted
        };
        for id in idle {
            self.release_agent(&id);
        }
        admitted.then_some((id, server))
    }

    /// Let the compositor release whatever the ended session's agent still
    /// holds on the shared seat. Off the async runtime: it is socket I/O.
    fn release_agent(&self, id: &str) {
        let Some(computer) = self.computer.clone() else {
            return;
        };
        let agent = crate::computer::Agent {
            id: agent_run_id(id),
            label: String::new(),
        };
        let leave = move || {
            if let Err(error) = computer.leave(&agent) {
                tracing::debug!(%error, "agent seat release");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn_blocking(leave)),
            Err(_) => leave(),
        }
    }

    fn find(&self, id: &str) -> Option<Arc<tokio::sync::Mutex<McpServer<H>>>> {
        self.sessions.lock().ok()?.get_mut(id).map(|session| {
            session.last_used = Instant::now();
            Arc::clone(&session.server)
        })
    }

    fn remove(&self, id: &str) -> bool {
        let removed = self
            .sessions
            .lock()
            .is_ok_and(|mut sessions| sessions.remove(id).is_some());
        if removed {
            self.release_agent(id);
        }
        removed
    }
}

/// Start, renew or end the watch lease.
async fn watch(lease: &Arc<WatchLease>, body: &[u8]) -> Response<Full<Bytes>> {
    let Ok(WatchRequest { watching }) = serde_json::from_slice(body) else {
        return plain(
            StatusCode::BAD_REQUEST,
            r#"the body must be {"watching": bool}"#,
        );
    };
    match lease.apply(watching).await {
        Ok(()) => json_response(
            StatusCode::OK,
            &json!({"watching": watching, "lease_seconds": lease.lease().as_secs()}),
            None,
        ),
        Err(error) => {
            tracing::warn!(%error, watching, "the compositor did not take the watch signal");
            plain(
                StatusCode::SERVICE_UNAVAILABLE,
                "the compositor did not take the watch signal",
            )
        }
    }
}

/// One `/viewer/invitations` route, with its invitation id.
enum ViewerCall {
    Issue,
    Revoke(String),
    Role(String),
    Renew(String),
}

/// The agent run an MCP session is.
fn agent_run_id(session: &str) -> String {
    format!("mcp:{session}")
}

/// The human label of an agent run: the client's declared name, bounded.
fn client_label(initialize: &Value) -> String {
    let name = initialize
        .pointer("/params/clientInfo/name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("MCP agent");
    name.chars()
        .filter(|character| !character.is_control())
        .take(MAX_LABEL_CHARS)
        .collect()
}

fn session_id(headers: &hyper::HeaderMap) -> Option<String> {
    headers
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn json_response(
    status: StatusCode,
    value: &Value,
    session: Option<&str>,
) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(value.to_string())));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Some(id) = session
        && let Ok(value) = HeaderValue::from_str(id)
    {
        response.headers_mut().insert(SESSION_HEADER, value);
    }
    response
}

fn plain(status: StatusCode, message: &'static str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from_static(message.as_bytes())));
    *response.status_mut() = status;
    response
}

/// Bind `config.address` and serve on the app runtime until the process ends.
///
/// # Errors
///
/// Returns bind or runtime-start errors; per-connection errors are logged.
pub fn spawn<H: ActionHost + Clone + Send + Sync + 'static>(
    config: &Config,
    endpoint: Arc<Endpoint<H>>,
) -> io::Result<SocketAddr> {
    config.validate()?;
    let listener = std::net::TcpListener::bind(config.address)?;
    listener.set_nonblocking(true)?;
    let bound = listener.local_addr()?;
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    runtime.spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                tracing::error!(%error, "MCP HTTP listener could not start");
                return;
            }
        };
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let endpoint = Arc::clone(&endpoint);
            tokio::spawn(async move {
                let service = service_fn(move |request| {
                    let endpoint = Arc::clone(&endpoint);
                    async move { Ok::<_, std::convert::Infallible>(endpoint.respond(request).await) }
                });
                if let Err(error) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await
                {
                    tracing::debug!(%error, "MCP HTTP connection ended");
                }
            });
        }
    });
    Ok(bound)
}

#[cfg(test)]
#[path = "mcp_http_tests.rs"]
mod tests;
