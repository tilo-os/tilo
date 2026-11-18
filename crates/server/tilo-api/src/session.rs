//! One agent session through the server: an MCP session of the agent, served here
//! by the agent surface `tilo mcp` serves ([`tilo_mcp::McpServer`]),
//! its actions asked of the workspace's relay over the grant's own session
//! ([`crate::server`], [`SessionActions`]). Record actions run on the server's
//! app workers, and each record the agent makes or changes opens on the
//! workspace's shell as a window of the person's, naming the session, so whoever
//! enters the workspace sees what the agent did.
//!
//! - **Pinned** (a one-workspace grant): the session serves that one
//!   workspace. It cannot name another: the server admits the grant nowhere
//!   else, and the server offers no tool that lists or makes workspaces.
//! - **Account** (an all-workspaces grant): the session offers the
//!   workspace tools with one more argument, `workspace`. A call naming a
//!   workspace is served in it, entering it the first time; a call naming
//!   none acts on the account itself, which the server answers:
//!   `account/workspaces#create`, `account/keys#mint` and the rest
//!   ([`crate::account`]).
//!
//! A program the API runs (`POST …/programs`) runs in a program session of
//! its own: the `tilo` tool's session (its `let`s, the references its answers
//! showed, the preview `confirm` applies, the steps `undo` takes back) on a
//! queue of the workspace's session that only that program session's calls
//! take. The caller names the program session each later program continues;
//! at most [`MAX_PROGRAM_SESSIONS`] are kept, the least recently used ending
//! first.
//!
//! A server inside a Tilo desktop serving its own person ([`Here`]) forwards nothing: the
//! desktop answers each workspace itself, and the account's own actions (making, branching and
//! deleting workspaces, keys) are not answered there.
//!
//! The session's name is the person's: the client's name and a number of
//! their own sessions ("Claude Code #2"), which History groups the session's
//! actions under, and which the workspace's host shows present, doing what it
//! does, while the session lives: the host is the session's one voice, so the
//! server says nothing of it itself.

pub mod hosts;
pub mod invocations;
pub mod keys;
pub mod members;
pub mod unattended;

pub use crate::server::Workspace;
use crate::server::{Connection, ServerError};
use crate::store::Scope;
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};
use tilo_mcp::McpServer;

/// The header naming an MCP session (Streamable HTTP).
pub const SESSION_HEADER: &str = "mcp-session-id";
/// The argument naming the workspace an account session's call acts in.
pub const WORKSPACE_ARGUMENT: &str = "workspace";
const PROTOCOL_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];
const MAX_NAME_CHARS: usize = 80;
/// How many program sessions one run keeps.
pub const MAX_PROGRAM_SESSIONS: usize = 16;

/// What a request to a session answers: JSON-RPC messages, the last of them
/// the reply. More than one only when the server tells the client something
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

/// The session in one workspace: the agent surface over the grant's session
/// there, and the agent present in it.
struct Served {
    server: Box<dyn Answering>,
}

/// What answers a workspace's MCP JSON-RPC for one session.
pub trait Answering: Send + Sync {
    /// The answer to `request`; `None` for a notification.
    fn handle(&self, request: Value) -> BoxFuture<'_, Option<Value>>;
}

type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

impl<A: tilo_actions::remote::ActionProvider + Send + Sync + 'static> Answering for McpServer<A> {
    fn handle(&self, request: Value) -> BoxFuture<'_, Option<Value>> {
        Box::pin(Self::handle(self, request))
    }
}

/// The Tilo desktop a server is part of, answering its person's workspaces itself: a desktop
/// session serving the API to its own person's programs on this computer.
pub trait Here: Send + Sync {
    /// The workspaces it holds.
    fn workspaces(&self) -> Vec<Workspace>;
    /// What answers `workspace` for the session `session`, named `label` in History.
    ///
    /// # Errors
    /// It holds no such workspace, or cannot serve it now.
    fn enter(
        &self,
        workspace: &str,
        session: &str,
        label: &str,
    ) -> Result<Box<dyn Answering>, String>;
}

/// A workspace a desktop holds, as the account lists it: the person's own.
#[must_use]
pub fn held(id: String, name: String) -> Workspace {
    Workspace {
        id,
        name,
        host: None,
        role: Some("owner".into()),
        labels: Labels::new(),
        unattended: false,
        address: None,
    }
}

/// How a session reaches the person's workspaces.
pub(crate) enum Reach {
    /// The grant's own connection, to wherever the call is forwarded.
    Forwarded(Arc<Connection>),
    /// The desktop this server is part of.
    Here(Arc<dyn Here>),
}

/// Why a workspace or the account did not answer.
type Refusal = ServerError;
type Labels = tilo_server_session::workspaces::WorkspaceLabels;

/// A session in a workspace one of the person's devices hosts, entered at
/// that device's MCP server: the device serves the whole session, its people
/// and its windows included, and shows the agent there itself.
struct OnDevice(tilo_mcp::enter::Entered);

/// One workspace's session, once whoever entered it holds it: a lock per
/// workspace, so entering one does not hold another's requests.
type Entered<Run> = Arc<tokio::sync::Mutex<Option<Arc<Run>>>>;

pub struct Session {
    pub grant: String,
    pub scope: Scope,
    /// The session's name, as History shows it.
    pub label: String,
    /// The session's id, which every invocation of it belongs to.
    id: String,
    /// The person, as their workspaces name them.
    person: String,
    /// Who its requests are by ([`Self::caller`]).
    caller: tilo_actions::remote::Caller,
    /// How the session reaches the workspaces.
    reach: Reach,
    /// The server, which answers the account's own actions.
    server: std::sync::Weak<crate::Server>,
    /// The sessions in workspaces the server hosts, by [`queue`], each entered
    /// under its own lock.
    served: tokio::sync::Mutex<HashMap<String, Entered<Served>>>,
    /// The sessions in workspaces the person's devices host, by [`queue`].
    on_devices: tokio::sync::Mutex<HashMap<String, Entered<OnDevice>>>,
    /// The program sessions, `(workspace, id)`, the most recently used last.
    programs: tokio::sync::Mutex<Vec<(String, String)>>,
    /// The workspace tools, once a workspace served them.
    tools: Mutex<Option<Vec<Value>>>,
    pub(crate) last_used: Mutex<Instant>,
}

/// One tool call of a session: what an MCP client's `tools/call` and every
/// route of the API make ([`Session::call`]).
#[derive(Debug)]
pub struct Call {
    /// The workspace it acts in; `None` for the account, or for the one
    /// workspace a session for one workspace acts in.
    pub workspace: Option<String>,
    /// The tool: `tilo_ls`, `tilo_read`, `tilo_do`, `tilo_status`, …
    pub tool: &'static str,
    pub arguments: Value,
    /// The invocation group it is a step of, `{"id", "title"}`, when the
    /// caller names one.
    pub group: Option<Value>,
}

/// Why a call answered nothing typed: the request itself was refused, before
/// any tool ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallError {
    /// The typed code: `not_found`, `beyond_scope`, `invalid_request` or
    /// `unavailable`.
    pub code: &'static str,
    pub message: String,
}

impl CallError {
    fn beyond_scope() -> Self {
        Self {
            code: "beyond_scope",
            message: "this key reaches another workspace only".into(),
        }
    }

    fn unanswered() -> Self {
        Self {
            code: "unavailable",
            message: "your workspace didn't answer; try again in a minute".into(),
        }
    }

    /// The JSON-RPC `error` a session answered, typed.
    fn of(error: &Value) -> Self {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the call was refused")
            .to_owned();
        let code = match error.get("code").and_then(Value::as_i64) {
            Some(-32602) if message.contains("no such workspace") => "not_found",
            Some(-32602 | -32600 | -32700 | -32601) => "invalid_request",
            _ => "unavailable",
        };
        Self { code, message }
    }
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
pub fn agent_session_label(registered: &str, initialize: &Value, number: u64) -> String {
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
    /// Who the session's requests are by: the key's person at the key, as its certificate names
    /// them, which the host reads as that person or as their agent. Never a label.
    pub(crate) fn caller(&self) -> tilo_actions::remote::Caller {
        self.caller.clone()
    }

    pub(crate) fn new(
        grant: String,
        scope: Scope,
        label: String,
        cert: &tilo_user_identity::DeviceCert,
        reach: Reach,
        server: std::sync::Weak<crate::Server>,
    ) -> Self {
        Self {
            grant,
            scope,
            label,
            id: uuid::Uuid::new_v4().simple().to_string(),
            person: hex::encode(cert.user_public_key),
            caller: tilo_actions::remote::Caller::authenticated(
                cert.user_public_key,
                cert.device_public_key,
            ),
            reach,
            server,
            served: tokio::sync::Mutex::default(),
            on_devices: tokio::sync::Mutex::default(),
            programs: tokio::sync::Mutex::default(),
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

    /// The grant's own connection, for the account's own actions.
    pub(crate) fn connection(&self) -> Result<&Arc<Connection>, Refusal> {
        match &self.reach {
            Reach::Forwarded(connection) => Ok(connection),
            Reach::Here(_) => Err(Refusal::NoAccountHere),
        }
    }

    /// Whether the session still reaches its workspaces.
    pub(crate) fn is_live(&self) -> bool {
        match &self.reach {
            Reach::Forwarded(connection) => connection.is_live(),
            Reach::Here(_) => true,
        }
    }

    /// The workspaces the session reaches, labelled with every one of `labels` where labels are
    /// kept.
    pub(crate) async fn workspaces(&self, labels: &Labels) -> Result<Vec<Workspace>, Refusal> {
        match &self.reach {
            Reach::Forwarded(connection) => connection.server().labelled(connection, labels).await,
            Reach::Here(here) => Ok(here.workspaces()),
        }
    }

    /// `workspace` as the session reaches it, when it does.
    pub(crate) async fn workspace(&self, workspace: &str) -> Result<Option<Workspace>, Refusal> {
        match &self.reach {
            Reach::Forwarded(connection) => {
                connection.server().workspace(connection, workspace).await
            }
            Reach::Here(here) => Ok(here
                .workspaces()
                .into_iter()
                .find(|held| held.id == workspace)),
        }
    }

    /// The session in `workspace`, entering it the first time. Each workspace's
    /// entry has a lock of its own, so entering one does not hold another's
    /// answers.
    async fn served(
        &self,
        workspace: &str,
        program: Option<&str>,
    ) -> Result<Arc<Served>, ServerError> {
        let slot = {
            let mut runs = self.served.lock().await;
            let slot = Arc::clone(runs.entry(queue(workspace, program)).or_default());
            drop(runs);
            slot
        };
        let mut entered = slot.lock().await;
        if let Some(known) = entered.as_ref() {
            return Ok(Arc::clone(known));
        }
        let connection = match &self.reach {
            Reach::Forwarded(connection) => connection,
            Reach::Here(here) => {
                if !here.workspaces().iter().any(|held| held.id == workspace) {
                    return Err(Refusal::NotFound);
                }
                let session = format!("{}-{}", self.id, program.unwrap_or_default());
                let server = here
                    .enter(workspace, &session, &self.label)
                    .map_err(Refusal::Unavailable)?;
                let held = Arc::new(Served { server });
                *entered = Some(Arc::clone(&held));
                drop(entered);
                return Ok(held);
            }
        };
        let id = connection.server().enter(connection, workspace).await?;
        let device = &connection.device;
        let actions = device.actions(id);
        let catalogue =
            tilo_mcp::task_hosts::WorkspaceCatalogue::new(actions.clone(), self.caller());
        let server = McpServer::new(actions, workspace.to_owned(), self.person.clone())
            .with_agent_session(&self.id, self.label.clone())
            .with_task_hosts(Some(Arc::new(tilo_mcp::task_hosts::TaskHosts::new(
                Some(&self.person),
                Arc::new(catalogue),
            ))));
        let held = Arc::new(Served {
            server: Box::new(server),
        });
        *entered = Some(Arc::clone(&held));
        drop(entered);
        Ok(held)
    }

    /// Answer `request` in `workspace`'s session, showing the person what a
    /// tool call did beside the agent. The workspace's people
    /// (`workspace/members`) are answered here ([`members`]).
    async fn serve(&self, workspace: &str, request: Value) -> Reply {
        self.serve_in(workspace, None, request).await
    }

    /// [`Self::serve`], on the queue of the program session `program` when it
    /// names one.
    async fn serve_in(&self, workspace: &str, program: Option<&str>, request: Value) -> Reply {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let looking = timing_start();
        match self.on_device(workspace, program).await {
            Ok(Some(held)) => {
                let handling = timing_start();
                let answer = held
                    .0
                    .handle(request)
                    .await
                    .map_or_else(Reply::accepted, Reply::one);
                timing_done("api.handle_on_device", handling);
                return answer;
            }
            Ok(None) => {}
            Err(refused) => return Reply::one(error(&id, -32000, &refused)),
        }
        timing_done("api.device", looking);
        if matches!(self.reach, Reach::Forwarded(_))
            && request.get("method").and_then(Value::as_str) == Some("tools/call")
        {
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
            if hosts::addresses(tool, arguments) {
                let structured = request
                    .pointer("/params/_meta/so.tilo.mcp/structured")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let answered = self.hosts(workspace, tool, arguments, structured).await;
                return Reply::one(result(&id, &answered));
            }
            if unattended::addresses(tool, arguments) {
                let structured = request
                    .pointer("/params/_meta/so.tilo.mcp/structured")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let answered = self
                    .unattended(workspace, tool, arguments, structured)
                    .await;
                return Reply::one(result(&id, &answered));
            }
            if invocations::addresses(tool, arguments) {
                let structured = request
                    .pointer("/params/_meta/so.tilo.mcp/structured")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let answered = self.invocations(workspace, arguments, structured).await;
                return Reply::one(result(&id, &answered));
            }
        }
        let entering = timing_start();
        let served = match self.served(workspace, program).await {
            Ok(held) => held,
            Err(ServerError::NotFound) => {
                return Reply::one(error(&id, -32602, "no such workspace"));
            }
            Err(other) => return Reply::one(error(&id, -32000, &other.to_string())),
        };
        timing_done("api.enter", entering);
        let handling = timing_start();
        let answer = Box::pin(served.server.handle(request))
            .await
            .map_or_else(Reply::accepted, Reply::one);
        timing_done("api.handle", handling);
        answer
    }

    /// The session in `workspace` when one of the person's devices hosts it, as
    /// their list says, entering it at that device's MCP server the first
    /// time (and again once the device closed it); `None` for a workspace the
    /// server keeps. As [`Self::served`], each workspace's entry has a lock of
    /// its own.
    async fn on_device(
        &self,
        workspace: &str,
        program: Option<&str>,
    ) -> Result<Option<Arc<OnDevice>>, String> {
        let Reach::Forwarded(connection) = &self.reach else {
            return Ok(None);
        };
        let queue = queue(workspace, program);
        // A workspace the session already works in on the server is the server's.
        if self.served.lock().await.contains_key(&queue) {
            return Ok(None);
        }
        let slot = {
            let mut runs = self.on_devices.lock().await;
            let slot = Arc::clone(runs.entry(queue).or_default());
            drop(runs);
            slot
        };
        let mut entered = slot.lock().await;
        if let Some(known) = entered.as_ref().filter(|known| known.0.is_live()) {
            return Ok(Some(Arc::clone(known)));
        }
        let Some(address) = self.device_address(workspace).await else {
            return Ok(None);
        };
        let endpoint = connection
            .endpoint()
            .await
            .map_err(|error| error.to_string())?;
        let on_device =
            tilo_mcp::enter::enter(&endpoint, &connection.entering_key(), workspace, &address)
                .await?;
        let session = Arc::new(OnDevice(on_device));
        *entered = Some(Arc::clone(&session));
        drop(entered);
        Ok(Some(session))
    }

    /// Where `workspace` is hosted when one of the person's devices hosts it,
    /// as their list says (a key for one workspace reads that one's).
    async fn device_address(
        &self,
        workspace: &str,
    ) -> Option<tilo_server_session::workspaces::KeepingDeviceAddress> {
        let address = self.workspace(workspace).await.ok()??.address?;
        // A listed address with a hint is a device's own, never the answering device's.
        address.address().ok()?;
        Some(address)
    }

    /// Answer the client's `initialize`. A pinned session enters its one
    /// workspace now, so a workspace that cannot be reached fails the
    /// connect, with the JSON-RPC error it failed with.
    pub(crate) async fn initialize(&self, request: &Value) -> Result<Value, Value> {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        if let Some(workspace) = self.pinned().map(str::to_owned) {
            let reply = self.serve(&workspace, request.clone()).await;
            let answer = reply.messages.into_iter().last().ok_or_else(|| {
                error(
                    &id,
                    -32000,
                    "your workspace didn't answer; try again in a minute",
                )
            })?;
            if answer.get("error").is_some() {
                return Err(answer);
            }
            return Ok(with_hosted_instructions(answer));
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
            ) {
                let tool = request
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let arguments = request.pointer("/params/arguments").unwrap_or(&Value::Null);
                let structured = request
                    .pointer("/params/_meta/so.tilo.mcp/structured")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if let Some(refused) = crate::account::beyond_workspace(tool, arguments, structured)
                {
                    return Reply::one(result(id, &refused));
                }
                // Its workspace answers a call in it; the account answers the
                // keys the key holds there.
                if crate::account::on_account(tool, arguments) {
                    return self.account(id, tool, arguments, structured).await;
                }
            }
            return Box::pin(self.serve(&workspace, request)).await;
        }
        let id = request.get("id").cloned();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(id) = id else {
            // Notifications concern the server's own session.
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

    /// The workspace tools, each with the `workspace` it acts in: the ones a
    /// workspace served, or, before one did, the same set the API and every
    /// session serve, so an account key and a one-workspace key see one
    /// vocabulary.
    fn account_tools(&self) -> Vec<Value> {
        self.known_tools().map_or_else(shared_tools, |known| {
            known.iter().map(with_workspace_argument).collect()
        })
    }

    /// The id of the workspace `named`: an id as it is, or the account's
    /// workspace of that name, so an agent may say `sdkfix` as the CLI and
    /// SDKs do.
    async fn workspace_id(&self, named: &str) -> Option<String> {
        if uuid::Uuid::parse_str(named).is_ok() {
            return Some(named.to_owned());
        }
        let listed = self
            .account(
                &json!("resolve"),
                "tilo_ls",
                &json!({"path": "account/workspaces"}),
                true,
            )
            .await;
        let rows = listed
            .messages
            .first()?
            .pointer("/result/structuredContent/rows")?
            .as_array()?;
        let wanted = named.trim().to_lowercase().replace(' ', "-");
        rows.iter().find_map(|row| {
            let label = row.get("label").and_then(Value::as_str)?;
            (label.trim().to_lowercase().replace(' ', "-") == wanted)
                .then(|| row.get("name").and_then(Value::as_str).map(str::to_owned))
                .flatten()
        })
    }

    fn known_tools(&self) -> Option<Vec<Value>> {
        self.tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Learn the workspace tools from `workspace`'s session.
    async fn learn_tools(&self, workspace: &str) -> Option<Vec<Value>> {
        let listed = self
            .serve(
                workspace,
                json!({"jsonrpc":"2.0","id":"tilo-api-tools","method":"tools/list"}),
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
        let named = arguments
            .get(WORKSPACE_ARGUMENT)
            .and_then(Value::as_str)
            .map(str::to_owned);
        let resolved = match named {
            Some(named) => Some(self.workspace_id(&named).await.unwrap_or(named)),
            None => None,
        };
        let Some(workspace) = resolved else {
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
                "name the workspace this acts in: `workspace`, its id or its name as \
                 account/workspaces#list answers them; only paths under `account` need none",
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
        let Some(server) = self.server.upgrade() else {
            return Reply::one(error(id, -32000, "the server is shutting down"));
        };
        let answered = server
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

    /// Answer `call` with its typed answer: the one way every call is made,
    /// whether an MCP client's `tools/call` or a route of the API (`crate::v1`).
    ///
    /// # Errors
    /// The session reaches no such workspace, or the call was malformed.
    pub async fn call(&self, call: Call) -> Result<Value, CallError> {
        let mut arguments = match call.arguments {
            Value::Null => json!({}),
            arguments => arguments,
        };
        match (self.pinned(), call.workspace.as_deref()) {
            (Some(pinned), Some(asked)) if pinned != asked => {
                return Err(CallError::beyond_scope());
            }
            (None, Some(workspace)) => {
                if let Some(fields) = arguments.as_object_mut() {
                    fields.insert(WORKSPACE_ARGUMENT.into(), json!(workspace));
                }
            }
            _ => {}
        }
        let mut meta = Map::new();
        meta.insert("structured".into(), json!(true));
        if let Some(group) = call.group {
            meta.insert("invocation".into(), group);
        }
        let request = json!({"jsonrpc": "2.0", "id": "api", "method": "tools/call",
            "params": {"name": call.tool, "arguments": arguments, "_meta": {"so.tilo.mcp": meta}}});
        let reply = Box::pin(self.handle(request)).await;
        let Some(message) = reply.messages.into_iter().last() else {
            return Err(CallError::unanswered());
        };
        if let Some(error) = message.get("error") {
            return Err(CallError::of(error));
        }
        Ok(message.get("result").cloned().unwrap_or_default())
    }

    /// Run `arguments` (the `tilo` tool's: `{program, confirm}`) in
    /// `workspace`, in the program session `session` names, or a new one
    /// when it names none: the tool's result, and the session it ran in.
    ///
    /// # Errors
    /// The run reaches no such workspace, or `session` names a session that
    /// ended.
    pub async fn run_program(
        &self,
        workspace: &str,
        session: Option<&str>,
        arguments: Value,
        group: Option<Value>,
    ) -> Result<(Value, String), CallError> {
        if self.pinned().is_some_and(|pinned| pinned != workspace) {
            return Err(CallError::beyond_scope());
        }
        self.touch();
        let session = self.program_session(workspace, session).await?;
        let mut meta = Map::new();
        meta.insert("structured".into(), json!(true));
        if let Some(group) = group {
            meta.insert("invocation".into(), group);
        }
        let request = json!({"jsonrpc": "2.0", "id": "api", "method": "tools/call",
            "params": {"name": "tilo", "arguments": arguments, "_meta": {"so.tilo.mcp": meta}}});
        let reply = Box::pin(self.serve_in(workspace, Some(&session), request)).await;
        let Some(message) = reply.messages.into_iter().last() else {
            return Err(CallError::unanswered());
        };
        if let Some(error) = message.get("error") {
            return Err(CallError::of(error));
        }
        Ok((message.get("result").cloned().unwrap_or_default(), session))
    }

    /// The program session `asked` names in `workspace`, now the most
    /// recently used, or a new one when it names none. A new one beyond
    /// [`MAX_PROGRAM_SESSIONS`] ends the least recently used.
    async fn program_session(
        &self,
        workspace: &str,
        asked: Option<&str>,
    ) -> Result<String, CallError> {
        let mut sessions = self.programs.lock().await;
        let id = match asked {
            Some(asked) => {
                let at = sessions
                    .iter()
                    .position(|(known, id)| known == workspace && id == asked)
                    .ok_or_else(|| CallError {
                        code: "not_found",
                        message: "that program session ended: its lets and references are gone; \
                                  leave `session` out to start a new one"
                            .into(),
                    })?;
                sessions.remove(at).1
            }
            None => uuid::Uuid::new_v4().simple().to_string(),
        };
        sessions.push((workspace.to_owned(), id.clone()));
        let over = sessions.len().saturating_sub(MAX_PROGRAM_SESSIONS);
        let ended: Vec<String> = sessions
            .drain(..over)
            .map(|(workspace, id)| queue(&workspace, Some(&id)))
            .collect();
        drop(sessions);
        for queue in &ended {
            self.served.lock().await.remove(queue);
            self.on_devices.lock().await.remove(queue);
        }
        Ok(id)
    }

    /// Call the workspace tool `name` with `arguments` in `workspace`, in
    /// this session: the tool's result, or why there is none.
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
                json!({"jsonrpc":"2.0","id":"tilo-api-call","method":"tools/call",
                        "params":{"name":name,"arguments":arguments}}),
            )
            .await;
        let message = reply
            .messages
            .last()
            .ok_or("your workspace didn't answer; try again in a minute")?;
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
    /// with the server, which its other runs share, ends with the last of
    /// them, or when the grant is revoked.
    pub(crate) async fn close(&self) {
        self.served.lock().await.clear();
        self.on_devices.lock().await.clear();
    }
}

/// The key a workspace's run is kept under: the workspace, or the queue of
/// one program session there.
fn queue(workspace: &str, program: Option<&str>) -> String {
    program.map_or_else(|| workspace.to_owned(), |id| format!("{workspace} {id}"))
}

/// What an account session is told first: how to reach the account and its
/// workspaces, and how a product gives each customer's agent a workspace of
/// its own.
pub const ACCOUNT_INSTRUCTIONS: &str = "You act in the person's Tilo workspaces. \
Paths under `account` manage them and need no `workspace`: `tilo_ls account` lists its actions; \
`tilo_do {path: \"account/workspaces#list\"}` names the workspaces, \
each with the part the person takes in it (`owner` for one of their own, else the role they were \
invited with), \
`account/workspaces#create {name, labels?}` makes one, labelled for your programs to find it by \
(`labels: {customer_id: \"cus_8121\"}`, which only the person's keys read), \
`account/workspaces#list {labels?}` lists those labelled so, \
`account/workspaces#branch {workspace, name, labels?}` makes a new one of the person's own that starts as \
a copy of one of theirs (every record with its history, and its files; not its conversations, \
nor anyone but the person) and goes on apart from it, so a workspace set up once is branched per task \
or customer, and `account/workspaces#delete {workspace}` deletes one \
of the person's own for good, with everything in it and every key minted for it (no undo; the call \
is the confirmation, so delete only what you were asked to). Every other call acts in one workspace, \
named by id in its `workspace` argument. To give another agent one workspace and nothing else \
(one workspace per customer), `tilo_do {path: \"account/keys#mint\", args: {workspace}}` answers \
an `mcp_url` and a `token` for it; revoking this key ends every key it minted. \
To bring another person in (a reviewer, say), `tilo_do {path: \"workspace/members#invite\", \
args: {role, email?}, workspace}` answers a `link` that signs them in and lands them there as \
`role` (viewer, commenter, editor or administrator); `workspace/members#list` names who is in it, and \
`workspace/members#name {name}` sets how it names the key's own person, which is what their \
invitations say. \
A workspace can be kept by more than one server: `tilo_do {path: \"workspace/servers#add\", \
args: {origin: \"https://tilo.sh\"}, workspace}` has that server keep it as well (both keep it, a \
change on either reaches the other, and its people reach it at either; that server takes it only \
for an owner it let in), `workspace/servers#list` names the servers keeping it and \
`workspace/servers#remove {server}` lets one go; only its owners choose. \
A command an agent runs in a workspace's Linux machine (`terminal#run`) asks the person each \
time; `tilo_do {path: \"workspace/unattended#set\", args: {on: true}, workspace}` asks them once \
to let keys minted for that workspace alone run commands there without asking (and in branches \
made from it), answering `pending` until they decide; this key itself is still asked, and \
`{on: false}` asks again from the next command. \
`tilo_read workspace/invocations` (with `workspace`) lists a workspace's invocations in records this key may read, \
newest first, whoever made them (`tilo_ls workspace/invocations` with `limit` and `cursor` pages on): who made \
it, how it stands \
(`waiting` for the person, `running`, `ended`, `failed`, `cancelled`, `undone`), what it changed and \
whether it can still be taken back; its `id` is the invocation `tilo_status` reads, stops (`cancel`) \
and takes back (`undo`). \
A call that changed a record answers its `link`, and `tilo_link {path, workspace}` answers the link \
to anything a path names: hand it to the person reviewing your work. It opens the record in its \
app for the workspace's members and carries no secret, so invite the person first. \
Files go in with `tilo_put_begin`/`_append`/`_finish` (or `PUT /w/<workspace>/content`), answering \
a `tilo.content:<hash>` an action takes; an action that makes a file (Writer's `export-content`) \
names its `tilo.content:` in the answer's resources, and `tilo_get {content, offset}` reads it back.";

/// `initialize`'s answer, telling the client what this server answers beside
/// the workspace's own tools: the workspace's runs ([`runs`]).
fn with_hosted_instructions(mut answer: Value) -> Value {
    if let Some(Value::String(instructions)) = answer.pointer_mut("/result/instructions") {
        instructions.push(' ');
        instructions.push_str(invocations::INSTRUCTIONS);
    }
    answer
}

/// The tools every session serves, each with the `workspace` an account
/// session's call names: what the API and every workspace session answer,
/// so a key to all the person's workspaces is offered the same vocabulary as
/// a key for one.
fn shared_tools() -> Vec<Value> {
    tilo_mcp::tools_json()
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| tools.iter().map(with_workspace_argument).collect())
        .unwrap_or_default()
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
            json!({"type":"string","description":"The workspace this acts in: its id, as account/workspaces#list answers it, or its name. Omit it for paths under `account`."}),
        );
    }
    tool
}

/// A clock read for timing one phase of a call, when the `tilo_api::timing`
/// target is at debug: off, it reads no clock.
fn timing_start() -> Option<Instant> {
    tracing::enabled!(target: "tilo_api::timing", tracing::Level::DEBUG).then(Instant::now)
}

/// Time the phase `step`, begun at `started` ([`timing_start`]), in
/// microseconds: `TIMING <step> <µs>` says where a slow call waited.
fn timing_done(step: &'static str, started: Option<Instant>) {
    if let Some(started) = started {
        tracing::debug!(
            target: "tilo_api::timing",
            "TIMING {step} {}",
            started.elapsed().as_micros()
        );
    }
}
