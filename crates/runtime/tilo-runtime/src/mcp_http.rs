//! Streamable HTTP transport for the MCP projection.
//!
//! Every MCP session is one agent run: concurrent clients of one machine are
//! attributed separately in history while acting with the profile actor's
//! authority.
//!
//! One endpoint serves every workspace this runtime holds:
//!
//! - `POST`/`DELETE /w/{workspace}/mcp` opens and answers a session in that
//!   workspace, and `POST /w/{workspace}/join` joins the workspace an
//!   invitation names. Anything else in the workspace's place answers 404.
//! - `/mcp` is the self-hosted agent host's own door: the configured
//!   workspace, reached by the bearer token alone, for an assistant that
//!   connects to its own machine directly. A hosted deployment turns it off
//!   ([`Grants::Required`]) so that its token alone reaches no workspace.
//! - `/health` says the endpoint is up, and needs no credential.
//!
//! The bearer token proves that the caller is whoever fronts this endpoint —
//! a hosting provider's door — and is required on every route but `/health`,
//! loopback included, which every process and account of the machine can
//! reach. It is not the person's authority: opening a session in a workspace
//! takes a grant certificate the person certified for one agent, and the
//! door proves it holds that key by answering a fresh challenge (see
//! [`Endpoint::hosted`]). A session stays bound to its workspace and to that
//! certificate, which every later request re-checks. Local assistants can use
//! the owner-only Unix socket (`--connect`) instead.
//!
//! Only JSON responses are produced: no server-initiated stream is offered, so
//! `GET` answers 405 as the specification allows.

use crate::mcp::McpServer;
use crate::remote_door::PersonsDevices;
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
    collections::{HashMap, HashSet},
    io,
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tilo_actions::remote::ActionHost;
use tilo_user_identity::{DeviceCert, SignInProof, agent_endpoint_audience, canonical};

pub const ADDRESS_ENV: &str = "TILO_RUNTIME_MCP_HTTP";
pub const TOKEN_FILE_ENV: &str = "TILO_RUNTIME_MCP_HTTP_TOKEN_FILE";
pub const WORKSPACE_ENV: &str = "TILO_RUNTIME_MCP_WORKSPACE";
/// What [`WORKSPACE_ENV`] was called, still read: a service manager's unit
/// written before the rename sets it.
const LEGACY_WORKSPACE_ENV: &str = "TILO_RUNTIME_MCP_ROOM";
/// `required` serves no workspace to the bearer token alone; `optional` (the
/// default) serves the configured one at `/mcp`.
pub const GRANTS_ENV: &str = "TILO_RUNTIME_MCP_HTTP_GRANTS";
const SESSION_HEADER: &str = "mcp-session-id";
/// The challenge a caller answers to open a session, and the header it
/// answers with.
const GRANT_CHALLENGE_HEADER: &str = "tilo-grant-challenge";
const GRANT_HEADER: &str = "tilo-grant";
/// How long a caller has to answer a challenge it was given.
const GRANT_CHALLENGE_LIFETIME: Duration = Duration::from_secs(60);
/// How many unanswered challenges one endpoint holds; beyond it the oldest is
/// forgotten, so the store stays bounded however many callers ask.
const MAX_CHALLENGES: usize = 1024;
const MAX_BODY_BYTES: usize = crate::mcp_process::MAX_REQUEST_BYTES;
const MAX_SESSIONS: usize = 256;
const SESSION_IDLE: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_LABEL_CHARS: usize = 80;
const VIEWER_PREFIX: &str = "/viewer/invitations";
const WORKSPACE_PREFIX: &str = "/w/";

/// What the bearer token alone may reach.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Grants {
    /// The token reaches the configured workspace at `/mcp`, and a grant
    /// proof reaches any workspace this runtime serves at `/w/…`: the
    /// self-hosted agent host, where an assistant connects to its own machine.
    #[default]
    Optional,
    /// Only a grant proof reaches a workspace: `/mcp` answers 404, so a
    /// hosted deployment's token alone reaches nothing.
    Required,
}

/// Where to listen, which bearer token callers must present, and what that
/// token alone reaches.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub address: SocketAddr,
    pub token: Option<String>,
    /// Explicit host-selected audience; validated against current admission
    /// before the endpoint is created.
    pub workspace: Option<String>,
    pub grants: Grants,
}

impl Config {
    /// Read the listen address and required token file named by the
    /// environment. `None` when no address is configured.
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
        let workspace = std::env::var_os(WORKSPACE_ENV)
            .or_else(|| std::env::var_os(LEGACY_WORKSPACE_ENV))
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| invalid(format!("{WORKSPACE_ENV} must be a workspace UUID")))
            })
            .transpose()?;
        let grants = match std::env::var_os(GRANTS_ENV)
            .map(std::ffi::OsString::into_string)
            .transpose()
            .map_err(|_| invalid(format!("{GRANTS_ENV} must be `required` or `optional`")))?
            .as_deref()
            .map(str::trim)
        {
            None | Some("" | "optional") => Grants::Optional,
            Some("required") => Grants::Required,
            Some(_) => {
                return Err(invalid(format!(
                    "{GRANTS_ENV} must be `required` or `optional`"
                )));
            }
        };
        let config = Self {
            address,
            token,
            workspace,
            grants,
        };
        config.validate()?;
        Ok(Some(config))
    }

    /// # Errors
    ///
    /// Rejects an endpoint without a nonempty token, including on loopback,
    /// or a non-canonical selected workspace UUID.
    pub fn validate(&self) -> io::Result<()> {
        if self
            .workspace
            .as_deref()
            .is_some_and(|workspace| !is_canonical_workspace(workspace))
        {
            return Err(invalid(format!(
                "{WORKSPACE_ENV} must be a canonical non-nil workspace UUID"
            )));
        }
        if self
            .token
            .as_deref()
            .is_none_or(|token| token.trim().is_empty())
        {
            return Err(invalid(format!(
                "{ADDRESS_ENV} requires a nonempty {TOKEN_FILE_ENV}, including on loopback; local assistants can use `--connect`"
            )));
        }
        Ok(())
    }
}

/// The config as a log line may repeat it: where, and whether a credential
/// is held — never the credential. Callers print this endpoint at startup
/// into session and tool logs, so the derive that would print the token in
/// clear is deliberately not given to the struct.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("address", &self.address)
            .field("token", &self.token.as_ref().map(|_| REDACTED))
            .field("workspace", &self.workspace)
            .field("grants", &self.grants)
            .finish()
    }
}

/// What a credential reads as wherever a log or a diagnostic names it.
const REDACTED: &str = "[redacted]";

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
    retire: Option<Box<dyn FnOnce() + Send + Sync>>,
    /// Shared, not locked: one session's requests overlap, so a
    /// `tilo_status` answers while its `tilo_invoke` waits.
    server: Arc<McpServer<H>>,
    last_used: Instant,
    /// The workspace the session was opened in, and the grant certificate
    /// that opened it; `None` when the bearer token alone did, at `/mcp`.
    workspace: String,
    grant: Option<DeviceCert>,
}

impl<H> Drop for Session<H> {
    fn drop(&mut self) {
        if let Some(retire) = self.retire.take() {
            retire();
        }
    }
}

/// How a session for a workspace is made when one opens: the host checks that
/// workspace's admission then, so a workspace joined after the endpoint
/// started is served, and one it refuses is not remembered.
pub type SessionResolver<H> =
    Arc<dyn Fn(&str) -> io::Result<crate::mcp::SessionFactory<H>> + Send + Sync>;

/// The per-endpoint state shared by every connection.
pub struct Endpoint<H> {
    host: H,
    actor: String,
    /// The workspace `/mcp` serves to the bearer token alone (`None` under
    /// [`Grants::Required`]), and the person whose devices certify the grants
    /// that open `/w/…` sessions (`None` when this endpoint serves none).
    workspace: Option<String>,
    devices: Option<Arc<dyn PersonsDevices>>,
    token: Option<String>,
    /// How this host joins the workspace an invitation names.
    joiner: Option<crate::Joiner>,
    viewer: OnceLock<Arc<crate::viewer::Viewer>>,
    /// Challenges issued and not yet answered.
    challenges: Challenges,
    /// Workspaces a `/join` on this endpoint confirmed.
    joined: Mutex<HashSet<String>>,
    sessions: Mutex<HashMap<String, Session<H>>>,
    open_files: Arc<crate::mcp::OpenFiles>,
    /// Where sessions read the content actions produced.
    media: OnceLock<Arc<dyn crate::MediaReader>>,
    sessions_for: OnceLock<SessionResolver<H>>,
}

impl<H: ActionHost + Clone + Send + Sync + 'static> Endpoint<H> {
    /// One workspace, served at `/mcp` to `token` alone; no grant-opened
    /// sessions and no joining. [`Self::with_viewer`] without a
    /// partner-hosted viewer.
    #[must_use]
    pub fn new(host: H, workspace: String, actor: String, token: Option<String>) -> Arc<Self> {
        Self::hosted(host, actor, Some(workspace), token, None, None)
    }

    /// Also answer `/viewer/invitations` for a partner-hosted viewer.
    #[must_use]
    pub fn with_viewer(
        host: H,
        workspace: String,
        actor: String,
        token: Option<String>,
        viewer: Option<Arc<crate::viewer::Viewer>>,
    ) -> Arc<Self> {
        let endpoint = Self::new(host, workspace, actor, token);
        endpoint.serve_viewer(viewer);
        endpoint
    }

    /// Let `/viewer/invitations` be answered by a partner-hosted viewer. Set
    /// once, before the endpoint serves.
    pub fn serve_viewer(&self, viewer: Option<Arc<crate::viewer::Viewer>>) {
        if let Some(viewer) = viewer {
            let _ = self.viewer.set(viewer);
        }
    }

    /// One endpoint for every workspace this runtime holds: `/w/{workspace}/mcp`
    /// and `/w/{workspace}/join` are served whenever `devices` is given.
    ///
    /// A request that opens something — an `initialize` without a session id,
    /// or a `/join` — proves the caller holds one of `devices`' agent grants
    /// by answering a challenge this endpoint issues, so the bearer token
    /// alone carries no authority. `workspace` is what the bearer token alone
    /// reaches at `/mcp`: the configured workspace, or `None` in a hosted
    /// deployment; `joiner` joins the workspace an invitation names.
    #[must_use]
    pub fn hosted(
        host: H,
        actor: String,
        workspace: Option<String>,
        token: Option<String>,
        devices: Option<Arc<dyn PersonsDevices>>,
        joiner: Option<crate::Joiner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            host,
            actor,
            workspace,
            devices,
            token,
            joiner,
            viewer: OnceLock::new(),
            challenges: Challenges::default(),
            joined: Mutex::new(HashSet::new()),
            sessions: Mutex::new(HashMap::new()),
            open_files: Arc::default(),
            media: OnceLock::new(),
            sessions_for: OnceLock::new(),
        })
    }

    /// Let sessions answer with the content actions produced, read from
    /// `media`. Set once, before the endpoint serves.
    pub fn serve_media(&self, media: Option<Arc<dyn crate::MediaReader>>) {
        if let Some(media) = media {
            let _ = self.media.set(media);
        }
    }

    /// Install the host's own per-workspace session resolver before serving
    /// requests: every session's open asks it, so it decides then which
    /// workspaces are served.
    ///
    /// # Errors
    /// Rejects replacing an already installed resolver.
    pub fn serve_workspaces(&self, resolve: SessionResolver<H>) -> io::Result<()> {
        self.sessions_for
            .set(resolve)
            .map_err(|_| io::Error::other("MCP session resolver already set"))
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
        if (method, path) == (&Method::GET, "/health") {
            return json_response(StatusCode::OK, &json!({"ok":true}), None);
        }
        // The token proves the caller is whoever fronts this endpoint. Who it
        // acts for is a grant's to say, where a grant is needed.
        if !self.authorized(headers) {
            return plain(StatusCode::UNAUTHORIZED, "missing or wrong bearer token");
        }
        if path == "/mcp" {
            let Some(workspace) = self.workspace.clone() else {
                // A hosted deployment serves no workspace to the token alone.
                return plain(StatusCode::NOT_FOUND, "not found");
            };
            return match *method {
                Method::POST => self.post(&workspace, Proof::Bearer, headers, body).await,
                Method::DELETE => self.delete(&workspace, headers),
                _ => plain(StatusCode::METHOD_NOT_ALLOWED, "POST or DELETE"),
            };
        }
        match workspace_route(path) {
            Some(WorkspaceRoute::Mcp(workspace)) if self.devices.is_some() => {
                return match *method {
                    Method::POST => self.post(workspace, Proof::Grant, headers, body).await,
                    Method::DELETE => self.delete(workspace, headers),
                    _ => plain(StatusCode::METHOD_NOT_ALLOWED, "POST or DELETE"),
                };
            }
            Some(WorkspaceRoute::Join(workspace)) if self.devices.is_some() => {
                return match method {
                    &Method::POST => self.join(workspace, headers, body).await,
                    _ => plain(StatusCode::METHOD_NOT_ALLOWED, "POST"),
                };
            }
            Some(_) => return plain(StatusCode::NOT_FOUND, "not found"),
            None if path.starts_with(VIEWER_PREFIX) => {
                return self.viewer_route(method, path, headers, body).await;
            }
            None => {}
        }
        plain(StatusCode::NOT_FOUND, "not found")
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
        let Some(viewer) = self.viewer.get().cloned() else {
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
            ViewerCall::Renew(id) => viewer.renew(&id, &body, tilo_clock::unix_now_ms()),
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
        let Some(expected) = self
            .token
            .as_deref()
            .filter(|token| !token.trim().is_empty())
        else {
            return false;
        };
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|presented| constant_time_eq(presented.as_bytes(), expected.as_bytes()))
    }

    async fn post(
        self: &Arc<Self>,
        workspace: &str,
        proof: Proof,
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
            (None, true) => {
                let grant = match proof {
                    Proof::Bearer => None,
                    Proof::Grant => match self.grant(headers, workspace, false) {
                        Ok(grant) => Some(grant),
                        Err(response) => return response,
                    },
                };
                match self.create(workspace, grant, &request) {
                    Some((id, server)) => (id, server, true),
                    None => {
                        return plain(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "MCP session admission or capacity unavailable",
                        );
                    }
                }
            }
            (Some(id), _) => match self.find(workspace, &id) {
                Some(server) => (id, server, false),
                None => return plain(StatusCode::NOT_FOUND, "unknown session; initialize again"),
            },
            (None, false) => return plain(StatusCode::BAD_REQUEST, "missing Mcp-Session-Id"),
        };
        let reply = server.handle(request).await;
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

    /// `DELETE`: end a session, whichever workspace path it is named under.
    fn delete(&self, workspace: &str, headers: &hyper::HeaderMap) -> Response<Full<Bytes>> {
        match session_id(headers) {
            Some(id) if self.remove(workspace, &id) => plain(StatusCode::NO_CONTENT, ""),
            Some(_) => plain(StatusCode::NOT_FOUND, "unknown session"),
            None => plain(StatusCode::BAD_REQUEST, "missing Mcp-Session-Id"),
        }
    }

    /// `POST /w/{workspace}/join`: join the workspace `invitation` names,
    /// which must be the one the path names.
    async fn join(
        self: &Arc<Self>,
        workspace: &str,
        headers: &hyper::HeaderMap,
        body: &[u8],
    ) -> Response<Full<Bytes>> {
        if let Err(response) = self.grant(headers, workspace, true) {
            return response;
        }
        let Some(joiner) = self.joiner.clone() else {
            return plain(
                StatusCode::SERVICE_UNAVAILABLE,
                "this host joins workspaces through its own surfaces",
            );
        };
        let invitation = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|request| {
                request
                    .get("invitation")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        let Some(invitation) = invitation else {
            return text(
                StatusCode::BAD_REQUEST,
                "a join request is {\"invitation\": \"…\"}".to_owned(),
            );
        };
        match tokio::task::spawn_blocking(move || joiner(&invitation)).await {
            Ok(Ok(here)) if here == workspace => {
                if let Ok(mut served) = self.joined.lock() {
                    served.insert(here.clone());
                }
                json_response(StatusCode::OK, &json!({"workspace": here}), None)
            }
            Ok(Ok(here)) => text(
                StatusCode::FORBIDDEN,
                format!("the invitation is for the workspace {here}"),
            ),
            // Already joined here: the workspace is served either way.
            Ok(Err(_)) if self.serves(workspace) => {
                json_response(StatusCode::OK, &json!({"workspace": workspace}), None)
            }
            Ok(Err(error)) => text(StatusCode::FORBIDDEN, error.to_string()),
            Err(_) => plain(StatusCode::SERVICE_UNAVAILABLE, "the join was interrupted"),
        }
    }

    /// Whether this endpoint serves `workspace` now: a join here confirmed it,
    /// or the host's own resolver admits it.
    fn serves(&self, workspace: &str) -> bool {
        if self
            .joined
            .lock()
            .is_ok_and(|served| served.contains(workspace))
        {
            return true;
        }
        self.sessions_for
            .get()
            .is_some_and(|resolve| resolve(workspace).is_ok())
    }

    /// The grant certificate `headers` proves for `workspace`, or the response
    /// that refuses the request: a fresh challenge to answer (401) when the
    /// caller offered no proof at all.
    ///
    /// A challenge answers one request, whatever the outcome, and expires
    /// within [`GRANT_CHALLENGE_LIFETIME`].
    #[allow(
        clippy::result_large_err,
        reason = "the refusal is the response the caller is answered with"
    )]
    fn grant(
        &self,
        headers: &hyper::HeaderMap,
        workspace: &str,
        make: bool,
    ) -> Result<DeviceCert, Response<Full<Bytes>>> {
        let Some(devices) = self.devices.clone() else {
            return Err(plain(StatusCode::NOT_FOUND, "not found"));
        };
        let Some(presented) = header(headers, GRANT_HEADER) else {
            return Err(self.challenge());
        };
        let proof =
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, presented)
                .ok()
                .and_then(|bytes| canonical::from_canonical_bytes::<SignInProof>(&bytes).ok())
                .ok_or_else(|| {
                    text(
                        StatusCode::FORBIDDEN,
                        "tilo-grant holds no signed proof".to_owned(),
                    )
                })?;
        if !self.challenges.spend(&proof.challenge) {
            return Err(text(
                StatusCode::FORBIDDEN,
                "the proof answers no challenge this endpoint issued, or one that expired"
                    .to_owned(),
            ));
        }
        let now_ns = tilo_clock::unix_now_ns();
        proof
            .verify(
                &agent_endpoint_audience(workspace),
                &proof.challenge,
                &devices.revocations(),
                now_ns,
            )
            .map_err(|error| text(StatusCode::FORBIDDEN, error.to_string()))?;
        // The certificate must be one of this runtime's person's own devices,
        // current, and not revoked in their store; its key is what the proof
        // was signed with, so it is its own dialer.
        devices
            .admit(proof.cert.device_public_key, &proof.cert)
            .map_err(|refusal| text(StatusCode::FORBIDDEN, refusal.reason()))?;
        let Some(scope) = proof.cert.capabilities.agent else {
            return Err(text(
                StatusCode::FORBIDDEN,
                "the certificate is not an agent's grant".to_owned(),
            ));
        };
        if !scope.reaches(workspace) {
            return Err(text(
                StatusCode::FORBIDDEN,
                "the grant does not reach this workspace".to_owned(),
            ));
        }
        if make && !scope.makes_workspaces() {
            return Err(text(
                StatusCode::FORBIDDEN,
                "the grant may not make workspaces".to_owned(),
            ));
        }
        Ok(proof.cert)
    }

    /// 401 with a fresh challenge for the caller to answer with a proof.
    fn challenge(&self) -> Response<Full<Bytes>> {
        let Some(challenge) = self.challenges.issue() else {
            return plain(
                StatusCode::SERVICE_UNAVAILABLE,
                "the endpoint cannot draw a challenge",
            );
        };
        let mut response = text(
            StatusCode::UNAUTHORIZED,
            "a grant proof is required: answer tilo-grant-challenge with tilo-grant".to_owned(),
        );
        if let Ok(value) = HeaderValue::from_str(&hex::encode(challenge)) {
            response.headers_mut().insert(GRANT_CHALLENGE_HEADER, value);
        }
        response
    }

    fn create(
        &self,
        workspace: &str,
        grant: Option<DeviceCert>,
        initialize: &Value,
    ) -> Option<(String, Arc<McpServer<H>>)> {
        let id = uuid::Uuid::new_v4().to_string();
        let server = match self.sessions_for.get() {
            Some(resolve) => resolve(workspace).ok()?().ok()?,
            None => McpServer::new(self.host.clone(), workspace.to_owned(), self.actor.clone()),
        };
        if !server.audience_matches(workspace, &self.actor) {
            return None;
        }
        let server = server
            .with_agent_run(agent_run_id(&id), client_label(initialize))
            .with_open_files(Arc::clone(&self.open_files))
            .with_media(self.media.get().cloned());
        let caller = server.execution_caller();
        let host = self.host.clone();
        let server = Arc::new(server);
        let now = Instant::now();
        let session = Session {
            retire: Some(Box::new(move || host.retire_execution(&caller))),
            server: Arc::clone(&server),
            last_used: now,
            workspace: workspace.to_owned(),
            grant,
        };
        let admitted = {
            let mut sessions = self.sessions.lock().ok()?;
            sessions.retain(|_, session| now.duration_since(session.last_used) < SESSION_IDLE);
            let admitted = sessions.len() < MAX_SESSIONS;
            if admitted {
                sessions.insert(id.clone(), session);
            }
            admitted
        };
        admitted.then_some((id, server))
    }

    /// The live session `id` in `workspace`, its grant certificate re-checked.
    ///
    /// A session whose certificate no longer holds — expired, or revoked by
    /// the person — is dropped, so the caller is told to initialize again and
    /// is refused there.
    fn find(&self, workspace: &str, id: &str) -> Option<Arc<McpServer<H>>> {
        let mut sessions = self.sessions.lock().ok()?;
        // A session named under another workspace's path is not this path's
        // to end: it keeps serving its own workspace.
        if sessions
            .get(id)
            .is_none_or(|session| session.workspace != workspace)
        {
            return None;
        }
        let now = Instant::now();
        let live = sessions.get(id).is_some_and(|session| {
            now.duration_since(session.last_used) < SESSION_IDLE && self.grant_holds(session)
        });
        if !live {
            let stale = sessions.remove(id);
            drop(sessions);
            drop(stale);
            return None;
        }
        sessions.get_mut(id).map(|session| {
            session.last_used = now;
            Arc::clone(&session.server)
        })
    }

    /// Whether the grant certificate a session opened with still holds.
    fn grant_holds(&self, session: &Session<H>) -> bool {
        let Some(grant) = &session.grant else {
            return true;
        };
        self.devices
            .as_ref()
            .is_some_and(|devices| devices.admit(grant.device_public_key, grant).is_ok())
    }

    fn remove(&self, workspace: &str, id: &str) -> bool {
        self.sessions.lock().is_ok_and(|mut sessions| {
            sessions
                .get(id)
                .is_some_and(|session| session.workspace == workspace)
                && sessions.remove(id).is_some()
        })
    }
}

/// Whether an opening request must prove a grant.
#[derive(Clone, Copy)]
enum Proof {
    /// The bearer token is the caller's whole authority (`/mcp`).
    Bearer,
    /// A grant certificate the person certified opens the session (`/w/…`).
    Grant,
}

/// The grant challenges this endpoint issued and has not spent, with when
/// each was issued. Bounded, so callers cannot grow it without limit.
#[derive(Default)]
struct Challenges(Mutex<HashMap<[u8; 32], Instant>>);

impl Challenges {
    /// A fresh challenge: 32 random bytes, held until it is answered or
    /// expires.
    fn issue(&self) -> Option<[u8; 32]> {
        let mut challenge = [0_u8; 32];
        getrandom::fill(&mut challenge).ok()?;
        let now = Instant::now();
        let mut issued = self.0.lock().ok()?;
        issued.retain(|_, at| now.duration_since(*at) < GRANT_CHALLENGE_LIFETIME);
        if issued.len() >= MAX_CHALLENGES
            && let Some(oldest) = issued
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(challenge, _)| *challenge)
        {
            issued.remove(&oldest);
        }
        issued.insert(challenge, now);
        drop(issued);
        Some(challenge)
    }

    /// Whether `challenge` is one this endpoint issued and has not spent:
    /// answering it spends it, whatever the caller then proves.
    fn spend(&self, challenge: &[u8; 32]) -> bool {
        let Ok(mut issued) = self.0.lock() else {
            return false;
        };
        issued
            .remove(challenge)
            .is_some_and(|at| Instant::now().duration_since(at) < GRANT_CHALLENGE_LIFETIME)
    }
}

/// A `/w/{workspace}/…` route: the workspace it names, and what it serves.
enum WorkspaceRoute<'a> {
    Mcp(&'a str),
    Join(&'a str),
}

/// `/w/{workspace}/mcp` and `/w/{workspace}/join`, for a canonical workspace.
fn workspace_route(path: &str) -> Option<WorkspaceRoute<'_>> {
    let (workspace, tail) = path.strip_prefix(WORKSPACE_PREFIX)?.split_once('/')?;
    if !is_canonical_workspace(workspace) {
        return None;
    }
    match tail {
        "mcp" => Some(WorkspaceRoute::Mcp(workspace)),
        "join" => Some(WorkspaceRoute::Join(workspace)),
        _ => None,
    }
}

/// Whether `text` is a canonical (lower-case, hyphenated) non-nil UUID.
fn is_canonical_workspace(text: &str) -> bool {
    uuid::Uuid::parse_str(text).is_ok_and(|id| !id.is_nil() && id.to_string() == text)
}

/// One `/viewer/invitations` route, with its invitation id.
enum ViewerCall {
    Issue,
    Revoke(String),
    Role(String),
    Renew(String),
}

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
    header(headers, SESSION_HEADER).map(str::to_owned)
}

fn header<'a>(headers: &'a hyper::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
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
    text(status, message.to_owned())
}

fn text(status: StatusCode, message: String) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(message)));
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
pub(crate) mod tests;
