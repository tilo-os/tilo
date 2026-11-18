use std::fs::File;
use std::io::{Seek, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::process::{Command, Stdio};

use rustix::fs::MemfdFlags;
use thiserror::Error;

use crate::CapsuleCommandPlan;

pub const SECCOMP_CHILD_FD: i32 = 32;
pub(crate) const PREFIX_CHILD_FD: i32 = 33;
pub(crate) const HOME_CHILD_FD: i32 = 34;
pub(crate) const RUNTIME_LOCK_CHILD_FD: i32 = 35;
pub(crate) const PASSWD_CHILD_FD: i32 = 36;
pub(crate) const GROUP_CHILD_FD: i32 = 37;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionOutcome {
    pub success: bool,
    pub exit_code: Option<i32>,
}

/// A program an executor started, which goes on after its launch request is answered: its end
/// is waited for on a thread of the service's own, and it can be ended at any time.
pub trait StartedProgram: Send + Sync {
    /// Wait until the program ended, and say how.
    fn wait(&self) -> Result<ExecutionOutcome, ExecutionError>;
    /// End the program now.
    fn stop(&self);
}

/// The command that starts a planned sandbox: Bubblewrap and its arguments, the descriptors it
/// inherits at the numbers its arguments name, and whether it starts with the game performance
/// hints.
///
/// Started directly ([`Self::spawn`]) or handed to whoever starts it for the session: a program
/// the session keeps open through a desktop restart starts behind the session's own launch, which
/// hands the same descriptors down ([`launcher::inherit`]).
pub struct SandboxCommand {
    pub program: std::path::PathBuf,
    pub args: Vec<std::ffi::OsString>,
    /// Each descriptor with the number the program finds it at.
    pub inherited: Vec<(std::os::fd::OwnedFd, i32)>,
    pub latency_sensitive: bool,
}

impl SandboxCommand {
    /// `program` with `args`, inheriting nothing.
    #[must_use]
    pub fn new(
        program: impl Into<std::path::PathBuf>,
        args: impl IntoIterator<Item = impl Into<std::ffi::OsString>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            inherited: Vec::new(),
            latency_sensitive: false,
        }
    }

    /// Start the command as this process's child.
    ///
    /// # Errors
    /// The program could not be started.
    pub fn spawn(self) -> Result<Box<dyn StartedProgram>, ExecutionError> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let handed: Vec<_> = self
            .inherited
            .iter()
            .map(|(fd, target)| (fd.as_raw_fd(), *target))
            .collect();
        launcher::inherit::hand_down(&mut command, handed, self.latency_sensitive);
        let child = command.spawn().map_err(ExecutionError::Spawn)?;
        drop(self.inherited);
        Ok(Box::new(ChildProgram::new(child)?))
    }
}

pub trait SandboxExecutor: Send + Sync {
    /// The command that starts the already-validated Bubblewrap plan, without a shell.
    fn command(&self, plan: &CapsuleCommandPlan) -> Result<SandboxCommand, ExecutionError>;

    /// Start the plan as this process's child.
    fn start(&self, plan: &CapsuleCommandPlan) -> Result<Box<dyn StartedProgram>, ExecutionError> {
        self.command(plan)?.spawn()
    }

    /// Waits for the plan to end: a setup or an uninstaller, whose end is the answer.
    fn execute(&self, plan: &CapsuleCommandPlan) -> Result<ExecutionOutcome, ExecutionError> {
        self.start(plan)?.wait()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BwrapExecutor;

impl SandboxExecutor for BwrapExecutor {
    fn command(&self, plan: &CapsuleCommandPlan) -> Result<SandboxCommand, ExecutionError> {
        let (uid, gid) = (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        );
        validate_identity_uid(plan.identity_uid, uid)?;
        // Every source is duplicated above the numbers the program finds them at, so handing one
        // down never overwrites the source of the next.
        let mut inherited = Vec::new();
        if let Some(profile) = plan.seccomp_profile {
            let filter = launcher::seccomp::compile_baseline(profile)
                .map_err(|error| ExecutionError::Seccomp(error.to_string()))?;
            let mut file = create_memfd("tilo-compat-seccomp")?;
            file.write_all(&filter)?;
            file.sync_all()?;
            // Bubblewrap reads from the inherited descriptor's current offset.
            // dup2 shares that offset, so an EOF position yields an empty BPF
            // program and PR_SET_SECCOMP fails with EINVAL.
            file.rewind()?;
            inherited.push((duplicate_above_child_slots(file.as_fd())?, SECCOMP_CHILD_FD));
        }
        let [prefix, home] = plan.directory_fds();
        inherited.push((duplicate_above_child_slots(prefix)?, PREFIX_CHILD_FD));
        inherited.push((duplicate_above_child_slots(home)?, HOME_CHILD_FD));
        for (name, contents, target) in identity_files(uid, gid) {
            let mut file = create_memfd(name)?;
            file.write_all(contents.as_bytes())?;
            file.rewind()?;
            inherited.push((duplicate_above_child_slots(file.as_fd())?, target));
        }
        if plan.runtime_lock_required {
            let lock = create_memfd("tilo-runtime-lock")?;
            inherited.push((
                duplicate_above_child_slots(lock.as_fd())?,
                RUNTIME_LOCK_CHILD_FD,
            ));
        }
        let (program, args) = sandbox_program(plan.audio_graph.as_deref(), &plan.bwrap_argv);
        Ok(SandboxCommand {
            program,
            args,
            inherited,
            latency_sensitive: plan.latency_sensitive,
        })
    }
}

/// A program this process started as its child: ended through its pidfd, so a stop never
/// reaches another process that took its pid after it was reaped.
pub struct ChildProgram {
    child: std::sync::Mutex<std::process::Child>,
    pidfd: std::os::fd::OwnedFd,
}

impl ChildProgram {
    /// Wait for and end `child`.
    ///
    /// # Errors
    /// Its pidfd could not be opened; the child is ended then.
    pub fn new(mut child: std::process::Child) -> Result<Self, ExecutionError> {
        let pid = rustix::process::Pid::from_child(&child);
        match rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()) {
            Ok(pidfd) => Ok(Self {
                child: std::sync::Mutex::new(child),
                pidfd,
            }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(ExecutionError::Io(error.into()))
            }
        }
    }
}

impl StartedProgram for ChildProgram {
    fn wait(&self) -> Result<ExecutionOutcome, ExecutionError> {
        let status = self
            .child
            .lock()
            .map_err(|_| ExecutionError::Io(std::io::Error::other("the program's state is lost")))?
            .wait()?;
        Ok(ExecutionOutcome {
            success: status.success(),
            exit_code: status.code(),
        })
    }

    fn stop(&self) {
        // Bubblewrap dies of it, and the sandbox's processes with it (`--die-with-parent`).
        let _ = rustix::process::pidfd_send_signal(&self.pidfd, rustix::process::Signal::KILL);
    }
}

fn duplicate_above_child_slots(fd: BorrowedFd<'_>) -> Result<std::os::fd::OwnedFd, std::io::Error> {
    Ok(rustix::io::fcntl_dupfd_cloexec(fd, GROUP_CHILD_FD + 1)?)
}

/// Bubblewrap with `bwrap_argv`, started by the launcher's `windows-audio` after the private
/// audio graph prepared in `audio_graph` when there is one: the graph's daemons are children of
/// the sandbox's own process then, so they end with the program whoever started it, and a program
/// kept open while the desktop restarts keeps its sound.
fn sandbox_program(
    audio_graph: Option<&std::path::Path>,
    bwrap_argv: &[String],
) -> (std::path::PathBuf, Vec<std::ffi::OsString>) {
    const BWRAP: &str = "/usr/bin/bwrap";
    let bwrap = bwrap_argv.iter().map(Into::into);
    match audio_graph {
        None => (BWRAP.into(), bwrap.collect()),
        Some(graph) => (
            crate::plan::SHIM_HOST.into(),
            [
                "windows-audio".into(),
                "--graph".into(),
                graph.into(),
                "--".into(),
                BWRAP.into(),
            ]
            .into_iter()
            .chain(bwrap)
            .collect(),
        ),
    }
}

fn validate_identity_uid(planned: u32, effective: u32) -> Result<(), ExecutionError> {
    if planned != effective {
        return Err(ExecutionError::IdentityMismatch { planned, effective });
    }
    Ok(())
}

fn identity_files(uid: u32, gid: u32) -> [(&'static str, String, i32); 2] {
    [
        (
            "tilo-capsule-passwd",
            format!(
                "tilo:x:{uid}:{gid}:Tilo capsule:{}:/usr/bin/nologin\n",
                crate::plan::HOME_MOUNT
            ),
            PASSWD_CHILD_FD,
        ),
        (
            "tilo-capsule-group",
            format!("tilo:x:{gid}:\n"),
            GROUP_CHILD_FD,
        ),
    ]
}

fn create_memfd(name: &str) -> Result<File, ExecutionError> {
    let fd = rustix::fs::memfd_create(name, MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)
        .map_err(|error| ExecutionError::Memfd(error.into()))?;
    Ok(fd.into())
}

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("capsule session UID {planned} differs from executor UID {effective}")]
    IdentityMismatch { planned: u32, effective: u32 },
    #[error("could not compile the Tilo seccomp profile: {0}")]
    Seccomp(String),
    #[error("could not create a private sandbox memfd: {0}")]
    Memfd(std::io::Error),
    #[error("could not spawn Bubblewrap: {0}")]
    Spawn(std::io::Error),
    #[error("sandbox process failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sandbox_with_sound_starts_after_its_audio_graph() {
        let bwrap_argv = vec!["--die-with-parent".to_owned()];
        let (program, args) = sandbox_program(None, &bwrap_argv);
        assert_eq!(program, std::path::Path::new("/usr/bin/bwrap"));
        assert_eq!(args, ["--die-with-parent"]);
        let (program, args) = sandbox_program(Some(std::path::Path::new("/lease")), &bwrap_argv);
        assert_eq!(program, std::path::Path::new(crate::plan::SHIM_HOST));
        assert_eq!(
            args,
            [
                "windows-audio",
                "--graph",
                "/lease",
                "--",
                "/usr/bin/bwrap",
                "--die-with-parent"
            ]
        );
    }

    #[test]
    fn execution_identity_is_fixed_and_fails_closed_when_changed() {
        {
            for (uid, gid) in [(0, 0), (1000, 2000), (u32::MAX, u32::MAX)] {
                let files = identity_files(uid, gid);
                assert_eq!(
                    files[0].1,
                    format!(
                        "tilo:x:{uid}:{gid}:Tilo capsule:/run/tilo/capsule/home:/usr/bin/nologin\n"
                    )
                );
                assert_eq!(files[1].1, format!("tilo:x:{gid}:\n"));
                assert_eq!(files[0].2, PASSWD_CHILD_FD);
                assert_eq!(files[1].2, GROUP_CHILD_FD);
            }
        }
        {
            assert!(validate_identity_uid(1000, 1000).is_ok());
            assert!(matches!(
                validate_identity_uid(1000, 0),
                Err(ExecutionError::IdentityMismatch {
                    planned: 1000,
                    effective: 0
                })
            ));
        }
    }
}
