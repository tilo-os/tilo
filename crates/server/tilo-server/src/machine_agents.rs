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
//! - **The question.** It is put to the person's approving devices as any question of this
//!   server's is ([`CertificationProtocol::confirm_for`]), naming the agent the connection would
//!   be (`reviewer@dev#1`), the environment and the program. "Always allow" is remembered for
//!   that person per environment, executable and client name, so allowing Reviewer once in
//!   *dev* covers its later sessions there, and only there.
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
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_actions::remote::Caller;
use tilo_agent_session::WorkspaceActions;
use tilo_lock::MutexRecover as _;
use tilo_mcp::McpServer;
use tilo_mcp::session_api::{Hello, proposed_agent_id};
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
pub use ush::hosted_vm::{AgentConnection, MachineAgents};

use crate::WorkspaceId;
use crate::certification::CertificationProtocol;
use crate::workspaces::Workspaces;
use tilo_workspace_service::agents::WorkspaceActionProvider;
use tilo_workspace_service::agents::agent_keys::{self, AgentSigner, Signed};

/// How long the person has to answer whether a program may work in their workspace.
const ANSWER_WAIT: Duration = Duration::from_secs(2 * 60);
/// How often a question is looked at for a client that left while it was asked.
const LEFT_EVERY: Duration = Duration::from_millis(250);
/// Where the answers the person said to keep are kept, under the server's state.
const REMEMBERED: &str = "machine-agents.json";

/// What the person's devices are asked: the agent asks to work in the workspace.
const WORKS_HERE: &str = "work in this workspace as an agent of its own";

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
}

/// What a connection is served through: the server's own services.
struct Bound {
    workspaces: Workspaces,
    actions: WorkspaceActions,
    certification: CertificationProtocol,
    runtime: tokio::runtime::Handle,
}

/// The person whose Terminal session started a shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Starter {
    person: [u8; 32],
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
        }
    }

    /// Serve through the server's own workspaces, their actions and its questions to people,
    /// waiting on `runtime`. Bound once; a later call keeps the first.
    pub fn bind(
        &self,
        workspaces: Workspaces,
        actions: WorkspaceActions,
        certification: CertificationProtocol,
        runtime: tokio::runtime::Handle,
    ) {
        let _ = self.bound.set(Bound {
            workspaces,
            actions,
            certification,
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
            Starter { person },
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
    ) -> Result<McpServer<Signed<WorkspaceActionProvider>>, String> {
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
        if !kept {
            let answer = Self::ask(bound, starter.person, (&agent, &name), from, connected)?;
            match answer {
                ConfirmationAnswer::AllowAlways => self.remember(remembered),
                ConfirmationAnswer::AllowOnce => {}
                ConfirmationAnswer::Deny
                | ConfirmationAnswer::Reply(_)
                | ConfirmationAnswer::Secret(_) => {
                    return Err(format!(
                        "the person did not let {name} work in the workspace"
                    ));
                }
            }
        }
        let session = format!("machine-{}", uuid::Uuid::new_v4().simple());
        // It signs every call under a key of its own, certified by its person's root for this
        // agent in this workspace: what it may do is what that certificate gives it.
        let (signer, caller) = agent_keys::this_device().and_then(|keys| {
            AgentSigner::grant(
                keys,
                &from.workspace,
                (&agent, &name),
                &hex::encode(starter.person),
            )
        })?;
        let server = McpServer::for_delegated(
            Signed::new(
                WorkspaceActionProvider::new(bound.actions.clone(), workspace_id),
                Some(signer),
            ),
            caller.in_agent_session(&session),
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

    /// Put to `person`'s approving devices whether `agent` may work in the workspace `from`
    /// names, withdrawn when its client leaves first.
    fn ask(
        bound: &Bound,
        person: [u8; 32],
        (agent, name): (&str, &str),
        from: &AgentConnection,
        connected: &dyn Fn() -> bool,
    ) -> Result<ConfirmationAnswer, String> {
        let mut id = [0_u8; 32];
        getrandom::fill(&mut id).map_err(|error| error.to_string())?;
        let workspace = bound
            .runtime
            .block_on(bound.workspaces.summary(&from.workspace))
            .map_or_else(|_| from.workspace.clone(), |summary| summary.name);
        let question = PendingConfirmation {
            id,
            agent_id: agent.to_owned(),
            agent_label: name.to_owned(),
            action_label: WORKS_HERE.to_owned(),
            resource_label: workspace.clone(),
            arguments: vec![
                ("Environment".to_owned(), from.environment.clone()),
                ("Program".to_owned(), from.program.clone()),
            ],
            risk: "privileged".to_owned(),
            workspace,
            expires_at_ns: tilo_clock::unix_now_ns()
                .saturating_add(u64::try_from(ANSWER_WAIT.as_nanos()).unwrap_or(u64::MAX)),
            allow_always: true,
            ..PendingConfirmation::default()
        };
        bound.runtime.block_on(async {
            let left = async {
                while connected() {
                    tokio::time::sleep(LEFT_EVERY).await;
                }
            };
            tokio::select! {
                answer = bound.certification.confirm_for(person, question) => answer,
                () = left => Err("the program left before the person answered".to_owned()),
            }
        })
    }

    /// Keep `remembered` for the person's later sessions there.
    fn remember(&self, remembered: Remembered) {
        let kept = {
            let mut held = self.remembered.lock_recover();
            held.insert(remembered);
            Kept {
                allowed: held.iter().cloned().collect(),
            }
        };
        let Some(path) = &self.path else {
            return;
        };
        let written = serde_json::to_vec_pretty(&kept)
            .map_err(std::io::Error::other)
            .and_then(|bytes| tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600));
        if let Err(error) = written {
            tracing::warn!(%error, path = %path.display(), "what a person let into their machine is kept until this server restarts");
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
