//! Agents a person runs in their workspace's machine: an agent's command typed in a hosted
//! workspace's Terminal, as an agent of the workspace's own.
//!
//! A program a Terminal session starts in the workspace's machine finds Tilo's MCP server at the
//! machine's agent socket, as an environment's programs do on a device
//! (`docs/app-model/environments.md`, "Agents in an environment"). Each connection one of its
//! processes makes there reaches this server ([`ush::hosted_vm::MachineAgents`]), named by the
//! workspace, the environment and the shell session it came from, and the program that made it.
//!
//! - **Who decides.** The person whose Terminal session started the program: the one whose `run`
//!   or `open` the workspace admitted for that session
//!   ([`MachineAgentsServer::started`]). Nobody else is asked and nothing is admitted on the
//!   host's own authority; a connection from a session nobody is known to have started is
//!   refused before anyone is asked.
//! - **The call.** A connection asks to work here as an action of this host's
//!   ([`PROVIDER`]`#`[`LET_IN`]), naming the environment it comes from and the program it is,
//!   made as the agent it would be (`claude-code@dev`, without the instance number, so the
//!   person's "always" covers its later connections there). The host answers it as one of its
//!   own actions ([`let_in`]), leaving it to the person until they answer it, so the dispatch
//!   keeps it as its own `tilo.invocations` record in status asked: their approve runs it, and
//!   their decline ends it declined. Nothing is remembered here beside that record: "always"
//!   is the grant their approve places, read by the next call as any grant is.
//! - **An agent per connection.** Each connection admitted is an agent of its own: the client's
//!   name, the environment's record key and an instance number counting from one in each
//!   workspace since this server started (`reviewer@dev#2`). It acts as that agent for the
//!   person, through the workspace's actions: held to the person's access and an agent's bound,
//!   attributed to its own id, never the person's and never the host's.
//! - **Where it shows, and Stop.** Each invocation it makes is present in the workspace under the
//!   agent's name, as every agent session the server executes for is (the workspace actions'
//!   own presence). [`MachineAgentsServer::running`] lists the agents working now and
//!   [`MachineAgentsServer::stop`] ends one alone; each ends with its program, so with the
//!   session or the machine that ran it.

use std::collections::{BTreeMap, HashMap};
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::remote::asked::{AskedBy, AskedFacts, Fact, Named};
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationId, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationUpdate};
use tilo_agent_session::WorkspaceActions;
use tilo_lock::MutexRecover as _;
use tilo_mcp::McpServer;
use tilo_mcp::session_api::{Hello, proposed_agent_id};
pub use ush::hosted_vm::{AgentConnection, MachineAgents};

use crate::WorkspaceId;
use crate::workspaces::Workspaces;
use tilo_workspace_service::agents::WorkspaceActionProvider;

/// The provider this host answers a program's ask to work in its workspace's machine under.
pub const PROVIDER: &str = "tilo.machine-agents";
/// The kind of resource [`LET_IN`] is an action on, as the person's grant names it.
pub const ENVIRONMENT_KIND: &str = "environment";
/// The action a program asks to work in the workspace's machine by.
pub const LET_IN: &str = "let-in";
/// The verb "Always let {asker} {verb} {plural}" reads.
pub const LET_IN_VERB: &str = "let in";
/// The plural of what [`LET_IN`] is an action on, which "always" is offered for.
pub const LET_IN_PLURAL: &str = "environments";

/// How long a follow of the asked call waits before it looks again for its client.
const FOLLOW_WAIT_MS: u32 = 1_000;
/// What an invocation the person declined ends with, as the dispatch says it.
const DECLINED: &str = "declined";

/// What a program asks to work in the workspace's machine for: itself, and the client that
/// speaks for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LetIn {
    /// The program, as the machine names its executable (`/home/tilo/.local/bin/claude`).
    pub program: String,
    /// The client, as its own `initialize` names it (`Claude Code`).
    pub client: String,
}

/// Who serves the connections the workspaces' programs make to their machine's agent socket.
pub struct MachineAgentsServer {
    /// What it serves them through, once the server runs.
    bound: OnceLock<Bound>,
    /// Who started each workspace's shell session, by workspace and session.
    starters: Mutex<HashMap<(String, String), Starter>>,
    /// The last instance number each workspace's agents reached.
    instances: Mutex<HashMap<String, u64>>,
    /// The agents working now, by workspace and agent id.
    running: Mutex<BTreeMap<(String, String), Running>>,
}

/// What a connection is served through: the server's own services.
struct Bound {
    workspaces: Workspaces,
    actions: WorkspaceActions,
    runtime: tokio::runtime::Handle,
}

/// The person whose Terminal session started a shell, and the device they did it on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Starter {
    person: [u8; 32],
    device: Option<[u8; 32]>,
}

/// An agent working now.
struct Running {
    person: [u8; 32],
    name: String,
    environment: String,
    /// Its connection, shut to stop it.
    stream: UnixStream,
}

/// One agent at work in a workspace's machine, as [`MachineAgentsServer::running`] lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineAgent {
    /// Its id: `reviewer@dev#1`.
    pub agent: String,
    /// What it is shown by: `Reviewer in dev #1`, or the session its client named.
    pub name: String,
    /// The environment's record key it works from.
    pub environment: String,
    /// The person it acts for, by their key in hex.
    pub person: String,
}

/// Answer `request`, a program asking to work in the workspace's machine as an agent of its
/// own ([`PROVIDER`]`#`[`LET_IN`]): what the person let it in for good, or refused for want of
/// their yes showing them who asks, which the dispatch keeps as the call's asked record.
///
/// # Errors
///
/// Why it cannot be answered: the request names no such action, its argument is not a
/// program's, or nobody could ask for it.
pub fn let_in(
    workspaces: &Workspaces,
    runtime: &tokio::runtime::Handle,
    caller: &Caller,
    request: &InvokeRequest,
) -> Result<Value, ActionCallError> {
    let let_in: LetIn = match &request.argument {
        InvokeArgument::Json(arguments) => serde_json::from_value(Value::Object(arguments.clone()))
            .map_err(|error| ActionCallError::InvalidRequest {
                reason: format!("a program asking to work here: {error}"),
            })?,
        InvokeArgument::None | InvokeArgument::Text(_) => {
            return Err(ActionCallError::InvalidRequest {
                reason: "a program asking to work here names itself and its client".into(),
            });
        }
    };
    let environment = request.target.address.resource.clone();
    if request.target.address.action != LET_IN || environment.is_empty() {
        return Err(ActionCallError::UnknownAction);
    }
    let Some(agent) = caller.acting().agent_id().map(str::to_owned) else {
        return Err(ActionCallError::NotPermitted);
    };
    let name = caller.agent_name().map_or_else(
        || format!("{} in {environment}", let_in.client),
        str::to_owned,
    );
    if request.approval().is_none()
        && !let_in_for_good(workspaces, runtime, caller, &agent, &environment)
    {
        return Err(ActionCallError::ConfirmationRequired(Box::new(
            AskedFacts {
                by: AskedBy::Agent(Named { id: agent, name }),
                on: Some(environment.clone()),
                facts: vec![
                    Fact::new("Environment", environment),
                    Fact::new("Program", let_in.program),
                ],
                verb: LET_IN_VERB.to_owned(),
                plural: Some(LET_IN_PLURAL.to_owned()),
                // What "always" is scoped to: this action on every environment's name.
                kind: ENVIRONMENT_KIND.to_owned(),
                effect: Some(tilo_actions::ActionKind::Outside),
                ..AskedFacts::default()
            },
        )));
    }
    Ok(serde_json::json!({"agent": agent, "name": name}))
}

/// Whether the person let `agent` in for good in the workspace `caller` acts in: a grant of
/// theirs for this action, read as any grant is.
fn let_in_for_good(
    workspaces: &Workspaces,
    runtime: &tokio::runtime::Handle,
    caller: &Caller,
    agent: &str,
    environment: &str,
) -> bool {
    let Some(workspace) = caller.workspace() else {
        return false;
    };
    let node = tilo_actions::policy::TreePath::workspace(workspace)
        .app(PROVIDER)
        .kind(ENVIRONMENT_KIND)
        .resource(environment)
        .action(LET_IN);
    runtime.block_on(workspaces.granted(workspace, agent, &node))
}

impl Default for MachineAgentsServer {
    fn default() -> Self {
        Self::new()
    }
}

impl MachineAgentsServer {
    /// Serving no connection until [`Self::bind`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            bound: OnceLock::new(),
            starters: Mutex::default(),
            instances: Mutex::default(),
            running: Mutex::default(),
        }
    }

    /// Serve through the server's own workspaces, their actions and its answers to the person,
    /// waiting on `runtime`. Bound once; a later call keeps the first.
    pub fn bind(
        &self,
        workspaces: Workspaces,
        actions: WorkspaceActions,
        runtime: tokio::runtime::Handle,
    ) {
        let _ = self.bound.set(Bound {
            workspaces,
            actions,
            runtime,
        });
    }

    /// `caller` started `workspace`'s shell session `session`: a run or an open of its Terminal
    /// the workspace admitted. They are the person its programs' agents ask, the newest such
    /// caller for the session.
    pub fn started(&self, workspace: &str, session: &str, caller: &Caller) {
        let Some(person) = caller.user_public_key().copied() else {
            return;
        };
        self.starters.lock_recover().insert(
            (workspace.to_owned(), session.to_owned()),
            Starter {
                person,
                device: caller.device_public_key().copied(),
            },
        );
    }

    /// The shell sessions of `workspace` whose starter is known, each with that person.
    #[must_use]
    pub fn sessions(&self, workspace: &str) -> Vec<(String, String)> {
        self.starters
            .lock_recover()
            .iter()
            .filter(|((held, _), _)| held == workspace)
            .map(|((_, session), starter)| (session.clone(), hex::encode(starter.person)))
            .collect()
    }

    /// The agents at work in `workspace`'s machine now.
    #[must_use]
    pub fn running(&self, workspace: &str) -> Vec<MachineAgent> {
        self.running
            .lock_recover()
            .iter()
            .filter(|((held, _), _)| held == workspace)
            .map(|((_, agent), running)| MachineAgent {
                agent: agent.clone(),
                name: running.name.clone(),
                environment: running.environment.clone(),
                person: hex::encode(running.person),
            })
            .collect()
    }

    /// Stop `agent` in `workspace`, alone: its connection ends, and every other goes on. Whether
    /// it was at work.
    pub fn stop(&self, workspace: &str, agent: &str) -> bool {
        let stopped = self
            .running
            .lock_recover()
            .remove(&(workspace.to_owned(), agent.to_owned()));
        stopped.is_some_and(|running| {
            let _ = running.stream.shutdown(std::net::Shutdown::Both);
            true
        })
    }

    /// Who is let in, and as whom: the person who started the session is asked about the agent
    /// the connection would be, and once they let it in the connection is the next agent of its
    /// program in this workspace, acting for them.
    fn admit(
        &self,
        bound: &Bound,
        from: &AgentConnection,
        stream: &UnixStream,
        (hello, connected): (&Hello, &dyn Fn() -> bool),
    ) -> Result<McpServer<WorkspaceActionProvider>, String> {
        let starter = self
            .starters
            .lock_recover()
            .get(&(from.workspace.clone(), from.session.clone()))
            .copied()
            .ok_or("this program's Terminal session was started by nobody this workspace knows")?;
        let workspace_id = WorkspaceId::parse(&from.workspace).ok_or("not a workspace id")?;
        let client = hello.client_name.clone();
        let device = match starter.device {
            Some(device) => device,
            None => bound
                .runtime
                .block_on(bound.workspaces.person_device(workspace_id, starter.person))
                .ok_or("the person who started the session is admitted here on no device")?,
        };
        let session = format!("machine-{}", uuid::Uuid::new_v4().simple());
        // The agent as it asks to work here: one per client in the environment, so the person's
        // "always" covers its later connections there, and no instance number of its own.
        let asking = format!("{}@{}", proposed_agent_id(&client), from.environment);
        let asking_name = format!("{client} in {}", from.environment);
        let caller = Caller::authenticated(starter.person, device)
            .in_workspace(from.workspace.clone())
            .delegated_to(asking.clone())
            .named(&asking_name)
            .in_agent_session(&session);
        Self::let_in(bound, workspace_id, &caller, &client, from, connected)?;
        let number = {
            let mut instances = self.instances.lock_recover();
            let number = instances.entry(from.workspace.clone()).or_default();
            *number = number.saturating_add(1);
            let next = *number;
            drop(instances);
            next
        };
        let agent = format!("{asking}#{number}");
        let name = hello
            .session_name
            .clone()
            .unwrap_or_else(|| format!("{client} in {} #{number}", from.environment));
        let caller = Caller::authenticated(starter.person, device)
            .in_workspace(from.workspace.clone())
            .delegated_to(agent.clone())
            .named(&name)
            .in_agent_session(&session);
        let server = McpServer::for_delegated(
            WorkspaceActionProvider::new(bound.actions.clone(), workspace_id),
            caller,
        )
        .map_err(|error| error.to_string())?
        .with_agent_session(session);
        let stream = stream.try_clone().map_err(|error| error.to_string())?;
        self.running.lock_recover().insert(
            (from.workspace.clone(), agent.clone()),
            Running {
                person: starter.person,
                name,
                environment: from.environment.clone(),
                stream,
            },
        );
        tracing::info!(workspace = %from.workspace, %agent, "a program in the workspace's machine works as its own agent");
        Ok(server)
    }

    /// Put to the person who started `from`'s session whether the program may work in the
    /// workspace's machine as `caller`'s agent, waiting on their answer while its client is
    /// still here; a client that leaves first withdraws its ask.
    fn let_in(
        bound: &Bound,
        workspace_id: WorkspaceId,
        caller: &Caller,
        client: &str,
        from: &AgentConnection,
        connected: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        let invocation = InvocationId::new();
        let request = InvokeRequest {
            invocation,
            target: RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: ActionAddress::new(from.environment.clone(), LET_IN),
            },
            argument: InvokeArgument::Json(Map::from_iter([
                ("program".to_owned(), Value::from(from.program.clone())),
                ("client".to_owned(), Value::from(client)),
            ])),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let status = Self::answer(
            bound,
            workspace_id,
            caller,
            RemoteActionMessage::Invoke(request),
        )?;
        match status {
            ReceiptStatus::Completed { .. } => return Ok(()),
            // The ask had nowhere to be kept, so nobody can answer it: the connection is told
            // so rather than left waiting for an answer that cannot come.
            ReceiptStatus::Rejected {
                error: ActionCallError::ConfirmationRequired(_),
            } => {
                return Err(
                    "this host keeps no record of what it asks, so nobody can let the program in"
                        .to_owned(),
                );
            }
            ReceiptStatus::Rejected { error } => return Err(error.to_string()),
            ReceiptStatus::Failed { error } => return Err(error.message),
            ReceiptStatus::Cancelled { message } => {
                return Err(message.unwrap_or_else(|| "the ask was withdrawn".to_owned()));
            }
            // Kept as its asked record: it runs once the person approves it.
            ReceiptStatus::Accepted => {}
        }
        loop {
            if !connected() {
                if let Some(asked) = bound.actions.serve(
                    workspace_id,
                    caller,
                    RemoteActionMessage::Cancel { invocation },
                ) {
                    let _ = bound.runtime.block_on(asked);
                }
                return Err("the program left before the person answered".to_owned());
            }
            let Some(state) = Self::follow(bound, workspace_id, caller, invocation)? else {
                return Err("this host let go of the question before it was answered".to_owned());
            };
            match state.update {
                InvocationUpdate::Succeeded { .. } => return Ok(()),
                InvocationUpdate::Failed { error, .. } => return Err(error.message),
                InvocationUpdate::Cancelled { message, .. } => {
                    return Err(message
                        .filter(|message| message != DECLINED)
                        .unwrap_or_else(|| {
                            format!(
                                "the person did not let {} work in the workspace",
                                caller.agent_name().unwrap_or("the program")
                            )
                        }));
                }
                InvocationUpdate::Accepted { .. }
                | InvocationUpdate::Progress { .. }
                | InvocationUpdate::Item { .. } => {}
            }
        }
    }

    /// `message`, asked of the workspace's actions for `caller`, and the receipt it answers with.
    fn answer(
        bound: &Bound,
        workspace_id: WorkspaceId,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Result<ReceiptStatus, String> {
        let Some(asked) = bound.actions.serve(workspace_id, caller, message) else {
            return Err("the workspace's actions do not answer that".to_owned());
        };
        let answered = bound
            .runtime
            .block_on(asked)
            .ok_or("the workspace's actions let go of that")?;
        match answered {
            RemoteActionMessage::Receipt(receipt) => Ok(receipt.status),
            other => Err(format!("the workspace's actions answered {other:?}")),
        }
    }

    /// `invocation`'s state so far, waited on for a moment: `None` where the host no longer
    /// has it.
    fn follow(
        bound: &Bound,
        workspace_id: WorkspaceId,
        caller: &Caller,
        invocation: InvocationId,
    ) -> Result<Option<InvocationState>, String> {
        let asked = RemoteActionMessage::Observe {
            correlation: 0,
            invocation,
            wait_ms: Some(FOLLOW_WAIT_MS),
            seen: None,
        };
        let Some(followed) = bound.actions.serve(workspace_id, caller, asked) else {
            return Err("the workspace's actions do not answer that".to_owned());
        };
        let answered = bound
            .runtime
            .block_on(followed)
            .ok_or("the workspace's actions let go of that")?;
        match answered {
            RemoteActionMessage::Update {
                state: Some(state), ..
            } => Ok(Some(state)),
            RemoteActionMessage::Update { state: None, .. } => Ok(None),
            other => Err(format!("the workspace's actions answered {other:?}")),
        }
    }
}

impl MachineAgents for MachineAgentsServer {
    fn serve(&self, from: AgentConnection, stream: UnixStream) {
        let Some(bound) = self.bound.get() else {
            tracing::info!(workspace = %from.workspace, "a program in a workspace's machine connected before this server serves");
            return;
        };
        let held = match stream.try_clone() {
            Ok(held) => held,
            Err(error) => {
                tracing::warn!(%error, "a machine's agent connection could not be held");
                return;
            }
        };
        let admitted = Mutex::new(None::<String>);
        let served = tilo_mcp::session_api::serve_as(stream, None, |hello, connected| {
            let server = self.admit(bound, &from, &held, (hello, connected));
            match &server {
                Ok(server) => {
                    *admitted.lock_recover() = server
                        .execution_caller()
                        .acting()
                        .agent_id()
                        .map(str::to_owned);
                }
                Err(reason) => {
                    tracing::info!(workspace = %from.workspace, environment = %from.environment, %reason, "a program in the workspace's machine was not let in");
                }
            }
            server
        });
        if let Some(agent) = admitted
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            self.running.lock_recover().remove(&(from.workspace, agent));
        }
        if let Err(error) = served {
            tracing::debug!(%error, "a machine's agent connection ended");
        }
    }
}
