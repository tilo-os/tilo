//! The API, `/v1`: JSON over HTTP, resource-oriented, as
//! `docs/developers/api.md` describes it.
//!
//! Every route is a typed call of the key's session in the workspace ([`Session::typed`]):
//! the same walk of the published tree and the same calls an agent makes over MCP, answered
//! typed, never as MCP. There is no second implementation underneath, only the mapping of
//! routes to calls and of typed results to HTTP:
//!
//! - [`operations`]: every operation, the one table routing
//!   ([`routing`]) and the `OpenAPI` description ([`openapi`]) both read;
//! - [`answers`]: the types every answer goes out through, which the
//!   description's schemas are made from;
//! - [`lines`]: the lines `tilo` prints for a listing of workspaces, keys and people;
//! - [`errors`]: the one refusal shape, and its statuses;
//! - [`idempotency`]: retried mutating calls answer what the first did;
//! - [`events`]: what happens, as a stream.

pub mod answers;
pub mod contents;
pub mod errors;
pub mod events;
pub mod idempotency;
pub mod lines;
mod model_asks;
pub mod names;
pub mod openapi;
pub mod operations;
pub mod routing;

use crate::session::Session;
use crate::store::Grant;
use crate::{Body, Reply, Server, now_ms};
use answers::typed;
use errors::ApiError;
use hyper::body::{Bytes, Frame};
use hyper::{HeaderMap, Method, StatusCode, header};
use idempotency::{Begun, Stored};
use operations::Operation;
use routing::{Params, Unmatched};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

/// The API's major version, which every answer names.
pub const API_VERSION: &str = "1";
/// The header every answer names the API's version in.
pub const VERSION_HEADER: &str = "tilo-api-version";
/// The header naming the invocation a call is a step of, and its title.
/// The code of a call that waits for its person to approve it on a device that
/// holds their identity: it is done once they allow it there.
pub const AWAITING_APPROVAL: &str = tilo_actions::remote::AWAITING_APPROVAL;
pub const INVOCATION_HEADER: &str = "tilo-invocation";
pub const INVOCATION_TITLE_HEADER: &str = "tilo-invocation-title";
/// The header a call made through generated types names their surface in.
pub const SURFACE_HEADER: &str = "tilo-surface";
/// How long an action is waited for when the caller does not say.
const DEFAULT_WAIT_SECONDS: u64 = 10;
/// The longest any call is held.
const MAX_WAIT_SECONDS: u64 = 60;

/// What the API keeps besides the server's store.
#[derive(Default)]
pub struct State {
    pub(crate) remembered: idempotency::Remembered,
    /// The content puts a key has yet to send every byte of: where each
    /// stands, so a connection that failed continues instead of starting again.
    pub(crate) contents: contents::Uploads,
    /// Each app's surface version per workspace, as last read.
    surfaces: std::sync::Mutex<HashMap<(String, String), String>>,
}

/// Whether `path` is the API's.
#[must_use]
pub fn serves(path: &str) -> bool {
    path == "/v1" || path.starts_with("/v1/")
}

/// Whether `method` on `path` carries a file's bytes, which may be larger than
/// other bodies.
#[must_use]
pub fn takes_bytes(method: &Method, path: &str) -> bool {
    routing::find(method, path)
        .is_ok_and(|(operation, _)| operation.body == operations::Body::Bytes)
}

/// The version of a surface: `<app>@<the first 16 hex of its SHA-256>`.
#[must_use]
pub fn surface_version(app: &str, surface: &str) -> String {
    let digest = hex::encode(Sha256::digest(surface.as_bytes()));
    format!("{app}@{}", digest.get(..16).unwrap_or(&digest))
}

/// Refuse a call made through types written for `sent` when the workspace publishes `published`.
fn changed(sent: &str, published: &str, workspace: &str) -> Result<(), ApiError> {
    if published == sent {
        return Ok(());
    }
    Err(ApiError::new(
        "surface_changed",
        format!("these types were made for {sent}; the workspace's app publishes {published}"),
    )
    .next(format!(
        "write the types again: tilo sdk gen -w {workspace}"
    )))
}

/// One request to the API.
pub struct Request<'a> {
    pub method: &'a Method,
    pub path: &'a str,
    pub query: &'a str,
    pub headers: &'a HeaderMap,
    pub body: &'a [u8],
}

/// What a handler answers.
enum Answer {
    Json {
        status: StatusCode,
        body: Value,
        location: Option<String>,
    },
    /// A body written as it is made, a piece at a time: server-sent events,
    /// or a JSON document whose arrays are filled as their members are read.
    Stream {
        content_type: &'static str,
        body: Body,
    },
    /// A file's bytes as they come: a piece fetched is a piece sent, so a
    /// file of hundreds of megabytes costs the server no more memory than
    /// one of its pieces.
    File { content_type: String, body: Body },
    /// Part of a file's bytes, from `range.0` to `range.1`, answered `206`.
    Partial {
        content_type: String,
        range: (u64, Option<u64>),
        size: u64,
        body: Body,
    },
}

impl Answer {
    const fn ok(body: Value) -> Self {
        Self::Json {
            status: StatusCode::OK,
            body,
            location: None,
        }
    }
    const fn created(body: Value) -> Self {
        Self::Json {
            status: StatusCode::CREATED,
            body,
            location: None,
        }
    }
}

/// Answer `request`, one of the API's.
pub async fn respond(server: &Arc<Server>, request: &Request<'_>) -> Reply {
    let mut reply = match routing::find(request.method, request.path) {
        Ok((operation, params)) => answer(server, operation, params, request).await,
        Err(Unmatched::NotFound) => refusal(
            &ApiError::new("not_found", "the API has no such route")
                .next("https://tilo.sh/docs/rest-api lists every route and what it takes"),
        ),
        Err(Unmatched::Method(allowed)) => {
            let mut reply = refusal(&ApiError::new(
                "method_not_allowed",
                format!("this route takes {}", allowed.join(", ")),
            ));
            if let Ok(value) = header::HeaderValue::from_str(&allowed.join(", ")) {
                reply.headers_mut().insert(header::ALLOW, value);
            }
            reply
        }
    };
    reply.headers_mut().insert(
        VERSION_HEADER,
        header::HeaderValue::from_static(API_VERSION),
    );
    reply
}

async fn answer(
    server: &Arc<Server>,
    operation: &'static Operation,
    params: Params,
    request: &Request<'_>,
) -> Reply {
    if operation.id == "getApi" {
        return reply(Ok(get_api(server, request.headers)));
    }
    let grant = match server.grant_of(request.headers) {
        Ok(grant) => grant,
        Err(denied) => return *denied,
    };
    let caller = match Caller::new(server, grant, params, request).await {
        Ok(caller) => caller,
        Err(refused) => return refusal(&refused),
    };
    if !operation.mutating {
        // A caller's answer is a large future: on the heap, not in the frames of
        // the request that awaits it.
        return reply(Box::pin(caller.handle(operation)).await);
    }
    let Some(key) = request
        .headers
        .get(idempotency::HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return reply(Box::pin(caller.handle(operation)).await);
    };
    if !idempotency::valid(key) {
        return refusal(&ApiError::invalid(
            "an Idempotency-Key is 1 to 255 printable characters",
        ));
    }
    let holder = caller.grant.id.clone();
    let digest = idempotency::request_digest(request.method.as_str(), request.path, request.body);
    match server.v1.remembered.begin(&holder, key, digest) {
        Begun::Replay(stored) => {
            let mut replayed = json_reply(
                StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK),
                &stored.body,
                stored.location.as_deref(),
            );
            replayed.headers_mut().insert(
                "idempotent-replayed",
                header::HeaderValue::from_static("true"),
            );
            replayed
        }
        // The caller gave up waiting on the first and asked again: it is busy, not refused, and
        // the same call answers the first's answer once it lands.
        Begun::Running => refusal(
            &ApiError::new(
                "unavailable",
                "a request with this Idempotency-Key is still running: ask again with the same key",
            )
            .retry_after_ms(RUNNING_RETRY_MS),
        ),
        Begun::Conflict(why) => refusal(&ApiError::new("idempotency_conflict", why)),
        Begun::Fresh => {
            let running = idempotency::Running::new(&server.v1.remembered, &holder, key);
            let answered = caller.with_idempotency(key).handle(operation).await;
            // A pending answer is asked about again by a retry, and a refusal
            // the same call may not meet again (busy, unreachable, or one the
            // service said to ask again about) is not kept: replaying it would
            // answer a retry with what asking again was meant to leave behind.
            // Nor is a branch that did not take everything: the same call
            // makes it once what it lacked is there.
            let kept = match &answered {
                Ok(Answer::Json {
                    status,
                    body,
                    location,
                }) if *status != StatusCode::ACCEPTED => Some(Stored {
                    status: status.as_u16(),
                    body: body.clone(),
                    location: location.clone(),
                }),
                Err(refused)
                    if !matches!(
                        refused.status,
                        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
                    ) && refused.body.code != crate::account::BRANCH_INCOMPLETE
                        && !asks_again(&refused.body.code) =>
                {
                    Some(Stored {
                        status: refused.status.as_u16(),
                        body: refused.json(),
                        location: None,
                    })
                }
                _ => None,
            };
            running.finish(kept);
            reply(answered)
        }
    }
}

/// `GET /v1`: needs no key; with one, says what it reaches.
fn get_api(server: &Arc<Server>, headers: &HeaderMap) -> Answer {
    let key = crate::bearer(headers)
        .and_then(|token| server.store.access(token, now_ms()).ok())
        .map(|grant| crate::account::grant_row(&grant));
    let info = with(
        json!({
            "api_version": API_VERSION,
            "server": format!("tilo-api {}", env!("CARGO_PKG_VERSION")),
        }),
        "key",
        key,
    );
    Answer::ok(typed::<answers::ApiInfo>(info).unwrap_or_default())
}

/// The key holder a request acts as, and what it asked.
struct Caller<'a> {
    server: &'a Arc<Server>,
    grant: Grant,
    session: Arc<Session>,
    params: Params,
    query: HashMap<String, String>,
    headers: &'a HeaderMap,
    body: &'a [u8],
    /// The invocation group the call is a step of, as its headers name it.
    group: Option<Value>,
    /// The invocation an idempotent action runs as.
    invocation: Option<String>,
}

impl<'a> Caller<'a> {
    async fn new(
        server: &'a Arc<Server>,
        grant: Grant,
        params: Params,
        request: &Request<'a>,
    ) -> Result<Self, ApiError> {
        let workspace = params.get("workspace");
        if !workspace.is_empty() {
            if !crate::oauth::scope_reaches(&grant.scope, Some(workspace)) {
                return Err(ApiError::new(
                    "beyond_scope",
                    "this key does not reach that workspace",
                )
                .next("account/workspaces#list names the workspaces it reaches"));
            }
            if uuid::Uuid::parse_str(workspace).is_err() {
                return Err(ApiError::new("not_found", "no such workspace")
                    .next("account/workspaces#list names them by id"));
            }
        }
        let session = server.api_session(&grant).await?;
        let header = |name: &str| {
            request
                .headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let group = header(INVOCATION_HEADER)
            .map(|id| json!({"id": id, "title": header(INVOCATION_TITLE_HEADER).unwrap_or(id)}));
        Ok(Self {
            server,
            grant,
            session,
            params,
            query: url::form_urlencoded::parse(request.query.as_bytes())
                .into_owned()
                .collect(),
            headers: request.headers,
            body: request.body,
            group,
            invocation: None,
        })
    }

    fn with_idempotency(mut self, key: &str) -> Self {
        self.invocation = Some(idempotency::invocation(&self.grant.id, key).to_string());
        self
    }

    fn workspace(&self) -> String {
        self.params.get("workspace").to_owned()
    }

    fn query(&self, name: &str) -> Option<&str> {
        self.query
            .get(name)
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }

    fn json_body<T: serde::de::DeserializeOwned>(&self) -> Result<T, ApiError> {
        let body = if self.body.is_empty() {
            b"{}" as &[u8]
        } else {
            self.body
        };
        serde_json::from_slice(body)
            .map_err(|error| ApiError::new("invalid_arguments", format!("the body: {error}")))
    }

    /// `tool` with `arguments`, in the route's workspace (`None` for the
    /// account): its typed result, or its refusal.
    async fn call(&self, tool: &'static str, arguments: Value) -> Result<Value, ApiError> {
        let workspace = self.workspace();
        let workspace = Some(workspace.as_str()).filter(|workspace| !workspace.is_empty());
        self.session
            .typed(workspace, tool, arguments, self.group.clone())
            .await
    }

    async fn handle(self, operation: &Operation) -> Result<Answer, ApiError> {
        // The account's routes reach what the account holds alone; a workspace's resources
        // are under its own routes, which admit the key for that workspace.
        if matches!(
            operation.id,
            "listAccountResources" | "readAccountResource" | "runAccountAction"
        ) && !Some(self.params.get("path"))
            .filter(|path| !path.is_empty())
            .or_else(|| self.query("path"))
            .is_none_or(|path| path == "account" || path.starts_with("account/"))
        {
            return Err(ApiError::new(
                "not_found",
                "the account holds only what is under `account/`",
            )
            .next("a workspace's resources are under /v1/workspaces/{workspace}/resources"));
        }
        match operation.id {
            "listResources"
            | "readResource"
            | "viewResource"
            | "listAccountResources"
            | "readAccountResource" => self.resource_operation(operation.id).await,
            "runAction" | "runAccountAction" | "getInvocation" | "cancelInvocation"
            | "undoInvocation" | "invocationOutput" => {
                self.invocation_operation(operation.id).await
            }
            "askModel" => model_asks::ask(&self.session, &self.workspace(), self.body).await,
            other => self.workspace_operation(other).await,
        }
    }

    /// Reading the workspace: listings, resources and views.
    async fn resource_operation(&self, id: &str) -> Result<Answer, ApiError> {
        match id {
            "listResources" | "listAccountResources" => {
                let limit = self
                    .query("limit")
                    .map(|limit| {
                        limit
                            .parse::<u64>()
                            .map_err(|_| ApiError::new("invalid_arguments", "`limit` is a number"))
                    })
                    .transpose()?;
                let mut args = json!({});
                for name in ["path", "cursor", "kind"] {
                    args = with(args, name, self.query(name));
                }
                let listed = self.call("tilo_ls", with(args, "limit", limit)).await?;
                shaped::<answers::Listing>(listed).map(Answer::ok)
            }
            "readResource" | "readAccountResource" => {
                self.check_surface(&self.workspace()).await?;
                let args = with(
                    json!({"path": self.params.get("path")}),
                    "cursor",
                    self.query("cursor"),
                );
                let read = answers::record_resource(
                    self.params.get("path"),
                    self.call("tilo_read", args).await?,
                );
                let version = read
                    .get("app")
                    .and_then(Value::as_str)
                    .zip(read.get("surface").and_then(Value::as_str))
                    .map(|(app, surface)| surface_version(app, surface));
                shaped::<answers::Resource>(with(read, "surface_version", version)).map(Answer::ok)
            }
            _ => {
                let path = self
                    .query("path")
                    .ok_or_else(|| ApiError::new("invalid_arguments", "`path` is required"))?;
                let picture = self
                    .query("picture")
                    .is_some_and(|picture| matches!(picture, "1" | "true" | "on" | "yes"));
                let scale = self
                    .query("scale")
                    .and_then(|scale| scale.parse::<f64>().ok());
                let args = with(
                    with(json!({"path": path}), "picture", Some(picture)),
                    "scale",
                    scale,
                );
                shaped::<answers::View>(self.call("tilo_view", args).await?).map(Answer::ok)
            }
        }
    }

    /// Actions, and following, stopping and undoing their invocations.
    async fn invocation_operation(&self, id: &str) -> Result<Answer, ApiError> {
        let ws = self.workspace();
        let invocation = self.params.get("invocation");
        match id {
            "runAction" | "runAccountAction" => self.run_action().await,
            "getInvocation" => self.get_invocation().await,
            "cancelInvocation" => {
                let args = json!({"invocation_id": invocation, "cancel": true});
                action_answer(self.call("tilo_status", args).await?, &ws)
            }
            // Exactly what the invocation changed, in every record, as the host takes one back.
            "undoInvocation" => {
                let args = json!({"invocation_id": invocation, "undo": true});
                action_answer(self.call("tilo_status", args).await?, &ws)
            }
            _ => Ok(Answer::Stream {
                content_type: "text/event-stream",
                body: events::stream(
                    events::Watch::new(Arc::clone(&self.session), ws, None, None),
                    Some(invocation.to_owned()),
                ),
            }),
        }
    }

    /// The workspace's people, programs, files and events.
    async fn workspace_operation(&self, id: &str) -> Result<Answer, ApiError> {
        let ws = self.workspace();
        match id {
            "runProgram" => {
                let program: answers::Program = self.json_body()?;
                let arguments = json!({ "program": program.program });
                let (mut typed, session) = self
                    .session
                    .run_program(
                        &ws,
                        program.session.as_deref(),
                        arguments,
                        self.group.clone(),
                    )
                    .await?;
                if let Some(fields) = typed.as_object_mut() {
                    // A program refused before it ran says why in the words a person reads.
                    if !fields.contains_key("text")
                        && let Some(why) = fields
                            .get("error")
                            .and_then(|error| error.get("message"))
                            .cloned()
                    {
                        fields.insert("text".into(), why);
                    }
                    fields.insert("session".into(), json!(session));
                }
                shaped::<answers::ProgramAnswer>(typed).map(Answer::ok)
            }
            "putContent" => {
                let hash = self.params.get("hash").to_owned();
                if !contents::address(&hash) {
                    return Err(contents::bad_address(&hash));
                }
                let (first, total) = match self
                    .headers
                    .get(header::CONTENT_RANGE)
                    .and_then(|value| value.to_str().ok())
                {
                    Some(value) => {
                        let (from, to, size) = contents::range(value).ok_or_else(|| {
                            ApiError::invalid("Content-Range is `bytes <from>-<to>/<size>`")
                        })?;
                        if to.saturating_sub(from).saturating_add(1) != self.body.len() as u64 {
                            return Err(ApiError::invalid(
                                "the body is not the piece Content-Range names",
                            ));
                        }
                        (from, size)
                    }
                    None => (0, self.body.len() as u64),
                };
                // A content put's future is large: on the heap, not in the frames
                // of the request that awaits it.
                let put = Box::pin(contents::put(
                    &self.server.v1.contents,
                    &self.session,
                    &self.grant.id,
                    &ws,
                    &hash,
                    first,
                    total,
                    self.body,
                ))
                .await?;
                match put {
                    contents::Put::More { offset, size } => {
                        shaped::<answers::PutContent>(json!({"offset": offset, "size": size}))
                            .map(Answer::ok)
                    }
                    contents::Put::Done { content, size } => shaped::<answers::PutContent>(
                        json!({"content": content, "offset": size, "size": size}),
                    )
                    .map(Answer::created),
                }
            }
            "getContent" => self.content().await,
            "events" => {
                let from = self
                    .headers
                    .get("last-event-id")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let path = self.query("path").map(str::to_owned);
                Ok(Answer::Stream {
                    content_type: "text/event-stream",
                    body: events::stream(
                        events::Watch::new(Arc::clone(&self.session), ws, path, from),
                        None,
                    ),
                })
            }
            other => Err(ApiError::new("not_found", format!("{other} is not served"))),
        }
    }

    async fn run_action(&self) -> Result<Answer, ApiError> {
        let ws = self.workspace();
        let path = format!("{}#{}", self.params.get("path"), self.params.get("action"));
        self.check_surface(&ws).await?;
        let args: Value = if self.body.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(self.body).map_err(|error| {
                ApiError::new("invalid_arguments", format!("the arguments: {error}"))
            })?
        };
        if !args.is_object() {
            return Err(ApiError::new(
                "invalid_arguments",
                "the arguments are a JSON object, `{}` for none",
            ));
        }
        let call = with(
            json!({"path": path, "args": args, "wait_ms": self.wait_ms(DEFAULT_WAIT_SECONDS)}),
            "invocation_id",
            self.invocation.as_ref(),
        );
        let answered = self.call("tilo_do", call).await?;
        action_answer(answered, &ws)
    }

    async fn get_invocation(&self) -> Result<Answer, ApiError> {
        let ws = self.workspace();
        let asked = json!({"invocation_id": self.params.get("invocation")});
        let now = std::time::Instant::now();
        let until = now
            .checked_add(std::time::Duration::from_millis(self.wait_ms(0)))
            .unwrap_or(now);
        let mut pause = std::time::Duration::from_millis(200);
        loop {
            let status = self.call("tilo_status", asked.clone()).await?;
            let pending = status.get("status").and_then(Value::as_str) == Some("pending");
            let next = std::time::Instant::now().checked_add(pause);
            if !pending || next.is_none_or(|next| next > until) {
                return action_answer(status, &ws);
            }
            tokio::time::sleep(pause).await;
            pause = pause
                .saturating_mul(2)
                .min(std::time::Duration::from_secs(1));
        }
    }

    /// How long the caller asks to be held: `Prefer: wait=<seconds>` or
    /// `?wait=`, else `default` seconds, never past [`MAX_WAIT_SECONDS`].
    fn wait_ms(&self, default: u64) -> u64 {
        let preferred = self
            .headers
            .get_all("prefer")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .find_map(|part| part.trim().strip_prefix("wait="))
            .or_else(|| self.query("wait"))
            .and_then(|seconds| seconds.trim().parse::<u64>().ok());
        preferred
            .unwrap_or(default)
            .min(MAX_WAIT_SECONDS)
            .saturating_mul(1000)
    }

    /// Refuse a call made through generated types for another surface of
    /// the app than the workspace publishes: `Tilo-Surface` names the app's id
    /// and the digest of the surface the types were made from.
    async fn check_surface(&self, workspace: &str) -> Result<(), ApiError> {
        let Some(sent) = self
            .headers
            .get(SURFACE_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
        else {
            return Ok(());
        };
        if !sent.contains('@') {
            return Err(ApiError::invalid("Tilo-Surface is <app>@<digest>"));
        }
        let path = self.params.get("path");
        if tilo_actions::remote::settings_resources::is_section(path) {
            // A section of the settings is typed by the surface the sections publish, whichever
            // app shows it: the workspace names it among the surfaces every holder declares.
            let app = tilo_actions::remote::settings_resources::SURFACE_APP;
            let published = collab_types::app_surface::bytes_of(app)
                .map(|kept| surface_version(app, &String::from_utf8_lossy(kept)))
                .unwrap_or_default();
            return changed(sent, &published, workspace);
        }
        // The app is the path's first part, as the workspace names it.
        let app = path.split('/').next().unwrap_or_default().to_owned();
        let id = (workspace.to_owned(), app.clone());
        let known = crate::lock(&self.server.v1.surfaces).get(&id).cloned();
        if known.as_deref() == Some(sent) {
            return Ok(());
        }
        let read = self.call("tilo_read", json!({"path": app})).await?;
        let (Some(id_of_app), Some(surface)) = (
            read.get("app").and_then(Value::as_str),
            read.get("surface").and_then(Value::as_str),
        ) else {
            // The types name a surface, and the workspace publishes none for
            // the app they act on: what they were written for is nowhere to
            // check this call against, so it is refused rather than run.
            return Err(ApiError::new(
                "surface_changed",
                format!(
                    "these types were made for {sent}, and the workspace's {app} publishes no surface: it is not installed there, or has not published yet"
                ),
            )
            .next(format!(
                "install and start {app} there, or write the types for this workspace: tilo sdk gen -w {workspace}"
            )));
        };
        let published = surface_version(id_of_app, surface);
        crate::lock(&self.server.v1.surfaces).insert(id, published.clone());
        changed(sent, &published, workspace)
    }

    /// The bytes of `content`, sent as each piece of them arrives: a file of
    /// any size streams through without being held whole. `Range:
    /// bytes=<from>-<to?>` asks for part of it, which a caller downloading a
    /// large file does a piece at a time.
    async fn content(&self) -> Result<Answer, ApiError> {
        let hash = self.params.get("hash").to_owned();
        let content = contents::reference(&hash).ok_or_else(|| contents::bad_address(&hash))?;
        let asked = self
            .headers
            .get(hyper::header::RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(range_of);
        let from = asked.map_or(0, |(from, _)| from);
        // The first piece is asked for now: content the workspace no longer
        // holds is refused as every other route refuses it, never answered
        // as a file of no bytes.
        let (size, mut first) = self.first_piece(&content, from).await?;
        let until = asked.and_then(|(_, until)| until);
        if let Some(end) = until {
            let wanted = end.saturating_add(1).saturating_sub(from);
            first.truncate(usize::try_from(wanted).unwrap_or(usize::MAX));
        }
        let sent = first.len() as u64;
        let (sender, pieces) = tokio::sync::mpsc::channel::<Bytes>(4);
        if !first.is_empty() {
            let _ = sender.try_send(Bytes::from(first));
        }
        let next = from.saturating_add(sent);
        if next < size && until.is_none_or(|end| next <= end) {
            let session = Arc::clone(&self.session);
            let workspace = self.workspace();
            tokio::spawn(stream_content(
                session,
                workspace,
                content.clone(),
                next,
                until,
                sender,
            ));
        }
        let stream = futures_util::stream::unfold(pieces, |mut pieces| async move {
            let piece = pieces.recv().await?;
            Some((
                Ok::<_, std::convert::Infallible>(Frame::data(piece)),
                pieces,
            ))
        });
        let body = http_body_util::BodyExt::boxed(http_body_util::StreamBody::new(stream));
        match asked {
            None => Ok(Answer::File {
                content_type: "application/octet-stream".to_owned(),
                body,
            }),
            Some((_, until)) => Ok(Answer::Partial {
                content_type: "application/octet-stream".to_owned(),
                range: (from, Some(until.unwrap_or_else(|| size.saturating_sub(1)))),
                size,
                body,
            }),
        }
    }

    /// The length of `content` and its bytes from `offset`: the first piece
    /// of a download, asked before any of it is answered.
    async fn first_piece(&self, content: &str, offset: u64) -> Result<(u64, Vec<u8>), ApiError> {
        use base64::Engine as _;
        let piece = self
            .call("tilo_get", json!({"content": content, "offset": offset}))
            .await?;
        let bytes = piece
            .get("bytes")
            .and_then(Value::as_str)
            .and_then(|text| base64::engine::general_purpose::STANDARD.decode(text).ok())
            .ok_or_else(|| ApiError::malformed("a file", "no bytes"))?;
        let size = piece.get("size").and_then(Value::as_u64).unwrap_or(0);
        Ok((size, bytes))
    }
}

/// `chunks` as a body, each frame sent as it comes.
fn chunked_body(chunks: tokio::sync::mpsc::Receiver<Bytes>) -> Body {
    let frames = futures_util::stream::unfold(chunks, |mut chunks| async move {
        chunks.recv().await.map(|bytes| {
            (
                Ok::<_, std::convert::Infallible>(Frame::data(bytes)),
                chunks,
            )
        })
    });
    http_body_util::BodyExt::boxed(http_body_util::StreamBody::new(frames))
}

/// `object` with `name` set to `value` when there is one.
fn with(mut object: Value, name: &str, value: Option<impl serde::Serialize>) -> Value {
    if let (Some(fields), Some(value)) = (object.as_object_mut(), value) {
        fields.insert(name.to_owned(), json!(value));
    }
    object
}

/// `value` as `T` answers it, or the refusal that it was not.
fn shaped<T: serde::de::DeserializeOwned + serde::Serialize>(
    value: Value,
) -> Result<Value, ApiError> {
    typed::<T>(value).map_err(|why| ApiError::malformed(std::any::type_name::<T>(), &why))
}

/// An action's answer as the API names it: `200` done, `202` pending with
/// where to follow it, a refusal when it failed.
/// Send `content`'s bytes into `pieces` as the workspace answers them, one
/// piece after another, until every byte is sent. A refusal ends the stream
/// with what is left unsent: a download that stops early is one the caller
/// sees, never a short file answered as a whole one.
async fn stream_content(
    session: Arc<Session>,
    workspace: String,
    content: String,
    from: u64,
    until: Option<u64>,
    pieces: tokio::sync::mpsc::Sender<Bytes>,
) {
    use base64::Engine as _;
    let mut offset = from;
    loop {
        let asked = json!({"content": content, "offset": offset});
        let in_workspace = Some(workspace.as_str()).filter(|workspace| !workspace.is_empty());
        let piece = match session.typed(in_workspace, "tilo_get", asked, None).await {
            Ok(piece) => piece,
            Err(error) => {
                tracing::warn!(why = %error.body.message, code = %error.body.code, %content, offset, "a file stopped part way");
                return;
            }
        };
        let mut decoded = piece
            .get("bytes")
            .and_then(Value::as_str)
            .and_then(|text| base64::engine::general_purpose::STANDARD.decode(text).ok())
            .unwrap_or_default();
        // A range ends where it was asked to, never with the rest of a piece.
        if let Some(end) = until {
            let wanted = end.saturating_add(1).saturating_sub(offset);
            decoded.truncate(usize::try_from(wanted).unwrap_or(usize::MAX));
        }
        let size = piece.get("size").and_then(Value::as_u64).unwrap_or(0);
        if decoded.is_empty() {
            if offset < size {
                tracing::warn!(%content, offset, size, "the workspace answered no bytes before its end");
            }
            return;
        }
        offset = offset.saturating_add(decoded.len() as u64);
        if pieces.send(Bytes::from(decoded)).await.is_err() {
            // The caller stopped reading.
            return;
        }
        if offset >= size || until.is_some_and(|end| offset > end) {
            return;
        }
    }
}

/// `Range: bytes=<from>-<to>` as the two ends a reader wants, the second one
/// inclusive like HTTP's; `None` for a range this server does not answer.
fn range_of(value: &str) -> Option<(u64, Option<u64>)> {
    let range = value.trim().strip_prefix("bytes=")?;
    let (from, to) = range.split_once('-')?;
    let from: u64 = from.trim().parse().ok()?;
    let to = to.trim();
    if to.is_empty() {
        return Some((from, None));
    }
    let to: u64 = to.parse().ok()?;
    (to >= from).then_some((from, Some(to)))
}

fn action_answer(mut answer: Value, workspace: &str) -> Result<Answer, ApiError> {
    if let Some(fields) = answer.as_object_mut()
        && let Some(invocation) = fields.remove("invocation_id")
    {
        fields.insert("invocation".into(), invocation);
    }
    let pending = answer.get("status").and_then(Value::as_str) == Some("pending");
    let invocation = answer
        .get("invocation")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let body = shaped::<answers::ActionAnswer>(answer)?;
    if pending {
        return Ok(Answer::Json {
            status: StatusCode::ACCEPTED,
            body,
            location: invocation
                .map(|invocation| format!("/v1/workspaces/{workspace}/invocations/{invocation}")),
        });
    }
    Ok(Answer::ok(body))
}

/// Whether a refusal's code tells the same call to ask again: the workspace was not ready
/// for what it changes yet. A retry is meant to reach the workspace, so the refusal is not
/// the answer remembered under its Idempotency-Key.
fn asks_again(code: &str) -> bool {
    matches!(code, "not_ready" | "unanswered")
}

fn reply(answered: Result<Answer, ApiError>) -> Reply {
    match answered {
        Ok(Answer::Json {
            status,
            body,
            location,
        }) => json_reply(status, &body, location.as_deref()),
        Ok(Answer::Stream { content_type, body }) => {
            let mut reply = hyper::Response::new(body);
            let headers = reply.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static(content_type),
            );
            headers.insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            reply
        }
        Ok(Answer::File { content_type, body }) => {
            let mut reply = hyper::Response::new(body);
            if let Ok(value) = header::HeaderValue::from_str(&content_type) {
                reply.headers_mut().insert(header::CONTENT_TYPE, value);
            }
            reply
        }
        Ok(Answer::Partial {
            content_type,
            range,
            size,
            body,
        }) => {
            let mut reply = hyper::Response::new(body);
            *reply.status_mut() = StatusCode::PARTIAL_CONTENT;
            let headers = reply.headers_mut();
            if let Ok(value) = header::HeaderValue::from_str(&content_type) {
                headers.insert(header::CONTENT_TYPE, value);
            }
            if let Ok(value) = header::HeaderValue::from_str(&format!(
                "bytes {}-{}/{size}",
                range.0,
                range.1.unwrap_or(range.0),
            )) {
                headers.insert(header::CONTENT_RANGE, value);
            }
            reply
        }
        Err(refused) => refusal(&refused),
    }
}

fn json_reply(status: StatusCode, body: &Value, location: Option<&str>) -> Reply {
    let mut reply = crate::json_reply(status, body);
    if let Some(location) =
        location.and_then(|location| header::HeaderValue::from_str(location).ok())
    {
        reply.headers_mut().insert(header::LOCATION, location);
    }
    reply
}

/// How long a retry that came while its first request still ran waits before asking again.
const RUNNING_RETRY_MS: u64 = 500;

pub(crate) fn refusal(refused: &ApiError) -> Reply {
    let mut reply = crate::json_reply(refused.status, &refused.json());
    if let Some(wait) = refused.body.retry_after_ms
        && let Ok(value) = header::HeaderValue::from_str(&wait.div_ceil(1000).to_string())
    {
        reply.headers_mut().insert(header::RETRY_AFTER, value);
    }
    reply
}
