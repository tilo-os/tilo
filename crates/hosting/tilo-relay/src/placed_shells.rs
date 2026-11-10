//! A workspace's shell, run on the member device that holds its VM.
//!
//! With more than one VM host, a workspace's VM stays on the host that first
//! woke it, where its disk is (`legacy_vm::hosted::placement`). Each VM host
//! is a member device that takes its workspaces' work: a Tilo device of the
//! operator, admitted to the workspaces it serves, holding their files as
//! any member does. So the shell runs there, beside the VM, and this host
//! only asks:
//!
//! 1. the placer which host runs the workspace's VM ([`Request::Place`]);
//!    while every host is full it answers [`Reply::Busy`], and the run waits,
//!    saying so once, never refused for room;
//! 2. that host's member device, over the device plane as this host's own
//!    device, to run the command line
//!    ([`environment_runs`](tilo_actions::remote::environment_runs)), and
//!    follows the run until it ends.
//!
//! The run comes back as the same [`ShellRan`] a local shell gives, and
//! becomes the same run record. What the command prints as it prints it is
//! passed on to the caller ([`Watching::seen`]), and a caller that stops the
//! run has the member's device end the command there
//! ([`Watching::stop`]): the run comes back
//! [`cancelled`](ShellRan::cancelled).

use std::collections::BTreeMap;
use std::net::ToSocketAddrs as _;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use iroh::{Endpoint, EndpointAddr, EndpointId};
use legacy_vm::hosted::control::{self, Reply, Request, VmRequest};
use legacy_vm::hosted::size::VmSize;
use serde::Deserialize;
use tilo_actions::remote::environment_runs::{self, CommandRan, RunCommand, RunOutput};
use tilo_actions::remote::{ActionHost, Caller, InvokeArgument, InvokeRequest, ReceiptStatus};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_agent_participant::workspace_actions::{ShellRan, Watching};
use tilo_peer_state::actions::ActionPlane;

/// The placer's address (`host:port`).
pub const PLACER_ENV: &str = "TILO_HOSTED_PLACER";
/// The VM hosts' member devices: a JSON file naming each host's device and
/// where it is reached ([`Members`]).
pub const MEMBERS_ENV: &str = "TILO_HOSTED_MEMBERS";

/// The environment a workspace's VM is: one per workspace.
const ENVIRONMENT: &str = "default";
/// How often a run waiting for room asks the placer again.
const BUSY_RETRY: Duration = Duration::from_secs(2);
/// How long the placer is given to answer.
const PLACE_PATIENCE: Duration = Duration::from_secs(30);
/// How long one run is followed: a long build included.
const RUN_PATIENCE: Duration = Duration::from_secs(6 * 60 * 60);
/// How long one answer from the member is waited for.
const ANSWER_PATIENCE: Duration = Duration::from_secs(60);
/// How long one wait for the member to move is, so a cancel is noticed
/// while the run is followed rather than after it ends.
const CHANGE_PATIENCE: Duration = Duration::from_millis(100);
/// How long the member is given to keep a cancel.
const CANCEL_PATIENCE: Duration = Duration::from_secs(10);

/// One VM host's member device.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Member {
    /// Its device key, hex.
    pub device: String,
    /// Where its endpoint listens, `host:port`.
    pub address: String,
}

/// The VM hosts' member devices, by the host name the placer answers.
pub type Members = BTreeMap<String, Member>;

/// Where this host's workspaces' shells run when it has VM hosts: the
/// placer, the members, and who pays.
pub struct PlacedShells {
    placer: String,
    members: Members,
    account: String,
    size: VmSize,
    /// This host's device plane and endpoint, once the host runs.
    asking: OnceLock<(ActionPlane, Endpoint)>,
}

impl PlacedShells {
    /// The placed shells the host's environment names: [`PLACER_ENV`],
    /// [`MEMBERS_ENV`], and the account and size VMs are run for
    /// (`TILO_HOSTED_ACCOUNT`, `TILO_HOSTED_VM_SIZE`). `None` without a
    /// placer: the shells run here.
    ///
    /// # Errors
    ///
    /// A placer is named but the members or the account are not usable.
    pub fn from_env() -> Result<Option<Self>, String> {
        let Ok(placer) = std::env::var(PLACER_ENV) else {
            return Ok(None);
        };
        let members = std::env::var_os(MEMBERS_ENV).ok_or_else(|| {
            format!("{PLACER_ENV} is set, so {MEMBERS_ENV} must name the VM hosts' members")
        })?;
        let account = std::env::var(ush::hosted_vm::ACCOUNT_ENV).map_err(|_| {
            format!(
                "{PLACER_ENV} is set, so {} must name the account",
                ush::hosted_vm::ACCOUNT_ENV
            )
        })?;
        let size = std::env::var(ush::hosted_vm::SIZE_ENV)
            .ok()
            .and_then(|size| VmSize::parse(&size))
            .unwrap_or_default();
        Ok(Some(Self::new(
            placer,
            read_members(Path::new(&members))?,
            account,
            size,
        )))
    }

    #[must_use]
    pub fn new(placer: String, members: Members, account: String, size: VmSize) -> Self {
        Self {
            placer,
            members,
            account,
            size,
            asking: OnceLock::new(),
        }
    }

    /// Ask as `plane`'s device, from `endpoint`: this host's own.
    pub fn ask_from(&self, plane: ActionPlane, endpoint: Endpoint) {
        let _ = self.asking.set((plane, endpoint));
    }

    /// This host's device plane and endpoint, once it runs.
    pub(crate) fn asking(&self) -> Result<&(ActionPlane, Endpoint), String> {
        self.asking
            .get()
            .ok_or_else(|| "the host is not running yet".to_owned())
    }

    /// Who pays for the VMs placed from here.
    pub(crate) fn account(&self) -> &str {
        &self.account
    }

    /// The VM hosts' member devices, by the name the placer answers.
    pub(crate) fn members(&self) -> &Members {
        &self.members
    }

    /// The host that runs the VM of `workspace`'s `environment` (of `size`),
    /// waiting while none has room and keeping the line that said so in
    /// `waited`; `None` when `stop` ended the wait.
    pub(crate) fn placed(
        &self,
        workspace: &str,
        environment: &str,
        size: VmSize,
        waited: &mut Option<String>,
        stop: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<Option<String>, String> {
        let vm = VmRequest {
            workspace: workspace.to_owned(),
            environment: environment.to_owned(),
            account: self.account.clone(),
            size,
        };
        loop {
            if stop.is_some_and(|stop| stop()) {
                return Ok(None);
            }
            match ask_placer(&self.placer, &Request::Place { vm: vm.clone() })? {
                Reply::Placed { host } => return Ok(Some(host)),
                Reply::Busy(words) => {
                    waited
                        .get_or_insert_with(|| format!("ush: {words} Waiting; Ctrl+C cancels.\n"));
                    wait(BUSY_RETRY, stop);
                }
                Reply::Refused(refusal) => return Err(refusal.message),
                Reply::Error(error) => return Err(format!("the placer: {error}")),
                other => return Err(format!("the placer answered {other:?}")),
            }
        }
    }

    /// Run `command` in `workspace`'s session `session` on the VM host the
    /// placer names for the workspace, showing the caller what it prints
    /// while it runs and stopping it there when the caller asks.
    ///
    /// # Errors
    ///
    /// The placer or the member could not be asked, or the member refused.
    pub fn run(
        &self,
        workspace: &str,
        session: &str,
        command: &str,
        agent: bool,
        watching: Watching<'_>,
    ) -> Result<ShellRan, String> {
        let (plane, endpoint) = self.asking()?;
        let mut waited = None;
        let Some(host) = self.placed(
            workspace,
            ENVIRONMENT,
            self.size,
            &mut waited,
            watching.stop,
        )?
        else {
            return Ok(shell_ran(stopped_before_start(command, waited)));
        };
        let member = self.members.get(&host).ok_or_else(|| {
            format!("the placer named {host}, which is no VM host this host knows")
        })?;
        let address = member_address(member)?;
        let device = plane.remote(endpoint, &address, workspace);
        let asked = RunCommand {
            session: session.to_owned(),
            command: command.to_owned(),
            agent,
            account: self.account.clone(),
            size: self.size.as_str().to_owned(),
        };
        let mut ran = run_on(&device, &asked, watching)?;
        if let Some(words) = waited {
            ran.output.insert_str(0, &words);
        }
        Ok(shell_ran(ran))
    }
}

/// Wait `for_`, or until `stop` says so.
fn wait(for_: Duration, stop: Option<&(dyn Fn() -> bool + Sync)>) {
    let until = Instant::now().checked_add(for_);
    while !stop.is_some_and(|stop| stop()) {
        let Some(until) = until.filter(|until| Instant::now() < *until) else {
            return;
        };
        std::thread::sleep(
            until
                .saturating_duration_since(Instant::now())
                .min(CHANGE_PATIENCE),
        );
    }
}

/// The run of a command stopped while it waited for room: it ran nothing.
fn stopped_before_start(command: &str, waited: Option<String>) -> CommandRan {
    CommandRan {
        command: command.to_owned(),
        output: waited.unwrap_or_default(),
        exit: 130,
        cancelled: true,
        ..CommandRan::default()
    }
}

/// Read the members file at `path`.
///
/// # Errors
///
/// It cannot be read, or is not a [`Members`] map.
pub fn read_members(path: &Path) -> Result<Members, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

pub(crate) fn member_address(member: &Member) -> Result<EndpointAddr, String> {
    let key: [u8; 32] = hex::decode(&member.device)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| format!("{} is not a device key", member.device))?;
    let id = EndpointId::from_bytes(&key).map_err(|error| error.to_string())?;
    let sockets = member
        .address
        .to_socket_addrs()
        .map_err(|error| format!("{}: {error}", member.address))?;
    Ok(EndpointAddr::from_parts(
        id,
        sockets.map(iroh::TransportAddr::Ip),
    ))
}

fn ask_placer(placer: &str, request: &Request) -> Result<Reply, String> {
    let socket = placer
        .to_socket_addrs()
        .map_err(|error| format!("the placer {placer}: {error}"))?
        .next()
        .ok_or_else(|| format!("the placer {placer} has no address"))?;
    let mut stream = std::net::TcpStream::connect_timeout(&socket, PLACE_PATIENCE)
        .map_err(|error| format!("the placer {placer}: {error}"))?;
    stream
        .set_read_timeout(Some(PLACE_PATIENCE))
        .map_err(|error| error.to_string())?;
    control::FRAMING
        .write_value(&mut stream, request)
        .map_err(|error| format!("the placer {placer}: {error}"))?;
    control::FRAMING
        .read_value::<_, Reply>(&mut stream)
        .map_err(|error| format!("the placer {placer}: {error}"))?
        .ok_or_else(|| format!("the placer {placer} closed without an answer"))
}

/// Run `asked` on `device`, showing `watching` what it prints and stopping
/// it there when asked, until it ends.
fn run_on(
    device: &tilo_peer_state::actions::RemoteDevice,
    asked: &RunCommand,
    watching: Watching<'_>,
) -> Result<CommandRan, String> {
    let serde_json::Value::Object(argument) =
        serde_json::to_value(asked).map_err(|error| error.to_string())?
    else {
        return Err("a command is an object".into());
    };
    let invocation = InvocationId::new();
    let request = InvokeRequest {
        invocation,
        target: environment_runs::target(),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    // Who asks is the plane's device: the caller here is only what the
    // trait asks for, never sent.
    let caller = Caller::local_app("tilo.relay");
    let receipt = settled(device.invoke(&caller, request))?
        .map_err(|error| format!("the VM host refused the run: {error}"))?;
    if let ReceiptStatus::Rejected { error } = receipt.status {
        return Err(format!("the VM host refused the run: {error}"));
    }
    let until = Instant::now().checked_add(RUN_PATIENCE);
    let mut stop_asked = false;
    loop {
        if until.is_some_and(|until| Instant::now() >= until) {
            return Err("the VM host did not finish the run in time".into());
        }
        // The cancel is asked for once, and then the run is followed to the
        // end it gives: what it printed until then comes back with it.
        if !stop_asked && watching.stop.is_some_and(|stop| stop()) {
            let answer = device
                .cancel(&caller, &invocation)
                .wait_timeout(CANCEL_PATIENCE);
            stop_asked = matches!(answer, Ok(Some(_)));
            continue;
        }
        if let Some(mut change) = device.next_change(&caller, &invocation) {
            while let Err(again) = change.wait_timeout(CHANGE_PATIENCE) {
                if watching.stop.is_some_and(|stop| stop())
                    || until.is_some_and(|until| Instant::now() >= until)
                {
                    break;
                }
                change = again;
            }
        } else {
            std::thread::sleep(CHANGE_PATIENCE);
        }
        let Some(state) = device.observe(&caller, &invocation) else {
            continue;
        };
        match state.update {
            InvocationUpdate::Succeeded { output, .. } => {
                return output
                    .value
                    .and_then(|value| serde_json::from_value(value).ok())
                    .ok_or_else(|| "the VM host answered no run".to_owned());
            }
            InvocationUpdate::Failed { error, .. } => return Err(error.message),
            InvocationUpdate::Cancelled { .. } => return Err("the run was cancelled".into()),
            InvocationUpdate::Accepted { .. } | InvocationUpdate::Progress { .. } => {}
            InvocationUpdate::Item { value, .. } => {
                if let Some(seen) = watching.seen
                    && let Ok(printed) = serde_json::from_value::<RunOutput>(value)
                {
                    seen(&printed);
                }
            }
        }
    }
}

pub(crate) fn settled<T>(pending: tilo_actions::remote::Pending<T>) -> Result<T, String> {
    match pending.wait_timeout(ANSWER_PATIENCE) {
        Ok(Some(value)) => Ok(value),
        _ => Err("the VM host did not answer".into()),
    }
}

fn shell_ran(ran: CommandRan) -> ShellRan {
    ShellRan {
        command: ran.command,
        cwd: ran.cwd,
        exit: ran.exit,
        output: ran.output.into_bytes(),
        truncated: ran.truncated,
        cancelled: ran.cancelled,
        started_ms: ran.started_ms,
        ended_ms: ran.ended_ms,
        refusal: ran.refusal,
    }
}
