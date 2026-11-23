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
//!   else; the account it reaches is its own keys ([`crate::account`]).
//! - **Account** (an all-workspaces grant): the session offers the
//!   workspace tools with one more argument, `workspace`. A call naming a
//!   workspace is served in it, entering it the first time; a call naming
//!   none walks the account's tree alone: `account/workspaces#create`,
//!   `account/keys#mint` and the rest.
//!
//! Every tree a session walks publishes the account beside the workspace's own actions
//! ([`crate::account::Joined`]), so its actions are reached as any other is.
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
//! desktop answers each workspace itself, and lists its workspaces; the account's other actions
//! go to the server its login reaches, where there is one.
//!
//! The session's name is the person's: the client's name and a number of
//! their own sessions ("Claude Code #2"), which History groups the session's
//! actions under, and which the workspace's host shows present, doing what it
//! does, while the session lives: the host is the session's one voice, so the
//! server says nothing of it itself.

use crate::account::{Account, Joined, ROOT_TREE};
pub use crate::server::Workspace;
use crate::server::{Connection, ServerError};
use crate::store::Scope;
use crate::v1::errors::ApiError;
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

/// A workspace's actions, as one session reaches them.
pub type Provider = Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>;

/// The session in one workspace: the workspace's published tree as the session walks it, over
/// the workspace's actions. The API's routes call it typed; the MCP endpoint speaks MCP over it.
struct Served {
    server: McpServer<Provider>,
}

/// A workspace as the desktop here reaches it.
pub struct Reached {
    /// Its actions, as the person.
    pub provider: Provider,
    /// The person, as the workspace names them.
    pub person: String,
    /// Where its programs run: the device here starts each task's host; `None` where it runs
    /// none.
    pub tasks: Option<Arc<dyn tilo_mcp::TaskHosts>>,
}

/// The Tilo desktop a server is part of, answering its person's workspaces itself: a desktop
/// session serving the API to its own person's programs on this computer.
pub trait Here: Send + Sync {
    /// The workspaces it holds that `key` reaches: none for a key of a person it does not know.
    fn workspaces(&self, key: &tilo_user_identity::DeviceCert) -> Vec<Workspace>;
    /// `workspace`'s actions as `key` reaches them here: its calls are the key's own, decided by
    /// what the workspace grants that key, never its person's.
    ///
    /// # Errors
    /// It holds no such workspace, the key's person is not in it, or it cannot serve it now.
    fn enter(&self, workspace: &str, key: &crate::grant_key::GrantKey) -> Result<Reached, String>;

    /// The connection the account's own actions go over, when this host forwards them to the
    /// server its login reaches: `None` for a desktop, which answers no account action itself.
    fn connection(&self) -> Option<Arc<crate::server::Connection>> {
        None
    }
    /// The device's record of every invocation it was asked for, shared with the account: `None`
    /// for a host that keeps none, whose account keeps its own.
    fn invocations(&self) -> Option<Arc<tilo_actions::remote::invocation_record::Invocations>> {
        None
    }
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
#[derive(Clone)]
pub(crate) enum Reach {
    /// The grant's own connection, to wherever the call is forwarded.
    Forwarded(Arc<Connection>),
    /// The desktop this server is part of, reached as the grant's key.
    Here(Arc<dyn Here>, Box<crate::grant_key::GrantKey>),
}

/// Why a workspace or the account did not answer.
type Refusal = ServerError;
type Labels = tilo_server_session::workspaces::WorkspaceLabels;

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
    /// The account's actions, in every tree the session walks.
    account: Account,
    /// The account's tree alone, which a call naming no workspace walks.
    own: McpServer<Provider>,
    /// The sessions in the workspaces it reaches, by [`queue`], each entered
    /// under its own lock.
    served: tokio::sync::Mutex<HashMap<String, Entered<Served>>>,
    /// The program sessions, `(workspace, id)`, the most recently used last.
    programs: tokio::sync::Mutex<Vec<(String, String)>>,
    /// The workspace tools, once a workspace served them.
    tools: Mutex<Option<Vec<Value>>>,
    pub(crate) last_used: Mutex<Instant>,
}

/// A call outside the one workspace a key for one reaches.
fn beyond_scope() -> ApiError {
    ApiError::new("beyond_scope", "this key reaches another workspace only")
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
        grant: &crate::store::Grant,
        scope: Scope,
        label: String,
        cert: &tilo_user_identity::DeviceCert,
        reach: Reach,
        server: std::sync::Weak<crate::Server>,
    ) -> Self {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let person = hex::encode(cert.user_public_key);
        let caller = tilo_actions::remote::Caller::authenticated(
            cert.user_public_key,
            cert.device_public_key,
        );
        let account = Account::new(server, grant.clone(), reach.clone(), caller.clone());
        let grant = grant.id.clone();
        let own = McpServer::new(
            Joined::provider(ROOT_TREE, None, account.clone()),
            ROOT_TREE.to_owned(),
            person.clone(),
        )
        .with_agent_session(&id);
        Self {
            grant,
            scope,
            label,
            id,
            person,
            caller,
            reach,
            account,
            own,
            served: tokio::sync::Mutex::default(),
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

    /// Whether the session still reaches its workspaces.
    pub(crate) fn is_live(&self) -> bool {
        match &self.reach {
            Reach::Forwarded(connection) => connection.is_live(),
            Reach::Here(..) => true,
        }
    }

    /// The workspaces the session reaches, labelled with every one of `labels` where labels are
    /// kept.
    pub(crate) async fn workspaces(&self, labels: &Labels) -> Result<Vec<Workspace>, Refusal> {
        match &self.reach {
            Reach::Forwarded(connection) => connection.server().labelled(connection, labels).await,
            Reach::Here(here, key) => Ok(here.workspaces(key.cert())),
        }
    }

    /// `workspace` as the session reaches it, when it does.
    pub(crate) async fn workspace(&self, workspace: &str) -> Result<Option<Workspace>, Refusal> {
        match &self.reach {
            Reach::Forwarded(connection) => {
                connection.server().workspace(connection, workspace).await
            }
            Reach::Here(here, key) => Ok(here
                .workspaces(key.cert())
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
            // The desktop here answers as its person, in a group of the session's own.
            Reach::Here(here, key) => {
                if !here
                    .workspaces(key.cert())
                    .iter()
                    .any(|held| held.id == workspace)
                {
                    return Err(Refusal::NotFound);
                }
                let reached = here.enter(workspace, key).map_err(Refusal::Unavailable)?;
                let provider =
                    Joined::provider(workspace, Some(reached.provider), self.account.clone());
                let server = McpServer::for_person(provider, workspace.to_owned(), &reached.person)
                    .with_work_group(tilo_actions::WorkGroup {
                        id: format!("{}-{}", self.id, program.unwrap_or_default()),
                        label: Some(self.label.clone()),
                        kind: "api".into(),
                        declared_id: None,
                        session: None,
                    })
                    .with_task_hosts(reached.tasks);
                let held = Arc::new(Served { server });
                *entered = Some(Arc::clone(&held));
                drop(entered);
                return Ok(held);
            }
        };
        // One of the person's devices keeps it: its actions are asked of that device over its
        // action service, signed by this key, and the device's members and policy decide them.
        let held = if let Some(address) = self.device_address(workspace).await {
            let device = address.address().map_err(Refusal::Unavailable)?;
            if device.workspace_id.to_string() != workspace {
                return Err(Refusal::Unavailable(
                    "that address is another workspace's".to_owned(),
                ));
            }
            let endpoint = connection.endpoint().await?;
            let actions = tilo_peer_state::actions::ActionService::new(
                connection.key.key().clone(),
                Arc::new(tilo_peer_state::actions::NoActions),
            )
            .remote(&endpoint, &device.endpoint_addr, workspace);
            Served {
                server: self.agent_surface(Arc::new(actions), workspace),
            }
        } else {
            let id = connection.server().enter(connection, workspace).await?;
            Served {
                server: self
                    .agent_surface(Arc::new(connection.device.workspace_actions(id)), workspace),
            }
        };
        let held = Arc::new(held);
        *entered = Some(Arc::clone(&held));
        drop(entered);
        Ok(held)
    }

    /// The agent surface over `actions`, the actions of `workspace`, with the account's beside
    /// them, in this session.
    fn agent_surface(&self, actions: Provider, workspace: &str) -> McpServer<Provider> {
        let actions = Joined::provider(workspace, Some(actions), self.account.clone());
        // A forwarded session starts no task host: only a device starts tilolang processes, in
        // their sandbox, and this server is none. Its agent works with single and batched calls.
        McpServer::new(actions, workspace.to_owned(), self.person.clone())
            .with_agent_session(&self.id)
    }

    /// Answer `request` in `workspace`'s session, showing the person what a
    /// tool call did beside the agent.
    async fn serve(&self, workspace: &str, request: Value) -> Reply {
        self.serve_in(workspace, None, request).await
    }

    /// [`Self::serve`], on the queue of the program session `program` when it
    /// names one.
    async fn serve_in(&self, workspace: &str, program: Option<&str>, request: Value) -> Reply {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let entering = timing_start();
        let served = match self.served(workspace, program).await {
            Ok(held) => held,
            Err(ServerError::NotFound) => {
                return Reply::one(error(&id, -32602, "no such workspace"));
            }
            // Said as such, so its caller asks the person for another key.
            Err(revoked @ ServerError::KeyRevoked) => {
                return Reply::one(json!({"jsonrpc": "2.0", "id": id, "error": {
                    "code": -32000, "message": revoked.to_string(),
                    "data": {"code": "key_revoked", "next_steps": [crate::APPROVE_AGAIN]},
                }}));
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
        let wanted = named.trim().to_lowercase().replace(' ', "-");
        self.workspaces(&Labels::new())
            .await
            .ok()?
            .into_iter()
            .find(|workspace| workspace.name.trim().to_lowercase().replace(' ', "-") == wanted)
            .map(|workspace| workspace.id)
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

    /// A call of an account session: in the workspace it names, or on the account's tree when
    /// it names none.
    async fn account_call(&self, id: &Value, mut request: Value) -> Reply {
        let named = request
            .pointer("/params/arguments")
            .and_then(|arguments| arguments.get(WORKSPACE_ARGUMENT))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(named) = named else {
            if !on_account(request.pointer("/params/arguments")) {
                return Reply::one(error(id, -32602, NAME_THE_WORKSPACE));
            }
            return Box::pin(self.own.handle(request))
                .await
                .map_or_else(Reply::accepted, Reply::one);
        };
        let workspace = self.workspace_id(&named).await.unwrap_or(named);
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

    /// Call `tool` with `arguments` in `workspace` (`None` for the account, or for the one
    /// workspace a session for one acts in), as a step of the invocation group `group`
    /// (`{"id", "title"}`): its typed result. Every route of the API calls this.
    ///
    /// # Errors
    /// The session reaches no such workspace, the call was malformed, or the tool refused it.
    pub async fn typed(
        &self,
        workspace: Option<&str>,
        tool: &str,
        arguments: Value,
        group: Option<Value>,
    ) -> Result<Value, ApiError> {
        self.touch();
        let arguments = match arguments {
            Value::Null => json!({}),
            arguments => arguments,
        };
        let workspace = match (self.pinned(), workspace) {
            (Some(pinned), Some(asked)) if pinned != asked => {
                return Err(beyond_scope());
            }
            (Some(pinned), _) => pinned.to_owned(),
            (None, Some(named)) => self
                .workspace_id(named)
                .await
                .unwrap_or_else(|| named.to_owned()),
            (None, None) if on_account(Some(&arguments)) => {
                return Box::pin(self.own.typed(tool, arguments, group))
                    .await
                    .map_err(|refused| {
                        ApiError::of_answer(Some(&refused.error), refused.action.as_ref())
                    });
            }
            (None, None) => return Err(ApiError::new("invalid_request", NAME_THE_WORKSPACE)),
        };
        self.typed_in(&workspace, None, tool, arguments, group)
            .await
    }

    /// [`Self::typed`] in `workspace`, on the queue of the program session `program` when it
    /// names one.
    async fn typed_in(
        &self,
        workspace: &str,
        program: Option<&str>,
        tool: &str,
        arguments: Value,
        group: Option<Value>,
    ) -> Result<Value, ApiError> {
        let served = self.served(workspace, program).await?;
        Box::pin(served.server.typed(tool, arguments, group))
            .await
            .map_err(|refused| ApiError::of_answer(Some(&refused.error), refused.action.as_ref()))
    }

    /// The account's actions, as this session reaches them.
    pub(crate) const fn account(&self) -> &Account {
        &self.account
    }

    /// Run `arguments` (the `tilo` tool's: `{program, confirm}`) in
    /// `workspace`, in the program session `session` names, or a new one
    /// when it names none: the run, and the session it ran in.
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
    ) -> Result<(Value, String), ApiError> {
        if self.pinned().is_some_and(|pinned| pinned != workspace) {
            return Err(beyond_scope());
        }
        self.touch();
        let session = self.program_session(workspace, session).await?;
        let ran = self
            .typed_in(workspace, Some(&session), "tilo", arguments, group)
            .await?;
        Ok((ran, session))
    }

    /// The program session `asked` names in `workspace`, now the most
    /// recently used, or a new one when it names none. A new one beyond
    /// [`MAX_PROGRAM_SESSIONS`] ends the least recently used.
    async fn program_session(
        &self,
        workspace: &str,
        asked: Option<&str>,
    ) -> Result<String, ApiError> {
        let mut sessions = self.programs.lock().await;
        let id = match asked {
            Some(asked) => {
                let at = sessions
                    .iter()
                    .position(|(known, id)| known == workspace && id == asked)
                    .ok_or_else(|| {
                        ApiError::new(
                            "not_found",
                            "that program session ended: its lets and references are gone; \
                             leave `session` out to start a new one",
                        )
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
        }
        Ok(id)
    }

    /// End the run: the agent leaves every workspace. The grant's session
    /// with the server, which its other runs share, ends with the last of
    /// them, or when the grant is revoked.
    pub(crate) async fn close(&self) {
        self.served.lock().await.clear();
    }
}

/// The key a workspace's run is kept under: the workspace, or the queue of
/// one program session there.
fn queue(workspace: &str, program: Option<&str>) -> String {
    program.map_or_else(|| workspace.to_owned(), |id| format!("{workspace} {id}"))
}

/// Why a call of an account session that names no workspace was refused.
const NAME_THE_WORKSPACE: &str = "name the workspace this acts in: `workspace`, its id or its \
name as account/workspaces#list answers them; only paths under `account` need none";

/// Whether a call with `arguments` and no workspace walks the account's tree: a path under
/// `account`, or none, which lists it.
fn on_account(arguments: Option<&Value>) -> bool {
    let path = arguments
        .and_then(|arguments| arguments.get("path"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    path.is_empty() || path.split(['/', '#']).next() == Some("account")
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
`tilo_ls workspace` (with `workspace`) lists the workspace's governance beside its apps, as any \
path lists: its people (`workspace/members`), its open invitations (`workspace/invitations`, whose \
`invite {role}` answers the `link` that brings a person in), its wait list (`workspace/waiting`), \
the servers keeping it (`workspace/servers`), the versions of apps and the bots it \
uses (`workspace/apps`) and its mail accounts (`workspace/mail-accounts`). \
A command an agent runs in a workspace's Linux machine (`terminal#run`) asks the person each \
time; `tilo_do {path: \"account/workspaces#unattended\", args: {workspace, on: true}}` asks them once \
to let keys minted for that workspace alone run commands there without asking (and in branches \
made from it), answering `pending` until they decide; this key itself is still asked, and \
`{on: false}` asks again from the next command. \
`tilo_ls invocations` (with `workspace`, `limit` and `cursor`) lists a workspace's invocations in records this \
key may read, newest first, whoever made them, and `invocations#list` answers a page of them whole: who made \
it, how it stands \
(`waiting` for the person, `running`, `ended`, `failed`, `cancelled`, `undone`), what it changed and \
whether it can still be taken back; its `id` is the invocation `tilo_status` reads, stops (`cancel`) \
and takes back (`undo`). \
A call that changed a record answers its `link`, and `tilo_link {path, workspace}` answers the link \
to anything a path names: hand it to the person reviewing your work. It opens the record in its \
app for the workspace's members and carries no secret, so invite the person first. \
Files go in with `tilo_put_begin`/`_append`/`_finish` (or `PUT /v1/workspaces/<workspace>/contents/<hash>`), answering \
a `tilo.content:<hash>` an action takes; an action that makes a file (Writer's `export-content`) \
names its `tilo.content:` in the answer's resources, and `tilo_get {content, offset}` reads it back.";

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
