//! One agent run through the door: an MCP session of the agent, carried to
//! the `tilo-runtime` endpoint of the workspace it acts in.
//!
//! - **Pinned** (a one-workspace grant): every message goes to that one
//!   workspace's endpoint, unchanged but for the run's name. The session
//!   cannot name another workspace: the endpoint it reaches serves only its
//!   own, and the door offers no tool that lists or makes workspaces.
//! - **Account** (an all-workspaces grant): the door answers
//!   `list_workspaces` and `create_workspace` itself, and offers the
//!   workspace endpoint's tools with one more required argument,
//!   `workspace`. A call is carried to that workspace's endpoint, opening a
//!   session there the first time, so each workspace attributes what the run
//!   did to the run.
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
    fmt::Write as _,
    sync::{Arc, Mutex},
    time::Instant,
};

pub use crate::endpoint::SESSION_HEADER;
pub const LIST_WORKSPACES: &str = "list_workspaces";
pub const CREATE_WORKSPACE: &str = "create_workspace";
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

fn tool_text(text: &str, structured: &Value, is_error: bool) -> Value {
    json!({
        "content": [{"type":"text","text":text}],
        "structuredContent": structured,
        "isError": is_error,
    })
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
                "instructions": "You act in the person's Tilo workspaces. `list_workspaces` names them; \
                    `create_workspace` makes one. Every other tool acts in one workspace, which its \
                    `workspace` argument names by id.",
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

    /// The account tools, then the workspace tools each with `workspace`.
    async fn account_tools(&self) -> Vec<Value> {
        let mut tools = account_tool_definitions();
        if let Some(known) = self.workspace_tools().await {
            tools.extend(known.iter().map(with_workspace_argument));
        }
        tools
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
        match name.as_str() {
            LIST_WORKSPACES => return Reply::one(result(id, &self.list().await)),
            CREATE_WORKSPACE => return self.create(id, &arguments).await,
            _ => {}
        }
        let Some(workspace) = arguments
            .get(WORKSPACE_ARGUMENT)
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Reply::one(error(
                id,
                -32602,
                "name the workspace this acts in: `workspace`, an id as list_workspaces answers it",
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

    async fn list(&self) -> Value {
        let (workspaces, hosts) = match (
            self.directory.workspaces(&self.key).await,
            self.directory.hosts(&self.key).await,
        ) {
            (Ok(workspaces), Ok(hosts)) => (workspaces, hosts),
            (Err(error), _) | (_, Err(error)) => {
                return tool_text(
                    &format!("Could not list workspaces: {error}"),
                    &json!({}),
                    true,
                );
            }
        };
        let mut text = if workspaces.is_empty() {
            "You have no workspaces yet; create_workspace makes one.".to_owned()
        } else {
            let mut text = String::from("Your workspaces:");
            for workspace in &workspaces {
                let _ = write!(
                    text,
                    "\n- {} (id {}, on {})",
                    workspace.name, workspace.id, workspace.host
                );
            }
            text
        };
        if !hosts.is_empty() {
            let names: Vec<&str> = hosts.iter().map(|host| host.name.as_str()).collect();
            let _ = write!(text, "\nHosts you can create on: {}", names.join(", "));
        }
        tool_text(
            &text,
            &json!({"workspaces": workspaces, "hosts": hosts}),
            false,
        )
    }

    async fn create(&self, id: &Value, arguments: &Value) -> Reply {
        let Some(name) = arguments.get("name").and_then(Value::as_str) else {
            return Reply::one(error(id, -32602, "`name` is required"));
        };
        let host = arguments.get("host").and_then(Value::as_str);
        let created = match self.directory.create(&self.key, name, host).await {
            Ok(created) => created,
            Err(error) => {
                return Reply::one(result(
                    id,
                    &tool_text(
                        &format!("The workspace was not created: {error}"),
                        &json!({}),
                        true,
                    ),
                ));
            }
        };
        let answer = result(
            id,
            &tool_text(
                &format!(
                    "Created {} (id {}) on {}. Name it as `workspace` to act in it.",
                    created.name, created.id, created.host
                ),
                &json!({"workspace": created}),
                false,
            ),
        );
        // The first workspace is where the workspace tools come from: tell the
        // client its tools changed, so it lists them again.
        if self.known_tools().is_none() && self.learn_tools(&created.id).await.is_some() {
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

/// The tools only an account session has.
#[must_use]
pub fn account_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": LIST_WORKSPACES,
            "title": "List workspaces",
            "description": "Your Tilo workspaces, each with its id and the host serving it, and the hosts you can create a workspace on.",
            "inputSchema": {"type":"object","properties":{},"additionalProperties":false},
            "annotations": {"readOnlyHint": true},
        }),
        json!({
            "name": CREATE_WORKSPACE,
            "title": "Create workspace",
            "description": "Create a workspace. The host may refuse (quota, payment, policy). Answers the new workspace's id, which other tools take as `workspace`.",
            "inputSchema": {
                "type":"object",
                "properties": {
                    "name": {"type":"string","description":"The workspace's name, 1 to 80 printable characters."},
                    "host": {"type":"string","description":"The host to create it on, as list_workspaces names hosts. The default host when omitted."},
                },
                "required": ["name"],
                "additionalProperties": false,
            },
        }),
    ]
}

/// `tool` with the required `workspace` argument an account session adds.
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
            json!({"type":"string","description":"The workspace this acts in: its id, as list_workspaces answers it."}),
        );
    }
    let required = schema
        .entry("required")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(required) = required.as_array_mut()
        && !required.iter().any(|name| name == WORKSPACE_ARGUMENT)
    {
        required.insert(0, json!(WORKSPACE_ARGUMENT));
    }
    tool
}
