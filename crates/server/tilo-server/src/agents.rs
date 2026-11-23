//! Agents on this host (feature `agents`): the link between the server and
//! `tilo-agent-session`'s [`AgentService`](tilo_agent_session::AgentService).
//!
//! [`ServerAgentLink`] is what the supervisor asks of the server:
//!
//! - **Approval.** A new grant is a certification request for a fresh agent
//!   key, queued by the host itself with the person's [`DelegateProposal`]. A
//!   root-holding approver answers it with the agent's certificate and the
//!   delegation together (`ApproverDecision::IssueDelegate`), so consent is one
//!   step and the host never holds the root.
//! - **Admission.** Approved delegations are registered in the server's agent
//!   register, which confines the device to its workspaces; withdrawn ones are
//!   refused from the next access audit on. Each workspace this server governs
//!   that the delegation names puts the agent on its governed roster
//!   (`WorkspaceOperation::DelegateMembership`), and every change to its grant is
//!   carried onto those rosters: a withdrawn or narrowed grant takes it off
//!   the workspace and its documents.
//! - **Asking first.** An action the person's grants leave to them is put
//!   to their approving device as a question for that one invocation
//!   ([`CertificationProtocol::confirm`]); the agent runs it only on the
//!   approved answer, and a question nobody answers in time is refused.

use std::path::PathBuf;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_actions::remote::Caller;

use tilo_agent_session::shell::{Opened, Opening, Recorder, ShellRan, Watching, WorkspaceShell};
use tilo_agent_session::{AgentHostLink, ApprovalDone};

use tilo_server_protocol::delegation::DelegateProposal;
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationRequest, DeviceCapabilities, DeviceCert,
};

use crate::certification::CertificationProtocol;
use crate::daemon::Server;
use crate::registry::AgentRegistry;
use tilo_lock::RwLockRecover as _;

mod roster;

/// How long an agent's approval request waits for an approver.
const APPROVAL_WAIT_NS: u64 = 10 * 60 * 1_000_000_000;

/// The server as an agent supervisor sees it.
#[derive(Clone)]
pub struct ServerAgentLink {
    certification: CertificationProtocol,
    agents: AgentRegistry,
    /// The workspaces this server governs, whose rosters follow each agent's grant.
    workspaces: Option<crate::workspaces::Workspaces>,
    runtime: tokio::runtime::Handle,
}

impl Server {
    /// The link an [`AgentService`](tilo_agent_session::AgentService) supervises this server's agents through.
    /// Call it inside the runtime the server runs on.
    #[must_use]
    pub fn agent_link(&self) -> ServerAgentLink {
        let web = self.web_context();
        ServerAgentLink {
            certification: web.certification.clone(),
            agents: web.server.agents.clone(),
            workspaces: self.workspaces().cloned(),
            runtime: tokio::runtime::Handle::current(),
        }
    }
}

impl ServerAgentLink {
    fn enqueue_approval(
        &self,
        acts_for: [u8; 32],
        key: &SigningKey,
        proposal: &DelegateProposal,
    ) -> Result<([u8; 32], String), String> {
        let now = tilo_clock::unix_now_ns();
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;
        let request = CertificationRequest {
            version: CERTIFICATION_SCHEMA_VERSION,
            user_public_key: acts_for,
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("Agent: {}", proposal.label),
            requested: DeviceCapabilities::nothing(),
            nonce,
            invite: [0; 16],
            issued_at_ns: now,
            expires_at_ns: now.saturating_add(APPROVAL_WAIT_NS),
            signature: [0; 64],
        }
        .sign(key)
        .map_err(|error| error.to_string())?;
        self.certification
            .enqueue_delegate(request, proposal.clone(), now)
    }

    fn withdraw_device(&self, device: [u8; 32]) {
        self.agents.write_recover().insert(device, None);
        crate::authority_epoch::bump();
        self.reconcile_workspaces(device, None);
    }

    /// Bring agent `device`'s place on the rosters of the governed workspaces in
    /// line with the register, off the caller's thread. `cert` is the
    /// agent's certificate when it was just approved or restored.
    fn reconcile_workspaces(&self, device: [u8; 32], cert: Option<DeviceCert>) {
        if let Some(workspaces) = self.workspaces.clone() {
            self.runtime.spawn(async move {
                workspaces.reconcile_agent(device, cert.as_ref()).await;
            });
        }
    }
}

impl AgentHostLink for ServerAgentLink {
    fn request_roster_approval(
        &self,
        key: &SigningKey,
        proposal: &DelegateProposal,
        created_at: i64,
        requester: &DeviceCert,
        done: ApprovalDone,
    ) -> Result<String, String> {
        self.approve_roster(key, proposal, created_at, requester, done)
    }

    fn admit_roster_session(
        &self,
        identity: &tilo_agent_session::AgentIdentity,
        created_at: i64,
        lifetime: tokio::sync::oneshot::Receiver<()>,
        done: tilo_agent_session::grants::RosterAdmissionDone,
    ) {
        self.join_roster(identity, created_at, lifetime, done);
    }

    fn withdraw(&self, device_public_key: [u8; 32]) {
        self.withdraw_device(device_public_key);
    }
}

/// The shells of the workspaces this host serves.
///
/// A host that pays for its workspaces' Linux machines without running them
/// ([`LinuxDevices::from_env`](crate::linux_devices::LinuxDevices::from_env))
/// runs a workspace's shell on the device of the workspace that takes its
/// Linux turn. Otherwise it runs here, its files under `state`, its Linux programs in the hosted VMs the
/// host's environment names (`TILO_HOSTED_VM_SOCKET`, `TILO_HOSTED_ACCOUNT`,
/// `TILO_HOSTED_VM_SIZE`, `TILO_HOSTED_PLANS`), and without those, ush's own
/// commands only.
///
/// # Errors
///
/// The host's shells cannot be set up.
pub fn host_shells(state: PathBuf) -> Result<Arc<HostShells>, String> {
    if let Some(linux) = crate::linux_devices::LinuxDevices::from_env() {
        return Ok(Arc::new(HostShells::at(ShellsAt::Devices(Arc::new(linux)))));
    }
    let agents = Arc::new(crate::machine_agents::MachineAgentsServer::new(Some(
        state.clone(),
    )));
    let shells = ush::workspace_shell::WorkspaceShells::from_env(state)
        .serving_agents(Arc::clone(&agents) as Arc<dyn ush::hosted_vm::MachineAgents>);
    let terminals = ush::environment_terminals::EnvironmentTerminals::new(Arc::new(shells));
    Ok(Arc::new(HostShells::at(ShellsAt::Here(terminals, agents))))
}

/// The shells whose terminal streams a host serves on
/// [`ALPN`](ush::environment_terminals::ALPN), once they run.
///
/// The person's device asks the session's `open-terminal`; the host's shells start the
/// session's shell on a terminal and hand its stream to that device, which dials this host
/// as itself and presents the stream's one-use id: a stream handed to another device is
/// refused. Shells that run here carry the stream themselves; a shell on another device of
/// the workspace is spliced to it through this host's own device.
#[derive(Clone, Default)]
pub struct TerminalShells(Arc<std::sync::OnceLock<Arc<HostShells>>>);

impl TerminalShells {
    /// Serve the terminal streams `shells` hand out.
    pub fn serve_for(&self, shells: Arc<HostShells>) {
        let _ = self.0.set(shells);
    }

    /// What answers [`ALPN`](ush::environment_terminals::ALPN) on the host's endpoint.
    #[must_use]
    pub fn route(&self) -> ush::environment_terminals::quic::Route {
        use ush::environment_terminals::wire;
        let shells = Arc::clone(&self.0);
        ush::environment_terminals::quic::Route::new(move |device, from, to| {
            let served = match shells.get().map(|shells| shells.shells()) {
                Some(ShellsAt::Here(terminals, _)) => {
                    terminals.serve_handed(device, from, to).map(drop)
                }
                Some(ShellsAt::Devices(linux)) => linux.splice(device, from, to),
                None => Err(wire::refuse(to, "this host runs no workspace shells")),
            };
            if let Err(error) = served {
                tracing::debug!(%error, "a terminal stream ended in error");
            }
        })
    }
}

/// Where a host's workspaces run their Terminal sessions' shells.
pub struct HostShells {
    at: ShellsAt,
}

/// Where a host's shells are.
pub enum ShellsAt {
    /// Here: ush's workspace shells, in this process, with their terminals, and who serves
    /// the agents their programs in the workspaces' machines start.
    Here(
        ush::environment_terminals::EnvironmentTerminals,
        Arc<crate::machine_agents::MachineAgentsServer>,
    ),
    /// On the device of the workspace that takes its Linux turn.
    Devices(Arc<crate::linux_devices::LinuxDevices>),
}

impl HostShells {
    /// The shells `at`.
    #[must_use]
    pub const fn at(at: ShellsAt) -> Self {
        Self { at }
    }

    /// Where they are.
    #[must_use]
    pub const fn shells(&self) -> &ShellsAt {
        &self.at
    }

    /// Who serves the agents the programs in the workspaces' machines start, where this host
    /// runs its shells itself.
    #[must_use]
    pub fn machine_agents(&self) -> Option<&Arc<crate::machine_agents::MachineAgentsServer>> {
        match &self.at {
            ShellsAt::Here(_, agents) => Some(agents),
            ShellsAt::Devices(_) => None,
        }
    }
}

impl WorkspaceShell for HostShells {
    fn run(
        &self,
        workspace: &str,
        session: &str,
        command: &str,
        _by: &Caller,
        agent: bool,
        person: Option<&tilo_actions::remote::environment_commands::Person>,
        watching: Watching<'_>,
    ) -> Result<ShellRan, String> {
        let secrets = Vec::new();
        let terminals = match &self.at {
            ShellsAt::Here(terminals, _) => terminals,
            ShellsAt::Devices(linux) => {
                let asked = tilo_actions::remote::environment_commands::CommandRequest {
                    session: session.to_owned(),
                    command: command.to_owned(),
                    agent,
                    // The account and size are the ones this host pays for.
                    account: String::new(),
                    size: String::new(),
                    secrets,
                    person: person.cloned(),
                };
                return linux.run_command(workspace, asked, watching);
            }
        };
        let ran = terminals
            .shells()
            .run_for(
                workspace,
                session,
                command,
                agent,
                ush::workspace_shell::RunWith {
                    person,
                    secrets: &secrets,
                    stopping: watching.stop,
                    seen: watching.seen,
                    ..ush::workspace_shell::RunWith::default()
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(ShellRan {
            command: ran.command,
            cwd: ran.cwd,
            exit: ran.exit,
            output: ran.output,
            truncated: ran.truncated,
            cancelled: ran.cancelled,
            started_ms: ran.started_ms,
            ended_ms: ran.ended_ms,
            refusal: ran.refusal,
        })
    }

    fn started(&self, workspace: &str, session: &str, caller: &Caller) {
        if let ShellsAt::Here(_, agents) = &self.at {
            agents.started(workspace, session, caller);
        }
    }

    fn open(&self, opening: &Opening, _by: &Caller, record: Recorder) -> Result<Opened, String> {
        let secrets = Vec::new();
        let terminals = match &self.at {
            ShellsAt::Here(terminals, _) => terminals,
            ShellsAt::Devices(linux) => return linux.open_terminal(opening, secrets, record),
        };
        let opened = terminals.open_for(
            opening.device,
            &ush::environment_terminals::TerminalAsk {
                workspace: &opening.workspace,
                session: &opening.session,
                payer: None,
                person: opening.person.as_ref(),
                cols: opening.cols,
                rows: opening.rows,
                ran: Some(Arc::new(move |ran| {
                    record(crate::linux_devices::terminal_ran(ran));
                })),
                attach: opening.attach.as_deref(),
                secrets: &secrets,
            },
        )?;
        Ok(Opened {
            stream: opened.stream,
            session: opened.session,
        })
    }
}
