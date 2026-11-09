//! Programs a hosted workspace's shell runs in the workspace's own VM
//! (tier 3 on a host that serves other people's workspaces).
//!
//! On such a host, a stranger's `cargo` or `python` never runs in a guest
//! sharing the host's kernel: [`HostedPrograms`] asks the host's hosted-VM
//! service ([`legacy_vm::hosted`]) for the VM of this workspace environment,
//! which boots it on first use and restores it from its snapshot after it
//! went idle, and runs each program there through the VM's agent, with the
//! shell's streams relayed and the environment's granted directories at the
//! same paths. The shell, its own commands and their run records are the
//! same as on a device; only where a program runs differs.
//!
//! When the host refuses the VM, the refusal is typed
//! ([`host_account::Refusal`]) and says what would let it through:
//!
//! - **A person's command** the account's standing would let through (a
//!   card added on a host that sells VMs) waits. Terminal is told it is held
//!   ([`held_mark`]) and shows the person the prompt, with the provider's
//!   page; once the host's plans file names the account, the command runs,
//!   without the person typing it again. Interrupting it (Terminal's Cancel
//!   is Ctrl+C) ends it with a plain line, having run nothing.
//! - **An agent's command** (the shell hands it on with `USH_RENDER=agent`)
//!   never waits: it fails at once with the refusal's words and what to ask
//!   the person, and Terminal gets the typed refusal for the agent's run and
//!   shows the person the same prompt.
//! - Anything else fails with the refusal's words.
//!
//! When the hosts have no room for the VM right now ([`Reply::Busy`]: every
//! machine full, another one starting), the command waits, a person's and an
//! agent's alike, after one line saying so; it is never refused for it.
//! Interrupting it ends it having run nothing.
//!
//! A program whose input and output are the session's terminal runs on a
//! pseudo-terminal in the VM ([`Relay::Terminal`]): the session's keys go
//! to it as typed (the session's terminal is raw meanwhile; the VM's
//! terminal does the line editing and turns Ctrl-C into a signal there),
//! its output comes back as the program's standard output, recorded like
//! any other, and the VM's terminal follows the session's size. So
//! editors, prompts and pagers work as they do on a device.
//!
//! A host turns this on for its sessions with [`SOCKET_ENV`], naming the
//! account billed with [`ACCOUNT_ENV`], the VM's size with [`SIZE_ENV`],
//! and the plans file the service admits by with [`PLANS_ENV`], which a
//! waiting command follows.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use legacy_agent_protocol::client::{self, LaunchIo, LaunchSignals};
use legacy_agent_protocol::link::{AgentLink, AgentStream};
use legacy_agent_protocol::{
    AGENT_PORT, Display, Launch, ProtocolError, Signal, Stdio as Relay, TerminalSize,
};
use legacy_vm::hosted::control::{self, Reply, Request, ShareRequest, VmRequest};
use legacy_vm::hosted::size::VmSize;
use tilo_actions::remote::host_account;
use ush_shell::{Input, Output, Program, Std, Stdio};
use ush_tilo::Programs;

pub use tilo_host_account::{ACCOUNT_ENV, PLANS_ENV, SOCKET_ENV};
/// The size of the session's VM (`small`, `default`, `large`, `xlarge`).
pub const SIZE_ENV: &str = "TILO_HOSTED_VM_SIZE";

#[cfg(any(test, feature = "test-host"))]
pub mod test_host;

/// How long a wake may take, a first boot included.
const WAKE_PATIENCE: Duration = Duration::from_secs(300);

/// How often a command waiting for room asks its host again.
const BUSY_RETRY: Duration = Duration::from_secs(2);

/// Where a hosted session's programs run, and who pays.
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
    /// The host's settings for its sessions, when it serves workspaces.
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

/// A hosted workspace environment's VM, as the place its programs run.
pub struct HostedPrograms {
    target: HostedTarget,
    /// The session's Terminal runs key ([`crate::runs::KEY_ENV`]): the
    /// held marks carry it, so no program can forge a prompt.
    runs: Option<String>,
    workspace: String,
    environment: String,
    /// The directories the environment grants now: writable ones read-write.
    shares: Box<Shares>,
    /// The session's terminal, when it has one: programs on it run on a
    /// terminal in the VM.
    terminal: Option<OwnedFd>,
    /// Where the last refusal is told, for a caller that answers it typed.
    refusals: Option<std::sync::Arc<std::sync::Mutex<Option<host_account::Refusal>>>>,
}

impl HostedPrograms {
    #[must_use]
    pub fn new(
        target: HostedTarget,
        workspace: String,
        environment: String,
        shares: Box<Shares>,
    ) -> Self {
        Self {
            target,
            runs: None,
            workspace,
            environment,
            shares,
            terminal: None,
            refusals: None,
        }
    }

    /// Tell `slot` each refusal that ends a program, so the caller can
    /// answer it typed (a run's answer to the agent that asked for it).
    #[must_use]
    pub fn reporting_refusals(
        mut self,
        slot: std::sync::Arc<std::sync::Mutex<Option<host_account::Refusal>>>,
    ) -> Self {
        self.refusals = Some(slot);
        self
    }

    /// Programs whose input and output are the session's own get a
    /// terminal in the VM sized and fed from `terminal`, when it is one.
    #[must_use]
    pub fn terminal(mut self, terminal: OwnedFd) -> Self {
        self.terminal = is_terminal(&terminal).then_some(terminal);
        self
    }

    /// Mark held commands for Terminal with the session's runs key.
    #[must_use]
    pub fn with_runs_key(mut self, key: Option<String>) -> Self {
        self.runs = key;
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

/// Wait `wait`, looking at `stop` as a running program's flag is looked
/// at: whether the wait ran out rather than being stopped.
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
    /// The VM's link once the host lets the program in; or the program's
    /// end: refused, or held and then cancelled.
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
                // A person's command that cannot wait, or an agent's: the
                // refusal now. Terminal shows the person the prompt for an
                // agent's run too, from the same mark.
                if refusal.awaits_the_account() {
                    self.mark(io, &|key| held_mark(key, agent, &refusal));
                }
                if let Some(slot) = &self.refusals {
                    *slot
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
                        // Terminal shows the prompt with the provider's page; any other
                        // terminal gets the whole refusal and where to add the card.
                        let words = if self.mark(io, &|key| held_mark(key, false, &refusal)) {
                            "needs a Linux machine.".to_owned()
                        } else {
                            match refusal
                                .step(host_account::NextStepKind::AddPaymentMethod)
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

    /// Write the mark `make` makes of the runs key to the session's
    /// terminal, when the session has a key and the command writes to the
    /// terminal itself.
    /// Write the mark `make` makes with the session's key to the terminal the session's runs are
    /// shown in, when it is one of the run's streams. Whether it was written: Terminal shows the
    /// prompt of a held mark, so the shell's own words can stay short.
    fn mark(&self, io: &Stdio, make: &dyn Fn(&str) -> String) -> bool {
        let Some(key) = &self.runs else {
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
pub fn held_mark(key: &str, agent: bool, refusal: &host_account::Refusal) -> String {
    use base64::Engine as _;
    let json = serde_json::to_vec(refusal).unwrap_or_default();
    format!(
        "\x1b]133;W;k={key};a={};r={}\x07",
        u8::from(agent),
        base64::engine::general_purpose::STANDARD.encode(json)
    )
}

/// `U`: the hold is over.
#[must_use]
pub fn released_mark(key: &str) -> String {
    format!("\x1b]133;U;k={key}\x07")
}

/// A held command following the plans file for the account to change.
struct Hold {
    changes: std::sync::mpsc::Receiver<()>,
    _watch: tilo_files::native::ChangeWatch,
}

impl Hold {
    fn start(plans: &Path) -> std::io::Result<Self> {
        let (tell, changes) = std::sync::mpsc::channel();
        let watch = tilo_host_account::watch_plans(plans, move || {
            let _ = tell.send(());
        })?;
        Ok(Self {
            changes,
            _watch: watch,
        })
    }

    /// Wait for the plans file to change: whether it did, or `stop` ended
    /// the wait first.
    fn changed(&self, stop: Option<&dyn Fn() -> bool>) -> bool {
        use std::sync::mpsc::RecvTimeoutError;
        let Some(stop) = stop else {
            return self.changes.recv().is_ok();
        };
        loop {
            // The file's change is the event; `stop` is the job's own flag,
            // looked at as a running program's is.
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
        let terminal = self.terminal.as_ref().filter(|_| {
            io.stdin.host_stream() == Some(Std::In) && io.stdout.host_stream() == Some(Std::Out)
        });
        let launch = Launch {
            argv: argv.to_vec(),
            cwd: Some(cwd.display().to_string()),
            env: env.to_vec(),
            stdio: terminal.map_or(Relay::Relay, |fd| Relay::Terminal(terminal_size(fd))),
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
        let result = std::thread::scope(|scope| {
            let running = scope.spawn(|| {
                let result = client::launch(
                    stream,
                    &launch,
                    LaunchIo {
                        stdin,
                        stdout: &mut stdout,
                        stderr: &mut stderr,
                        started: &mut |_| {},
                    },
                    move |signals| {
                        let _ = handing.send(signals);
                    },
                );
                finished.store(true, Ordering::SeqCst);
                result
            });
            if let Ok(signals) = handed.recv() {
                if let Some(fd) = terminal {
                    let relay = signals.clone();
                    let finished = &finished;
                    scope.spawn(move || relay_terminal(fd, &relay, finished));
                }
                // Ended from outside (the job was killed): end it in the VM.
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
fn terminal_size(fd: &OwnedFd) -> TerminalSize {
    // SAFETY: TIOCGWINSZ writes only into the struct, which lives across
    // the call.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
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

/// The session's terminal in raw mode while a program in the VM has it:
/// every key goes to the VM's terminal, which edits lines and makes Ctrl-C
/// a signal there. Restored when dropped.
struct Raw<'a> {
    fd: &'a OwnedFd,
    cooked: Option<libc::termios>,
}

impl<'a> Raw<'a> {
    fn new(fd: &'a OwnedFd) -> Self {
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

impl Drop for Raw<'_> {
    fn drop(&mut self) {
        if let Some(cooked) = self.cooked {
            // SAFETY: as in `new`.
            unsafe { libc::tcsetattr(self.fd.as_raw_fd(), libc::TCSADRAIN, &raw const cooked) };
        }
    }
}

/// How often a waiting relay looks at the terminal's size and whether the
/// program ended.
const RELAY_TICK: Duration = Duration::from_millis(100);

/// Relay the session's keys and size to a program on a terminal in the VM
/// until it ends. Reads only what is ready, so no read is left waiting for
/// a key the shell needs next.
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
        // SAFETY: one pollfd, owned here, for the length of the call.
        let ready = unsafe { libc::poll(&raw mut poll, 1, tick) };
        if ready > 0 && poll.revents & libc::POLLIN != 0 {
            // SAFETY: `buffer` is valid for writes of its length.
            let read =
                unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            let Ok(read) = usize::try_from(read) else {
                continue;
            };
            if read == 0
                || signals
                    .input(buffer.get(..read).unwrap_or_default())
                    .is_err()
            {
                return;
            }
        } else if ready > 0 && poll.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ush_shell::io::Captured;

    #[test]
    fn writable_grants_win_over_readable_ones() {
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

    #[test]
    fn a_plans_refusal_reaches_the_program_word_for_word() {
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
                    &Reply::Refused(host_account::Refusal {
                        reason: host_account::Reason::TooManyRunning {
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

    /// A hybrid-vsock socket whose port connections reach a real agent
    /// running here as the current user.
    fn fake_vm(dir: &Path) -> PathBuf {
        use std::io::BufRead as _;
        let socket = dir.join("v.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let user = legacy_agent::user::GuestUser {
            name: "me".into(),
            uid: rustix::process::getuid().as_raw(),
            gid: rustix::process::getgid().as_raw(),
            home: std::env::temp_dir(),
            shell: "/bin/sh".into(),
        };
        let agent = std::sync::Arc::new(legacy_agent::server::Agent::new(user, "boot".into()));
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

    /// The hosted-VM service, answering every wake with `link`.
    fn fake_service(dir: &Path, vm: PathBuf) -> PathBuf {
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
                            link: legacy_agent_protocol::link::AgentLink::Hybrid {
                                socket: vm.clone(),
                            },
                            woke_ms: None,
                        },
                    )
                    .unwrap();
            }
        });
        socket
    }

    #[test]
    fn a_program_on_the_sessions_terminal_gets_its_keys_and_size_in_the_vm() {
        use std::os::fd::FromRawFd as _;
        let dir = tempfile::tempdir().unwrap();
        let socket = fake_service(dir.path(), fake_vm(dir.path()));
        // The session's terminal: the test types into its master.
        let (mut master, mut slave) = (-1, -1);
        let size = libc::winsize {
            ws_row: 33,
            ws_col: 101,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: out-parameters and a size that live across the call.
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
            Box::new(Vec::new),
        )
        .terminal(slave.try_clone().unwrap());
        assert!(programs.terminal.is_some(), "a pty is a terminal");
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
        // The session's terminal is back as it was: canonical, echoing.
        // SAFETY: tcgetattr fills the struct owned here.
        let mut modes: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), &raw mut modes) },
            0
        );
        assert_ne!(modes.c_lflag & libc::ICANON, 0);
    }

    /// An [`Output`] as a writer, for nesting a buffer under a host stream.
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

    /// A session on `host` for `account`, its terminal captured.
    fn session(
        host: &test_host::TestHost,
        account: &str,
    ) -> (HostedPrograms, Stdio, Captured, Captured) {
        let programs = HostedPrograms::new(
            host.target(account),
            "ws1".into(),
            "default".into(),
            Box::new(Vec::new),
        )
        .with_runs_key(Some(KEY.into()));
        let (_, out) = Output::buffer();
        let (_, err) = Output::buffer();
        let io = Stdio {
            stdout: Output::host(Std::Out, out.clone()),
            stderr: Output::host(Std::Err, err.clone()),
            ..Stdio::null()
        };
        (programs, io, out, err)
    }

    /// The refusal a held mark in `text` carries, and whether it is an
    /// agent's.
    fn held(text: &str) -> Option<(bool, host_account::Refusal)> {
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

    #[test]
    fn a_persons_command_without_a_card_waits_for_one_then_runs() {
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
        // phase: the command is held, and the terminal shows the prompt's mark
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
        assert_eq!(refusal.reason, host_account::Reason::NoPaymentMethod);
        assert_eq!(
            refusal
                .step(host_account::NextStepKind::AddPaymentMethod)
                .and_then(|step| step.url.as_deref()),
            Some("https://tilo.sh/billing/card?account=acct-1")
        );
        assert!(!running.is_finished(), "a person's command waits");
        // phase: the provider adds the card, and the command runs untyped again
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

    #[test]
    fn cancelling_a_held_command_ends_it_having_run_nothing() {
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

    #[test]
    fn an_agents_command_is_refused_at_once_with_the_typed_refusal() {
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
        assert_eq!(refusal.reason, host_account::Reason::NoPaymentMethod);
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
            "ush: cargo: Starting another machine for it, about 3 minutes. Waiting; Ctrl+C \
             cancels.\n",
            "said once, however long it waits"
        );
    }

    #[test]
    fn a_command_waiting_for_room_ends_when_cancelled_having_run_nothing() {
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

    #[test]
    fn a_host_that_sells_nothing_runs_the_command_with_no_prompt() {
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
