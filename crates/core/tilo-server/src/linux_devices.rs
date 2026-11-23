//! A workspace's Linux work, run on a device of the workspace that runs Linux
//! machines: its shell commands, its Terminal sessions on a terminal, its
//! machine's branches and deletes, and its Browser.
//!
//! This host decides nothing about where the work runs. It asks the turn
//! (`tilo.work#take` on [`LINUX_RESOURCE`](tilo_workspace_service::work::LINUX_RESOURCE))
//! of the devices it reaches: the workspace's own, and its person's servers,
//! at the address each one's entry names.
//! Each device asked decides with
//! [`takes_turn`](tilo_workspace_service::work::takes_turn): the one keeping
//! the workspace's machine takes it, else one with room. The one that took it
//! runs the work and passes what it prints back as it prints it; a stop asked
//! here reaches the run there.
//!
//! While no device takes it, the work waits and says so ([`WAITING`]), until
//! one does or the caller stops it. A device that stops answering for
//! [`Timing::quiet`] loses the turn: it is asked again, and once its claim
//! lapsed another device takes it over.
//!
//! A Terminal session's stream is carried through this host: the person's
//! device dials this host, which splices the stream to the device running the
//! shell.

use std::io;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use iroh::{Endpoint, EndpointAddr, EndpointId};
use linux_vm::hosted::control::{self, ENVIRONMENT, Reply, Request, VmRequest};
use linux_vm::hosted::size::VmSize;
use tilo_actions::remote::browser::{self, OpenBrowser, OpenedBrowser};
use tilo_actions::remote::environment_commands::{
    self, CommandOutput, CommandRan, CommandRequest, VmSecret,
};
use tilo_actions::remote::environment_machines::{
    self, BranchMachine, DeleteMachine, MachineBranched,
};
use tilo_actions::remote::environment_terminals::{self, OpenTerminal, TerminalOpened};
use tilo_actions::remote::linux_apps::{self, AppLaunch, OpenApp};
use tilo_actions::remote::server_account::Refusal;
use tilo_actions::remote::{
    ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{ActionFailure, InvocationId, InvocationUpdate};
use tilo_agent_session::shell::{Opened, Opening, Recorder, ShellRan, Watching};
use tilo_peer_state::actions::{ActionService, RemoteDevice};
use tilo_server_protocol::hosted::Material;
use ush::environment_terminals::quic::{Receiving, Sending, dial};
use ush::environment_terminals::{HANDED_FOR, Handed, NOT_HANDED, Ran as TerminalRan, wire};

use crate::hosted_browser::{AppRefusal, Window};
use crate::workspaces::{WorkspaceMachines, Workspaces};

/// What a run says while no device takes it.
pub const WAITING: &str = "Waiting for a device that runs Linux.";

/// How long this host waits on the work's turn and on the device running it.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// How long the devices asked have to take the turn.
    pub claim: Duration,
    /// How long a turn nobody took waits before it is asked again.
    pub retry: Duration,
    /// How long the device running the work may stay silent before the turn
    /// is asked again: a minute, after which its claim lapses too.
    pub quiet: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            claim: Duration::from_secs(5),
            retry: Duration::from_secs(2),
            quiet: Duration::from_secs(60),
        }
    }
}

/// How long one wait for the device running the work to move is, so a stop
/// is noticed while it runs.
const LOOK: Duration = Duration::from_millis(100);
/// How long the device running the work is given to keep a stop.
const STOP_PATIENCE: Duration = Duration::from_secs(10);
/// How long a Browser open, or a machine's branch or delete, waits for a
/// device before it says why it did not happen.
const OPEN_PATIENCE: Duration = Duration::from_secs(5 * 60);

/// What this host asks as, once it serves.
struct Asking {
    actions: ActionService,
    endpoint: Endpoint,
    workspaces: Workspaces,
    runtime: tokio::runtime::Handle,
}

/// The work of this host's workspaces that runs on their devices that run
/// Linux machines, and who pays for those machines.
pub struct LinuxDevices {
    account: String,
    size: VmSize,
    timing: Timing,
    asking: OnceLock<Asking>,
    /// The terminal streams the devices running the shells handed this host,
    /// handed on to the devices that opened them.
    terminals: Handed<OnDevice>,
}

/// How a run is followed: what it prints, and when the caller stops it.
#[derive(Clone, Copy, Default)]
pub struct Follow<'a> {
    /// Each item the run produces, as it produces it.
    pub seen: Option<&'a (dyn Fn(serde_json::Value) + Sync)>,
    /// Whether the caller stopped it.
    pub stop: Option<&'a (dyn Fn() -> bool + Sync)>,
    /// How long the run may wait for a device before it gives up.
    pub patience: Option<Duration>,
}

/// A run some device took, and how it ended.
pub struct Ran {
    /// Where the device that ran it is reached.
    pub address: EndpointAddr,
    /// Whether it waited for a device first.
    pub waited: bool,
    /// What it answered.
    pub output: Option<serde_json::Value>,
}

/// How following a run ended.
enum Followed {
    /// It ended with what it answered.
    Ended(Option<serde_json::Value>),
    /// The device running it stayed silent for [`Timing::quiet`].
    Quiet,
}

/// Why a run did not end in an answer.
#[derive(Debug)]
pub enum NotRun {
    /// The caller stopped it, after it waited for a device when `waited`.
    Stopped { waited: bool },
    /// No device took it within the patience it was given.
    NoDevice,
    /// The device that ran it failed it.
    Failed(Box<ActionFailure>),
    /// Asking went wrong.
    Error(String),
}

impl std::fmt::Display for NotRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped { .. } => f.write_str("it was stopped"),
            Self::NoDevice => f.write_str("no device that runs Linux took it in time"),
            Self::Failed(failure) => f.write_str(&failure.message),
            Self::Error(words) => f.write_str(words),
        }
    }
}

impl LinuxDevices {
    /// The Linux devices the host's environment pays for: its account
    /// (`TILO_HOSTED_ACCOUNT`) and the size of its machines
    /// (`TILO_HOSTED_VM_SIZE`), when it runs no hosted-VM service of its own
    /// (`TILO_HOSTED_VM_SOCKET`).
    #[must_use]
    pub fn from_env() -> Option<Self> {
        if std::env::var_os(tilo_server_account::SOCKET_ENV).is_some() {
            return None;
        }
        let account = std::env::var(tilo_server_account::ACCOUNT_ENV).ok()?;
        let size = std::env::var(ush::hosted_vm::SIZE_ENV)
            .ok()
            .and_then(|size| VmSize::parse(&size))
            .unwrap_or_default();
        Some(Self::new(account, size))
    }

    #[must_use]
    pub fn new(account: String, size: VmSize) -> Self {
        Self {
            account,
            size,
            timing: Timing::default(),
            asking: OnceLock::new(),
            terminals: Handed::new(HANDED_FOR),
        }
    }

    /// Wait on turns and devices as `timing` says.
    #[must_use]
    pub const fn with_timing(mut self, timing: Timing) -> Self {
        self.timing = timing;
        self
    }

    /// Ask as `actions`'s device, from `endpoint`: this host's own, serving `workspaces`, on
    /// `runtime`.
    pub fn ask_from(
        &self,
        actions: ActionService,
        endpoint: Endpoint,
        workspaces: Workspaces,
        runtime: tokio::runtime::Handle,
    ) {
        let _ = self.asking.set(Asking {
            actions,
            endpoint,
            workspaces,
            runtime,
        });
    }

    fn asking(&self) -> Result<&Asking, NotRun> {
        self.asking
            .get()
            .ok_or_else(|| NotRun::Error("the host is not running yet".into()))
    }

    /// Who pays for the workspaces' machines.
    pub(crate) fn account(&self) -> &str {
        &self.account
    }

    /// The devices asked for `workspace`'s Linux turns: its own devices, and
    /// this host's person's servers, each reached at the address its entry
    /// names: whether one runs Linux is its own to say. Each is asked with this host's signed
    /// address for the workspace, so a server of the person that does not
    /// keep the workspace yet enters it from here.
    fn devices(asking: &Asking, workspace: &str) -> Vec<(EndpointAddr, RemoteDevice)> {
        let own = asking.workspaces.host_device();
        let (keyed, servers, hint) = asking.runtime.block_on(async {
            let keyed = asking.actions.key_holders(workspace).await;
            let servers = crate::workspaces::servers::read(&asking.workspaces)
                .await
                .ok();
            let hint = asking
                .workspaces
                .hosts(workspace)
                .await
                .ok()
                .and_then(|hints| {
                    hints
                        .into_iter()
                        .find(|hint| hint.host.device_public_key == own)
                });
            (keyed, servers, hint)
        });
        let mut addresses: Vec<EndpointAddr> = servers
            .iter()
            .flat_map(|servers| servers.servers.iter())
            .filter(|(_, server)| !server.retired)
            .filter_map(|(_, server)| serde_json::from_str(&server.endpoint).ok())
            .collect();
        for device in keyed {
            let Ok(id) = EndpointId::from_bytes(&device) else {
                continue;
            };
            if !addresses.iter().any(|address| address.id == id) {
                addresses.push(EndpointAddr::new(id));
            }
        }
        addresses.retain(|address| *address.id.as_bytes() != own);
        addresses
            .into_iter()
            .map(|address| {
                let device = match &hint {
                    Some(hint) => {
                        asking
                            .actions
                            .placing(&asking.endpoint, &address, workspace, hint)
                    }
                    None => asking.actions.remote(&asking.endpoint, &address, workspace),
                };
                (address, device)
            })
            .collect()
    }

    /// Run `asked` at `target` on the device of `workspace` that takes it,
    /// following it as `follow` says, until it ends.
    ///
    /// # Errors
    ///
    /// It was stopped, no device took it in time, the device failed it, or
    /// asking went wrong.
    pub fn run(
        &self,
        workspace: &str,
        target: RemoteActionTarget,
        asked: &impl serde::Serialize,
        follow: Follow<'_>,
    ) -> Result<Ran, NotRun> {
        let asking = self.asking()?;
        let serde_json::Value::Object(argument) =
            serde_json::to_value(asked).map_err(|error| NotRun::Error(error.to_string()))?
        else {
            return Err(NotRun::Error("what a device is asked is an object".into()));
        };
        let invocation = InvocationId::new();
        let run = InvokeRequest {
            invocation,
            target,
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let take = tilo_workspace_service::work::linux_take_request(&run);
        let until = follow
            .patience
            .and_then(|patience| Instant::now().checked_add(patience));
        let stopped = || follow.stop.is_some_and(|stop| stop());
        let mut waited = false;
        loop {
            if stopped() {
                return Err(NotRun::Stopped { waited });
            }
            if until.is_some_and(|until| Instant::now() >= until) {
                return Err(NotRun::NoDevice);
            }
            match self.first_to_take(asking, workspace, &take) {
                Ok((address, device)) => match self.follow(&device, invocation, follow)? {
                    Followed::Ended(output) => {
                        return Ok(Ran {
                            address,
                            waited,
                            output,
                        });
                    }
                    // It went quiet: the turn is asked again, and another
                    // device takes it once its claim lapsed.
                    Followed::Quiet => {
                        tracing::info!(%workspace, "the device running a Linux turn went quiet");
                    }
                },
                Err(why) => {
                    if !waited {
                        tracing::info!(%workspace, %why, "{WAITING}");
                    }
                    waited = true;
                    pause(self.timing.retry, follow.stop);
                }
            }
        }
    }

    /// The first of the devices asked that took `take`, and where it is reached; or why none did.
    fn first_to_take(
        &self,
        asking: &Asking,
        workspace: &str,
        take: &InvokeRequest,
    ) -> Result<(EndpointAddr, RemoteDevice), String> {
        let caller = asker();
        let mut asked: Vec<_> = Self::devices(asking, workspace)
            .into_iter()
            .map(|(address, device)| {
                let answer = device.invoke(&caller, take.clone());
                (address, device, Some(answer))
            })
            .collect();
        if asked.is_empty() {
            return Err("no device of the workspace is reachable".into());
        }
        let asked_at = Instant::now();
        let mut why = Vec::new();
        while asked_at.elapsed() < self.timing.claim
            && asked.iter().any(|(_, _, answer)| answer.is_some())
        {
            let mut took = None;
            for (at, (_, _, waiting)) in asked.iter_mut().enumerate() {
                let Some(answer) = waiting.take() else {
                    continue;
                };
                match answer.wait_timeout(Duration::from_millis(10)) {
                    Err(again) => *waiting = Some(again),
                    Ok(Some(Ok(receipt))) => {
                        if let ReceiptStatus::Rejected { error } = receipt.status {
                            why.push(error.to_string());
                        } else {
                            took = Some(at);
                            break;
                        }
                    }
                    Ok(Some(Err(error))) => why.push(error.to_string()),
                    Ok(None) => why.push("a device did not answer".into()),
                }
            }
            if let Some(at) = took {
                let (address, device, _) = asked.swap_remove(at);
                return Ok((address, device));
            }
        }
        Err(if why.is_empty() {
            "no device answered in time".into()
        } else {
            why.join("; ")
        })
    }

    /// Follow `invocation` on `device` until it ends, or until the device
    /// stayed silent for [`Timing::quiet`].
    fn follow(
        &self,
        device: &RemoteDevice,
        invocation: InvocationId,
        follow: Follow<'_>,
    ) -> Result<Followed, NotRun> {
        let caller = asker();
        let mut heard = Instant::now();
        let mut stop_asked = false;
        let mut last_item = None;
        loop {
            // The stop is asked for once, and then the run is followed to the
            // end it gives: what it printed until then comes back with it.
            if !stop_asked && follow.stop.is_some_and(|stop| stop()) {
                stop_asked = true;
                let _ = device
                    .cancel(&caller, &invocation)
                    .wait_timeout(STOP_PATIENCE);
            }
            if let Some(change) = device.next_change(&caller, &invocation) {
                let _ = change.wait_timeout(LOOK);
            }
            let Some(state) = device.observe(&caller, &invocation) else {
                if heard.elapsed() >= self.timing.quiet {
                    return Ok(Followed::Quiet);
                }
                std::thread::sleep(LOOK);
                continue;
            };
            heard = Instant::now();
            match state.update {
                InvocationUpdate::Succeeded { output, .. } => {
                    return Ok(Followed::Ended(output.value));
                }
                InvocationUpdate::Failed { error, .. } => {
                    return Err(NotRun::Failed(Box::new(error)));
                }
                InvocationUpdate::Cancelled { .. } => {
                    return Err(NotRun::Stopped { waited: false });
                }
                InvocationUpdate::Item {
                    sequence, value, ..
                } => {
                    if last_item != Some(sequence) {
                        last_item = Some(sequence);
                        if let Some(seen) = follow.seen {
                            seen(value);
                        }
                    }
                }
                InvocationUpdate::Accepted { .. } | InvocationUpdate::Progress { .. } => {}
            }
        }
    }

    /// Run `asked`, a command line of one of `workspace`'s sessions, on the device running
    /// its Linux work, paid for by this host's account, showing the caller what it prints
    /// while it runs and stopping it there when the caller asks.
    ///
    /// # Errors
    ///
    /// The device could not be asked, or it refused.
    pub fn run_command(
        &self,
        workspace: &str,
        mut asked: CommandRequest,
        watching: Watching<'_>,
    ) -> Result<ShellRan, String> {
        asked.account.clone_from(&self.account);
        self.size.as_str().clone_into(&mut asked.size);
        let command = asked.command.clone();
        let seen = |value: serde_json::Value| {
            if let (Some(seen), Ok(printed)) = (
                watching.seen,
                serde_json::from_value::<CommandOutput>(value),
            ) {
                seen(&printed);
            }
        };
        let follow = Follow {
            seen: Some(&seen),
            stop: watching.stop,
            patience: None,
        };
        let waiting = format!("ush: {WAITING} Ctrl+C cancels.\n");
        match self.run(workspace, environment_commands::target(), &asked, follow) {
            Ok(ran) => {
                let mut answered: CommandRan = ran
                    .output
                    .and_then(|value| serde_json::from_value(value).ok())
                    .ok_or("the device answered no run")?;
                if ran.waited {
                    answered.output.insert_str(0, &waiting);
                }
                Ok(shell_ran(answered))
            }
            Err(NotRun::Stopped { waited }) => Ok(shell_ran(CommandRan {
                command,
                output: if waited { waiting } else { String::new() },
                exit: 130,
                cancelled: true,
                ..CommandRan::default()
            })),
            Err(not_run) => Err(not_run.to_string()),
        }
    }

    /// Open `opening`'s session on a terminal on the device running its workspace's Linux
    /// work, every program its shell starts given `secrets`, and hand the stream on to the
    /// device that asked.
    ///
    /// # Errors
    ///
    /// The device could not be asked, or it refused.
    pub fn open_terminal(
        &self,
        opening: &Opening,
        secrets: Vec<VmSecret>,
        record: Recorder,
    ) -> Result<Opened, String> {
        let asked = OpenTerminal {
            session: opening.session.clone(),
            cols: opening.cols,
            rows: opening.rows,
            attach: opening.attach.clone(),
            person: opening.person.clone(),
            account: self.account.clone(),
            size: self.size.as_str().to_owned(),
            secrets,
        };
        let ran = self
            .run(
                &opening.workspace,
                environment_terminals::target(),
                &asked,
                Follow {
                    patience: Some(OPEN_PATIENCE),
                    ..Follow::default()
                },
            )
            .map_err(|not_run| not_run.to_string())?;
        let opened: TerminalOpened = ran
            .output
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or("the device answered no terminal")?;
        if let Some(runs) = opened.runs {
            self.follow_runs(ran.address.clone(), runs, record)
                .map_err(|not_run| not_run.to_string())?;
        }
        let stream = self
            .terminals
            .hand(
                opening.device,
                OnDevice {
                    address: ran.address,
                    stream: opened.stream,
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(Opened {
            stream,
            session: opened.session,
        })
    }

    /// Follow the runs the device at `address` handed this host as `runs`, recording each
    /// with `record`, until the session ends there.
    fn follow_runs(
        &self,
        address: EndpointAddr,
        runs: String,
        record: Recorder,
    ) -> Result<(), NotRun> {
        let asking = self.asking()?;
        let (endpoint, runtime) = (asking.endpoint.clone(), asking.runtime.clone());
        std::thread::Builder::new()
            .name("terminal-runs".into())
            .spawn(move || {
                let followed = (|| {
                    let (_connection, mut to, mut from) = dial(&endpoint, address, &runtime)?;
                    wire::write_hello(&mut to, &wire::Hello { stream: runs })?;
                    match wire::read_welcome(&mut from)? {
                        Some(wire::Welcome::Opened { .. }) => {}
                        other => return Err(io::Error::other(format!("{other:?}"))),
                    }
                    while let Some(ran) = wire::read_ran(&mut from)? {
                        record(terminal_ran(ran));
                    }
                    Ok::<_, io::Error>(())
                })();
                if let Err(error) = followed {
                    tracing::warn!(%error, "a terminal's runs stopped being followed");
                }
            })
            .map(drop)
            .map_err(|error| NotRun::Error(error.to_string()))
    }

    /// Splice the stream `device` presents on `from` to the device running its shell, as
    /// this host's device.
    ///
    /// # Errors
    ///
    /// The stream was handed to no such device, or the device running the shell was not
    /// reached.
    pub(crate) fn splice(
        &self,
        device: [u8; 32],
        mut from: Receiving,
        to: &mut Sending,
    ) -> io::Result<()> {
        let Some(hello) = wire::read_hello(&mut from)? else {
            return Ok(());
        };
        let Some(on_device) = self.terminals.take(&hello.stream, &device) else {
            return Err(wire::refuse(to, NOT_HANDED));
        };
        let asking = self
            .asking()
            .map_err(|not_run| wire::refuse(to, &not_run.to_string()))?;
        let (connection, mut there_to, mut there_from) =
            dial(&asking.endpoint, on_device.address, &asking.runtime).map_err(|error| {
                wire::refuse(
                    to,
                    &format!("the device running the shell is not reached: {error}"),
                )
            })?;
        wire::write_hello(
            &mut there_to,
            &wire::Hello {
                stream: on_device.stream,
            },
        )?;
        // What the person types goes on until they leave; what the shell says, its answer
        // to the hello first, comes back until the shell is over there.
        std::thread::Builder::new()
            .name("terminal-splice".into())
            .spawn(move || {
                let _ = io::copy(&mut from, &mut there_to);
                there_to.finish();
            })?;
        let carried = io::copy(&mut there_from, to);
        drop(connection);
        carried.map(drop)
    }

    /// Start `asked` at `target` in `workspace`'s machine, waiting for a device as long as
    /// an open does: what it answered, or the failure it answered with.
    fn open(
        &self,
        workspace: &str,
        target: RemoteActionTarget,
        asked: &impl serde::Serialize,
    ) -> Result<Option<serde_json::Value>, NotRun> {
        let follow = Follow {
            patience: Some(OPEN_PATIENCE),
            ..Follow::default()
        };
        self.run(workspace, target, asked, follow)
            .map(|ran| ran.output)
    }

    /// The device that keeps `workspace`'s machine, when one does.
    fn keeper(&self, workspace: &str) -> Result<Option<[u8; 32]>, String> {
        let asking = self.asking().map_err(|not_run| not_run.to_string())?;
        let lease = tilo_peer_state::vm_lease(ENVIRONMENT);
        asking
            .runtime
            .block_on(asking.workspaces.lease_holder(workspace, &lease))
            .map_err(|error| error.to_string())
    }
}

/// Wait `for_`, or until `stop` says so.
fn pause(for_: Duration, stop: Option<&(dyn Fn() -> bool + Sync)>) {
    let started = Instant::now();
    while !stop.is_some_and(|stop| stop()) && started.elapsed() < for_ {
        std::thread::sleep(for_.saturating_sub(started.elapsed()).min(LOOK));
    }
}

/// Who this host asks as: the action service's device, whose signature the device asked
/// reads. The caller is only what the trait asks for, never sent.
fn asker() -> Caller {
    Caller::local_app("tilo.server")
}

/// A stream the device running a workspace's shell handed this host's own device, handed
/// on to the person's device that opened it.
pub struct OnDevice {
    address: EndpointAddr,
    stream: String,
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

/// A run a session's shell told of, as a command the host records.
#[must_use]
pub fn terminal_ran(ran: TerminalRan) -> ShellRan {
    ShellRan {
        command: ran.command,
        cwd: ran.cwd,
        exit: ran.exit,
        output: ran.output,
        truncated: ran.truncated,
        cancelled: false,
        started_ms: ran.started_ms,
        ended_ms: ran.ended_ms,
        refusal: None,
    }
}

/// A workspace's machine is branched and deleted by the device keeping it.
impl WorkspaceMachines for LinuxDevices {
    fn branch(&self, source: &str, branch: &str) -> Result<bool, String> {
        // A source whose machine nobody keeps has none to copy.
        if self.keeper(source)?.is_none() {
            return Ok(false);
        }
        let made = BranchMachine {
            from: source.to_owned(),
            account: self.account.clone(),
            size: self.size.as_str().to_owned(),
        };
        let branched = self
            .open(branch, environment_machines::branch_target(), &made)
            .map_err(|not_run| not_run.to_string())
            .and_then(|answer| {
                answer
                    .and_then(|value| serde_json::from_value::<MachineBranched>(value).ok())
                    .ok_or_else(|| "the device answered no branch".to_owned())
            });
        match branched {
            Ok(branched) => Ok(branched.branched),
            // A machine the device did make is deleted there.
            Err(why) => match self.delete(branch) {
                Ok(()) => Err(why),
                Err(left) => Err(format!("{why}; and what it left stays: {left}")),
            },
        }
    }

    fn delete(&self, workspace: &str) -> Result<(), String> {
        if self.keeper(workspace)?.is_none() {
            return Ok(());
        }
        self.open(
            workspace,
            environment_machines::delete_target(),
            &DeleteMachine {},
        )
        .map(drop)
        .map_err(|not_run| not_run.to_string())
    }

    /// The machines are on the devices that keep them, which suspend their
    /// own when they restart: none is on this one.
    fn drain(&self) -> Result<(), String> {
        Ok(())
    }
}

impl std::fmt::Debug for LinuxDevices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxDevices").finish_non_exhaustive()
    }
}

/// The plans' refusal a failure carries, when the account's plan refused it.
fn plan_refusal(failure: &ActionFailure) -> Option<Refusal> {
    (failure.code == linux_apps::REFUSED)
        .then(|| failure.details.clone())
        .flatten()
        .and_then(|details| serde_json::from_value(details).ok())
}

/// What the server's Browser asks of a workspace's machine, on the device that runs it.
impl crate::hosted_browser::BrowserSessions for LinuxDevices {
    /// Hand `material` to the session in the workspace's machine, and answer its window once
    /// it is up.
    fn start(&self, material: &Material) -> Result<Window, String> {
        let written = serde_json::to_string(material).map_err(|error| {
            tracing::warn!(%error, "the Browser's material could not be written");
            "Browser couldn't start. Try again in a minute.".to_owned()
        })?;
        let asked = OpenBrowser {
            workspace: material.workspace.clone(),
            account: self.account.clone(),
            material: written,
        };
        let opened: OpenedBrowser = match self.open(&material.workspace, browser::target(), &asked)
        {
            Ok(answer) => answer
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default(),
            Err(NotRun::NoDevice) => {
                return Err(format!("{WAITING} None has room yet."));
            }
            Err(NotRun::Failed(failure)) if plan_refusal(&failure).is_some() => {
                return Err(plan_refusal(&failure)
                    .map(|refusal| refusal.message)
                    .unwrap_or_default());
            }
            Err(not_run) => {
                tracing::warn!(%not_run, "opening the workspace's Browser failed");
                return Err("Browser couldn't start. Open it again to try once more.".to_owned());
            }
        };
        Ok(Window {
            device: material.device.device_public_key,
            provider: opened.provider,
        })
    }

    /// End the browser session in the workspace's machine now.
    fn stop(&self, workspace: &str) {
        let asked = OpenBrowser {
            workspace: workspace.to_owned(),
            account: String::new(),
            material: String::new(),
        };
        if let Err(not_run) = self.open(workspace, browser::stop_target(), &asked) {
            tracing::warn!(%workspace, %not_run, "a browser session did not stop");
        }
    }

    /// Hand `launch` to the workspace's Linux apps session, started with `material` when none
    /// runs.
    fn start_app(&self, material: &Material, launch: &AppLaunch) -> Result<Window, AppRefusal> {
        let written = serde_json::to_string(material).map_err(|error| {
            tracing::warn!(%error, "the apps session's material could not be written");
            AppRefusal::Words("The app couldn't start. Try again in a minute.".to_owned())
        })?;
        let asked = OpenApp {
            workspace: material.workspace.clone(),
            account: self.account.clone(),
            material: written,
            launch: launch.clone(),
        };
        let opened: OpenedBrowser =
            match self.open(&material.workspace, linux_apps::target(), &asked) {
                Ok(answer) => answer
                    .and_then(|value| serde_json::from_value(value).ok())
                    .unwrap_or_default(),
                Err(NotRun::Failed(failure)) => {
                    return Err(plan_refusal(&failure).map_or_else(
                        || {
                            tracing::warn!(%failure.message, "a machine failed to start a session");
                            AppRefusal::Words("The machine couldn't start it.".to_owned())
                        },
                        AppRefusal::Refused,
                    ));
                }
                Err(NotRun::NoDevice) => {
                    return Err(AppRefusal::Words(format!("{WAITING} None has room yet.")));
                }
                Err(not_run) => return Err(AppRefusal::Words(not_run.to_string())),
            };
        Ok(Window {
            device: material.device.device_public_key,
            provider: opened.provider,
        })
    }

    /// End the Linux apps session in the workspace's machine now.
    fn stop_apps(&self, workspace: &str) {
        let asked = OpenBrowser {
            workspace: workspace.to_owned(),
            account: String::new(),
            material: String::new(),
        };
        if let Err(not_run) = self.open(workspace, linux_apps::stop_target(), &asked) {
            tracing::warn!(%workspace, %not_run, "a Linux apps session did not stop");
        }
    }

    fn ask_from(
        &self,
        actions: ActionService,
        endpoint: Endpoint,
        workspaces: Workspaces,
        runtime: tokio::runtime::Handle,
    ) {
        Self::ask_from(self, actions, endpoint, workspaces, runtime);
    }
}

/// The machines of the workspaces of a host that runs its shells itself, on
/// the hosted-VM service its environment names
/// ([`ush::hosted_vm::HostedTarget`]).
pub struct LocalMachines {
    target: ush::hosted_vm::HostedTarget,
}

impl LocalMachines {
    #[must_use]
    pub const fn new(target: ush::hosted_vm::HostedTarget) -> Self {
        Self { target }
    }

    fn ask(&self, request: &Request) -> Result<Reply, String> {
        let socket = &self.target.socket;
        control::ask(socket, request, OPEN_PATIENCE)
            .map_err(|error| format!("the hosted-VM service at {}: {error}", socket.display()))?
            .ok_or_else(|| {
                format!(
                    "no hosted-VM service answers at {}, so no machine was changed",
                    socket.display()
                )
            })
    }
}

impl WorkspaceMachines for LocalMachines {
    fn branch(&self, source: &str, branch: &str) -> Result<bool, String> {
        let asked = Request::Branch {
            vm: VmRequest {
                workspace: branch.to_owned(),
                environment: ENVIRONMENT.to_owned(),
                account: self.target.account.clone(),
                size: self.target.size,
            },
            from: source.to_owned(),
        };
        match self.ask(&asked)? {
            Reply::Done => Ok(true),
            Reply::NoVm { .. } => Ok(false),
            Reply::Refused(refusal) => Err(refusal.message),
            Reply::Busy(words) | Reply::Error(words) => Err(words),
            other => Err(format!("the hosted-VM service answered {other:?}")),
        }
    }

    fn delete(&self, workspace: &str) -> Result<(), String> {
        let asked = Request::Delete {
            workspace: workspace.to_owned(),
            environment: ENVIRONMENT.to_owned(),
        };
        match self.ask(&asked)? {
            Reply::Done => Ok(()),
            Reply::Busy(words) | Reply::Error(words) => Err(words),
            other => Err(format!("the hosted-VM service answered {other:?}")),
        }
    }

    /// Have the service take no more wakes and suspend every awake machine,
    /// which it answers once it has, then read back that none is awake.
    fn drain(&self) -> Result<(), String> {
        match control::ask(&self.target.socket, &Request::Drain, OPEN_PATIENCE) {
            // No service answers, so no machine is awake either.
            Ok(None) => return Ok(()),
            Ok(Some(Reply::Done)) => {}
            Ok(Some(Reply::Busy(words) | Reply::Error(words))) => return Err(words),
            Ok(Some(other)) => {
                return Err(format!("the hosted-VM service answered {other:?}"));
            }
            Err(error) => {
                return Err(format!(
                    "the hosted-VM service at {}: {error}",
                    self.target.socket.display()
                ));
            }
        }
        let Reply::Status { vms, .. } = self.ask(&Request::Status)? else {
            return Err("the hosted-VM service answered no status".to_owned());
        };
        let awake: Vec<String> = vms
            .into_iter()
            .filter(|vm| !matches!(vm.phase.as_str(), "Suspended" | "Off" | "NotCreated"))
            .map(|vm| format!("{} ({})", vm.vm, vm.phase))
            .collect();
        if awake.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "these machines did not suspend: {}",
                awake.join(", ")
            ))
        }
    }
}

impl std::fmt::Debug for LocalMachines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalMachines").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Mutex};

    use linux_vm::hosted::control::{HostLoad, VmStatus};
    use linux_vm::hosted::size::VmSize;

    use super::*;

    type Asked = Arc<Mutex<Vec<Request>>>;

    fn target(socket: std::path::PathBuf) -> LocalMachines {
        LocalMachines::new(ush::hosted_vm::HostedTarget {
            socket,
            account: "person".into(),
            size: VmSize::default(),
            plans: None,
        })
    }

    /// A hosted-VM service on a socket that answers a drain, then reports
    /// its machines in `phases`, noting what it was asked.
    fn service(phases: &'static [&'static str]) -> (tempfile::TempDir, LocalMachines, Asked) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("hosted.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let asked = Asked::default();
        let noted = Arc::clone(&asked);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let Some(request) = control::FRAMING
                    .read_value::<_, Request>(&mut stream)
                    .unwrap()
                else {
                    continue;
                };
                let reply = match request {
                    Request::Drain => Reply::Done,
                    Request::Status => Reply::Status {
                        vms: phases
                            .iter()
                            .enumerate()
                            .map(|(at, phase)| VmStatus {
                                vm: format!("ws{at}/default"),
                                account: "person".into(),
                                size: VmSize::default(),
                                phase: (*phase).to_owned(),
                                last_wake_ms: None,
                                last_wake_restored: None,
                                snapshot_bytes: None,
                                resident_mib: None,
                                balloon_mib: 0,
                            })
                            .collect(),
                        load: HostLoad::default(),
                    },
                    _ => Reply::Error("not asked here".into()),
                };
                noted.lock().unwrap().push(request);
                control::FRAMING.write_value(&mut stream, &reply).unwrap();
            }
        });
        (dir, target(socket), asked)
    }

    /// The service is told to drain, and the drain is done only once it
    /// reports every machine asleep.
    #[test]
    fn a_drain_suspends_every_machine_and_reads_back_that_each_is() {
        let (_dir, machines, asked) = service(&["Suspended", "Off"]);
        machines.drain().unwrap();
        assert_eq!(*asked.lock().unwrap(), [Request::Drain, Request::Status]);

        let (_dir, machines, _) = service(&["Suspended", "Running"]);
        let error = machines.drain().unwrap_err();
        assert!(error.contains("ws1/default (Running)"), "{error}");
    }

    /// A server with no hosted-VM service has no machine to suspend.
    #[test]
    fn a_drain_without_a_service_has_nothing_to_suspend() {
        let dir = tempfile::tempdir().unwrap();
        target(dir.path().join("none.sock")).drain().unwrap();
    }
}
