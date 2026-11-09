//! One agent run through the door: an MCP session of the agent, carried to
//! the `tilo-runtime` endpoint of the workspace it acts in.
//!
//! - **Pinned** (a one-workspace grant): every message goes to that one
//!   workspace's endpoint, unchanged but for the run's name. The session
//!   cannot name another workspace: the endpoint it reaches serves only its
//!   own, and the door offers no tool that lists or makes workspaces.
//! - **Account** (an all-workspaces grant): the session offers the
//!   workspace endpoint's tools with one more argument, `workspace`. A call
//!   naming a workspace is carried to that workspace's endpoint, opening a
//!   session there the first time, so each workspace attributes what the run
//!   did to the run. A call naming none acts on the account itself, which the
//!   door answers: `account/workspaces#create`, `account/keys#mint` and the
//!   rest ([`crate::account`]).
//!
//! The run's name is the person's: the client's name and a number of their
//! own runs ("Claude Code #2"), which the door hands the endpoint as the
//! client's declared name, the name `tilo-runtime` gives the run in History.
//!
//! Every session at a runtime is opened as the grant's key
//! ([`crate::endpoint`]), which the runtime checks and binds the session to:
//! once the grant's certificate no longer holds there (revoked by the
//! person, or expired), the runtime drops the session and opening another
//! is refused.

use crate::directory::{Directory, DirectoryError, Route};
use crate::endpoint;
use crate::grant_key::GrantKey;
use crate::store::Scope;
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

pub use crate::endpoint::SESSION_HEADER;
/// The argument naming the workspace an account session's call acts in.
pub const WORKSPACE_ARGUMENT: &str = "workspace";
const PROTOCOL_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];
const MAX_NAME_CHARS: usize = 80;

/// What a request to a session answers: JSON-RPC messages, the last of them
/// the reply. More than one only when the door tells the client something
/// first (that its tools changed), which needs an event stream.
pub struct Reply {
    pub messages: Vec<Value>,
    /// The upstream's HTTP status, when the reply is the upstream's own
    /// non-JSON answer (202 for a notification).
    pub status: u16,
}

impl Reply {
    fn one(message: Value) -> Self {
        Self {
            messages: vec![message],
            status: 200,
        }
    }
    fn accepted() -> Self {
        Self {
            messages: Vec::new(),
            status: 202,
        }
    }
}

/// An upstream session: the workspace endpoint and the session it gave.
#[derive(Clone)]
struct Upstream {
    route: Route,
    url: String,
    session: String,
}

pub struct Session {
    pub grant: String,
    /// What the session acts as.
    key: GrantKey,
    pub scope: Scope,
    /// The run's name, as History shows it.
    pub label: String,
    /// The client's `initialize` params, with the run's name.
    initialize: Value,
    directory: Arc<dyn Directory>,
    /// The door, which answers the account's own actions.
    door: std::sync::Weak<crate::Door>,
    http: tilo_net::Client,
    upstreams: tokio::sync::Mutex<HashMap<String, Upstream>>,
    /// The workspace endpoint's tools, once one answered.
    tools: Mutex<Option<Vec<Value>>>,
    pub(crate) last_used: Mutex<Instant>,
}

/// A JSON-RPC error reply.
#[must_use]
pub fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

fn result(id: &Value, value: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":value})
}

/// The run's name: the name the client registered under (or declared), and
/// the person's run number.
#[must_use]
pub fn run_label(registered: &str, initialize: &Value, number: u64) -> String {
    let declared = initialize
        .pointer("/clientInfo/title")
        .or_else(|| initialize.pointer("/clientInfo/name"))
        .and_then(Value::as_str);
    let name = [Some(registered), declared]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|name| !name.is_empty() && *name != "MCP agent")
        .unwrap_or("Agent");
    let name: String = name
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    format!("{name} #{number}")
}

impl Session {
    pub(crate) fn new(
        grant: String,
        key: GrantKey,
        scope: Scope,
        label: String,
        mut initialize: Value,
        directory: Arc<dyn Directory>,
        door: std::sync::Weak<crate::Door>,
        http: tilo_net::Client,
    ) -> Self {
        if let Some(params) = initialize.as_object_mut() {
            let info = params.entry("clientInfo").or_insert_with(|| json!({}));
            if let Some(info) = info.as_object_mut() {
                info.insert("name".into(), Value::String(label.clone()));
                info.remove("title");
                info.entry("version").or_insert_with(|| json!("1"));
            }
        }
        Self {
            grant,
            key,
            scope,
            label,
            initialize,
            directory,
            door,
            http,
            upstreams: tokio::sync::Mutex::default(),
            tools: Mutex::default(),
            last_used: Mutex::new(Instant::now()),
        }
    }

    fn pinned(&self) -> Option<&str> {
        match &self.scope {
            Scope::Workspace(workspace) => Some(workspace),
            Scope::Account => None,
        }
    }

    /// Answer the client's `initialize`. A pinned session opens its one
    /// upstream session now and answers with what the workspace endpoint
    /// answered, so a workspace that cannot be reached fails the connect.
    pub(crate) async fn initialize(&self, request: &Value) -> Result<Value, String> {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        if let Some(workspace) = self.pinned().map(str::to_owned) {
            let (_, answer) = self
                .open(&workspace)
                .await
                .map_err(|error| error.to_string())?;
            let mut answer = answer.unwrap_or_else(|| json!({}));
            if let Some(object) = answer.as_object_mut() {
                object.insert("id".into(), id);
            }
            return Ok(answer);
        }
        let asked = request
            .pointer("/params/protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let version = PROTOCOL_VERSIONS
            .into_iter()
            .find(|version| *version == asked)
            .unwrap_or("2025-06-18");
        Ok(result(
            &id,
            &json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": true}},
                "serverInfo": {"name": "tilo", "title": "Tilo", "version": env!("CARGO_PKG_VERSION")},
                "instructions": ACCOUNT_INSTRUCTIONS,
            }),
        ))
    }

    /// Answer one message after `initialize`.
    pub(crate) async fn handle(&self, request: Value) -> Reply {
        *self
            .last_used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
        if let Some(workspace) = self.pinned().map(str::to_owned) {
            return self.forward(&workspace, &request).await;
        }
        let id = request.get("id").cloned();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(id) = id else {
            // Notifications concern the door's own session: the upstream
            // sessions were told `initialized` as they opened.
            return Reply::accepted();
        };
        match method {
            "ping" => Reply::one(result(&id, &json!({}))),
            "tools/list" => Reply::one(result(&id, &json!({"tools": self.account_tools().await}))),
            "tools/call" => self.account_call(&id, request).await,
            "resources/list" => Reply::one(result(&id, &json!({"resources": []}))),
            _ => Reply::one(error(&id, -32601, "Method not found")),
        }
    }

    /// The workspace tools, each with `workspace`; before the person has a
    /// workspace, the ones the account answers.
    async fn account_tools(&self) -> Vec<Value> {
        match self.workspace_tools().await {
            Some(known) => known.iter().map(with_workspace_argument).collect(),
            None => account_tool_definitions(),
        }
    }

    /// The workspace endpoint's tools: as one of the person's workspaces
    /// answers them, asked once per session.
    async fn workspace_tools(&self) -> Option<Vec<Value>> {
        if let Some(known) = self.known_tools() {
            return Some(known);
        }
        let first = self
            .directory
            .workspaces(&self.key)
            .await
            .ok()?
            .into_iter()
            .next()?;
        self.learn_tools(&first.id).await
    }

    fn known_tools(&self) -> Option<Vec<Value>> {
        self.tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    async fn learn_tools(&self, workspace: &str) -> Option<Vec<Value>> {
        let listed = self
            .forward(
                workspace,
                &json!({"jsonrpc":"2.0","id":"tilo-door-tools","method":"tools/list"}),
            )
            .await;
        let tools = listed
            .messages
            .last()?
            .pointer("/result/tools")?
            .as_array()?
            .clone();
        *self
            .tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tools.clone());
        Some(tools)
    }

    async fn account_call(&self, id: &Value, mut request: Value) -> Reply {
        let name = request
            .pointer("/params/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let arguments = request
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let Some(workspace) = arguments
            .get(WORKSPACE_ARGUMENT)
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            if crate::account::addresses(&name, &arguments) {
                let structured = request
                    .pointer("/params/_meta/so.tilo.mcp/structured")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                return self.account(id, &name, &arguments, structured).await;
            }
            return Reply::one(error(
                id,
                -32602,
                "name the workspace this acts in: `workspace`, an id as account/workspaces#list \
                 answers it; only paths under `account` need none",
            ));
        };
        if let Some(arguments) = request
            .pointer_mut("/params/arguments")
            .and_then(Value::as_object_mut)
        {
            arguments.remove(WORKSPACE_ARGUMENT);
        }
        self.forward(&workspace, &request).await
    }

    /// Answer a call on the account. The first workspace made is where the
    /// workspace tools come from: the client is told its tools changed, so
    /// it lists them again.
    async fn account(&self, id: &Value, name: &str, arguments: &Value, structured: bool) -> Reply {
        let Some(door) = self.door.upgrade() else {
            return Reply::one(error(id, -32000, "the door is shutting down"));
        };
        let answered = door
            .account_call(&self.grant, &self.key, name, arguments, structured)
            .await;
        let created = answered
            .pointer("/structuredContent/result/workspace/id")
            .and_then(Value::as_str)
            .filter(|_| name == "tilo_do")
            .map(str::to_owned);
        let answer = result(id, &answered);
        if let Some(created) = created
            && self.known_tools().is_none()
            && self.learn_tools(&created).await.is_some()
        {
            return Reply {
                messages: vec![
                    json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}),
                    answer,
                ],
                status: 200,
            };
        }
        Reply::one(answer)
    }

    /// Call the workspace runtime's tool `name` with `arguments` in
    /// `workspace`, in this run: the tool's result, or why there is none.
    pub(crate) async fn call_in(
        &self,
        workspace: &str,
        name: &str,
        arguments: Value,
    ) -> Result<Value, String> {
        *self
            .last_used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
        let reply = self
            .forward(
                workspace,
                &json!({"jsonrpc":"2.0","id":"tilo-door-call","method":"tools/call",
                        "params":{"name":name,"arguments":arguments}}),
            )
            .await;
        let message = reply
            .messages
            .last()
            .ok_or("the workspace answered nothing")?;
        if let Some(error) = message.get("error") {
            return Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the workspace refused")
                .to_owned());
        }
        Ok(message.get("result").cloned().unwrap_or_default())
    }

    /// Carry `request` to `workspace`'s endpoint, opening the session there
    /// first, and opening it again once if the endpoint forgot it.
    async fn forward(&self, workspace: &str, request: &Value) -> Reply {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        for attempt in 0..2 {
            let upstream = match self.upstream(workspace, attempt > 0).await {
                Ok(upstream) => upstream,
                Err(DirectoryError::NotFound) => {
                    return Reply::one(error(&id, -32602, "no such workspace"));
                }
                Err(other) => return Reply::one(error(&id, -32000, &other.to_string())),
            };
            match self
                .post(
                    &upstream.route,
                    &upstream.url,
                    Some(&upstream.session),
                    request,
                )
                .await
            {
                Ok((404, _, _)) if attempt == 0 => {}
                Ok((200, _, Some(body))) => return Reply::one(body),
                Ok((status, _, _)) if (200..300).contains(&status) => {
                    return Reply {
                        messages: Vec::new(),
                        status,
                    };
                }
                Ok((status, _, _)) => {
                    return Reply::one(error(
                        &id,
                        -32000,
                        &format!("the workspace's endpoint answered HTTP {status}"),
                    ));
                }
                Err(message) => return Reply::one(error(&id, -32000, &message)),
            }
        }
        Reply::one(error(
            &id,
            -32000,
            "the workspace's endpoint keeps forgetting the session",
        ))
    }

    async fn upstream(&self, workspace: &str, reopen: bool) -> Result<Upstream, DirectoryError> {
        if !reopen && let Some(known) = self.upstreams.lock().await.get(workspace).cloned() {
            return Ok(known);
        }
        self.open(workspace).await.map(|(upstream, _)| upstream)
    }

    /// Open a session at `workspace`'s endpoint as the grant's key, which the
    /// runtime checks. Answers the endpoint's `initialize` reply too.
    async fn open(&self, workspace: &str) -> Result<(Upstream, Option<Value>), DirectoryError> {
        let mut upstreams = self.upstreams.lock().await;
        let route = self.directory.route(&self.key, workspace).await?;
        let url = route.mcp(workspace);
        let initialize = json!({
            "jsonrpc":"2.0", "id":"tilo-door-initialize", "method":"initialize",
            "params": self.initialize,
        });
        let (status, headers, body) = endpoint::post(
            &self.http,
            &route,
            &url,
            None,
            Some((&self.key, workspace)),
            &initialize,
        )
        .await
        .map_err(DirectoryError::Unavailable)?;
        let session = headers
            .get(SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .filter(|_| status == 200)
            .map(str::to_owned)
            .ok_or_else(|| match status {
                401 | 403 => {
                    DirectoryError::Refused("the workspace's runtime refused this grant".into())
                }
                _ => DirectoryError::Unavailable(format!(
                    "the workspace's endpoint answered HTTP {status}"
                )),
            })?;
        let upstream = Upstream {
            route,
            url,
            session,
        };
        let _ = self
            .post(
                &upstream.route,
                &upstream.url,
                Some(&upstream.session),
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            )
            .await;
        let replaced = upstreams.insert(workspace.to_owned(), upstream.clone());
        drop(upstreams);
        if let Some(replaced) = replaced {
            self.close_one(replaced).await;
        }
        Ok((upstream, body))
    }

    async fn post(
        &self,
        route: &Route,
        url: &str,
        session: Option<&str>,
        body: &Value,
    ) -> Result<endpoint::Answer, String> {
        endpoint::post(&self.http, route, url, session, None, body).await
    }

    /// End every upstream session: the run is over.
    pub(crate) async fn close(&self) {
        let upstreams: Vec<Upstream> = self
            .upstreams
            .lock()
            .await
            .drain()
            .map(|(_, up)| up)
            .collect();
        for upstream in upstreams {
            self.close_one(upstream).await;
        }
    }

    async fn close_one(&self, upstream: Upstream) {
        endpoint::close(
            &self.http,
            &upstream.route,
            &upstream.url,
            &upstream.session,
        )
        .await;
    }
}

/// What an account session is told first: how to reach the account and its
/// workspaces, and how a product gives each customer's agent a workspace of
/// its own.
pub const ACCOUNT_INSTRUCTIONS: &str = "You act in the person's Tilo workspaces. \
Paths under `account` manage them and need no `workspace`: `tilo_ls account` lists its actions; \
`tilo_do {path: \"account/workspaces#list\"}` names the workspaces, \
`account/workspaces#create {name}` makes one. Every other call acts in one workspace, \
named by id in its `workspace` argument. To give another agent one workspace and nothing else \
(one workspace per customer), `tilo_do {path: \"account/keys#mint\", args: {workspace}}` answers \
an `mcp_url` and a `token` for it; revoking this key ends every key it minted.";

/// The tools an account session offers before the person has a workspace
/// whose endpoint names them: the ones the account itself answers.
#[must_use]
pub fn account_tool_definitions() -> Vec<Value> {
    let path = json!({"type":"string","description":"A path under `account`, like account/workspaces#create."});
    vec![
        json!({
            "name": "tilo_ls",
            "description": "List what is under a path: `account` lists the account's actions.",
            "inputSchema": {"type":"object","properties":{"path":path},"additionalProperties":false},
            "annotations": {"readOnlyHint": true},
        }),
        json!({
            "name": "tilo_read",
            "description": "Read `account/workspaces` or `account/keys`.",
            "inputSchema": {"type":"object","properties":{"path":path},"required":["path"],"additionalProperties":false},
            "annotations": {"readOnlyHint": true},
        }),
        json!({
            "name": "tilo_do",
            "description": "Run an account action: `{path: \"account/workspaces#create\", args: {name: \"Applicant: Jane Doe\"}}`. Answers `ok` with the result, `failed: why`, or `pending <id>` for tilo_status.",
            "inputSchema": {"type":"object","properties":{"path":path,"args":{"type":"object"}},"required":["path"],"additionalProperties":false},
        }),
        json!({
            "name": "tilo_status",
            "description": "How a pending invocation went.",
            "inputSchema": {"type":"object","properties":{"invocation_id":{"type":"string"}},"required":["invocation_id"],"additionalProperties":false},
        }),
    ]
}

/// `tool` with the `workspace` argument an account session adds: required
/// but for paths under `account`.
fn with_workspace_argument(tool: &Value) -> Value {
    let mut tool = tool.clone();
    let Some(schema) = tool.get_mut("inputSchema").and_then(Value::as_object_mut) else {
        return tool;
    };
    let properties = schema
        .entry("properties")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(properties) = properties.as_object_mut() {
        properties.insert(
            WORKSPACE_ARGUMENT.into(),
            json!({"type":"string","description":"The workspace this acts in: its id, as account/workspaces#list answers it. Omit it for paths under `account`."}),
        );
    }
    tool
}
