//! Who is on the other end of a connection.
//!
//! Every caller must be the person at this machine's seat. Among their
//! processes, only the compositor may open drives: it is the one that draws
//! the consent prompt, which no app can draw or fake. The compositor is
//! recognised by the program it runs, read through a pidfd so a recycled
//! process id cannot impersonate it, and by sharing this service's user and
//! mount namespaces, which no sandboxed app does. `kernel.yama.ptrace_scope`
//! is 3 on Tilo, so no process can be steered from outside once running.

use std::fs;
use std::path::{Path, PathBuf};

use super::Caller;

/// Where the facts about callers come from.
#[derive(Clone, Debug)]
pub struct PeerPolicy {
    /// The compositor's executable.
    pub trusted_ui: PathBuf,
    /// logind's record of the seat (`ACTIVE_UID=`).
    pub seat: PathBuf,
    /// logind's per-user records, present while the user has a session.
    pub users: PathBuf,
}

impl Default for PeerPolicy {
    fn default() -> Self {
        Self {
            trusted_ui: PathBuf::from("/usr/bin/shell"),
            seat: PathBuf::from("/run/systemd/seats/seat0"),
            users: PathBuf::from("/run/systemd/users"),
        }
    }
}

/// What the kernel says about a connecting process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerFacts {
    pub uid: u32,
    /// Its executable, when it is still the process that connected.
    pub exe: Option<PathBuf>,
    /// Whether it shares this service's user and mount namespaces.
    pub host_namespaces: bool,
}

impl PeerPolicy {
    /// The uid logind reports at the seat.
    pub fn active_uid(&self) -> Option<u32> {
        fs::read_to_string(&self.seat).ok().and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("ACTIVE_UID="))
                .and_then(|uid| uid.trim().parse().ok())
        })
    }

    /// Whether `uid` still has any session.
    pub fn has_session(&self, uid: u32) -> bool {
        self.users.join(uid.to_string()).exists()
    }

    /// Decide who `peer` is, or why it is turned away.
    ///
    /// # Errors
    ///
    /// The reason, for a caller that is not the person at the seat.
    pub fn decide(&self, peer: &PeerFacts) -> Result<Caller, String> {
        if self.active_uid() != Some(peer.uid) {
            return Err("only the person at this computer may use its drives".into());
        }
        let trusted =
            peer.host_namespaces && peer.exe.as_deref() == Some(self.trusted_ui.as_path());
        Ok(if trusted {
            Caller::Shell { uid: peer.uid }
        } else {
            Caller::Person { uid: peer.uid }
        })
    }
}

/// Read a connected peer's facts from the kernel.
///
/// # Errors
///
/// Returns the I/O error if the socket has no peer credentials.
#[cfg(target_os = "linux")]
pub fn facts(stream: &std::os::unix::net::UnixStream) -> std::io::Result<PeerFacts> {
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let fd = stream.as_raw_fd();
    // Raw libc rather than rustix: rustix's credentials carry a non-zero pid,
    // and the kernel reports 0 for a peer in another PID namespace.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = libc::socklen_t::try_from(size_of::<libc::ucred>()).map_err(io::Error::other)?;
    // SAFETY: `cred` is a properly sized, writable `ucred`.
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &raw mut size,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut pidfd: libc::c_int = -1;
    let mut size = libc::socklen_t::try_from(size_of::<libc::c_int>()).map_err(io::Error::other)?;
    // SAFETY: `pidfd` is a properly sized, writable int.
    let pinned = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERPIDFD,
            (&raw mut pidfd).cast(),
            &raw mut size,
        )
    } == 0
        && pidfd >= 0;
    // SAFETY: the kernel just handed us this descriptor.
    let pidfd = pinned.then(|| unsafe { OwnedFd::from_raw_fd(pidfd) });
    let proc_dir = PathBuf::from(format!("/proc/{}", cred.pid));
    let exe = if cred.pid > 0 {
        fs::read_link(proc_dir.join("exe")).ok()
    } else {
        None
    };
    let host_namespaces = cred.pid > 0
        && ["user", "mnt"].iter().all(|ns| {
            let theirs = fs::read_link(proc_dir.join("ns").join(ns));
            let ours = fs::read_link(Path::new("/proc/self/ns").join(ns));
            matches!((theirs, ours), (Ok(theirs), Ok(ours)) if theirs == ours)
        });
    // The pid named the connecting process only if that process is still
    // alive now that its facts were read: the pidfd pins it.
    let still_same = pidfd.is_some_and(|pidfd| {
        // SAFETY: signal 0 only checks that the process exists.
        let sent = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd.as_raw_fd(),
                0,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        sent == 0
    });
    Ok(PeerFacts {
        uid: cred.uid,
        exe: exe.filter(|_| still_same),
        host_namespaces: host_namespaces && still_same,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(dir: &Path, active: Option<u32>) -> PeerPolicy {
        let seat = dir.join("seat0");
        if let Some(uid) = active {
            fs::write(&seat, format!("IS_SEAT0=1\nACTIVE=c2\nACTIVE_UID={uid}\n")).unwrap();
        }
        PeerPolicy {
            trusted_ui: PathBuf::from("/usr/bin/shell"),
            seat,
            users: dir.join("users"),
        }
    }

    fn shell(uid: u32) -> PeerFacts {
        PeerFacts {
            uid,
            exe: Some(PathBuf::from("/usr/bin/shell")),
            host_namespaces: true,
        }
    }

    #[test]
    fn only_the_seats_compositor_is_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let policy = policy(dir.path(), Some(1000));
        assert_eq!(policy.decide(&shell(1000)), Ok(Caller::Shell { uid: 1000 }));
        let sandboxed = PeerFacts {
            host_namespaces: false,
            ..shell(1000)
        };
        assert_eq!(policy.decide(&sandboxed), Ok(Caller::Person { uid: 1000 }));
        let other_program = PeerFacts {
            exe: Some(PathBuf::from("/usr/bin/python3")),
            ..shell(1000)
        };
        assert_eq!(
            policy.decide(&other_program),
            Ok(Caller::Person { uid: 1000 })
        );
        let exited = PeerFacts {
            exe: None,
            ..shell(1000)
        };
        assert_eq!(policy.decide(&exited), Ok(Caller::Person { uid: 1000 }));
    }

    #[test]
    fn nobody_but_the_person_at_the_seat_is_answered() {
        let dir = tempfile::tempdir().unwrap();
        assert!(policy(dir.path(), Some(1000)).decide(&shell(1001)).is_err());
        let empty = tempfile::tempdir().unwrap();
        assert!(policy(empty.path(), None).decide(&shell(1000)).is_err());
    }

    #[test]
    fn sessions_are_read_from_logind() {
        let dir = tempfile::tempdir().unwrap();
        let policy = policy(dir.path(), Some(1000));
        assert!(!policy.has_session(1000));
        fs::create_dir_all(dir.path().join("users")).unwrap();
        fs::write(dir.path().join("users/1000"), "STATE=active\n").unwrap();
        assert!(policy.has_session(1000));
    }
}
