//! Linux process-identity leases for privileged first-use operations.
//!
//! A PID alone is reusable. Pairing therefore carries both the compositor's
//! PID and the kernel start-time tick from `/proc/<pid>/stat`. The privileged
//! worker checks that pair while waiting and at every publication checkpoint;
//! a dead or replaced compositor cannot leave an enrollment running.

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The stable identity of one live Linux process for its current lifetime.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ProcessLease {
    pid: u32,
    start_time: u64,
}

impl ProcessLease {
    /// Capture a live process. The lease is valid only while the same kernel
    /// process occupies `pid`.
    pub fn capture(pid: u32) -> io::Result<Self> {
        Ok(Self {
            pid,
            start_time: process_start_time(pid)?,
        })
    }

    /// Reconstruct a lease received over a narrow command-line boundary.
    #[must_use]
    pub const fn from_parts(pid: u32, start_time: u64) -> Self {
        Self { pid, start_time }
    }

    /// Process identifier carried by this lease.
    #[must_use]
    pub const fn pid(self) -> u32 {
        self.pid
    }

    /// Kernel start-time tick carried by this lease.
    #[must_use]
    pub const fn start_time(self) -> u64 {
        self.start_time
    }

    /// Verify that the original process is still alive and has not been
    /// replaced through PID reuse.
    ///
    /// # Errors
    ///
    /// Returns an error if the process vanished, procfs cannot be read, or
    /// the PID now identifies a different process lifetime.
    pub fn validate(self) -> io::Result<()> {
        let observed = process_start_time(self.pid)?;
        if observed != self.start_time {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "requesting process was replaced",
            ));
        }
        Ok(())
    }

    /// Verify the lease and require the process to belong to `uid`. The
    /// polkit broker uses this before forwarding an unprivileged request.
    #[cfg(unix)]
    pub fn validate_owner(self, uid: u32) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt as _;

        self.validate()?;
        let owner = std::fs::metadata(proc_path(self.pid))?.uid();
        if owner != uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "requesting process belongs to another user",
            ));
        }
        Ok(())
    }
}

fn proc_path(pid: u32) -> PathBuf {
    PathBuf::from("/proc").join(pid.to_string())
}

fn process_start_time(pid: u32) -> io::Result<u64> {
    let stat = std::fs::read_to_string(proc_path(pid).join("stat"))?;
    // `comm` is parenthesised and may itself contain spaces or `)`. The last
    // closing parenthesis precedes field 3 (`state`); starttime is field 22,
    // therefore the twentieth token in the remaining tail.
    let tail = stat
        .rsplit_once(')')
        .map(|(_, tail)| tail)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "process stat has no command"))?;
    tail.split_whitespace()
        .nth(19)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "process stat is truncated"))?
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid process start time"))
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn process_leases_bind_start_time_and_expire_with_the_process() {
        {
            use std::os::unix::fs::MetadataExt as _;

            let lease = ProcessLease::capture(std::process::id()).unwrap();
            lease.validate().unwrap();
            let uid = std::fs::metadata("/proc/self").unwrap().uid();
            lease.validate_owner(uid).unwrap();
            assert!(lease.validate_owner(uid.wrapping_add(1)).is_err());
            assert_eq!(lease.pid(), std::process::id());
            assert!(
                ProcessLease::from_parts(lease.pid(), lease.start_time().wrapping_add(1))
                    .validate()
                    .is_err()
            );
        }
        {
            let mut child = Command::new("/bin/sh")
                .args(["-c", "while :; do sleep 1; done"])
                .spawn()
                .unwrap();
            let lease = ProcessLease::capture(child.id()).unwrap();
            lease.validate().unwrap();
            child.kill().unwrap();
            child.wait().unwrap();
            assert!(lease.validate().is_err());
        }
    }
}
