//! The API, `/v1`: JSON over HTTP, resource-oriented, as
//! `docs/developers/api.md` describes it.
//!
//! Every route is a call of the key's run ([`Session::call`]): the same tool
//! calls an agent makes over MCP, answered typed. There is no second
//! implementation underneath, only the mapping of routes to calls and of
//! typed answers to HTTP:
//!
//! - [`operations`]: every operation, the one table routing
//!   ([`routing`]) and the `OpenAPI` description ([`openapi`]) both read;
//! - [`answers`]: the types every answer goes out through, which the
//!   description's schemas are made from;
//! - [`errors`]: the one refusal shape, and its statuses;
//! - [`idempotency`]: retried mutating calls answer what the first did;
//! - [`events`] and [`webhooks`]: what happens, as a stream and posted.

pub mod answers;
pub mod errors;
pub mod events;
pub mod idempotency;
pub mod openapi;
pub mod operations;
pub mod routing;
pub mod webhooks;

use crate::session::{Call, Session};
use crate::store::Grant;
use crate::{Body, Reply, Server, now_ms};
use answers::typed;
use errors::ApiError;
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
/// The header a call names the run it is a step of in, and its title.
pub const RUN_HEADER: &str = "tilo-run";
pub const RUN_TITLE_HEADER: &str = "tilo-run-title";
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
    pub(crate) deliveries: webhooks::Deliveries,
    /// Each app's surface version per workspace, as last read.
    surfaces: std::sync::Mutex<HashMap<(String, String), String>>,
}

/// Whether `path` is the API's.
#[must_use]
pub fn serves(path: &str) -> bool {
    path == "/v1" || path.starts_with("/v1/")
}

/// Whether `method` on `path` uploads a file, which may be larger than other
/// bodies.
#[must_use]
pub fn uploads(method: &Method, path: &str) -> bool {
    routing::find(method, path)
        .is_ok_and(|(operation, _)| operation.body == operations::Body::Bytes)
}

/// The version of a surface: `<app>@<the first 16 hex of its SHA-256>`.
#[must_use]
pub fn surface_version(app: &str, surface: &str) -> String {
    let digest = hex::encode(Sha256::digest(surface.as_bytes()));
    format!("{app}@{}", digest.get(..16).unwrap_or(&digest))
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
    Stream(Body),
    Bytes {
        content_type: String,
        body: Vec<u8>,
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
                .next("docs/developers/api.md lists them; docs/api/openapi.json describes them"),
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
        return reply(caller.handle(operation).await);
    }
    let Some(key) = request
        .headers
        .get(idempotency::HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return reply(caller.handle(operation).await);
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
        Begun::Conflict(why) => refusal(&ApiError::new("idempotency_conflict", why)),
        Begun::Fresh => {
            let answered = caller.with_idempotency(key).handle(operation).await;
            // A pending answer is asked about again by a retry, and a refusal
            // the host may not repeat (busy, unreachable) is not kept.
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
                    ) =>
                {
                    Some(Stored {
                        status: refused.status.as_u16(),
                        body: refused.json(),
                        location: None,
                    })
                }
                _ => None,
            };
            server.v1.remembered.finish(&holder, key, kept);
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
    /// The run the call is a step of, as its headers name it.
    run: Option<Value>,
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
                .next("GET /v1/workspaces names the workspaces it reaches"));
            }
            if uuid::Uuid::parse_str(workspace).is_err() {
                return Err(ApiError::new("not_found", "no such workspace")
                    .next("GET /v1/workspaces names them by id"));
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
        let run = header(RUN_HEADER)
            .map(|id| json!({"id": id, "title": header(RUN_TITLE_HEADER).unwrap_or(id)}));
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
            run,
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
    /// account): its typed answer, a refusal when it says it failed.
    async fn call(&self, tool: &'static str, arguments: Value) -> Result<Value, ApiError> {
        let workspace = Some(self.workspace()).filter(|workspace| !workspace.is_empty());
        let answer = self
            .session
            .call(Call {
                workspace,
                tool,
                arguments,
                run: self.run.clone(),
            })
            .await?;
        let typed = typed_answer(&answer)?;
        if typed.get("status").and_then(Value::as_str) == Some("failed") {
            return Err(ApiError::of_answer(typed.get("error"), typed.get("action")));
        }
        Ok(typed)
    }

    /// An account action: its `result`.
    async fn account(&self, path: &str, args: Value) -> Result<Value, ApiError> {
        let answer = self
            .session
            .call(Call {
                workspace: None,
                tool: "tilo_do",
                arguments: json!({"path": path, "args": args}),
                run: self.run.clone(),
            })
            .await?;
        let typed = typed_answer(&answer)?;
        match typed.get("status").and_then(Value::as_str) {
            Some("failed") => Err(ApiError::of_answer(typed.get("error"), None)),
            _ => Ok(typed.get("result").cloned().unwrap_or_default()),
        }
    }

    async fn handle(self, operation: &Operation) -> Result<Answer, ApiError> {
        match operation.id {
            "getAccount" | "listWorkspaces" | "createWorkspace" | "deleteWorkspace"
            | "listKeys" | "mintKey" | "revokeKey" => self.account_operation(operation.id).await,
            "listResources" | "readResource" | "search" | "getLink" => {
                self.resource_operation(operation.id).await
            }
            "runAction" | "getRun" | "cancelRun" | "undoRun" | "runOutput" => {
                self.run_operation(operation.id).await
            }
            "listWebhooks" | "createWebhook" | "deleteWebhook" => {
                self.webhook_operation(operation.id)
            }
            other => self.workspace_operation(other).await,
        }
    }

    /// The account: its workspaces and keys.
    async fn account_operation(&self, id: &str) -> Result<Answer, ApiError> {
        match id {
            "getAccount" => {
                let read = self
                    .call("tilo_read", json!({"path": "account/usage"}))
                    .await?;
                let usage = read.get("fields").cloned().unwrap_or(read);
                shaped::<answers::Account>(usage).map(Answer::ok)
            }
            "listWorkspaces" => {
                // The person's list, as the key reaches it: one workspace for a
                // key for one, every one wherever it is hosted for the rest.
                let listed = self
                    .server
                    .relay
                    .workspaces(self.session.connection())
                    .await
                    .map_err(ApiError::from)?;
                shaped::<answers::WorkspaceList>(json!({"workspaces": listed})).map(Answer::ok)
            }
            "createWorkspace" => {
                let new: answers::NewWorkspace = self.json_body()?;
                // A retry asks the host for the workspace the first try made.
                let args = with(json!({"name": new.name}), "id", self.invocation.as_ref());
                let made = self.account("account/workspaces#create", args).await?;
                shaped::<answers::Workspace>(made.get("workspace").cloned().unwrap_or_default())
                    .map(Answer::created)
            }
            "deleteWorkspace" => {
                let args = json!({"workspace": self.workspace()});
                let deleted = self.account("account/workspaces#delete", args).await?;
                shaped::<answers::DeletedWorkspace>(deleted).map(Answer::ok)
            }
            "listKeys" => {
                let listed = self.account("account/keys#list", json!({})).await?;
                shaped::<answers::KeyList>(listed).map(Answer::ok)
            }
            "mintKey" => {
                let new: answers::NewKey = self.json_body()?;
                let args = serde_json::to_value(new).unwrap_or_default();
                let minted = self.account("account/keys#mint", args).await?;
                shaped::<answers::MintedKey>(minted).map(Answer::created)
            }
            _ => {
                let args = json!({"grant": self.params.get("grant")});
                let revoked = self.account("account/keys#revoke", args).await?;
                shaped::<answers::RevokedKey>(revoked).map(Answer::ok)
            }
        }
    }

    /// Reading the workspace: listings, resources, search and links.
    async fn resource_operation(&self, id: &str) -> Result<Answer, ApiError> {
        match id {
            "listResources" => {
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
            "readResource" => {
                let args = with(
                    json!({"path": self.params.get("path")}),
                    "cursor",
                    self.query("cursor"),
                );
                let read = self.call("tilo_read", args).await?;
                let version = read
                    .get("app")
                    .and_then(Value::as_str)
                    .zip(read.get("surface").and_then(Value::as_str))
                    .map(|(app, surface)| surface_version(app, surface));
                shaped::<answers::Resource>(with(read, "surface_version", version)).map(Answer::ok)
            }
            "search" => {
                let query = self
                    .query("q")
                    .ok_or_else(|| ApiError::new("invalid_arguments", "`q` is required"))?;
                let limit = self
                    .query("limit")
                    .and_then(|limit| limit.parse::<u64>().ok());
                let args = with(json!({"query": query}), "app", self.query("app"));
                let found = self.call("tilo_find", with(args, "limit", limit)).await?;
                shaped::<answers::SearchHits>(found).map(Answer::ok)
            }
            _ => {
                let path = self
                    .query("path")
                    .ok_or_else(|| ApiError::new("invalid_arguments", "`path` is required"))?;
                let args = with(json!({"path": path}), "mode", self.query("mode"));
                shaped::<answers::Link>(self.call("tilo_link", args).await?).map(Answer::ok)
            }
        }
    }

    /// Actions, and following, stopping and undoing their runs.
    async fn run_operation(&self, id: &str) -> Result<Answer, ApiError> {
        let ws = self.workspace();
        let run = self.params.get("run");
        match id {
            "runAction" => self.run_action().await,
            "getRun" => self.get_run().await,
            "cancelRun" => {
                let args = json!({"invocation_id": run, "cancel": true});
                action_answer(self.call("tilo_status", args).await?, &ws)
            }
            "undoRun" => {
                let status = self
                    .call("tilo_status", json!({"invocation_id": run}))
                    .await?;
                let resource = status
                    .get("path")
                    .and_then(Value::as_str)
                    .and_then(|path| path.split_once('#'))
                    .map(|(resource, _)| resource.to_owned())
                    .ok_or_else(|| {
                        ApiError::new(
                            "not_found",
                            "this run names no resource, so there is no step of its app to take back",
                        )
                    })?;
                let undo = json!({"path": format!("{resource}#undo"), "args": {"steps": 1}});
                action_answer(self.call("tilo_do", undo).await?, &ws)
            }
            _ => Ok(Answer::Stream(events::stream(
                events::Watch::new(Arc::clone(&self.session), ws, None, None),
                Some(run.to_owned()),
            ))),
        }
    }

    /// The workspace's people, programs, files and events.
    async fn workspace_operation(&self, id: &str) -> Result<Answer, ApiError> {
        let ws = self.workspace();
        match id {
            "listMembers" => {
                let listed = self
                    .call("tilo_do", json!({"path": "workspace/members#list"}))
                    .await?;
                shaped::<answers::Members>(listed.get("result").cloned().unwrap_or_default())
                    .map(Answer::ok)
            }
            "invitePerson" => {
                let new: answers::NewInvitation = self.json_body()?;
                let invite = json!({"path": "workspace/members#invite", "args": new});
                let invited = self.call("tilo_do", invite).await?;
                shaped::<answers::Invitation>(invited.get("result").cloned().unwrap_or_default())
                    .map(Answer::created)
            }
            "runProgram" => {
                let program: answers::Program = self.json_body()?;
                let answer = self
                    .session
                    .call(Call {
                        workspace: Some(ws),
                        tool: "tilo",
                        arguments: serde_json::to_value(program).unwrap_or_default(),
                        run: self.run.clone(),
                    })
                    .await?;
                shaped::<answers::ProgramAnswer>(typed_answer(&answer)?).map(Answer::ok)
            }
            "upload" => {
                let content = crate::content::put(&self.session, &ws, self.body)
                    .await
                    .map_err(|why| {
                        ApiError::new("unavailable", why).next("upload the file again")
                    })?;
                Ok(Answer::created(
                    json!({"content": content, "size": self.body.len()}),
                ))
            }
            "getContent" => self.content().await,
            "events" => {
                let from = self
                    .headers
                    .get("last-event-id")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let path = self.query("path").map(str::to_owned);
                Ok(Answer::Stream(events::stream(
                    events::Watch::new(Arc::clone(&self.session), ws, path, from),
                    None,
                )))
            }
            other => Err(ApiError::new("not_found", format!("{other} is not served"))),
        }
    }

    /// The key's webhooks in the workspace.
    fn webhook_operation(&self, id: &str) -> Result<Answer, ApiError> {
        let ws = self.workspace();
        match id {
            "listWebhooks" => {
                let hooks: Vec<Value> = self
                    .server
                    .store
                    .webhooks(Some((&self.grant.id, &ws)))
                    .into_iter()
                    .map(|hook| webhook_row(&hook, false))
                    .collect();
                Ok(Answer::ok(json!({"webhooks": hooks})))
            }
            "createWebhook" => self.create_webhook(),
            _ => {
                let id = self.params.get("webhook");
                let removed = self
                    .server
                    .store
                    .remove_webhook(&self.grant.id, &ws, id)
                    .map_err(|_| {
                        ApiError::new("unavailable", "the webhook could not be removed")
                    })?;
                if !removed {
                    return Err(ApiError::new(
                        "not_found",
                        "no such webhook of this key here",
                    ));
                }
                self.server.v1.deliveries.stop(id);
                Ok(Answer::ok(json!({"deleted": id})))
            }
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

    async fn get_run(&self) -> Result<Answer, ApiError> {
        let ws = self.workspace();
        let asked = json!({"invocation_id": self.params.get("run")});
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
        // The app is the path's first part, as the workspace names it.
        let app = self
            .params
            .get("path")
            .split('/')
            .next()
            .unwrap_or_default()
            .to_owned();
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
            return Ok(());
        };
        let published = surface_version(id_of_app, surface);
        crate::lock(&self.server.v1.surfaces).insert(id, published.clone());
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

    async fn content(&self) -> Result<Answer, ApiError> {
        use base64::Engine as _;
        let content = self.params.get("content");
        let mut bytes = Vec::new();
        loop {
            let piece = self
                .call(
                    "tilo_get",
                    json!({"content": content, "offset": bytes.len()}),
                )
                .await?;
            let decoded = piece
                .get("bytes")
                .and_then(Value::as_str)
                .and_then(|text| base64::engine::general_purpose::STANDARD.decode(text).ok())
                .ok_or_else(|| ApiError::malformed("a file", "no bytes"))?;
            let size = piece.get("size").and_then(Value::as_u64).unwrap_or(0);
            if decoded.is_empty() && (bytes.len() as u64) < size {
                return Err(ApiError::malformed(
                    "a file",
                    "an empty piece before its end",
                ));
            }
            bytes.extend(decoded);
            if bytes.len() as u64 >= size || bytes.len() >= crate::content::MAX_CONTENT_BYTES {
                return Ok(Answer::Bytes {
                    content_type: "application/octet-stream".to_owned(),
                    body: bytes,
                });
            }
        }
    }

    fn create_webhook(&self) -> Result<Answer, ApiError> {
        let new: answers::NewWebhook = self.json_body()?;
        if !webhooks::deliverable(&new.url) {
            return Err(ApiError::new(
                "invalid_arguments",
                "a webhook posts to an https URL (or plain http to this machine)",
            ));
        }
        let kinds = ["change", "run", "presence"];
        if let Some(unknown) = new
            .events
            .iter()
            .find(|kind| !kinds.contains(&kind.as_str()))
        {
            return Err(ApiError::new(
                "invalid_arguments",
                format!("{unknown} is no kind of event: change, run or presence"),
            ));
        }
        let (Ok(id), Ok(secret)) = (crate::store::secret(12), crate::store::secret(32)) else {
            return Err(ApiError::new("unavailable", "no randomness is available"));
        };
        let hook = crate::store::Webhook {
            id: format!("wh_{id}"),
            grant: self.grant.id.clone(),
            workspace: self.workspace(),
            url: new.url,
            events: new.events,
            path: new.path,
            secret,
            cursor: None,
        };
        self.server
            .store
            .add_webhook(hook.clone())
            .map_err(|_| ApiError::new("unavailable", "the webhook could not be kept"))?;
        let row = webhook_row(&hook, true);
        webhooks::Deliveries::start(self.server, hook);
        Ok(Answer::created(row))
    }
}

fn webhook_row(hook: &crate::store::Webhook, with_secret: bool) -> Value {
    let row = with(
        json!({"id": hook.id, "url": hook.url, "events": hook.events}),
        "path",
        hook.path.as_ref(),
    );
    with(row, "secret", with_secret.then_some(&hook.secret))
}

/// `object` with `name` set to `value` when there is one.
fn with(mut object: Value, name: &str, value: Option<impl serde::Serialize>) -> Value {
    if let (Some(fields), Some(value)) = (object.as_object_mut(), value) {
        fields.insert(name.to_owned(), json!(value));
    }
    object
}

/// A tool's typed answer (`structuredContent`), or its refusal when it gave
/// none.
fn typed_answer(answer: &Value) -> Result<Value, ApiError> {
    if let Some(typed) = answer.get("structuredContent") {
        return Ok(typed.clone());
    }
    let text = answer
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if answer.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(ApiError::new("failed", text));
    }
    serde_json::from_str(text).map_err(|_| ApiError::malformed("an answer", text))
}

/// `value` as `T`, or the refusal that it was not.
fn shaped<T: serde::de::DeserializeOwned + serde::Serialize>(
    value: Value,
) -> Result<Value, ApiError> {
    typed::<T>(value).map_err(|why| ApiError::malformed(std::any::type_name::<T>(), &why))
}

/// An action's answer as the API names it: `200` done, `202` pending with
/// where to follow it, a refusal when it failed.
fn action_answer(mut answer: Value, workspace: &str) -> Result<Answer, ApiError> {
    if let Some(fields) = answer.as_object_mut()
        && let Some(run) = fields.remove("invocation_id")
    {
        fields.insert("run".into(), run);
    }
    let pending = answer.get("status").and_then(Value::as_str) == Some("pending");
    let run = answer.get("run").and_then(Value::as_str).map(str::to_owned);
    let body = shaped::<answers::ActionAnswer>(answer)?;
    if pending {
        return Ok(Answer::Json {
            status: StatusCode::ACCEPTED,
            body,
            location: run.map(|run| format!("/v1/workspaces/{workspace}/runs/{run}")),
        });
    }
    Ok(Answer::ok(body))
}

fn reply(answered: Result<Answer, ApiError>) -> Reply {
    match answered {
        Ok(Answer::Json {
            status,
            body,
            location,
        }) => json_reply(status, &body, location.as_deref()),
        Ok(Answer::Stream(body)) => {
            let mut reply = hyper::Response::new(body);
            let headers = reply.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/event-stream"),
            );
            headers.insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            reply
        }
        Ok(Answer::Bytes { content_type, body }) => {
            let mut reply = hyper::Response::new(crate::body(body));
            if let Ok(value) = header::HeaderValue::from_str(&content_type) {
                reply.headers_mut().insert(header::CONTENT_TYPE, value);
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

fn refusal(refused: &ApiError) -> Reply {
    let mut reply = crate::json_reply(refused.status, &refused.json());
    if let Some(wait) = refused.body.retry_after_ms
        && let Ok(value) = header::HeaderValue::from_str(&wait.div_ceil(1000).to_string())
    {
        reply.headers_mut().insert(header::RETRY_AFTER, value);
    }
    reply
}
