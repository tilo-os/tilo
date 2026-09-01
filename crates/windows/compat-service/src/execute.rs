use std::ffi::CString;
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use thiserror::Error;

use crate::CapsuleCommandPlan;

pub const SECCOMP_CHILD_FD: i32 = 32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionOutcome {
    pub success: bool,
    pub exit_code: Option<i32>,
}

pub trait SandboxExecutor: Send + Sync {
    /// Runs the already-validated Bubblewrap plan without a shell.
    fn execute(&self, plan: &CapsuleCommandPlan) -> Result<ExecutionOutcome, ExecutionError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BwrapExecutor;

impl SandboxExecutor for BwrapExecutor {
    fn execute(&self, plan: &CapsuleCommandPlan) -> Result<ExecutionOutcome, ExecutionError> {
        let mut filter_file = if let Some(profile) = plan.seccomp_profile {
            let filter = launcher::seccomp::compile_baseline(profile)
                .map_err(|error| ExecutionError::Seccomp(error.to_string()))?;
            let mut file = create_memfd()?;
            file.write_all(&filter)?;
            file.sync_all()?;
            Some(file)
        } else {
            None
        };
        let filter_fd = filter_file.as_ref().map(AsRawFd::as_raw_fd);

        let mut command = Command::new("/usr/bin/bwrap");
        command
            .args(&plan.bwrap_argv)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if let Some(filter_fd) = filter_fd {
            // SAFETY: `pre_exec` calls only async-signal-safe libc functions.
            // The owned memfd remains alive until spawn returns.
            unsafe {
                command.pre_exec(move || {
                    if libc::dup2(filter_fd, SECCOMP_CHILD_FD) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let flags = libc::fcntl(SECCOMP_CHILD_FD, libc::F_GETFD);
                    if flags == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::fcntl(SECCOMP_CHILD_FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut child = command.spawn().map_err(ExecutionError::Spawn)?;
        drop(filter_file.take());
        let status = child.wait()?;
        Ok(ExecutionOutcome {
            success: status.success(),
            exit_code: status.code(),
        })
    }
}

fn create_memfd() -> Result<File, ExecutionError> {
    let name = CString::new("qos-compat-seccomp")
        .map_err(|_| ExecutionError::Seccomp("invalid memfd name".to_string()))?;
    // SAFETY: `name` is a valid NUL-terminated C string. On success this call
    // returns a new owned file descriptor, transferred immediately to `File`.
    let raw = unsafe {
        libc::syscall(
            libc::SYS_memfd_create,
            name.as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    let fd =
        i32::try_from(raw).map_err(|_| ExecutionError::Memfd(std::io::Error::last_os_error()))?;
    if fd < 0 {
        return Err(ExecutionError::Memfd(std::io::Error::last_os_error()));
    }
    // SAFETY: `fd` is newly returned by memfd_create and has no other owner.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("could not compile the qOS seccomp profile: {0}")]
    Seccomp(String),
    #[error("could not create the private seccomp memfd: {0}")]
    Memfd(std::io::Error),
    #[error("could not spawn Bubblewrap: {0}")]
    Spawn(std::io::Error),
    #[error("sandbox process failed: {0}")]
    Io(#[from] std::io::Error),
}
