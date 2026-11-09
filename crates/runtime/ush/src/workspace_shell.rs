//! A hosted workspace's shell, run on the workspace's host.
//!
//! A workspace whose host serves it (tilo.sh) has no device of the person's
//! running its Terminal sessions: its shell lives with the workspace, beside
//! the host's hosted-VM service. [`WorkspaceShells`] keeps one shell per
//! Terminal session there and runs a command line in it, the way a device's
//! Terminal session runs what is typed at its prompt:
//!
//! - ush's own commands (`ls`, `cat`, `grep`, `echo`, …) run in-process, on
//!   the workspace's own directory on the host, and cost nothing;
//! - the file-changing ones (`cp`, `mv`, `rm`, a `>` redirect) are the
//!   environment's file actions, checked against its one grant (the
//!   workspace's directory) and carried out keeping what they replace;
//! - any other program (`cargo`, `npm`, `python`) runs in the workspace's own
//!   jailed VM through [`HostedPrograms`], metered per second, with the
//!   workspace's directory shared at the same path. A host that runs no VMs
//!   runs none, and the shell says which tier the program needs.
//!
//! A session's shell keeps its working directory and variables from one
//! command to the next. What a command printed comes back whole (standard
//! output and error, in the order written), up to [`MAX_OUTPUT`], with how it
//! ended and, when the host refused it a VM, the host's typed refusal: the
//! words ush already gives an agent, and the steps that let it through.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use collab_types::caps::CapOverride;
use tilo_actions::remote::host_account::Refusal;
use ush_shell::commands::Render;
use ush_shell::{Output, Run, Shell, Stdio};
use ush_tilo::{Acted, Environment, Keeping, NativeFiles, NoPrograms, Place, Programs, TiloHost};

use crate::content::EnvironmentContent;
use crate::file_actions::{FileAction, Recoverable};
use crate::hosted_vm::{HostedPrograms, HostedTarget, shares_of};
use crate::policy::Policy;

/// The most a run's output keeps; past it the newest bytes are dropped and
/// the run says it was cut.
pub const MAX_OUTPUT: usize = 1 << 20;

/// What the workspace's VM is called on its host: one per workspace.
const ENVIRONMENT: &str = "default";

/// How one command line went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellRun {
    /// The command line, as run.
    pub command: String,
    /// The directory it ran in.
    pub cwd: String,
    /// Its exit status: 126 when the host refused what it needed.
    pub exit: i32,
    /// What it printed, standard output and error as written.
    pub output: Vec<u8>,
    /// It printed more than [`MAX_OUTPUT`].
    pub truncated: bool,
    pub started_ms: i64,
    pub ended_ms: i64,
    /// Why the host would not run a program it needed, typed.
    pub refusal: Option<Refusal>,
}

/// The shells of the workspaces one host serves.
pub struct WorkspaceShells {
    /// Each workspace's content and kept files, and its directory unless
    /// [`Self::with_files`] says where it is, below here.
    state: PathBuf,
    /// Where each workspace's files are, when not below `state`.
    files_of: Option<Box<FilesOf>>,
    /// Where Linux programs run, when this host runs them.
    vms: Option<HostedTarget>,
    sessions: Mutex<HashMap<(String, String), Shared>>,
}

/// Where a workspace's files are on this host.
type FilesOf = dyn Fn(&str) -> PathBuf + Send + Sync;

/// Who pays for a workspace's VM, and its size, when the one asking says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payer {
    pub account: String,
    pub size: legacy_vm::hosted::size::VmSize,
}

/// What a caller is shown of a command while it runs: everything it printed
/// so far, each time that grew.
pub type Seen<'a> = &'a (dyn Fn(&[u8]) + Sync);

/// How often a running command's output is looked at for [`Seen`].
const SEEN_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

/// One session's shell, which its runs take turns in.
type Shared = Arc<Mutex<Session>>;

struct Session {
    shell: Shell,
    output: Output,
    captured: ush_shell::io::Captured,
    notices: ush_shell::io::Captured,
    refused: Arc<Mutex<Option<Refusal>>>,
}

impl WorkspaceShells {
    /// Shells keeping each workspace's files under `state`, running Linux
    /// programs in `vms` when given.
    #[must_use]
    pub fn new(state: impl Into<PathBuf>, vms: Option<HostedTarget>) -> Self {
        Self {
            state: state.into(),
            files_of: None,
            vms,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// The same shells, each workspace's working in the directory `files_of`
    /// names: its files as this host holds them, a member's synced replica.
    #[must_use]
    pub fn with_files(
        mut self,
        files_of: impl Fn(&str) -> PathBuf + Send + Sync + 'static,
    ) -> Self {
        self.files_of = Some(Box::new(files_of));
        self
    }

    /// Shells under `state`, running Linux programs in the VMs the host's
    /// environment names ([`HostedTarget::from_env`]).
    #[must_use]
    pub fn from_env(state: impl Into<PathBuf>) -> Self {
        Self::new(state, HostedTarget::from_env())
    }

    /// Whether this host runs Linux programs in VMs.
    #[must_use]
    pub fn runs_programs(&self) -> bool {
        self.vms.is_some()
    }

    /// The directory `workspace`'s shells work in: its files on this host.
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
        self.run_for(workspace, session, command, agent, None, None)
    }

    /// [`Self::run`], its VM paid for by `payer` rather than this host's
    /// own account when given (the first run of a workspace's session
    /// decides it), and `seen` shown what the command printed so far while
    /// it runs.
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
        seen: Option<Seen<'_>>,
    ) -> io::Result<ShellRun> {
        let shell = self.session(workspace, session, payer)?;
        let mut session = shell.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(session.run(command, agent, seen))
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
        let host = TiloHost::new(
            environment,
            Arc::new(NativeFiles),
            programs,
            Arc::new(HostPlace),
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
        })
    }
}

impl Session {
    fn run(&mut self, command: &str, agent: bool, seen: Option<Seen<'_>>) -> ShellRun {
        let cwd = self.shell.cwd().to_string_lossy().into_owned();
        *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = None;
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
        let exit = match seen {
            Some(seen) => {
                let printed = self.captured.clone();
                let done = std::sync::atomic::AtomicBool::new(false);
                std::thread::scope(|scope| {
                    scope.spawn(|| {
                        let mut shown = 0;
                        while !done.load(std::sync::atomic::Ordering::Acquire) {
                            std::thread::sleep(SEEN_EVERY);
                            let now = printed.peek();
                            if now.len() != shown {
                                shown = now.len();
                                seen(&now);
                            }
                        }
                    });
                    let exit = self.shell.run_script(command);
                    done.store(true, std::sync::atomic::Ordering::Release);
                    exit
                })
            }
            None => self.shell.run_script(command),
        };
        self.shell.set_run(None);
        let ended_ms = now_ms();
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
            started_ms,
            ended_ms,
            refusal: self
                .refused
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        }
    }
}

/// A workspace id as one directory name.
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

/// The workspace's directory as the shell's environment: one grant, and its
/// file actions carried out there, keeping what they replace.
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

/// What the host offers the shell besides files and programs: its clock.
struct HostPlace;

impl Place for HostPlace {
    fn now(&self) -> i64 {
        i64::try_from(tilo_clock::unix_now()).unwrap_or(i64::MAX)
    }

    fn now_ms(&self) -> i64 {
        now_ms()
    }

    fn sleep(&self, millis: u64) {
        std::thread::sleep(std::time::Duration::from_millis(millis));
    }

    fn hostname(&self) -> String {
        "tilo".to_owned()
    }
}

#[cfg(test)]
#[path = "workspace_shell_tests.rs"]
mod tests;
