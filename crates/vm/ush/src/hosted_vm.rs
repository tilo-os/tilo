//! Programs a hosted workspace's shell runs in the workspace's own VM, on a host that
//! serves other people's workspaces: a stranger's `cargo` or `python` never runs in a
//! guest sharing the host's kernel.
//!
//! [`HostedPrograms`] asks the hosted-VM service ([`linux_vm::hosted`]) for the
//! workspace environment's VM, which it boots on first use and restores from its snapshot
//! after it went idle, and runs each program there through the VM's agent with the
//! environment's granted directories at the same paths. The shell, its own commands and
//! their run records are the same as on a device.
//!
//! A refused VM is typed ([`server_account::Refusal`]) and says what would let it through: a
//! person's command the account's access would admit waits and runs once the host's
//! plans file names the account ([`held_mark`], Terminal showing the prompt), while an
//! agent's command fails at once with the refusal and what to ask the person. When the
//! hosts have no room ([`Reply::Busy`]) a command waits after one line saying so, and
//! interrupting it ends it having run nothing.
//!
//! A program starts with the shell's variables and, set over them, the secrets the run carries
//! ([`HostedPrograms::carrying_secrets`]): items of the workspace's vault its VM may use. They
//! travel to the VM's agent in the launch alone. It also records who the run is for, a person
//! or an agent, as git's author and committer ([`HostedPrograms::carrying_identity`]).
//!
//! Its `~` is the workspace's files, the folder the shell's `~` names, so what a program
//! writes there is the workspace's, versioned and bounded by its grant. What is the
//! machine's rather than the workspace's stays on the VM's own disk, in the tenant's folders
//! there (`/home/tilo`), and a run moves neither ([`HostedPrograms::machine_environment`]).
//!
//! A program whose input and output are the session's terminal runs on a pseudo-terminal
//! in the VM ([`Stream::Terminal`]), so editors, prompts and pagers work as on a device.
//!
//! Where the host serves its workspaces' agents ([`HostedPrograms::serving_agents`]), each
//! program finds Tilo's MCP server where an environment's programs find it on a device
//! (`/run/tilo/agent.sock`, which `tilo-runtime --connect` reads from `TILO_AGENT_SOCK`): the
//! VM's agent carries each connection one of the program's own processes makes there to
//! this host on the program's own connection ([`client::launch_serving_agents`]), and the
//! host hands it to [`MachineAgents`], which decides what it is. The connections end with
//! the program.
//! [`SOCKET_ENV`], [`ACCOUNT_ENV`], [`SIZE_ENV`] and [`PLANS_ENV`] turn this on for a
//! host's sessions.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use linux_vm::hosted::control::{self, Reply, Request, ShareRequest, VmRequest};
use linux_vm_protocol::client::{self, LaunchIo, LaunchSignals};
use linux_vm_protocol::link::{AgentLink, AgentStream};
use linux_vm_protocol::{
    AGENT_PORT, Display, Launch, ProtocolError, Signal, Stdio as Stream, TerminalSize,
};
use tilo_actions::remote::server_account;
use tilo_server_protocol::hosted_vm::VmSize;
use ush_shell::{Input, Output, Program, Std, Stdio};
use ush_tilo::Programs;

pub use tilo_server_account::{ACCOUNT_ENV, PLANS_ENV, SOCKET_ENV};
pub const SIZE_ENV: &str = "TILO_HOSTED_VM_SIZE";

/// The tenant's home on the VM's own disk in the guest image
/// (`os/Containerfile.hosted-guest`): where a program's caches, toolchains and build output go
/// while its `~` is the workspace's files.
const GUEST_HOME: &str = "/home/tilo";

/// Where a program's build output goes: on the VM's disk, which a build may fill, never in the
/// workspace's files.
const GUEST_TARGET: &str = "/var/tmp/tilo/target";

#[cfg(any(test, feature = "test-host"))]
pub mod test_host;

const WAKE_PATIENCE: Duration = Duration::from_secs(300);

const BUSY_RETRY: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedTarget {
    pub socket: PathBuf,
    pub account: String,
    pub size: VmSize,
    /// The plans file a held command follows; without one a refusal is
    /// never waited out.
    pub plans: Option<PathBuf>,
}

impl HostedTarget {
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let socket = PathBuf::from(std::env::var_os(SOCKET_ENV)?);
        let account = std::env::var(ACCOUNT_ENV).ok()?;
        let size = std::env::var(SIZE_ENV)
            .ok()
            .and_then(|size| VmSize::parse(&size))
            .unwrap_or_default();
        Some(Self {
            socket,
            account,
            size,
            plans: std::env::var_os(PLANS_ENV).map(PathBuf::from),
        })
    }
}

type Shares = dyn Fn() -> Vec<ShareRequest> + Send + Sync;

/// Where a connection to a machine's agent socket came from: the workspace's environment
/// whose program made it, and the shell session that started the program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentConnection {
    pub workspace: String,
    /// The environment's record key: what the agent is named after.
    pub environment: String,
    /// The session of the workspace's shells whose command started the program.
    pub session: String,
    /// Whether an agent's command started it, rather than a person's.
    pub agent: bool,
    /// The executable of the process that connected, as the machine names it.
    pub program: String,
}

/// What serves the connections a workspace's programs make to their machine's agent
/// socket: the workspace's server, which asks the person whether each may work in the
/// workspace as an agent of its own, and serves it as that agent once they allow it.
pub trait MachineAgents: Send + Sync {
    /// Serve `stream`, the host's end of the connection `from` describes, for as long as it
    /// lasts; it ends when the program closes it or the program that made it ends.
    fn serve(&self, from: AgentConnection, stream: std::os::unix::net::UnixStream);
}

/// The terminal a session's programs run on, when it has one: its pseudo-terminal's slave,
/// shared by whoever needs it while the session is attached to it.
pub type HeldTerminal = std::sync::Arc<std::sync::Mutex<Option<OwnedFd>>>;

pub struct HostedPrograms {
    target: HostedTarget,
    /// The session's Terminal marks key ([`crate::command_marks::KEY_ENV`]): the
    /// held marks carry it, so no program can forge a prompt.
    marks_key: Option<String>,
    workspace: String,
    environment: String,
    /// The workspace's files, which the VM's programs see as `~`.
    home: PathBuf,
    shares: Box<Shares>,
    terminal: HeldTerminal,
    refusals: Option<std::sync::Arc<std::sync::Mutex<Option<server_account::Refusal>>>>,
    secrets: Option<crate::workspace_shell::Secrets>,
    identity: Option<crate::workspace_shell::Identity>,
    /// Who serves the machine's agent socket to the programs, and the session they are of.
    agents: Option<(std::sync::Arc<dyn MachineAgents>, String)>,
}

impl HostedPrograms {
    /// Programs of `workspace`'s `environment` run in its VM with `home`, the workspace's files,
    /// as `~`, and `shares` bound at their own paths.
    #[must_use]
    pub fn new(
        target: HostedTarget,
        workspace: String,
        environment: String,
        home: PathBuf,
        shares: Box<Shares>,
    ) -> Self {
        Self {
            target,
            marks_key: None,
            workspace,
            environment,
            home,
            shares,
            terminal: HeldTerminal::default(),
            refusals: None,
            secrets: None,
            identity: None,
            agents: None,
        }
    }

    /// Serve each program's connections to the machine's agent socket with `agents`, as
    /// programs of the shell session `session`.
    #[must_use]
    pub fn serving_agents(
        mut self,
        agents: std::sync::Arc<dyn MachineAgents>,
        session: impl Into<String>,
    ) -> Self {
        self.agents = Some((agents, session.into()));
        self
    }

    /// What serves the agent socket to a program `agent` asked for, when the host serves it.
    fn agent_server(&self, agent: bool) -> Option<client::AgentServer> {
        let (agents, session) = self.agents.as_ref()?;
        let (workspace, environment, session) = (
            self.workspace.clone(),
            self.environment.clone(),
            session.clone(),
        );
        let agents = std::sync::Arc::clone(agents);
        Some(std::sync::Arc::new(
            move |caller: client::AgentCaller, stream| {
                let from = AgentConnection {
                    workspace: workspace.clone(),
                    environment: environment.clone(),
                    session: session.clone(),
                    agent,
                    program: caller.program,
                };
                agents.serve(from, stream);
            },
        ))
    }

    /// Start each program with what `secrets` holds when it starts, set over the shell's
    /// variables of the same names: the secrets of the run in progress.
    #[must_use]
    pub(crate) fn carrying_secrets(mut self, secrets: crate::workspace_shell::Secrets) -> Self {
        self.secrets = Some(secrets);
        self
    }

    /// Start each program as whoever `identity` holds when it starts: the person or agent the
    /// run is for, which its programs record as git's author and committer.
    #[must_use]
    pub(crate) fn carrying_identity(mut self, identity: crate::workspace_shell::Identity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// The environment a program starts with: the shell's `env`, the run's secrets over it, and
    /// the machine's own variables over both ([`Self::machine_environment`]).
    fn environment(&self, env: &[(String, String)]) -> Vec<(String, String)> {
        let mut started = crate::workspace_shell::with_secrets(env, self.secrets.as_ref());
        for (name, value) in self.machine_environment() {
            started.retain(|(existing, _)| *existing != name);
            started.push((name.to_owned(), value));
        }
        self.own_programs(&mut started);
        started
    }

    /// What a person installs for themselves is found and kept, as in a home on any Linux
    /// machine: `~/.local/bin` comes first on `PATH`, and `npm install --global` puts its
    /// programs there (`NPM_CONFIG_PREFIX`) rather than into the image's own Node. `~` is the
    /// workspace's files, so a program installed once is there for the next run, the next VM and
    /// every member. A `PATH` the shell set keeps its order behind it, and a prefix the shell
    /// set is kept.
    fn own_programs(&self, env: &mut Vec<(String, String)>) {
        let bin = format!("{}/.local/bin", self.home.display());
        let path = env.iter().position(|(name, _)| name == "PATH").map_or_else(
            || "/usr/local/bin:/usr/bin:/bin".to_owned(),
            |at| env.remove(at).1,
        );
        let rest = path.split(':').filter(|dir| !dir.is_empty() && *dir != bin);
        let path = std::iter::once(bin.as_str())
            .chain(rest)
            .collect::<Vec<_>>()
            .join(":");
        env.push(("PATH".to_owned(), path));
        if !env.iter().any(|(name, _)| name == "NPM_CONFIG_PREFIX") {
            env.push((
                "NPM_CONFIG_PREFIX".to_owned(),
                format!("{}/.local", self.home.display()),
            ));
        }
    }

    /// What every program the VM runs starts with, set over the variables it was asked to run
    /// with — even a run's own or its vault's: `HOME` is the workspace's files, the folder the
    /// shell's `~` names, and the caches, toolchains and build output that belong to the machine
    /// rather than to the workspace stay on the VM's disk, in the tenant's folders there. The
    /// toolchain is the one the image ships, so `cargo` and `rustup` work as they do for the
    /// tenant's own login there. Whoever the run is for signs what its programs make, by the
    /// name and address the workspace gives them.
    fn machine_environment(&self) -> Vec<(&'static str, String)> {
        let mut machine = vec![
            ("HOME", self.home.display().to_string()),
            ("XDG_CACHE_HOME", format!("{GUEST_HOME}/.cache")),
            ("XDG_DATA_HOME", format!("{GUEST_HOME}/.local/share")),
            ("CARGO_HOME", format!("{GUEST_HOME}/.cargo")),
            ("RUSTUP_HOME", format!("{GUEST_HOME}/.rustup")),
            ("NPM_CONFIG_CACHE", format!("{GUEST_HOME}/.npm")),
            ("CARGO_TARGET_DIR", GUEST_TARGET.to_owned()),
        ];
        // One VM and one shell serve the workspace's people and agents in turn, so git's
        // identity is the run's own: the actor this command line is asked for.
        machine.extend(crate::workspace_shell::git_identity(self.identity.as_ref()));
        machine
    }

    /// Tell `refusals` each refusal that ends a program, so the caller can
    /// answer it typed (a run's answer to the agent that asked for it).
    #[must_use]
    pub fn reporting_refusals(
        mut self,
        refusals: std::sync::Arc<std::sync::Mutex<Option<server_account::Refusal>>>,
    ) -> Self {
        self.refusals = Some(refusals);
        self
    }

    #[must_use]
    pub fn terminal(self, terminal: OwnedFd) -> Self {
        *self
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            is_terminal(&terminal).then_some(terminal);
        self
    }

    /// Run programs on whatever terminal `held` holds when each starts: a session whose
    /// terminal comes and goes ([`crate::workspace_shell::WorkspaceShells::interactive`]).
    #[must_use]
    pub fn on_terminal_of(mut self, held: HeldTerminal) -> Self {
        self.terminal = held;
        self
    }

    #[must_use]
    pub fn with_marks_key(mut self, key: Option<String>) -> Self {
        self.marks_key = key;
        self
    }

    fn request(&self) -> Request {
        Request::Ensure {
            vm: VmRequest {
                workspace: self.workspace.clone(),
                environment: self.environment.clone(),
                account: self.target.account.clone(),
                size: self.target.size,
            },
            shares: (self.shares)(),
        }
    }
}

/// The shares a policy's grants make: writable directories read-write,
/// How long the hosted-VM service is given to say which VMs it keeps.
const STATUS_PATIENCE: Duration = Duration::from_secs(5);

/// The VMs the hosted-VM service at `socket` keeps, each as its workspace and
/// environment; none when no service answers there.
#[must_use]
pub fn kept(socket: &Path) -> Vec<(String, String)> {
    let Ok(Some(Reply::Status { vms, .. })) =
        control::ask(socket, &Request::Status, STATUS_PATIENCE)
    else {
        return Vec::new();
    };
    vms.into_iter()
        .filter_map(|vm| {
            vm.vm
                .split_once('/')
                .map(|(workspace, environment)| (workspace.to_owned(), environment.to_owned()))
        })
        .collect()
}

/// readable ones read-only.
#[must_use]
pub fn shares_of(writable: Vec<PathBuf>, readable: Vec<PathBuf>) -> Vec<ShareRequest> {
    let mut shares: Vec<ShareRequest> = writable
        .into_iter()
        .map(|path| ShareRequest {
            path,
            read_only: false,
        })
        .collect();
    for path in readable {
        if !shares.iter().any(|share| share.path == path) {
            shares.push(ShareRequest {
                path,
                read_only: true,
            });
        }
    }
    shares
}

struct InputReader(Input);

impl Read for InputReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

struct OutputWriter<'a>(&'a Output);

impl Write for OutputWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn pause(wait: Duration, stop: Option<&dyn Fn() -> bool>) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < wait {
        if stop.is_some_and(|stopped| stopped()) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// A command the hosts have no room for yet: say so once, wait a while, and
/// end it, having run nothing, if it was cancelled meanwhile.
fn wait_for_room(
    name: &str,
    io: &Stdio,
    words: &str,
    told: &mut bool,
    stop: Option<&dyn Fn() -> bool>,
) -> Result<(), Program> {
    if !*told {
        let _ = io
            .stderr
            .write_str(&format!("ush: {name}: {words} Waiting; Ctrl+C cancels.\n"));
        *told = true;
    }
    if pause(BUSY_RETRY, stop) {
        return Ok(());
    }
    let _ = io.stderr.write_str(&format!(
        "ush: {name}: cancelled. Nothing ran, because no machine had room yet.\n"
    ));
    Err(Program::Exited(130))
}

fn failed(name: &str, io: &Stdio, reason: &str) -> Program {
    let _ = io.stderr.write_str(&format!("ush: {name}: {reason}\n"));
    Program::Exited(126)
}

impl HostedPrograms {
    fn admitted(
        &self,
        name: &str,
        agent: bool,
        io: &Stdio,
        stop: Option<&dyn Fn() -> bool>,
    ) -> Result<AgentLink, Program> {
        let mut waiting: Option<Hold> = None;
        let mut busy_told = false;
        loop {
            let refusal = match control::ask(&self.target.socket, &self.request(), WAKE_PATIENCE) {
                Ok(Some(Reply::Ready { link, .. })) => {
                    if waiting.is_some() {
                        self.mark(io, &released_mark);
                    }
                    return Ok(link);
                }
                Ok(Some(Reply::Busy(words))) => {
                    wait_for_room(name, io, &words, &mut busy_told, stop)?;
                    continue;
                }
                Ok(Some(Reply::Refused(refusal))) => refusal,
                Ok(Some(Reply::Error(error))) => return Err(failed(name, io, &error)),
                Ok(Some(other)) => {
                    return Err(failed(
                        name,
                        io,
                        &format!("the hosted-VM service answered {other:?}"),
                    ));
                }
                Ok(None) => {
                    return Err(failed(
                        name,
                        io,
                        "this workspace's Linux VM service is not running on its host",
                    ));
                }
                Err(error) => {
                    return Err(failed(
                        name,
                        io,
                        &format!("this workspace's Linux VM: {error}"),
                    ));
                }
            };
            let waits = !agent && refusal.awaits_the_account();
            let plans = self.target.plans.as_deref().filter(|_| waits);
            let Some(plans) = plans else {
                if waiting.is_some() {
                    self.mark(io, &released_mark);
                }
                if refusal.awaits_the_account() {
                    self.mark(io, &|key| held_mark(key, agent, &refusal));
                }
                if let Some(told) = &self.refusals {
                    *told
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(refusal.clone());
                }
                let words = if agent && refusal.awaits_the_account() {
                    format!(
                        "{} Ask the person to add a card; Terminal shows them where. Then \
                         run the command again.",
                        refusal.message
                    )
                } else {
                    refusal.message
                };
                return Err(failed(name, io, &words));
            };
            match &waiting {
                // Watch before asking again, so a change between the
                // refusal and the watch is not missed.
                None => match Hold::start(plans) {
                    Ok(hold) => {
                        let words = if self.mark(io, &|key| held_mark(key, false, &refusal)) {
                            "needs a Linux machine.".to_owned()
                        } else {
                            match refusal
                                .step(server_account::NextStepKind::AddPaymentMethod)
                                .and_then(|step| step.url.as_deref())
                            {
                                Some(url) => format!("{} Add it at {url}.", refusal.message),
                                None => refusal.message.clone(),
                            }
                        };
                        let _ = io.stderr.write_str(&format!(
                            "ush: {name}: {words} Waiting for the card; Ctrl+C cancels.\n"
                        ));
                        waiting = Some(hold);
                    }
                    Err(error) => {
                        return Err(failed(name, io, &format!("{} ({error})", refusal.message)));
                    }
                },
                Some(hold) => {
                    if !hold.changed(stop) {
                        self.mark(io, &released_mark);
                        let _ = io.stderr.write_str(&format!(
                            "ush: {name}: cancelled. Nothing ran, because no card was added.\n"
                        ));
                        return Err(Program::Exited(130));
                    }
                }
            }
        }
    }

    /// Write the mark `make` makes of the marks key to the session's
    /// terminal, when the session has a key and the command writes to the
    /// terminal itself.
    /// Write the mark `make` makes with the session's key to the terminal the session's runs are
    /// shown in, when it is one of the run's streams. Whether it was written: Terminal shows the
    /// prompt of a held mark, so the shell's own words can stay short.
    fn mark(&self, io: &Stdio, make: &dyn Fn(&str) -> String) -> bool {
        let Some(key) = &self.marks_key else {
            return false;
        };
        let terminal = if io.stderr.host_stream() == Some(Std::Err) {
            &io.stderr
        } else if io.stdout.host_stream() == Some(Std::Out) {
            &io.stdout
        } else {
            return false;
        };
        terminal.write_str(&make(key)).is_ok()
    }
}

/// `W`: the run going on waits on its host, which refused it for
/// `refusal`; `agent` says an agent asked for it, so the shell is not
/// waiting.
#[must_use]
pub fn held_mark(key: &str, agent: bool, refusal: &server_account::Refusal) -> String {
    use base64::Engine as _;
    let json = serde_json::to_vec(refusal).unwrap_or_default();
    format!(
        "\x1b]133;W;k={key};a={};r={}\x07",
        u8::from(agent),
        base64::engine::general_purpose::STANDARD.encode(json)
    )
}

#[must_use]
pub fn released_mark(key: &str) -> String {
    format!("\x1b]133;U;k={key}\x07")
}

struct Hold {
    changes: std::sync::mpsc::Receiver<()>,
    _watch: tilo_files::native::ChangeWatch,
}

impl Hold {
    fn start(plans: &Path) -> std::io::Result<Self> {
        let (tell, changes) = std::sync::mpsc::channel();
        let watch = tilo_server_account::watch_plans(plans, move || {
            let _ = tell.send(());
        })?;
        Ok(Self {
            changes,
            _watch: watch,
        })
    }

    fn changed(&self, stop: Option<&dyn Fn() -> bool>) -> bool {
        use std::sync::mpsc::RecvTimeoutError;
        let Some(stop) = stop else {
            return self.changes.recv().is_ok();
        };
        loop {
            match self.changes.recv_timeout(Duration::from_millis(50)) {
                Ok(()) => {
                    while self.changes.try_recv().is_ok() {}
                    return true;
                }
                Err(RecvTimeoutError::Timeout) if stop() => return false,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return false,
            }
        }
    }
}

impl Programs for HostedPrograms {
    fn run(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        stop: Option<&dyn Fn() -> bool>,
    ) -> Program {
        let Some(name) = argv.first() else {
            return Program::Exited(0);
        };
        let agent = env
            .iter()
            .any(|(key, value)| key == "USH_RENDER" && value == "agent");
        let link = match self.admitted(name, agent, io, stop) {
            Ok(link) => link,
            Err(program) => return program,
        };
        let stream = match link.connect(AGENT_PORT) {
            Ok(stream) => stream,
            Err(error) => return failed(name, io, &format!("this workspace's Linux VM: {error}")),
        };
        let terminal = self
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|fd| is_terminal(fd))
            .and_then(|fd| fd.try_clone().ok());
        let terminal = terminal.as_ref().filter(|_| {
            io.stdin.host_stream() == Some(Std::In) && io.stdout.host_stream() == Some(Std::Out)
        });
        let launch = Launch {
            argv: argv.to_vec(),
            cwd: Some(cwd.display().to_string()),
            env: self.environment(env),
            stdio: terminal.map_or(Stream::Pump, |fd| Stream::Terminal(terminal_size(fd))),
            display: Display::None,
        };
        // On the session's terminal the keys are relayed below. Otherwise
        // the VM has no terminal of the session's: a program reading the
        // terminal gets an empty input rather than taking the shell's keys.
        let stdin: Option<Box<dyn Read + Send>> = match io.stdin.host_stream() {
            Some(Std::In) => None,
            _ => Some(Box::new(InputReader(io.stdin.clone()))),
        };
        let (mut stdout, mut stderr) = (OutputWriter(&io.stdout), OutputWriter(&io.stderr));
        let (handing, handed) = std::sync::mpsc::channel::<LaunchSignals<AgentStream>>();
        let finished = AtomicBool::new(false);
        let agents = self.agent_server(agent);
        let result = std::thread::scope(|scope| {
            let running = scope.spawn(|| {
                let io = LaunchIo {
                    stdin,
                    stdout: &mut stdout,
                    stderr: &mut stderr,
                    started: &mut |_| {},
                };
                let handed = move |signals| {
                    let _ = handing.send(signals);
                };
                let result = match &agents {
                    Some(agents) => {
                        client::launch_serving_agents(stream, &launch, io, agents, handed)
                    }
                    None => client::launch(stream, &launch, io, handed),
                };
                finished.store(true, Ordering::SeqCst);
                result
            });
            if let Ok(signals) = handed.recv() {
                if let Some(fd) = terminal {
                    let relay = signals.clone();
                    let finished = &finished;
                    scope.spawn(move || relay_terminal(fd, &relay, finished));
                }
                if let Some(stop) = stop {
                    while !running.is_finished() {
                        if stop() {
                            let _ = signals.send(Signal::Kill);
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
            running.join().unwrap_or(Err(ProtocolError::Closed))
        });
        match result {
            Ok(status) => Program::Exited(status.shell_code()),
            Err(ProtocolError::Refused(message))
                if message.ends_with("No such file or directory (os error 2)") =>
            {
                Program::NotFound
            }
            Err(error) => failed(name, io, &format!("this workspace's Linux VM: {error}")),
        }
    }
}

fn is_terminal(fd: &OwnedFd) -> bool {
    // SAFETY: `isatty` only inspects the descriptor.
    unsafe { libc::isatty(fd.as_raw_fd()) == 1 }
}

/// The terminal's size, 80x24 when it will not say.
pub(crate) fn terminal_size(fd: &impl std::os::fd::AsRawFd) -> TerminalSize {
    // SAFETY: TIOCGWINSZ writes only into the struct, which lives across
    // the call.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCGWINSZ, &raw mut size) } == 0
        && size.ws_col > 0
        && size.ws_row > 0
    {
        TerminalSize {
            cols: size.ws_col,
            rows: size.ws_row,
        }
    } else {
        TerminalSize { cols: 80, rows: 24 }
    }
}

/// A terminal in raw mode while this is held: keys and output pass through as they are.
pub(crate) struct Raw<'a> {
    fd: &'a OwnedFd,
    cooked: Option<libc::termios>,
}

impl<'a> Raw<'a> {
    pub(crate) fn new(fd: &'a OwnedFd) -> Self {
        // SAFETY: tcgetattr and tcsetattr read and write only the termios
        // structs owned here.
        unsafe {
            let mut cooked: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd.as_raw_fd(), &raw mut cooked) != 0 {
                return Self { fd, cooked: None };
            }
            let mut raw = cooked;
            libc::cfmakeraw(&raw mut raw);
            raw.c_cc[libc::VMIN] = 1;
            raw.c_cc[libc::VTIME] = 0;
            let set = libc::tcsetattr(fd.as_raw_fd(), libc::TCSADRAIN, &raw const raw) == 0;
            Self {
                fd,
                cooked: set.then_some(cooked),
            }
        }
    }
}

impl Raw<'_> {
    /// Leave the terminal raw for good: the window it is lent to never has it cooked.
    pub(crate) fn keep(mut self) {
        self.cooked = None;
    }
}

impl Drop for Raw<'_> {
    fn drop(&mut self) {
        if let Some(cooked) = self.cooked {
            unsafe { libc::tcsetattr(self.fd.as_raw_fd(), libc::TCSADRAIN, &raw const cooked) };
        }
    }
}

const RELAY_TICK: Duration = Duration::from_millis(100);

/// Pump the session's keys and size to a program on a terminal in the VM
/// until it ends. Reads only what is ready, so no read is left waiting for
/// a key the shell needs next. A terminal that hangs up (its window closed, or nobody
/// came back to a kept session) ends the program, as a hangup ends one on a device.
fn relay_terminal(fd: &OwnedFd, signals: &LaunchSignals<AgentStream>, finished: &AtomicBool) {
    let _raw = Raw::new(fd);
    let mut size = terminal_size(fd);
    let mut buffer = [0_u8; 4096];
    let tick = libc::c_int::try_from(RELAY_TICK.as_millis()).unwrap_or(100);
    while !finished.load(Ordering::SeqCst) {
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&raw mut poll, 1, tick) };
        if ready > 0 && poll.revents & libc::POLLIN != 0 {
            let read =
                unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            let Ok(read) = usize::try_from(read) else {
                let error = std::io::Error::last_os_error();
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) {
                    continue;
                }
                // A pty's slave reads EIO once its master is gone: a hangup.
                let _ = signals.send(Signal::Terminate);
                return;
            };
            if read == 0 {
                let _ = signals.send(Signal::Terminate);
                return;
            }
            if signals
                .input(buffer.get(..read).unwrap_or_default())
                .is_err()
            {
                return;
            }
        } else if ready > 0 && poll.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
            let _ = signals.send(Signal::Terminate);
            return;
        }
        let now = terminal_size(fd);
        if now != size {
            size = now;
            if signals.resize(size).is_err() {
                return;
            }
        }
    }
}

/// A workspace's machine for tests: a stand-in for the hosted-VM service in
/// front of the real guest agent, run in this process as the test's own user.
#[cfg(test)]
pub(crate) mod testing {
    #![allow(
        clippy::unwrap_used,
        reason = "a test fixture: a machine it cannot set up is a test that failed"
    )]

    use super::*;

    pub(crate) fn fake_vm(dir: &Path) -> PathBuf {
        use std::io::BufRead as _;
        let socket = dir.join("v.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let user = linux_vm_agent::user::GuestUser {
            name: "me".into(),
            uid: rustix::process::getuid().as_raw(),
            gid: rustix::process::getgid().as_raw(),
            home: std::env::temp_dir(),
            shell: "/bin/sh".into(),
        };
        let agent = std::sync::Arc::new(
            linux_vm_agent::server::Agent::new(user, "boot".into())
                .with_agent_socket(dir.join("run/tilo/agent.sock")),
        );
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let agent = std::sync::Arc::clone(&agent);
                std::thread::spawn(move || {
                    let mut line = String::new();
                    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                    reader.read_line(&mut line).unwrap();
                    assert_eq!(line, format!("CONNECT {AGENT_PORT}\n"));
                    stream.write_all(b"OK 1\n").unwrap();
                    let _ = agent.serve(stream);
                });
            }
        });
        socket
    }

    pub(crate) fn fake_service(dir: &Path, vm: PathBuf) -> PathBuf {
        let socket = dir.join("hosted.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let _: Option<Request> = control::FRAMING.read_value(&mut stream).unwrap();
                control::FRAMING
                    .write_value(
                        &mut stream,
                        &Reply::Ready {
                            link: linux_vm_protocol::link::AgentLink::Hybrid { socket: vm.clone() },
                            woke_ms: None,
                        },
                    )
                    .unwrap();
            }
        });
        socket
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ush_shell::io::Captured;

    use super::testing::{fake_service, fake_vm};

    tilo_test_env::scenario! {
        /// A hosted workspace's program in its VM: the grants it is given, a refusal's words and the session's terminal.
        fn a_program_reaches_the_workspaces_vm_with_its_grants_refusals_and_terminal;

        phase writable_grants_win_over_readable_ones;
        {
            let shares = shares_of(
                vec!["/srv/ws/files".into()],
                vec!["/srv/ws/files".into(), "/srv/shared/docs".into()],
            );
            assert_eq!(
                shares,
                [
                    ShareRequest {
                        path: "/srv/ws/files".into(),
                        read_only: false
                    },
                    ShareRequest {
                        path: "/srv/shared/docs".into(),
                        read_only: true
                    }
                ]
            );
        }

        phase a_plans_refusal_reaches_the_program_word_for_word;
        {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("hosted.sock");
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request: Request = control::FRAMING.read_value(&mut stream).unwrap().unwrap();
                let Request::Ensure { vm, shares } = request else {
                    panic!("an ensure");
                };
                assert_eq!(
                    (
                        vm.workspace.as_str(),
                        vm.environment.as_str(),
                        vm.account.as_str(),
                        vm.size
                    ),
                    ("ws1", "default", "acct-1", VmSize::Large)
                );
                assert_eq!(shares.len(), 1);
                control::FRAMING
                    .write_value(
                        &mut stream,
                        &Reply::Refused(server_account::Refusal {
                            reason: server_account::Reason::TooManyRunning {
                                limit: 1,
                                running: vec!["ws2".into()],
                            },
                            message: "Your plan runs at most 1 Linux VM at a time.".into(),
                            next: Vec::new(),
                        }),
                    )
                    .unwrap();
            });
            let programs = HostedPrograms::new(
                HostedTarget {
                    socket,
                    account: "acct-1".into(),
                    size: VmSize::Large,
                    plans: None,
                },
                "ws1".into(),
                "default".into(),
                "/srv/ws1/files".into(),
                Box::new(|| shares_of(vec!["/srv/ws1/files".into()], Vec::new())),
            );
            let (stderr, err) = Output::buffer();
            let io = Stdio {
                stderr,
                ..Stdio::null()
            };
            let ran = programs.run(
                &["cargo".into()],
                &[],
                Path::new("/srv/ws1/files"),
                &io,
                None,
            );
            server.join().unwrap();
            assert_eq!(ran, Program::Exited(126));
            assert_eq!(
                String::from_utf8(err.take()).unwrap(),
                "ush: cargo: Your plan runs at most 1 Linux VM at a time.\n"
            );
        }

        phase a_program_on_the_sessions_terminal_gets_its_keys_and_size_in_the_vm;
        {
            use std::os::fd::FromRawFd as _;
            let dir = tempfile::tempdir().unwrap();
            let socket = fake_service(dir.path(), fake_vm(dir.path()));
            let (mut master, mut slave) = (-1, -1);
            let size = libc::winsize {
                ws_row: 33,
                ws_col: 101,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            let opened = unsafe {
                libc::openpty(
                    &raw mut master,
                    &raw mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &raw const size,
                )
            };
            assert_eq!(opened, 0);
            // SAFETY: openpty succeeded; both descriptors are owned only here.
            let (master, slave) =
                unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
            let mut keys = std::fs::File::from(master);
            let programs = HostedPrograms::new(
                HostedTarget {
                    socket,
                    account: "acct-1".into(),
                    size: VmSize::Small,
                    plans: None,
                },
                "ws1".into(),
                "default".into(),
                "/srv/ws1/files".into(),
                Box::new(Vec::new),
            )
            .terminal(slave.try_clone().unwrap());
            assert!(
                programs.terminal.lock().unwrap().is_some(),
                "a pty is a terminal"
            );
            let (stdout, out) = Output::buffer();
            let io = Stdio {
                stdin: Input::host(std::fs::File::from(slave.try_clone().unwrap())),
                stdout: Output::host(Std::Out, OutputWriter2(stdout)),
                stderr: Output::null(),
            };
            let typist = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                keys.write_all(b"secret\r").unwrap();
                keys
            });
            let ran = programs.run(
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    "stty size; printf 'name? '; read name; echo \"hi $name\"".into(),
                ],
                &[],
                Path::new("/"),
                &io,
                None,
            );
            let _keys = typist.join().unwrap();
            assert_eq!(ran, Program::Exited(0));
            let out = String::from_utf8(out.take()).unwrap();
            assert!(
                out.contains("33 101"),
                "the VM's terminal has the session's size: {out:?}"
            );
            assert!(out.contains("name? "), "{out:?}");
            assert!(
                out.contains("hi secret"),
                "the typed line reached the program: {out:?}"
            );
            let mut modes: libc::termios = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::tcgetattr(slave.as_raw_fd(), &raw mut modes) },
                0
            );
            assert_ne!(modes.c_lflag & libc::ICANON, 0);
        }
    }

    /// The workspace's server, as far as the machine's agent socket sees it: it answers each
    /// line a connection sends with `admitted <line>`, and keeps where each came from.
    #[derive(Default)]
    struct Admitting(std::sync::Mutex<Vec<AgentConnection>>);

    impl MachineAgents for Admitting {
        fn serve(&self, mut from: AgentConnection, mut stream: std::os::unix::net::UnixStream) {
            assert!(from.program.contains("python"), "{from:?}");
            from.program.clear();
            self.0.lock().unwrap().push(from);
            let mut line = Vec::new();
            let mut byte = [0_u8; 1];
            while std::io::Read::read(&mut stream, &mut byte).is_ok_and(|read| read == 1) {
                if byte[0] == b'\n' {
                    let answer = format!("admitted {}\n", String::from_utf8_lossy(&line));
                    let _ = stream.write_all(answer.as_bytes());
                    line.clear();
                } else {
                    line.push(byte[0]);
                }
            }
        }
    }

    /// A stand-in for `tilo-runtime --connect`: connects where `TILO_AGENT_SOCK` says, says
    /// one line and prints its answer.
    const CONNECTS: &str = r#"
import os, socket
s = socket.socket(socket.AF_UNIX)
s.connect(os.environ["TILO_AGENT_SOCK"])
s.sendall(b"initialize\n")
print("answered " + s.makefile("rb").readline().decode().strip(), flush=True)
"#;

    tilo_test_env::scenario! {
        fn a_program_in_the_machine_reaches_the_workspaces_server_at_its_agent_socket;

        phase its_connection_reaches_the_server_named_by_its_workspace_and_session;
        {
            let dir = tempfile::tempdir().unwrap();
            let socket = fake_service(dir.path(), fake_vm(dir.path()));
            let server = std::sync::Arc::new(Admitting::default());
            let programs = HostedPrograms::new(
                HostedTarget {
                    socket,
                    account: "acct-1".into(),
                    size: VmSize::Small,
                    plans: None,
                },
                "ws1".into(),
                "dev".into(),
                FILES.into(),
                Box::new(Vec::new),
            )
            .serving_agents(std::sync::Arc::clone(&server) as std::sync::Arc<dyn MachineAgents>, "s1");
            let (stdout, out) = Output::buffer();
            let io = Stdio {
                stdout,
                ..Stdio::null()
            };
            let ran = programs.run(
                &["python3".into(), "-I".into(), "-c".into(), CONNECTS.into()],
                &[],
                Path::new("/"),
                &io,
                None,
            );
            let out = String::from_utf8(out.take()).unwrap();
            assert_eq!(ran, Program::Exited(0), "{out}");
            assert!(out.contains("answered admitted initialize"), "{out}");
            assert_eq!(
                *server.0.lock().unwrap(),
                [AgentConnection {
                    workspace: "ws1".into(),
                    environment: "dev".into(),
                    session: "s1".into(),
                    agent: false,
                    program: String::new(),
                }]
            );
        }

        phase a_host_that_serves_no_agents_leaves_the_socket_unnamed;
        {
            let dir = tempfile::tempdir().unwrap();
            let socket = fake_service(dir.path(), fake_vm(dir.path()));
            let programs = HostedPrograms::new(
                HostedTarget {
                    socket,
                    account: "acct-1".into(),
                    size: VmSize::Small,
                    plans: None,
                },
                "ws1".into(),
                "dev".into(),
                FILES.into(),
                Box::new(Vec::new),
            );
            let (stdout, out) = Output::buffer();
            let io = Stdio {
                stdout,
                ..Stdio::null()
            };
            let ran = programs.run(
                &["/bin/sh".into(), "-c".into(), "echo \"[${TILO_AGENT_SOCK-}]\"".into()],
                &[],
                Path::new("/"),
                &io,
                None,
            );
            assert_eq!(ran, Program::Exited(0));
            assert_eq!(String::from_utf8(out.take()).unwrap(), "[]\n");
        }
    }

    struct OutputWriter2(Output);

    impl Write for OutputWriter2 {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.write_all(buf)?;
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    const KEY: &str = "0123456789abcdef0123";

    /// The workspace's files as a session's shell names them.
    const FILES: &str = "/srv/ws1/files";

    fn session(
        host: &test_host::TestHost,
        account: &str,
    ) -> (HostedPrograms, Stdio, Captured, Captured) {
        let programs = HostedPrograms::new(
            host.target(account),
            "ws1".into(),
            "default".into(),
            FILES.into(),
            Box::new(Vec::new),
        )
        .with_marks_key(Some(KEY.into()));
        let (_, out) = Output::buffer();
        let (_, err) = Output::buffer();
        let io = Stdio {
            stdout: Output::host(Std::Out, out.clone()),
            stderr: Output::host(Std::Err, err.clone()),
            ..Stdio::null()
        };
        (programs, io, out, err)
    }

    fn held(text: &str) -> Option<(bool, server_account::Refusal)> {
        use base64::Engine as _;
        let mark = text.split("\x1b]133;W;").nth(1)?.split('\x07').next()?;
        let mut agent = None;
        let mut refusal = None;
        for field in mark.split(';') {
            match field.split_once('=') {
                Some(("k", key)) => assert_eq!(key, KEY),
                Some(("a", flag)) => agent = Some(flag == "1"),
                Some(("r", json)) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(json)
                        .unwrap();
                    refusal = Some(serde_json::from_slice(&bytes).unwrap());
                }
                _ => panic!("an unknown field in {mark:?}"),
            }
        }
        Some((agent?, refusal?))
    }

    tilo_test_env::scenario! {
        /// A command that needs a Linux machine the account or the host cannot give yet.
        fn a_command_the_account_cannot_run_yet_is_held_refused_or_cancelled;

        phase a_persons_command_without_a_card_waits_for_one_then_runs;
        {
            let dir = tempfile::tempdir().unwrap();
            let host = test_host::TestHost::start(dir.path());
            let (programs, io, out, err) = session(&host, "acct-1");
            let running = std::thread::spawn(move || {
                programs.run(
                    &["cargo".into(), "build".into()],
                    &[],
                    Path::new("/"),
                    &io,
                    None,
                )
            });
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mark = loop {
                let text = String::from_utf8_lossy(&err.peek()).into_owned();
                if let Some(mark) = held(&text) {
                    break mark;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "no held mark: {text:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            };
            let (agent, refusal) = mark;
            assert!(!agent);
            assert_eq!(refusal.reason, server_account::Reason::NoPaymentMethod);
            assert_eq!(
                refusal
                    .step(server_account::NextStepKind::AddPaymentMethod)
                    .and_then(|step| step.url.as_deref()),
                Some("https://tilo.sh/billing/card?account=acct-1")
            );
            assert!(!running.is_finished(), "a person's command waits");
            host.add_card("acct-1");
            assert_eq!(running.join().unwrap(), Program::Exited(0));
            assert_eq!(String::from_utf8(out.take()).unwrap(), "ran cargo build\n");
            let terminal = String::from_utf8(err.take()).unwrap();
            assert!(
                terminal.ends_with(&format!("\x1b]133;U;k={KEY}\x07")),
                "{terminal:?}"
            );
            assert!(host.wakes_asked() >= 2);
        }

        phase cancelling_a_held_command_ends_it_having_run_nothing;
        {
            let dir = tempfile::tempdir().unwrap();
            let host = test_host::TestHost::start(dir.path());
            let (programs, io, out, err) = session(&host, "acct-1");
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let cancelled = std::sync::Arc::clone(&cancel);
            let running = std::thread::spawn(move || {
                let stop = move || cancelled.load(std::sync::atomic::Ordering::SeqCst);
                programs.run(&["cargo".into()], &[], Path::new("/"), &io, Some(&stop))
            });
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while held(&String::from_utf8_lossy(&err.peek())).is_none() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the command was never held"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(running.join().unwrap(), Program::Exited(130));
            assert!(out.take().is_empty());
            let terminal = String::from_utf8(err.take()).unwrap();
            assert!(
                terminal.contains("ush: cargo: cancelled. Nothing ran, because no card was added.\n"),
                "{terminal:?}"
            );
            assert!(terminal.contains(&format!("\x1b]133;U;k={KEY}\x07")));
        }

        phase an_agents_command_is_refused_at_once_with_the_typed_refusal;
        {
            let dir = tempfile::tempdir().unwrap();
            let host = test_host::TestHost::start(dir.path());
            let (programs, io, out, err) = session(&host, "acct-1");
            let ran = programs.run(
                &["cargo".into()],
                &[("USH_RENDER".into(), "agent".into())],
                Path::new("/"),
                &io,
                None,
            );
            assert_eq!(ran, Program::Exited(126));
            assert_eq!(host.wakes_asked(), 1, "an agent's command never waits");
            assert!(out.take().is_empty());
            let terminal = String::from_utf8(err.take()).unwrap();
            let (agent, refusal) = held(&terminal).expect("Terminal gets the typed refusal");
            assert!(agent);
            assert_eq!(refusal.reason, server_account::Reason::NoPaymentMethod);
            assert!(
                terminal.ends_with(
                    "ush: cargo: This needs a Linux machine. Tilo Cloud runs Linux machines for \
                 accounts with a card on file, and this account has none yet. Add a card to \
                 continue. Ask the person to add a card; Terminal shows them where. Then run \
                 the command again.\n"
                ),
                "{terminal:?}"
            );
        }

        phase a_command_waiting_for_room_ends_when_cancelled_having_run_nothing;
        {
            let dir = tempfile::tempdir().unwrap();
            let host = test_host::TestHost::start(dir.path());
            std::fs::write(host.plans(), b"{}").unwrap();
            host.full_for(usize::MAX);
            let (programs, io, out, err) = session(&host, "acct-1");
            let stop = || true;
            let ran = programs.run(&["cargo".into()], &[], Path::new("/"), &io, Some(&stop));
            assert_eq!(ran, Program::Exited(130));
            assert!(out.take().is_empty());
            assert!(
                String::from_utf8(err.take()).unwrap().ends_with(
                    "ush: cargo: cancelled. Nothing ran, because no machine had room yet.\n"
                )
            );
        }

        phase a_host_that_sells_nothing_runs_the_command_with_no_prompt;
        {
            let dir = tempfile::tempdir().unwrap();
            let host = test_host::TestHost::start(dir.path());
            std::fs::write(host.plans(), b"{}").unwrap();
            let (programs, io, out, err) = session(&host, "acct-1");
            let ran = programs.run(&["make".into()], &[], Path::new("/"), &io, None);
            assert_eq!(ran, Program::Exited(0));
            assert_eq!(String::from_utf8(out.take()).unwrap(), "ran make\n");
            assert!(err.take().is_empty(), "no prompt, no mark");
        }
    }

    #[test]
    fn a_command_waits_while_no_machine_has_room_then_runs() {
        let dir = tempfile::tempdir().unwrap();
        let host = test_host::TestHost::start(dir.path());
        std::fs::write(host.plans(), b"{}").unwrap();
        host.full_for(2);
        let (programs, io, out, err) = session(&host, "acct-1");
        // An agent's command waits too: being full is never a refusal.
        let ran = programs.run(
            &["cargo".into(), "build".into()],
            &[("USH_RENDER".into(), "agent".into())],
            Path::new("/"),
            &io,
            None,
        );
        assert_eq!(ran, Program::Exited(0));
        assert_eq!(host.wakes_asked(), 3, "asked again until there was room");
        assert_eq!(String::from_utf8(out.take()).unwrap(), "ran cargo build\n");
        assert_eq!(
            String::from_utf8(err.take()).unwrap(),
            "ush: cargo: The machine is full until one of its VMs goes idle. Waiting; Ctrl+C \
             cancels.\n",
            "said once, however long it waits"
        );
    }

    #[test]
    fn a_run_reaches_the_vm_with_the_workspaces_files_as_home_and_the_vms_disk_for_caches() {
        let dir = tempfile::tempdir().unwrap();
        let host = test_host::TestHost::start(dir.path());
        std::fs::write(host.plans(), b"{}").unwrap();
        let files = dir.path().join("files");
        std::fs::create_dir_all(&files).unwrap();
        let programs = HostedPrograms::new(
            host.target("acct-1"),
            "ws1".into(),
            "default".into(),
            files.clone(),
            Box::new(Vec::new),
        );
        let (stdout, _out) = Output::buffer();
        let io = Stdio {
            stdout,
            ..Stdio::null()
        };
        let ran = programs.run(
            &["cargo".into(), "build".into()],
            &[("HOME".into(), "/root".into())],
            &files,
            &io,
            None,
        );
        assert_eq!(ran, Program::Exited(0));
        let (argv, env) = host.launched().pop().expect("the VM was asked to run it");
        assert_eq!(argv, ["cargo", "build"]);
        let get = |name: &str| {
            env.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(
            get("HOME"),
            Some(files.display().to_string()),
            "the shell's `~`, even over what the run was given"
        );
        assert_eq!(get("XDG_CACHE_HOME"), Some("/home/tilo/.cache".into()));
        assert_eq!(get("XDG_DATA_HOME"), Some("/home/tilo/.local/share".into()));
        assert_eq!(get("CARGO_HOME"), Some("/home/tilo/.cargo".into()));
        assert_eq!(get("RUSTUP_HOME"), Some("/home/tilo/.rustup".into()));
        assert_eq!(get("NPM_CONFIG_CACHE"), Some("/home/tilo/.npm".into()));
        assert_eq!(get("CARGO_TARGET_DIR"), Some("/var/tmp/tilo/target".into()));
        assert_eq!(
            get("PATH"),
            Some(format!(
                "{}/.local/bin:/usr/local/bin:/usr/bin:/bin",
                files.display()
            )),
            "what the person installed in `~` comes first"
        );
        assert_eq!(
            get("NPM_CONFIG_PREFIX"),
            Some(format!("{}/.local", files.display())),
            "`npm install --global` installs into `~`"
        );
    }

    #[test]
    fn a_program_works_in_the_workspaces_files_and_writes_its_machines_cache_outside_them() {
        let dir = tempfile::tempdir().unwrap();
        let socket = fake_service(dir.path(), fake_vm(dir.path()));
        let files = dir.path().join("files");
        std::fs::create_dir_all(&files).unwrap();
        let shared = files.clone();
        let programs = HostedPrograms::new(
            HostedTarget {
                socket,
                account: "acct-1".into(),
                size: VmSize::Small,
                plans: None,
            },
            "ws1".into(),
            "default".into(),
            files.clone(),
            Box::new(move || shares_of(vec![shared.clone()], Vec::new())),
        );
        let (stdout, out) = Output::buffer();
        let io = Stdio {
            stdout,
            ..Stdio::null()
        };
        let asked = files.display().to_string();
        let ran = programs.run(
            &[
                "/bin/sh".into(),
                "-c".into(),
                "printf '%s\\n' \"$HOME\" \"$XDG_CACHE_HOME\" \"$CARGO_HOME\" \
                 \"$CARGO_TARGET_DIR\"; echo x > ~/f"
                    .into(),
            ],
            &[
                ("HOME".into(), "/root".into()),
                (
                    "XDG_CACHE_HOME".into(),
                    files.join(".cache").display().to_string(),
                ),
            ],
            &files,
            &io,
            None,
        );
        assert_eq!(ran, Program::Exited(0));
        let said = String::from_utf8(out.take()).unwrap();
        assert_eq!(
            said.lines().collect::<Vec<_>>(),
            [
                asked.as_str(),
                "/home/tilo/.cache",
                "/home/tilo/.cargo",
                "/var/tmp/tilo/target",
            ],
            "the machine's own paths, over the run's"
        );
        assert_eq!(
            std::fs::read_to_string(files.join("f")).unwrap(),
            "x\n",
            "`~` is the workspace's files"
        );
        assert!(
            !files.join(".cache").exists(),
            "no cache of the machine's landed in the workspace's files"
        );
    }
}
