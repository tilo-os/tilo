use std::fs::File;
use std::io::{Seek, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::process::CommandExt;
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
use launcher::performance::apply_game_performance_hints;

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
        let (uid, gid) = (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        );
        validate_identity_uid(plan.identity_uid, uid)?;
        let mut filter_file = if let Some(profile) = plan.seccomp_profile {
            let filter = launcher::seccomp::compile_baseline(profile)
                .map_err(|error| ExecutionError::Seccomp(error.to_string()))?;
            let mut file = create_memfd("tilo-compat-seccomp")?;
            file.write_all(&filter)?;
            file.sync_all()?;
            // Bubblewrap reads from the inherited descriptor's current offset.
            // dup2 shares that offset, so an EOF position yields an empty BPF
            // program and PR_SET_SECCOMP fails with EINVAL.
            file.rewind()?;
            Some(file)
        } else {
            None
        };
        // Duplicate above every destination before fork. Otherwise dup2 of
        // one mapping could overwrite a source needed by the next mapping.
        let [prefix, home] = plan.directory_fds();
        let mut inherited = vec![
            (duplicate_above_child_slots(prefix)?, PREFIX_CHILD_FD),
            (duplicate_above_child_slots(home)?, HOME_CHILD_FD),
        ];
        for (name, contents, target) in identity_files(uid, gid) {
            let mut file = create_memfd(name)?;
            file.write_all(contents.as_bytes())?;
            file.rewind()?;
            inherited.push((duplicate_above_child_slots(file.as_fd())?, target));
        }
        if let Some(filter) = &filter_file {
            inherited.push((
                duplicate_above_child_slots(filter.as_fd())?,
                SECCOMP_CHILD_FD,
            ));
        }
        if plan.runtime_lock_required {
            let lock = create_memfd("tilo-runtime-lock")?;
            inherited.push((
                duplicate_above_child_slots(lock.as_fd())?,
                RUNTIME_LOCK_CHILD_FD,
            ));
        }
        let mappings: Vec<_> = inherited
            .iter()
            .map(|(file, target)| (file.as_raw_fd(), *target))
            .collect();

        let mut command = Command::new("/usr/bin/bwrap");
        command
            .args(&plan.bwrap_argv)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let latency_sensitive = plan.latency_sensitive;
        // SAFETY: the closure only iterates preallocated mappings and calls
        // async-signal-safe syscalls (performance hints and dup2). Every source differs from every target, so
        // dup2 also clears CLOEXEC on the inherited target descriptors.
        // The owned source descriptors remain alive until spawn returns.
        unsafe {
            command.pre_exec(move || {
                if latency_sensitive {
                    apply_game_performance_hints()?;
                }
                for &(source, target) in &mappings {
                    if libc::dup2(source, target) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(ExecutionError::Spawn)?;
        drop(inherited);
        drop(filter_file.take());
        let status = child.wait()?;
        Ok(ExecutionOutcome {
            success: status.success(),
            exit_code: status.code(),
        })
    }
}

fn duplicate_above_child_slots(fd: BorrowedFd<'_>) -> Result<File, std::io::Error> {
    // The duplicate stays close-on-exec in the parent.
    Ok(rustix::io::fcntl_dupfd_cloexec(fd, GROUP_CHILD_FD + 1)?.into())
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
