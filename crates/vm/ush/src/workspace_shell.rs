//! A hosted workspace's shell, run on the workspace's host: one shell per Terminal session,
//! running a command line the way a device's session runs what is typed at its prompt.
//!
//! ush's own commands (`ls`, `cat`, `grep`, `echo`) run in-process on the workspace's
//! directory and cost nothing; the file-changing ones are the environment's file actions,
//! checked against its one grant and keeping what they replace; any other program (`cargo`,
//! `npm`, `python`) runs in the workspace's jailed VM through [`HostedPrograms`], metered
//! per second, and a host that runs no VMs says which tier the program needs.
//! A server that is its operator's own machine may start them on it instead
//! ([`crate::machine_programs`]).
//!
//! A shell keeps its working directory and variables between commands. Output comes back
//! in order, its last [`MAX_OUTPUT`] bytes when it printed more (where a build's errors and a
//! test summary are), with how the command ended and, when the host
//! refused it a VM, the host's typed refusal. A caller passing [`Seen`] is shown output as it
//! grows, and [`Stopping`] ends a command as Ctrl+C does, in the shell and in the VM.
//!
//! The same session's shell also runs interactively on a pseudo-terminal
//! ([`WorkspaceShells::interactive`]): it prompts and edits lines on the terminal, and its
//! programs run on a terminal in the VM, so editors, prompts and `claude` work as on a device.
//! [`crate::environment_terminals`] carries that terminal to a person's device.
//!
//! A run may carry secrets ([`RunWith::secrets`]): items of the workspace's vault its VM may
//! use, which the host resolved for this run. Every program the
//! command starts in the VM gets them in its environment, set over a shell variable of the
//! same name. The shell itself never holds them: `echo $NAME` and `env` among its own commands
//! do not see them, they are not kept between runs, and nothing the run answers carries them
//! unless a program prints its own environment.
//!
//! A run is asked for by someone ([`RunWith::person`]): a person or an agent, named as the
//! workspace names them. Every program the command starts in the VM records them as git's
//! author and committer, and the shell keeps no identity for its next command line.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tilo_action_services::environment_commands::{CommandOutput, Person, VmSecret};
use tilo_action_services::server_account::Refusal;
use tilo_permissions::caps::CapOverride;
use ush_shell::commands::Render;
use ush_shell::{Output, Run, Shell, Stdio};
use ush_tilo::{Acted, Environment, Keeping, NativeFiles, NoPrograms, Place, Programs, TiloHost};

use crate::content::EnvironmentContent;
use crate::file_actions::{FileAction, Recoverable};
use crate::hosted_vm::{HeldTerminal, HostedPrograms, HostedTarget, shares_of};
use crate::machine_programs::MachinePrograms;
use crate::policy::Policy;

pub const MAX_OUTPUT: usize = 1 << 20;

const ENVIRONMENT: &str = "default";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellRun {
    pub command: String,
    pub cwd: String,
    /// Its exit status: 126 when the host refused what it needed.
    pub exit: i32,
    pub output: Vec<u8>,
    pub truncated: bool,
    pub cancelled: bool,
    pub started_ms: i64,
    pub ended_ms: i64,
    /// Why the host would not run a program it needed, typed.
    pub refusal: Option<Refusal>,
}

pub struct WorkspaceShells {
    /// Each workspace's content and kept files, and its directory unless
    /// [`Self::with_files`] says where it is, below here.
    state: PathBuf,
    /// Where each workspace's files are, when not below `state`.
    files_of: Option<Box<FilesOf>>,
    vms: Option<HostedTarget>,
    /// Without a hosted-VM service, whether programs start on this machine itself
    /// ([`MachinePrograms`]).
    on_machine: bool,
    /// The hosted-VM service the workspaces' Browser starts on, when it is
    /// not the one the environment names.
    browsers: Option<PathBuf>,
    /// Who serves the agent socket of the workspaces' machines to their sessions' programs.
    agents: Option<Arc<dyn crate::hosted_vm::MachineAgents>>,
    sessions: Arc<Mutex<HashMap<(String, String), Shared>>>,
}

type FilesOf = dyn Fn(&str) -> PathBuf + Send + Sync;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payer {
    pub account: String,
    pub size: tilo_server_protocol::hosted_vm::VmSize,
}

/// What a caller is shown of a command while it runs: everything it printed
/// so far, each time that grew.
pub type Seen<'a> = &'a (dyn Fn(&CommandOutput) + Sync);

/// Whether the caller asked the command to stop: a person's Ctrl+C, or an
/// agent's cancel. Looked at between the command's own steps, so a loop, a
/// wait or a running program ends within a fraction of a second.
pub type Stopping<'a> = &'a (dyn Fn() -> bool + Sync);

/// What a run is asked with besides its command line.
#[derive(Clone, Copy, Default)]
pub struct RunWith<'a> {
    /// Who pays for the VM rather than this host's own account (the first run of a
    /// workspace's session decides it).
    pub payer: Option<&'a Payer>,
    /// Who the command runs as, a person or an agent: the name and address the VM's
    /// programs record as the author of what they make. `None` where the host cannot name
    /// them.
    pub person: Option<&'a Person>,
    /// What the programs the command starts in the VM get in their environment, this run
    /// only.
    pub secrets: &'a [VmSecret],
    /// Whether the caller asked the command to stop.
    pub stopping: Option<Stopping<'a>>,
    /// What the caller is shown of the command while it runs.
    pub seen: Option<Seen<'a>>,
}

/// The secrets the run in progress hands the programs it starts in the VM.
pub(crate) type Secrets = Arc<Mutex<Vec<VmSecret>>>;

/// Who the run in progress is for: the person or agent its programs record as git's author
/// and committer, and nobody between runs.
pub(crate) type Identity = Arc<Mutex<Option<Person>>>;

/// `env` with the run's secrets over it: a secret's variable replaces one of the same name.
pub(crate) fn with_secrets(
    env: &[(String, String)],
    secrets: Option<&Secrets>,
) -> Vec<(String, String)> {
    let Some(kept) = secrets else {
        return env.to_vec();
    };
    let secrets = kept.lock().unwrap_or_else(PoisonError::into_inner);
    let mut started: Vec<(String, String)> = env
        .iter()
        .filter(|(name, _)| !secrets.iter().any(|secret| secret.variable == *name))
        .cloned()
        .collect();
    started.extend(
        secrets
            .iter()
            .map(|secret| (secret.variable.clone(), secret.value.clone())),
    );
    started
}

/// git's author and committer for whoever the run in progress is for, by the name and address
/// the workspace gives them: what a program sets over any identity its machine configures.
pub(crate) fn git_identity(identity: Option<&Identity>) -> Vec<(&'static str, String)> {
    let person =
        identity.and_then(|kept| kept.lock().unwrap_or_else(PoisonError::into_inner).clone());
    let Some(person) = person else {
        return Vec::new();
    };
    let (name, address) = (person.name.trim(), person.address.trim());
    if name.is_empty() || address.is_empty() {
        return Vec::new();
    }
    let mut identity = Vec::with_capacity(4);
    for key in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
        identity.push((key, name.to_owned()));
    }
    for key in ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"] {
        identity.push((key, address.to_owned()));
    }
    identity
}

const SEEN_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

const STOP_EVERY: std::time::Duration = std::time::Duration::from_millis(25);

type Shared = Arc<Mutex<Session>>;

struct Session {
    shell: Shell,
    output: Output,
    captured: ush_shell::io::Captured,
    notices: ush_shell::io::Captured,
    refused: Arc<Mutex<Option<Refusal>>>,
    stopped: Arc<AtomicBool>,
    /// The terminal the shell is on while it runs interactively; its programs and its line
    /// editor find it here.
    terminal: HeldTerminal,
    secrets: Secrets,
    identity: Identity,
}

/// A session's shell running interactively on a terminal ([`WorkspaceShells::interactive`]).
pub struct Interactive {
    stopped: Arc<AtomicBool>,
    done: std::thread::JoinHandle<i32>,
}

impl Interactive {
    /// Ctrl+C for what the shell runs itself (a loop, `sleep`, a wait on the hosts): a
    /// terminal in cooked mode turns the key into a signal, and this shell's terminal is no
    /// process's controlling one, so its holder tells the shell here.
    pub fn interrupt(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.done.is_finished()
    }

    /// Wait for the shell to end, answering its exit status.
    #[must_use]
    pub fn wait(self) -> i32 {
        self.done.join().unwrap_or(1)
    }
}

impl WorkspaceShells {
    #[must_use]
    pub fn new(state: impl Into<PathBuf>, vms: Option<HostedTarget>) -> Self {
        Self {
            state: state.into(),
            files_of: None,
            vms,
            on_machine: false,
            browsers: None,
            agents: None,
            sessions: Arc::default(),
        }
    }

    /// The same shells, whose sessions' programs in a workspace's machine find Tilo's MCP
    /// server at the machine's agent socket, each connection served by `agents`
    /// ([`crate::hosted_vm::MachineAgents`]).
    #[must_use]
    pub fn serving_agents(mut self, agents: Arc<dyn crate::hosted_vm::MachineAgents>) -> Self {
        self.agents = Some(agents);
        self
    }

    #[must_use]
    pub fn with_files(
        mut self,
        files_of: impl Fn(&str) -> PathBuf + Send + Sync + 'static,
    ) -> Self {
        self.files_of = Some(Box::new(files_of));
        self
    }

    /// The shells the server's environment asks for: Linux programs in the hosted VMs it names,
    /// else on this machine when [`crate::machine_programs::MACHINE_ENV`] says so, else none.
    #[must_use]
    pub fn from_env(state: impl Into<PathBuf>) -> Self {
        Self::new(state, HostedTarget::from_env())
            .on_machine(crate::machine_programs::on_machine_from_env())
    }

    /// The same shells, starting the programs of a workspace with no hosted VM on this machine
    /// when `on_machine`.
    #[must_use]
    pub const fn on_machine(mut self, on_machine: bool) -> Self {
        self.on_machine = on_machine;
        self
    }

    #[must_use]
    pub fn runs_programs(&self) -> bool {
        self.vms.is_some() || self.on_machine
    }

    /// Where a run `payer` pays for runs its Linux programs when the environment names the hosted-VM
    /// service but no account: a member taking a server's work, whose every run names who pays,
    /// runs them on the service its workspaces' Browser starts on.
    fn paid_on_browsers(&self, payer: Option<&Payer>) -> Option<HostedTarget> {
        let payer = payer?;
        Some(HostedTarget {
            socket: self.browsers.clone()?,
            account: payer.account.clone(),
            size: payer.size,
            plans: std::env::var_os(crate::hosted_vm::PLANS_ENV).map(PathBuf::from),
        })
    }

    /// The hosted-VM service these shells run Linux programs on, and for whom.
    #[must_use]
    pub const fn hosted(&self) -> Option<&HostedTarget> {
        self.vms.as_ref()
    }

    /// The same shells, whose workspaces' Browser starts on the hosted-VM
    /// service at `socket` (the one the environment names, unless a host
    /// names it another way).
    #[must_use]
    pub fn with_browsers_on(mut self, socket: Option<PathBuf>) -> Self {
        self.browsers = socket;
        self
    }

    #[must_use]
    pub fn hosted_socket(&self) -> Option<PathBuf> {
        self.browsers
            .clone()
            .or_else(|| self.vms.as_ref().map(|target| target.socket.clone()))
    }

    #[must_use]
    pub fn hosted_size(&self) -> tilo_server_protocol::hosted_vm::VmSize {
        self.vms
            .as_ref()
            .map(|target| target.size)
            .unwrap_or_default()
    }

    #[must_use]
    pub fn files(&self, workspace: &str) -> PathBuf {
        self.files_of.as_ref().map_or_else(
            || self.state.join(safe(workspace)).join("files"),
            |files_of| files_of(workspace),
        )
    }

    /// Run `command` in `workspace`'s Terminal session `session`, as an agent
    /// asked (its output rendered for an agent, and a refused VM never
    /// waited for) when `agent`.
    ///
    /// # Errors
    ///
    /// The workspace's directory could not be made.
    pub fn run(
        &self,
        workspace: &str,
        session: &str,
        command: &str,
        agent: bool,
    ) -> io::Result<ShellRun> {
        self.run_for(workspace, session, command, agent, RunWith::default())
    }

    /// [`Self::run`], with what `with` says: who pays for the VM, who the command runs as
    /// (whose git identity its programs record), the secrets its programs get, when to stop,
    /// and what the caller is shown of the command while it runs.
    ///
    /// # Errors
    ///
    /// The workspace's directory could not be made.
    pub fn run_for(
        &self,
        workspace: &str,
        session: &str,
        command: &str,
        agent: bool,
        with: RunWith<'_>,
    ) -> io::Result<ShellRun> {
        let shell = self.session(workspace, session, with.payer)?;
        let mut session = shell.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(session.run(command, agent, with))
    }

    /// Run `workspace`'s Terminal session `session` interactively on the pseudo-terminal whose
    /// slave is `terminal`, until the shell exits or the terminal hangs up (its master closed).
    /// It is the shell [`Self::run_for`] runs commands in, kept by the same session, paid for
    /// by `payer` the same way; `marked` marks its runs for Terminal and hears each
    /// ([`ush_shell::Session`]), and every program it starts in the VM is given `secrets`.
    /// The session ends with the shell: a later run starts a new one.
    ///
    /// `person` is who the shell runs as for as long as it is on this terminal: whoever asks
    /// for the stream is one actor, and a commit typed at the prompt names them.
    ///
    /// # Errors
    ///
    /// The workspace's directory could not be made, or the terminal could not be shared.
    pub fn interactive(
        &self,
        workspace: &str,
        session: &str,
        payer: Option<&Payer>,
        person: Option<&Person>,
        terminal: OwnedFd,
        marked: ush_shell::Session,
        secrets: Vec<VmSecret>,
    ) -> io::Result<Interactive> {
        let person = person.cloned();
        let key = (workspace.to_owned(), session.to_owned());
        let shared = self.session(workspace, session, payer)?;
        let stopped = Arc::clone(
            &shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .stopped,
        );
        let io = Stdio {
            stdin: ush_shell::Input::host(std::fs::File::from(terminal.try_clone()?)),
            stdout: Output::host(
                ush_shell::Std::Out,
                std::fs::File::from(terminal.try_clone()?),
            ),
            stderr: Output::host(
                ush_shell::Std::Err,
                std::fs::File::from(terminal.try_clone()?),
            ),
        };
        let sessions = Arc::clone(&self.sessions);
        let done = std::thread::Builder::new()
            .name("workspace-shell".into())
            .spawn(move || {
                let status = {
                    let mut session = shared.lock().unwrap_or_else(PoisonError::into_inner);
                    session.interactive(terminal, io, &marked, person.as_ref(), secrets)
                };
                let mut sessions = sessions.lock().unwrap_or_else(PoisonError::into_inner);
                if sessions
                    .get(&key)
                    .is_some_and(|kept| Arc::ptr_eq(kept, &shared))
                {
                    sessions.remove(&key);
                }
                status
            })?;
        Ok(Interactive { stopped, done })
    }

    fn session(&self, workspace: &str, session: &str, payer: Option<&Payer>) -> io::Result<Shared> {
        let key = (workspace.to_owned(), session.to_owned());
        let open = self
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .cloned();
        if let Some(open) = open {
            return Ok(open);
        }
        let opened = Arc::new(Mutex::new(self.open(workspace, session, payer)?));
        Ok(Arc::clone(
            self.sessions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(key)
                .or_insert(opened),
        ))
    }

    /// The hosted-VM service a workspace's Linux programs run on, paid for
    /// by `payer` rather than this host's own account when given.
    fn target_for(&self, payer: Option<&Payer>) -> Option<HostedTarget> {
        self.vms
            .clone()
            .or_else(|| self.paid_on_browsers(payer))
            .map(|mut target| {
                if let Some(payer) = payer {
                    target.account.clone_from(&payer.account);
                    target.size = payer.size;
                }
                target
            })
    }

    fn open(&self, workspace: &str, session: &str, payer: Option<&Payer>) -> io::Result<Session> {
        let root = self.state.join(safe(workspace));
        let files = self.files(workspace);
        std::fs::create_dir_all(&files)?;
        let target = self.target_for(payer);
        let programs_here = target.is_none() && self.on_machine;
        // A shell whose programs start on this machine sees it as they do: its own commands (cd,
        // ls, cat, rm) reach every file the server's user may, not only the workspace's files,
        // or a command could name a checkout its `git` reaches while `cd` refuses it.
        let grants = vec![if programs_here {
            tilo_environment::machine_grant()
        } else {
            tilo_environment::dir_grant(&files, true).map_err(io::Error::other)?
        }];
        let environment = Arc::new(Files {
            grants,
            kept: Recoverable::at(root.join("recoverable")),
            content: EnvironmentContent::at(root.join("content")),
        });
        let refused = Arc::new(Mutex::new(None));
        let terminal = HeldTerminal::default();
        let secrets = Secrets::default();
        let identity = Identity::default();
        let programs: Arc<dyn Programs> = match target {
            Some(target) => {
                let shared = files.clone();
                let programs = HostedPrograms::new(
                    target,
                    workspace.to_owned(),
                    ENVIRONMENT.to_owned(),
                    shared.clone(),
                    Box::new(move || shares_of(vec![shared.clone()], Vec::new())),
                )
                .reporting_refusals(Arc::clone(&refused))
                .on_terminal_of(Arc::clone(&terminal))
                .carrying_secrets(Arc::clone(&secrets))
                .carrying_identity(Arc::clone(&identity));
                Arc::new(match &self.agents {
                    Some(agents) => programs.serving_agents(Arc::clone(agents), session),
                    None => programs,
                })
            }
            None if programs_here => Arc::new(
                MachinePrograms::default()
                    .carrying_secrets(Arc::clone(&secrets))
                    .carrying_identity(Arc::clone(&identity)),
            ),
            None => Arc::new(NoPrograms),
        };
        let (notices_out, notices) = Output::buffer();
        let stopped = Arc::new(AtomicBool::new(false));
        let host = TiloHost::new(
            environment,
            Arc::new(NativeFiles),
            programs,
            Arc::new(HostPlace {
                stopped: Arc::clone(&stopped),
                terminal: Arc::clone(&terminal),
                cooked: Mutex::new(None),
            }),
            notices_out,
        );
        let (output, captured) = Output::buffer();
        let home = files.to_string_lossy().into_owned();
        let shell = Shell::new(
            Arc::new(host),
            shell_env(home, programs_here),
            files,
            Stdio::null(),
        );
        Ok(Session {
            shell,
            output,
            captured,
            notices,
            refused,
            stopped,
            terminal,
            secrets,
            identity,
        })
    }
}

impl Session {
    fn interactive(
        &mut self,
        terminal: OwnedFd,
        io: Stdio,
        session: &ush_shell::Session,
        person: Option<&Person>,
        secrets: Vec<VmSecret>,
    ) -> i32 {
        let held = || self.terminal.lock().unwrap_or_else(PoisonError::into_inner);
        *held() = Some(terminal);
        // The session's secrets, read when it started, for every program it starts.
        *self.secrets.lock().unwrap_or_else(PoisonError::into_inner) = secrets;
        self.stopped.store(false, Ordering::SeqCst);
        self.shell.set_stdio(io);
        self.shell.set_render(Render::Text);
        // The terminal is a real one now: programs may draw on it.
        self.shell.set_var("TERM", TERMINAL);
        // Whoever is at the terminal is the one the whole session runs as: every command line
        // typed at the prompt, and every program it starts, records them as an author, and
        // nobody once the terminal is gone.
        *self.identity.lock().unwrap_or_else(PoisonError::into_inner) = person.cloned();
        let status = self.shell.interactive(session);
        *self.identity.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.shell.set_stdio(Stdio::null());
        *held() = None;
        self.secrets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        status
    }

    fn run(&mut self, command: &str, agent: bool, with: RunWith<'_>) -> ShellRun {
        let RunWith {
            person,
            secrets,
            stopping,
            seen,
            ..
        } = with;
        // This run's secrets, and none of an earlier run's.
        *self.secrets.lock().unwrap_or_else(PoisonError::into_inner) = secrets.to_vec();
        let cwd = self.shell.cwd().to_string_lossy().into_owned();
        *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.stopped.store(false, Ordering::SeqCst);
        self.shell.set_stdio(Stdio {
            stdin: ush_shell::Input::null(),
            stdout: self.output.clone(),
            stderr: self.output.clone(),
        });
        self.shell
            .set_render(if agent { Render::Agent } else { Render::Text });
        let started_ms = now_ms();
        self.shell.set_run(Some(Run {
            id: format!("{started_ms:x}"),
            label: command.to_owned(),
        }));
        // Who this command line runs as is whoever asked for it, a person or
        // an agent, and only this one: the shell goes on as it was, and the
        // next run records its own actor.
        *self.identity.lock().unwrap_or_else(PoisonError::into_inner) = person.cloned();
        let exit = if stopping.is_none() && seen.is_none() {
            self.shell.run_script(command)
        } else {
            self.run_watched(command, stopping, seen)
        };
        *self.identity.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.shell.set_run(None);
        self.secrets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        let ended_ms = now_ms();
        let cancelled = self.stopped.swap(false, Ordering::SeqCst);
        let mut output = self.captured.take();
        output.extend(self.notices.take());
        let truncated = output.len() > MAX_OUTPUT;
        output.drain(..output.len().saturating_sub(MAX_OUTPUT));
        ShellRun {
            command: command.to_owned(),
            cwd,
            exit,
            output,
            truncated,
            cancelled,
            started_ms,
            ended_ms,
            refusal: self
                .refused
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        }
    }

    fn run_watched(
        &mut self,
        command: &str,
        stopping: Option<Stopping<'_>>,
        seen: Option<Seen<'_>>,
    ) -> i32 {
        let stopped = Arc::clone(&self.stopped);
        let printed = self.captured.clone();
        let done = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut shown = 0;
                let mut shown_at = std::time::Instant::now();
                while !done.load(Ordering::Acquire) {
                    std::thread::sleep(STOP_EVERY);
                    if stopping.is_some_and(|stopping| stopping()) {
                        stopped.store(true, Ordering::SeqCst);
                    }
                    if let Some(seen) = seen
                        && shown_at.elapsed() >= SEEN_EVERY
                    {
                        let now = printed.peek();
                        if now.len() != shown {
                            shown = now.len();
                            shown_at = std::time::Instant::now();
                            seen(&CommandOutput::of(&now));
                        }
                    }
                }
            });
            let exit = self.shell.run_script(command);
            done.store(true, Ordering::Release);
            exit
        })
    }
}

fn safe(workspace: &str) -> String {
    workspace
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// What a new session's shell starts with. Programs in a VM start from a clean environment,
/// `HOME` the workspace's files. Programs on this machine start as its user's own would: from the
/// server's environment (its `PATH`, `HOME` and toolchain), so `cargo` and `git` work as in that
/// user's own shell; only the terminal is a plain one.
fn shell_env(files: String, programs_here: bool) -> Vec<(String, String)> {
    let term = ("TERM".to_owned(), "dumb".to_owned());
    if programs_here {
        let mut env: Vec<(String, String)> =
            std::env::vars().filter(|(key, _)| key != "TERM").collect();
        env.push(term);
        return env;
    }
    vec![
        ("HOME".to_owned(), files),
        ("PATH".to_owned(), "/usr/local/bin:/usr/bin:/bin".to_owned()),
        term,
    ]
}

fn now_ms() -> i64 {
    i64::try_from(tilo_clock::unix_now_ms()).unwrap_or(i64::MAX)
}

struct Files {
    grants: Vec<CapOverride>,
    kept: Recoverable,
    content: EnvironmentContent,
}

impl Environment for Files {
    fn grants(&self) -> Vec<CapOverride> {
        self.grants.clone()
    }

    fn act(&self, action: FileAction, _run: Option<&Run>) -> Result<Acted, String> {
        let policy = Policy::new(self.grants.clone());
        let now = Some(tilo_clock::unix_now());
        crate::file_actions::perform(&action, &policy, &self.kept, &self.content.store(), now)
            .map(|done| Acted {
                entry: None,
                operations: done.len(),
            })
            .map_err(|(error, _)| error.to_string())
    }

    fn keep(&self) -> io::Result<Box<dyn Keeping>> {
        self.content.keep()
    }
}

struct HostPlace {
    stopped: Arc<AtomicBool>,
    terminal: HeldTerminal,
    cooked: Mutex<Option<libc::termios>>,
}

/// What a session's interactive terminal says it is: Terminal's emulator.
const TERMINAL: &str = "xterm-256color";

impl Place for HostPlace {
    fn interrupted(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    fn clear_interrupt(&self) {
        self.stopped.store(false, Ordering::SeqCst);
    }

    fn now(&self) -> i64 {
        i64::try_from(tilo_clock::unix_now()).unwrap_or(i64::MAX)
    }

    fn now_ms(&self) -> i64 {
        now_ms()
    }

    fn sleep(&self, millis: u64) {
        let until = std::time::Instant::now()
            .checked_add(std::time::Duration::from_millis(millis))
            .unwrap_or_else(std::time::Instant::now);
        while !self.interrupted() {
            let left = until.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return;
            }
            std::thread::sleep(left.min(STOP_EVERY));
        }
    }

    fn hostname(&self) -> String {
        "tilo".to_owned()
    }

    fn edit_mode(&self, on: bool) -> bool {
        let terminal = self.terminal.lock().unwrap_or_else(PoisonError::into_inner);
        terminal
            .as_ref()
            .is_some_and(|fd| crate::tilo_side::edit_mode(fd.as_raw_fd(), &self.cooked, on))
    }

    fn columns(&self) -> usize {
        let terminal = self.terminal.lock().unwrap_or_else(PoisonError::into_inner);
        terminal.as_ref().map_or(80, |fd| {
            usize::from(crate::hosted_vm::terminal_size(fd).cols)
        })
    }
}

#[cfg(test)]
#[path = "workspace_shell_tests.rs"]
mod tests;
