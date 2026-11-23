//! Agents a person runs in their workspace's machine: an agent's command typed in a hosted
//! workspace's Terminal, as an agent of the workspace's own.
//!
//! A program a Terminal session starts in the workspace's machine finds Tilo's MCP server at the
//! machine's agent socket, as an environment's programs do on a device
//! (`docs/app-model/environments.md`, "Agents in an environment"). Each connection one of its
//! processes makes there reaches this server ([`ush::hosted_vm::MachineAgents`]), named by the workspace, the
//! environment and the shell session it came from, and the program that made it.
//!
//! - **Who decides.** The person whose Terminal session started the program: the one whose `run`
//!   or `open` the workspace admitted for that session ([`MachineAgentsServer::started`]). Nobody
//!   else is asked and nothing is admitted on the host's own authority; a connection from a
//!   session nobody is known to have started is refused before anyone is asked.
//! - **The question.** The connection waits for the person's yes as every call that needs it
//!   does: kept as its asked `tilo.invocations` record in the workspace, naming the agent the
//!   connection would be (`reviewer@dev#1`), the environment and the program. Their approve of
//!   the record lets it in, their decline refuses it. An "always" kept before is remembered for
//!   that person per environment, executable and client name, so it covers Reviewer's later
//!   sessions in *dev*, and only there.
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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_actions::ActionAddress;
use tilo_actions::remote::asked::{AskedBy, AskedDispatch, AskedFacts, Fact, Named};
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationId, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_lock::MutexRecover as _;
use tilo_mcp::McpServer;
use tilo_mcp::session_api::{Hello, proposed_agent_id};
pub use ush::hosted_vm::{AgentConnection, MachineAgents};

use crate::WorkspaceId;
use crate::workspaces::Workspaces;
use tilo_workspace_service::agents::{EngineActions, WorkspaceActionProvider};

/// How long the person has to answer whether a program may work in their workspace.
const ANSWER_WAIT: Duration = Duration::from_secs(2 * 60);
/// How often a question is looked at for a client that left while it was asked.
const LEFT_EVERY: Duration = Duration::from_millis(250);
/// Where the answers the person said to keep are kept, under the server's state.
const REMEMBERED: &str = "machine-agents.json";

/// What the person is asked: the agent asks to work in the workspace.
const WORKS_HERE: &str = "work in this workspace as an agent of its own";

/// The provider a connection's question is the call of, and the dispatch name its record is
/// kept under: its approve comes back here.
const PROVIDER: &str = "tilo.machine-agents";
/// The action a connection asks for: to work in the workspace.
const WORK: &str = "work";

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
    /// The answers the people said to keep.
    remembered: Mutex<BTreeSet<Remembered>>,
    /// Where those are kept; nowhere for a server that keeps no state.
    path: Option<PathBuf>,
    /// The questions waiting for their person's answer, by the invocation each is the call of.
    answers: Arc<Answers>,
}

/// The connections whose question waits, each handed whether its person let it in.
#[derive(Default)]
struct Answers(Mutex<HashMap<InvocationId, mpsc::SyncSender<bool>>>);

impl Answers {
    fn answer(&self, invocation: InvocationId, allowed: bool) -> Result<(), ActionCallError> {
        let waiting = self.0.lock_recover().remove(&invocation);
        let waiting = waiting.ok_or_else(|| ActionCallError::Refused {
            reason: "the program no longer waits for this answer".into(),
        })?;
        let _ = waiting.send(allowed);
        Ok(())
    }
}

impl AskedDispatch for Answers {
    fn context(&self, _: InvocationId) -> serde_json::Value {
        serde_json::Value::Null
    }

    fn readmit(
        &self,
        _: &Caller,
        request: InvokeRequest,
        _: &serde_json::Value,
    ) -> Result<(), ActionCallError> {
        self.answer(request.invocation, true)
    }

    fn declined(&self, _: &Caller, invocation: InvocationId) -> Result<(), ActionCallError> {
        self.answer(invocation, false)
    }
}

/// What a connection is served through: the server's own services.
struct Bound {
    workspaces: Workspaces,
    actions: EngineActions,
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

/// "Always allow", as one person said it: this client, this executable, in this environment of
/// this workspace.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Remembered {
    person: String,
    workspace: String,
    environment: String,
    program: String,
    client: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Kept {
    allowed: Vec<Remembered>,
}

impl MachineAgentsServer {
    /// Serving no connection until [`Self::bind`]; the answers the people said to keep are kept
    /// under `state`, when it names one.
    #[must_use]
    pub fn new(state: Option<PathBuf>) -> Self {
        let path = state.map(|state| state.join(REMEMBERED));
        let remembered = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Kept>(&bytes).ok())
            .map(|kept| kept.allowed.into_iter().collect())
            .unwrap_or_default();
        Self {
            bound: OnceLock::new(),
            starters: Mutex::default(),
            instances: Mutex::default(),
            running: Mutex::default(),
            remembered: Mutex::new(remembered),
            path,
            answers: Arc::default(),
        }
    }

    /// Serve through the server's own workspaces and their actions, waiting on `runtime`.
    /// Bound once; a later call keeps the first.
    pub fn bind(
        &self,
        workspaces: Workspaces,
        actions: EngineActions,
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

    /// Who is let in, and as whom: the person who started the session is asked unless they said
    /// to keep their yes, and once they allow it the connection is the next agent of its
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
        let remembered = Remembered {
            person: hex::encode(starter.person),
            workspace: from.workspace.clone(),
            environment: from.environment.clone(),
            program: from.program.clone(),
            client: client.clone(),
        };
        let number = {
            let mut instances = self.instances.lock_recover();
            let number = instances.entry(from.workspace.clone()).or_default();
            *number = number.saturating_add(1);
            let next = *number;
            drop(instances);
            next
        };
        let agent = format!(
            "{}@{}#{number}",
            proposed_agent_id(&client),
            from.environment
        );
        let name = hello
            .session_name
            .clone()
            .unwrap_or_else(|| format!("{client} in {} #{number}", from.environment));
        let kept = self.remembered.lock_recover().contains(&remembered);
        if !kept && !self.ask(bound, starter, (&agent, &name), from, connected)? {
            return Err(format!(
                "the person did not let {name} work in the workspace"
            ));
        }
        let device = match starter.device {
            Some(device) => device,
            None => bound
                .runtime
                .block_on(bound.workspaces.person_device(workspace_id, starter.person))
                .ok_or("the person who started the session is admitted here on no device")?,
        };
        let session = format!("machine-{}", uuid::Uuid::new_v4().simple());
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

    /// Ask `starter`'s person whether `agent` may work in the workspace `from` names: the
    /// connection waits as its asked record until they answer, or until its client leaves.
    fn ask(
        &self,
        bound: &Bound,
        starter: Starter,
        (agent, name): (&str, &str),
        from: &AgentConnection,
        connected: &dyn Fn() -> bool,
    ) -> Result<bool, String> {
        let store = bound
            .workspaces
            .asked_store()
            .ok_or("this server keeps no questions for its people")?;
        let dispatch: std::sync::Weak<dyn AskedDispatch> =
            Arc::downgrade(&(Arc::clone(&self.answers) as Arc<dyn AskedDispatch>));
        let records = bound
            .workspaces
            .told(store.for_dispatch(PROVIDER, dispatch));
        let device = starter.device.unwrap_or(starter.person);
        let caller =
            Caller::authenticated(starter.person, device).in_workspace(from.workspace.clone());
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: ActionAddress {
                    resource: from.environment.clone(),
                    action: WORK.to_owned(),
                },
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let facts = AskedFacts {
            words: format!("{name} asks to {WORKS_HERE}."),
            on: Some(from.environment.clone()),
            facts: vec![
                Fact::new("Environment", from.environment.clone()),
                Fact::new("Program", from.program.clone()),
            ],
            verb: "let in".to_owned(),
            ..AskedFacts::by(AskedBy::Agent(Named {
                id: agent.to_owned(),
                name: name.to_owned(),
            }))
        };
        let (answered, answer) = mpsc::sync_channel(1);
        self.answers
            .0
            .lock_recover()
            .insert(request.invocation, answered);
        if let Err(error) = records.write_asked(&from.workspace, &caller, &request, &facts) {
            self.answers.0.lock_recover().remove(&request.invocation);
            return Err(error);
        }
        let waited_until = std::time::Instant::now() + ANSWER_WAIT;
        loop {
            match answer.recv_timeout(LEFT_EVERY) {
                Ok(allowed) => return Ok(allowed),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(false),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if !connected() {
                        self.answers.0.lock_recover().remove(&request.invocation);
                        return Err("the program left before the person answered".to_owned());
                    }
                    if std::time::Instant::now() >= waited_until {
                        self.answers.0.lock_recover().remove(&request.invocation);
                        return Err("the person did not answer in time".to_owned());
                    }
                }
            }
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
