//! Who is on the other end of a root service's local socket.
//!
//! Tilo's root services (the NTFS mount service, the device VPN helper) open
//! their socket to every local process and decide per connection, from the
//! kernel's account of the caller. The facts are the same for each of them:
//!
//! - the caller's uid, which must be the uid logind reports at the seat;
//! - its executable, read through `/proc/<pid>/exe` and pinned with the
//!   connection's pidfd, so a recycled process id cannot impersonate it;
//! - whether it shares the service's user and mount namespaces, which no
//!   sandboxed app does.
//!
//! The compositor (`/usr/bin/shell`) of the person at the seat is recognised
//! by all three. `kernel.yama.ptrace_scope` is 3 on Tilo, so no process can be
//! steered from outside once running.

use std::fs;
use std::path::{Path, PathBuf};

/// What the kernel says about a connecting process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerFacts {
    pub uid: u32,
    /// Its executable, when it is still the process that connected.
    pub exe: Option<PathBuf>,
    /// Whether it shares this service's user and mount namespaces.
    pub host_namespaces: bool,
}

/// Where the facts about the seat come from.
#[derive(Clone, Debug)]
pub struct Seat {
    /// The compositor's executable.
    pub trusted_ui: PathBuf,
    /// logind's record of the seat (`ACTIVE_UID=`).
    pub seat: PathBuf,
    /// logind's per-user records, present while the user has a session.
    pub users: PathBuf,
}

impl Default for Seat {
    fn default() -> Self {
        Self {
            trusted_ui: PathBuf::from("/usr/bin/shell"),
            seat: PathBuf::from("/run/systemd/seats/seat0"),
            users: PathBuf::from("/run/systemd/users"),
        }
    }
}

impl Seat {
    /// The uid logind reports at the seat.
    pub fn active_uid(&self) -> Option<u32> {
        active_uid(&self.seat)
    }

    /// Whether `uid` has a session, by logind's per-user records.
    pub fn has_session(&self, uid: u32) -> bool {
        has_session(&self.users, uid)
    }

    /// Whether `peer` is the compositor of the person at the seat.
    pub fn is_seat_shell(&self, peer: &PeerFacts) -> bool {
        self.active_uid() == Some(peer.uid) && self.is_trusted_ui(peer)
    }

    /// Whether `peer` runs the compositor's program in the host's namespaces,
    /// whoever's it is.
    pub fn is_trusted_ui(&self, peer: &PeerFacts) -> bool {
        peer.host_namespaces && peer.exe.as_deref() == Some(self.trusted_ui.as_path())
    }
}

/// The uid logind's seat record at `seat` names as active.
pub fn active_uid(seat: &Path) -> Option<u32> {
    fs::read_to_string(seat).ok().and_then(|text| {
        text.lines()
            .find_map(|line| line.strip_prefix("ACTIVE_UID="))
            .and_then(|uid| uid.trim().parse().ok())
    })
}

/// Whether logind's per-user records under `users` show a session of `uid`.
pub fn has_session(users: &Path, uid: u32) -> bool {
    users.join(uid.to_string()).exists()
}

/// Read a connected peer's facts from the kernel.
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

    fn seat(dir: &Path, active: Option<u32>) -> Seat {
        let seat = dir.join("seat0");
        if let Some(uid) = active {
            fs::write(&seat, format!("IS_SEAT0=1\nACTIVE=c2\nACTIVE_UID={uid}\n")).unwrap();
        }
        Seat {
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
    fn only_the_compositor_of_the_person_at_the_seat_is_the_seat_shell() {
        let dir = tempfile::tempdir().unwrap();
        let seat = seat(dir.path(), Some(1000));
        assert!(seat.is_seat_shell(&shell(1000)));
        assert!(!seat.is_seat_shell(&shell(1001)), "another person's shell");
        for other in [
            PeerFacts {
                host_namespaces: false,
                ..shell(1000)
            },
            PeerFacts {
                exe: Some(PathBuf::from("/usr/bin/python3")),
                ..shell(1000)
            },
            PeerFacts {
                exe: None,
                ..shell(1000)
            },
        ] {
            assert!(!seat.is_seat_shell(&other), "{other:?}");
        }
        let empty = tempfile::tempdir().unwrap();
        assert!(!self::seat(empty.path(), None).is_seat_shell(&shell(1000)));
    }

    #[test]
    fn sessions_are_read_from_logind() {
        let dir = tempfile::tempdir().unwrap();
        let seat = seat(dir.path(), Some(1000));
        assert_eq!(seat.active_uid(), Some(1000));
        assert!(!seat.has_session(1000));
        fs::create_dir_all(dir.path().join("users")).unwrap();
        fs::write(dir.path().join("users/1000"), "STATE=active\n").unwrap();
        assert!(seat.has_session(1000));
    }
}
