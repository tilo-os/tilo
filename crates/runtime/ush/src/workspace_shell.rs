//! A hosted workspace's shell, run on the workspace's host: one shell per Terminal session,
//! running a command line the way a device's session runs what is typed at its prompt.
//!
//! ush's own commands (`ls`, `cat`, `grep`, `echo`) run in-process on the workspace's
//! directory and cost nothing; the file-changing ones are the environment's file actions,
//! checked against its one grant and keeping what they replace; any other program (`cargo`,
//! `npm`, `python`) runs in the workspace's jailed VM through [`HostedPrograms`], metered
//! per second, and a host that runs no VMs says which tier the program needs.
//!
//! A shell keeps its working directory and variables between commands. Output comes back
//! whole and in order up to [`MAX_OUTPUT`], with how the command ended and, when the host
//! refused it a VM, the host's typed refusal. A caller passing [`Seen`] is shown output as it
//! grows, and [`Stopping`] ends a command as Ctrl+C does, in the shell and in the VM.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use collab_types::caps::CapOverride;
use tilo_actions::remote::environment_runs::RunOutput;
use tilo_actions::remote::host_account::Refusal;
use ush_shell::commands::Render;
use ush_shell::{Output, Run, Shell, Stdio};
use ush_tilo::{Acted, Environment, Keeping, NativeFiles, NoPrograms, Place, Programs, TiloHost};

use crate::content::EnvironmentContent;
use crate::file_actions::{FileAction, Recoverable};
use crate::hosted_vm::{HostedPrograms, HostedTarget, shares_of};
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
    /// The hosted-VM service the workspaces' Browser starts on, when it is
    /// not the one the environment names.
    browsers: Option<PathBuf>,
    sessions: Mutex<HashMap<(String, String), Shared>>,
}

type FilesOf = dyn Fn(&str) -> PathBuf + Send + Sync;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payer {
    pub account: String,
    pub size: legacy_vm::hosted::size::VmSize,
}

/// What a caller is shown of a command while it runs: everything it printed
/// so far, each time that grew.
pub type Seen<'a> = &'a (dyn Fn(&RunOutput) + Sync);

/// Whether the caller asked the command to stop: a person's Ctrl+C, or an
/// agent's cancel. Looked at between the command's own steps, so a loop, a
/// wait or a running program ends within a fraction of a second.
pub type Stopping<'a> = &'a (dyn Fn() -> bool + Sync);

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
}

impl WorkspaceShells {
    #[must_use]
    pub fn new(state: impl Into<PathBuf>, vms: Option<HostedTarget>) -> Self {
        Self {
            state: state.into(),
            files_of: None,
            vms,
            browsers: None,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn with_files(
        mut self,
        files_of: impl Fn(&str) -> PathBuf + Send + Sync + 'static,
    ) -> Self {
        self.files_of = Some(Box::new(files_of));
        self
    }

    #[must_use]
    pub fn from_env(state: impl Into<PathBuf>) -> Self {
        Self::new(state, HostedTarget::from_env())
    }

    #[must_use]
    pub fn runs_programs(&self) -> bool {
        self.vms.is_some()
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
    pub fn hosted_size(&self) -> legacy_vm::hosted::size::VmSize {
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
        self.run_for(workspace, session, command, agent, None, None, None)
    }

    /// [`Self::run`], its VM paid for by `payer` rather than this host's
    /// own account when given (the first run of a workspace's session
    /// decides it), ended when `stopping` says so, and `seen` shown what the
    /// command printed so far while it runs.
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
        payer: Option<&Payer>,
        stopping: Option<Stopping<'_>>,
        seen: Option<Seen<'_>>,
    ) -> io::Result<ShellRun> {
        let shell = self.session(workspace, session, payer)?;
        let mut session = shell.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(session.run(command, agent, stopping, seen))
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
        let opened = Arc::new(Mutex::new(self.open(workspace, payer)?));
        Ok(Arc::clone(
            self.sessions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(key)
                .or_insert(opened),
        ))
    }

    fn open(&self, workspace: &str, payer: Option<&Payer>) -> io::Result<Session> {
        let root = self.state.join(safe(workspace));
        let files = self.files(workspace);
        std::fs::create_dir_all(&files)?;
        let grants = vec![tilo_environment::dir_grant(&files, true).map_err(io::Error::other)?];
        let environment = Arc::new(Files {
            grants,
            kept: Recoverable::at(root.join("recoverable")),
            content: EnvironmentContent::at(root.join("content")),
        });
        let refused = Arc::new(Mutex::new(None));
        let target = self.vms.clone().map(|mut target| {
            if let Some(payer) = payer {
                target.account.clone_from(&payer.account);
                target.size = payer.size;
            }
            target
        });
        let programs: Arc<dyn Programs> = match target {
            Some(target) => {
                let shared = files.clone();
                Arc::new(
                    HostedPrograms::new(
                        target,
                        workspace.to_owned(),
                        ENVIRONMENT.to_owned(),
                        Box::new(move || shares_of(vec![shared.clone()], Vec::new())),
                    )
                    .reporting_refusals(Arc::clone(&refused)),
                )
            }
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
            }),
            notices_out,
        );
        let (output, captured) = Output::buffer();
        let home = files.to_string_lossy().into_owned();
        let shell = Shell::new(
            Arc::new(host),
            [
                ("HOME".to_owned(), home),
                ("PATH".to_owned(), "/usr/local/bin:/usr/bin:/bin".to_owned()),
                ("TERM".to_owned(), "dumb".to_owned()),
            ],
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
        })
    }
}

impl Session {
    fn run(
        &mut self,
        command: &str,
        agent: bool,
        stopping: Option<Stopping<'_>>,
        seen: Option<Seen<'_>>,
    ) -> ShellRun {
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
        let exit = if stopping.is_none() && seen.is_none() {
            self.shell.run_script(command)
        } else {
            self.run_watched(command, stopping, seen)
        };
        self.shell.set_run(None);
        let ended_ms = now_ms();
        let cancelled = self.stopped.swap(false, Ordering::SeqCst);
        let mut output = self.captured.take();
        output.extend(self.notices.take());
        let truncated = output.len() > MAX_OUTPUT;
        output.truncate(MAX_OUTPUT);
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
                            seen(&RunOutput::of(&now));
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
}

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
}

#[cfg(test)]
#[path = "workspace_shell_tests.rs"]
mod tests;
