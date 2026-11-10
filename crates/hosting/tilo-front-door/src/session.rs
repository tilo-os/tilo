//! One agent run through the door: an MCP session of the agent, served here
//! by the agent surface `tilo mcp` serves ([`tilo_runtime::mcp::McpServer`]),
//! its actions asked of the workspace's relay over the grant's own session
//! ([`crate::relay`], [`SessionActions`]). Record actions run on the relay's
//! app workers, with no tab of the person's open.
//!
//! - **Pinned** (a one-workspace grant): the session serves that one
//!   workspace. It cannot name another: the relay admits the grant nowhere
//!   else, and the door offers no tool that lists or makes workspaces.
//! - **Account** (an all-workspaces grant): the session offers the
//!   workspace tools with one more argument, `workspace`. A call naming a
//!   workspace is served in it, entering it the first time; a call naming
//!   none acts on the account itself, which the door answers:
//!   `account/workspaces#create`, `account/keys#mint` and the rest
//!   ([`crate::account`]).
//!
//! The run's name is the person's: the client's name and a number of their
//! own runs ("Claude Code #2"), which History groups the run's actions under,
//! and which the workspace shows present, doing what it does, while the
//! session lives (`mcp_presence`, as `tilo mcp` shows its agent).

#[path = "../../../system/tilo-cli/src/mcp_presence.rs"]
#[allow(
    dead_code,
    unreachable_pub,
    reason = "the presence `tilo mcp` says of its agent, the same source here"
)]
mod mcp_presence;

use crate::relay::{Connection, Relay, RelayError};
use crate::store::Scope;
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};
use tilo_browser_peer::session_actions::SessionActions;
use tilo_runtime::mcp::{McpServer, Surface};

/// The header naming an MCP session (Streamable HTTP).
pub const SESSION_HEADER: &str = "mcp-session-id";
/// The argument naming the workspace an account session's call acts in.
pub const WORKSPACE_ARGUMENT: &str = "workspace";
const PROTOCOL_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];
const MAX_NAME_CHARS: usize = 80;

/// What a request to a session answers: JSON-RPC messages, the last of them
/// the reply. More than one only when the door tells the client something
/// first (that its tools changed), which needs an event stream.
pub struct Reply {
    pub messages: Vec<Value>,
    /// 202 for a notification, which answers nothing.
    pub status: u16,
}

impl Reply {
    fn one(message: Value) -> Self {
        Self {
            messages: vec![message],
            status: 200,
        }
    }
    const fn accepted() -> Self {
        Self {
            messages: Vec::new(),
            status: 202,
        }
    }
}

/// The run in one workspace: the agent surface over the grant's session
/// there, and the agent present in it.
struct Served {
    server: McpServer<SessionActions>,
    presence: Arc<mcp_presence::Presence>,
}

impl Drop for Served {
    fn drop(&mut self) {
        // The agent leaves with the run: nothing of it is drawn after this.
        self.presence.leave();
    }
}

pub struct Session {
    pub grant: String,
    pub scope: Scope,
    /// The run's name, as History shows it.
    pub label: String,
    /// The run's id, which every invocation of the session belongs to.
    run: String,
    /// The person, as their workspaces name them.
    person: String,
    relay: Arc<Relay>,
    /// The grant's session with the relay.
    connection: Arc<Connection>,
    /// For a minted grant, the session of the grant that minted it, which
    /// asks for its invitation.
    parent: Option<Arc<Connection>>,
    /// The door, which answers the account's own actions.
    door: std::sync::Weak<crate::Door>,
    served: tokio::sync::Mutex<HashMap<String, Arc<Served>>>,
    /// The workspace tools, once a workspace served them.
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
        scope: Scope,
        label: String,
        relay: Arc<Relay>,
        connection: Connection,
        parent: Option<Connection>,
        door: std::sync::Weak<crate::Door>,
    ) -> Self {
        Self {
            grant,
            scope,
            label,
            run: uuid::Uuid::new_v4().simple().to_string(),
            person: collab_types::Principal::of_person(connection.key.cert().user_public_key)
                .id()
                .to_owned(),
            relay,
            connection: Arc::new(connection),
            parent: parent.map(Arc::new),
            door,
            served: tokio::sync::Mutex::default(),
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

    fn touch(&self) {
        *self
            .last_used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    }

    /// The grant's session with the relay, for the account's own actions.
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    /// The run in `workspace`, entering it the first time.
    async fn served(&self, workspace: &str) -> Result<Arc<Served>, RelayError> {
        let mut runs = self.served.lock().await;
        if let Some(known) = runs.get(workspace) {
            return Ok(Arc::clone(known));
        }
        let id = self
            .relay
            .enter(&self.connection, self.parent.as_deref(), workspace)
            .await?;
        let peer = &self.connection.peer;
        let actions = SessionActions::new(peer, id, self.connection.host_cert.device_public_key);
        // The person's desktops see the agent present from the moment it
        // enters the workspace, not from its first action, and each action it
        // takes in a record as it goes.
        let presence = mcp_presence::Presence::new(
            peer,
            actions.clone(),
            self.person.clone(),
            (workspace.to_owned(), id),
            self.label.clone(),
        );
        presence.arrive();
        let narrating = Arc::clone(&presence);
        let server = McpServer::new(actions, workspace.to_owned(), self.person.clone())
            .with_surface(Surface::Agent)
            .with_agent_run(&self.run, self.label.clone())
            .with_narration(Arc::new(move |acting| narrating.acting(&acting)));
        let run = Arc::new(Served { server, presence });
        runs.insert(workspace.to_owned(), Arc::clone(&run));
        drop(runs);
        Ok(run)
    }

    /// Answer `request` in `workspace`'s run, showing the person what a
    /// tool call did beside the agent.
    async fn serve(&self, workspace: &str, request: Value) -> Reply {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let run = match self.served(workspace).await {
            Ok(run) => run,
            Err(RelayError::NotFound) => {
                return Reply::one(error(&id, -32602, "no such workspace"));
            }
            Err(other) => return Reply::one(error(&id, -32000, &other.to_string())),
        };
        let call = (request.get("method").and_then(Value::as_str) == Some("tools/call"))
            .then(|| request.clone());
        let Some(answer) = run.server.handle(request).await else {
            return Reply::accepted();
        };
        if let Some(call) = call {
            let failed = answer.pointer("/result/isError") == Some(&Value::Bool(true))
                || answer.get("error").is_some();
            let acted = run.server.acted();
            let shown = Arc::clone(&run);
            tokio::spawn(async move { shown.presence.called(&call, failed, acted).await });
        }
        Reply::one(answer)
    }

    /// Answer the client's `initialize`. A pinned session enters its one
    /// workspace now, so a workspace that cannot be reached fails the
    /// connect.
    pub(crate) async fn initialize(&self, request: &Value) -> Result<Value, String> {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        if let Some(workspace) = self.pinned().map(str::to_owned) {
            let reply = self.serve(&workspace, request.clone()).await;
            let answer = reply
                .messages
                .into_iter()
                .last()
                .ok_or("the workspace answered nothing")?;
            if let Some(error) = answer.get("error") {
                return Err(error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the workspace refused")
                    .to_owned());
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
        self.touch();
        if let Some(workspace) = self.pinned().map(str::to_owned) {
            return self.serve(&workspace, request).await;
        }
        let id = request.get("id").cloned();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(id) = id else {
            // Notifications concern the door's own session.
            return Reply::accepted();
        };
        match method {
            "ping" => Reply::one(result(&id, &json!({}))),
            "tools/list" => Reply::one(result(&id, &json!({"tools": self.account_tools()}))),
            "tools/call" => self.account_call(&id, request).await,
            "resources/list" => Reply::one(result(&id, &json!({"resources": []}))),
            _ => Reply::one(error(&id, -32601, "Method not found")),
        }
    }

    /// The workspace tools, each with `workspace`; before a workspace served
    /// them, the ones the account answers.
    fn account_tools(&self) -> Vec<Value> {
        self.known_tools()
            .map_or_else(account_tool_definitions, |known| {
                known.iter().map(with_workspace_argument).collect()
            })
    }

    fn known_tools(&self) -> Option<Vec<Value>> {
        self.tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Learn the workspace tools from `workspace`'s run.
    async fn learn_tools(&self, workspace: &str) -> Option<Vec<Value>> {
        let listed = self
            .serve(
                workspace,
                json!({"jsonrpc":"2.0","id":"tilo-door-tools","method":"tools/list"}),
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
        let reply = self.serve(&workspace, request).await;
        if self.known_tools().is_none() {
            self.learn_tools(&workspace).await;
        }
        reply
    }

    /// Answer a call on the account. The first workspace made is where the
    /// workspace tools come from: the client is told its tools changed, so
    /// it lists them again.
    async fn account(&self, id: &Value, name: &str, arguments: &Value, structured: bool) -> Reply {
        let Some(door) = self.door.upgrade() else {
            return Reply::one(error(id, -32000, "the door is shutting down"));
        };
        let answered = door
            .account_call(&self.grant, self, name, arguments, structured)
            .await;
        let created = answered
            .pointer("/structuredContent/result/workspace/id")
            .or_else(|| answered.pointer("/structuredContent/result/workspace"))
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

    /// Call the workspace tool `name` with `arguments` in `workspace`, in
    /// this run: the tool's result, or why there is none.
    pub(crate) async fn call_in(
        &self,
        workspace: &str,
        name: &str,
        arguments: Value,
    ) -> Result<Value, String> {
        self.touch();
        let reply = self
            .serve(
                workspace,
                json!({"jsonrpc":"2.0","id":"tilo-door-call","method":"tools/call",
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

    /// End the run: the agent leaves every workspace and the grant's session
    /// with the relay closes.
    pub(crate) async fn close(&self) {
        self.served.lock().await.clear();
        self.connection.peer.close();
        if let Some(parent) = &self.parent {
            parent.peer.close();
        }
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

/// The tools an account session offers before a workspace served the
/// workspace tools: the ones the account itself answers.
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
