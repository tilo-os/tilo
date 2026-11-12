//! One agent run through the door: an MCP session of the agent, served here
//! by the agent surface `tilo mcp` serves ([`tilo_runtime::mcp::McpServer`]),
//! its actions asked of the workspace's relay over the grant's own session
//! ([`crate::relay`], [`SessionActions`]). Record actions run on the relay's
//! app workers, and each record the agent makes or changes opens on the
//! workspace's desk as a window of the person's, naming the run, so whoever
//! enters the workspace sees what the agent did.
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
//! and which the workspace's host shows present, doing what it does, while
//! the session lives: the host is the run's one voice, so the door says
//! nothing of it itself, and opens the records the run works in as windows
//! (`mcp_windows`, as `tilo mcp` does).

pub mod members;

use crate::relay::{Connection, Relay, RelayError};
use crate::store::Scope;
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};
use tilo_runtime::mcp::{McpServer, Surface};
use tilo_runtime::mcp_windows;
use tilo_session_core::remote::SessionActions;

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
    /// The windows of the records the run works in.
    windows: Arc<mcp_windows::AgentWindows>,
}

/// A run in a workspace one of the person's devices hosts, entered at that
/// device's agent door: the device serves the whole run, its people and its
/// windows included, and shows the agent there itself.
struct OnDevice(tilo_runtime::enter::Entered);

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
    /// The door, which answers the account's own actions.
    door: std::sync::Weak<crate::Door>,
    served: tokio::sync::Mutex<HashMap<String, Arc<Served>>>,
    /// The runs in workspaces the person's devices host, by workspace.
    on_devices: tokio::sync::Mutex<HashMap<String, Arc<OnDevice>>>,
    /// The workspace tools, once a workspace served them.
    tools: Mutex<Option<Vec<Value>>>,
    pub(crate) last_used: Mutex<Instant>,
}

/// A JSON-RPC error reply.
#[must_use]
pub fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

/// The JSON-RPC error of a call the relay ran nothing for because it is busy
/// with the person's changes: `data` says so typed, with the wait, so a
/// client retries the same call then.
fn rate_limited(id: &Value, retry_after_ms: u64) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{
        "code": -32000,
        "message": RelayError::RateLimited { retry_after_ms }.to_string(),
        "data": {"code": crate::account::RATE_LIMITED, "retry_after_ms": retry_after_ms},
    }})
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
        connection: Arc<Connection>,
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
            connection,
            door,
            served: tokio::sync::Mutex::default(),
            on_devices: tokio::sync::Mutex::default(),
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
        let id = self.relay.enter(&self.connection, workspace).await?;
        let device = &self.connection.device;
        let actions = device.actions(id);
        // The records the run works in open as windows on the workspace's
        // desk; the host that executes the run shows it there.
        let windows = Arc::new(mcp_windows::AgentWindows::new(
            actions.clone(),
            self.person.clone(),
            workspace.to_owned(),
        ));
        let server = McpServer::new(actions, workspace.to_owned(), self.person.clone())
            .with_surface(Surface::Agent)
            .with_agent_run(&self.run, self.label.clone());
        let run = Arc::new(Served { server, windows });
        runs.insert(workspace.to_owned(), Arc::clone(&run));
        drop(runs);
        Ok(run)
    }

    /// Answer `request` in `workspace`'s run, showing the person what a
    /// tool call did beside the agent. The workspace's people
    /// (`workspace/members`) are answered here ([`members`]).
    async fn serve(&self, workspace: &str, request: Value) -> Reply {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        match self.on_device(workspace).await {
            Ok(Some(run)) => {
                return run
                    .0
                    .handle(request)
                    .await
                    .map_or_else(Reply::accepted, Reply::one);
            }
            Ok(None) => {}
            Err(refused) => return Reply::one(error(&id, -32000, &refused)),
        }
        if request.get("method").and_then(Value::as_str) == Some("tools/call") {
            let tool = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = request.pointer("/params/arguments").unwrap_or(&Value::Null);
            if members::addresses(tool, arguments) {
                let structured = request
                    .pointer("/params/_meta/so.tilo.mcp/structured")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let answered = self.members(workspace, tool, arguments, structured).await;
                return Reply::one(result(&id, &answered));
            }
        }
        let run = match self.served(workspace).await {
            Ok(run) => run,
            Err(RelayError::NotFound) => {
                return Reply::one(error(&id, -32602, "no such workspace"));
            }
            Err(RelayError::RateLimited { retry_after_ms }) => {
                return Reply::one(rate_limited(&id, retry_after_ms));
            }
            Err(other) => return Reply::one(error(&id, -32000, &other.to_string())),
        };
        let call = (request.get("method").and_then(Value::as_str) == Some("tools/call"))
            .then(|| request.clone());
        let Some(answer) = run.server.handle(request).await else {
            return Reply::accepted();
        };
        if let Some(call) = call {
            let (changed, step) = (run.server.take_changed(), run.server.run_of(&call));
            let windows = Arc::clone(&run.windows);
            tokio::spawn(async move { windows.changed(changed, step).await });
        }
        Reply::one(answer)
    }

    /// The run in `workspace` when one of the person's devices hosts it, as
    /// their list says, entering it at that device's agent door the first
    /// time (and again once the device closed it); `None` for a workspace the
    /// relay keeps.
    async fn on_device(&self, workspace: &str) -> Result<Option<Arc<OnDevice>>, String> {
        // A workspace the run already works in on the relay is the relay's.
        if self.served.lock().await.contains_key(workspace) {
            return Ok(None);
        }
        let mut runs = self.on_devices.lock().await;
        if let Some(known) = runs.get(workspace).filter(|known| known.0.is_live()) {
            return Ok(Some(Arc::clone(known)));
        }
        let Some(address) = self.device_address(workspace).await else {
            return Ok(None);
        };
        let endpoint = self
            .connection
            .endpoint()
            .await
            .map_err(|error| error.to_string())?;
        let entered = tilo_runtime::enter::enter(
            &endpoint,
            &self.connection.entering_key(),
            workspace,
            &address,
            &self.label,
        )
        .await?;
        let run = Arc::new(OnDevice(entered));
        runs.insert(workspace.to_owned(), Arc::clone(&run));
        drop(runs);
        Ok(Some(run))
    }

    /// Where `workspace` is hosted when one of the person's devices hosts it,
    /// as their list says (a key for one workspace reads that one's).
    async fn device_address(
        &self,
        workspace: &str,
    ) -> Option<tilo_relay_session::workspaces::WorkspaceHostAddress> {
        let address = self
            .relay
            .workspaces(&self.connection)
            .await
            .ok()?
            .into_iter()
            .find(|listed| listed.id == workspace)?
            .address?;
        let host = address.address().ok()?;
        (host.mode
            == tilo_delivery_protocol::workspace_join::WorkspaceHostMode::NativeInteractiveWorkspaceOwner)
            .then_some(address)
    }

    /// Answer the client's `initialize`. A pinned session enters its one
    /// workspace now, so a workspace that cannot be reached fails the
    /// connect, with the JSON-RPC error it failed with.
    pub(crate) async fn initialize(&self, request: &Value) -> Result<Value, Value> {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        if let Some(workspace) = self.pinned().map(str::to_owned) {
            let reply = self.serve(&workspace, request.clone()).await;
            let answer = reply
                .messages
                .into_iter()
                .last()
                .ok_or_else(|| error(&id, -32000, "the workspace answered nothing"))?;
            if answer.get("error").is_some() {
                return Err(answer);
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
            if let (Some(id), Some("tools/call")) = (
                request.get("id"),
                request.get("method").and_then(Value::as_str),
            ) && let Some(refused) = crate::account::beyond_workspace(
                request
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                request.pointer("/params/arguments").unwrap_or(&Value::Null),
                request
                    .pointer("/params/_meta/so.tilo.mcp/structured")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ) {
                return Reply::one(result(id, &refused));
            }
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

    /// End the run: the agent leaves every workspace. The grant's session
    /// with the relay, which its other runs share, ends with the last of
    /// them, or when the grant is revoked.
    pub(crate) async fn close(&self) {
        self.served.lock().await.clear();
        self.on_devices.lock().await.clear();
    }
}

/// What an account session is told first: how to reach the account and its
/// workspaces, and how a product gives each customer's agent a workspace of
/// its own.
pub const ACCOUNT_INSTRUCTIONS: &str = "You act in the person's Tilo workspaces. \
Paths under `account` manage them and need no `workspace`: `tilo_ls account` lists its actions; \
`tilo_do {path: \"account/workspaces#list\"}` names the workspaces, \
each with the part the person takes in it (`owner` for one of their own, else the role they were \
invited with), \
`account/workspaces#create {name}` makes one and `account/workspaces#delete {workspace}` deletes one \
of the person's own for good, with everything in it and every key minted for it (no undo; the call \
is the confirmation, so delete only what you were asked to). Every other call acts in one workspace, \
named by id in its `workspace` argument. To give another agent one workspace and nothing else \
(one workspace per customer), `tilo_do {path: \"account/keys#mint\", args: {workspace}}` answers \
an `mcp_url` and a `token` for it; revoking this key ends every key it minted. \
To bring another person in (a reviewer, say), `tilo_do {path: \"workspace/members#invite\", \
args: {role, email?}, workspace}` answers a `link` that signs them in and lands them there as \
`role` (viewer, commenter or editor); `workspace/members#list` names who is in it. \
A call that changed a record answers its `link`, and `tilo_link {path, workspace}` answers the link \
to anything a path names: hand it to the person reviewing your work. It opens the record in its \
app for the workspace's members and carries no secret, so invite the person first. \
Files go in with `tilo_put_begin`/`_append`/`_finish` (or `PUT /w/<workspace>/content`), answering \
a `tilo.content:<hash>` an action takes; an action that makes a file (Writer's `export-content`) \
names its `tilo.content:` in the answer's resources, and `tilo_get {content, offset}` reads it back.";

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
            "description": "Read `account/workspaces`, `account/keys` or `account/usage` (VM time and storage per workspace).",
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
