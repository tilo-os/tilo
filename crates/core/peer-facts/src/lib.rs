//! Who is on the other end of a local socket.
//!
//! Services open their socket to every local process and decide per connection
//! from the kernel's account of the caller: its account, its process (pinned with
//! the connection's pidfd, so a recycled process id cannot impersonate it), its
//! executable, and whether it shares the service's namespaces.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
pub mod server;

/// Which of this process's namespaces a peer shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Namespaces {
    /// The peer's user and mount namespaces are this process's own: a program of the same login
    /// that no sandbox puts elsewhere.
    Shared,
    /// The peer is in another user or mount namespace: a program a sandbox confines.
    Other,
    /// The kernel did not show the peer's namespaces, as it does not for a process that made
    /// itself non-dumpable. Not sharing them, since nothing was shown to say so.
    NotShown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerFacts {
    pub uid: u32,
    /// The process at the other end, or `None` when the kernel names no pid this namespace can
    /// look up: a peer in another pid namespace.
    pub pid: Option<u32>,
    /// The peer's executable, `None` when the kernel does not name it, or the pid is no longer
    /// the process that connected.
    pub exe: Option<PathBuf>,
    /// Which of this process's namespaces the peer shares, when the kernel shows it and the pid
    /// is still the process that connected.
    pub namespaces: Namespaces,
}

#[derive(Clone, Debug)]
pub struct LogindSeat {
    /// The compositor's executable.
    pub trusted_ui: PathBuf,
    /// logind's record of the seat (`ACTIVE_UID=`).
    pub seat: PathBuf,
    /// logind's per-user records; present while the user has a session.
    pub users: PathBuf,
}

impl Default for LogindSeat {
    fn default() -> Self {
        Self {
            trusted_ui: PathBuf::from("/usr/bin/tilo-session"),
            seat: PathBuf::from("/run/systemd/seats/seat0"),
            users: PathBuf::from("/run/systemd/users"),
        }
    }
}

impl LogindSeat {
    pub fn active_uid(&self) -> Option<u32> {
        active_uid(&self.seat)
    }

    pub fn has_session(&self, uid: u32) -> bool {
        has_session(&self.users, uid)
    }

    pub fn is_active_session_shell(&self, peer: &PeerFacts) -> bool {
        self.active_uid() == Some(peer.uid) && self.is_trusted_ui(peer)
    }

    /// Whether `peer` runs the compositor's program in the host's namespaces,
    /// whoever started it.
    pub fn is_trusted_ui(&self, peer: &PeerFacts) -> bool {
        peer.namespaces == Namespaces::Shared
            && peer.exe.as_deref() == Some(self.trusted_ui.as_path())
    }
}

pub fn active_uid(seat: &Path) -> Option<u32> {
    fs::read_to_string(seat).ok().and_then(|text| {
        text.lines()
            .find_map(|line| line.strip_prefix("ACTIVE_UID="))
            .and_then(|uid| uid.trim().parse().ok())
    })
}

pub fn has_session(users: &Path, uid: u32) -> bool {
    users.join(uid.to_string()).exists()
}

/// Read a connected peer's facts from the kernel.
///
/// # Errors
/// The kernel does not say who the peer is.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn facts(stream: &impl std::os::fd::AsRawFd) -> std::io::Result<PeerFacts> {
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd, OwnedFd};

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
            SO_PEERPIDFD,
            (&raw mut pidfd).cast(),
            &raw mut size,
        )
    } == 0
        && pidfd >= 0;
    // SAFETY: the kernel just handed us this descriptor.
    let pidfd = pinned.then(|| unsafe { OwnedFd::from_raw_fd(pidfd) });
    let pid = u32::try_from(cred.pid).ok().filter(|pid| *pid > 0);
    let proc_dir = pid.map(|pid| PathBuf::from(format!("/proc/{pid}")));
    let exe = proc_dir
        .as_ref()
        .and_then(|dir| fs::read_link(dir.join("exe")).ok());
    let namespaces = proc_dir
        .as_ref()
        .map_or(Namespaces::NotShown, |dir| namespaces_at(dir));
    // The pid still names the connecting process only if it is alive after its
    // facts were read: a recycled pid is no peer of this connection.
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
        pid,
        exe: exe.filter(|_| still_same),
        // A process that is gone is not one whose namespaces are still known.
        namespaces: if still_same {
            namespaces
        } else {
            Namespaces::NotShown
        },
    })
}

/// The socket option that hands over the peer's pidfd. Android's libc bindings do not name it;
/// its value is the kernel's on every architecture, and a kernel without it refuses the option, so
/// the peer is then not pinned and its executable and namespaces are not shown.
#[cfg(target_os = "linux")]
const SO_PEERPIDFD: libc::c_int = libc::SO_PEERPIDFD;
#[cfg(target_os = "android")]
const SO_PEERPIDFD: libc::c_int = 77;

/// Which of this process's namespaces the process at `proc_dir` shares.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn namespaces_at(proc_dir: &Path) -> Namespaces {
    use std::os::unix::fs::MetadataExt as _;

    let mut shared = true;
    for space in ["user", "mnt"] {
        let (theirs, ours) = (
            fs::metadata(proc_dir.join("ns").join(space)),
            fs::metadata(Path::new("/proc/self/ns").join(space)),
        );
        match (theirs, ours) {
            (Ok(theirs), Ok(ours)) => {
                shared &= theirs.dev() == ours.dev() && theirs.ino() == ours.ino();
            }
            // One of the two is not shown: which namespaces both processes are in is unknown.
            _ => return Namespaces::NotShown,
        }
    }
    if shared {
        Namespaces::Shared
    } else {
        Namespaces::Other
    }
}

/// Which of this process's namespaces the process `pid` shares, as the reader of a connection's
/// facts takes them.
#[cfg(all(test, target_os = "linux"))]
fn namespaces_of(pid: u32) -> Namespaces {
    namespaces_at(Path::new(&format!("/proc/{pid}")))
}

/// Read a connected peer's facts from the kernel.
///
/// macOS keeps no user or mount namespaces of a process, so a peer of the account is in the same
/// ones; nor does it keep a process's executable where this can read it.
///
/// # Errors
/// The kernel does not say who the peer is.
#[cfg(target_os = "macos")]
pub fn facts(stream: &impl std::os::fd::AsRawFd) -> std::io::Result<PeerFacts> {
    use std::io;
    use std::os::fd::AsRawFd as _;

    let fd = stream.as_raw_fd();
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: both are writable ids.
    if unsafe { libc::getpeereid(fd, &raw mut uid, &raw mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut pid: libc::pid_t = 0;
    let mut length =
        libc::socklen_t::try_from(size_of::<libc::pid_t>()).map_err(io::Error::other)?;
    // SAFETY: `pid` is a writable `pid_t` of `length` bytes.
    let read = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &raw mut length,
        )
    };
    Ok(PeerFacts {
        uid,
        pid: u32::try_from(pid)
            .ok()
            .filter(|pid| *pid > 0)
            .filter(|_| read == 0),
        exe: None,
        namespaces: Namespaces::Shared,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat(dir: &Path, active: Option<u32>) -> LogindSeat {
        let seat = dir.join("seat0");
        if let Some(uid) = active {
            fs::write(&seat, format!("IS_SEAT0=1\nACTIVE=c2\nACTIVE_UID={uid}\n")).unwrap();
        }
        LogindSeat {
            trusted_ui: PathBuf::from("/usr/bin/tilo-session"),
            seat,
            users: dir.join("users"),
        }
    }

    fn shell(uid: u32) -> PeerFacts {
        PeerFacts {
            uid,
            pid: Some(1234),
            exe: Some(PathBuf::from("/usr/bin/tilo-session")),
            namespaces: Namespaces::Shared,
        }
    }

    #[test]
    fn only_the_compositor_of_the_person_at_the_seat_is_the_seat_shell() {
        let dir = tempfile::tempdir().unwrap();
        let seat = seat(dir.path(), Some(1000));
        assert!(seat.is_active_session_shell(&shell(1000)));
        assert!(
            !seat.is_active_session_shell(&shell(1001)),
            "another person's shell"
        );
        for other in [
            PeerFacts {
                namespaces: Namespaces::Other,
                ..shell(1000)
            },
            PeerFacts {
                namespaces: Namespaces::NotShown,
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
            assert!(!seat.is_active_session_shell(&other), "{other:?}");
        }
        let empty = tempfile::tempdir().unwrap();
        assert!(!self::seat(empty.path(), None).is_active_session_shell(&shell(1000)));
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

    #[cfg(target_os = "linux")]
    #[test]
    fn a_connection_of_our_own_is_our_program_in_our_namespaces() {
        use std::os::unix::net::UnixStream;

        let (ours, _theirs) = UnixStream::pair().unwrap();
        let facts = facts(&ours).unwrap();
        assert_eq!(facts.uid, rustix::process::geteuid().as_raw());
        assert_eq!(facts.pid, Some(std::process::id()));
        assert_eq!(
            facts.exe,
            Some(fs::read_link("/proc/self/exe").unwrap()),
            "the kernel's own record of this program"
        );
        assert_eq!(facts.namespaces, Namespaces::Shared);
    }

    /// A process that makes itself non-dumpable, as one does that loads the device key, is not
    /// shown to the reader: the kernel denies its namespaces to another process.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_non_dumpable_process_is_not_shown() {
        let mut ready = [0_i32; 2];
        // SAFETY: a pipe into a buffer of two descriptors.
        assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0);
        // SAFETY: the child only makes itself non-dumpable, says so on the pipe and waits to be
        // killed: async-signal-safe calls, no allocation and no lock after the fork.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            // SAFETY: as above.
            unsafe {
                libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
                libc::write(ready[1], b"r".as_ptr().cast(), 1);
                loop {
                    libc::pause();
                }
            }
        }
        let mut byte = [0_u8; 1];
        // SAFETY: one byte into a buffer of one, from the pipe this test made.
        assert_eq!(
            unsafe { libc::read(ready[0], byte.as_mut_ptr().cast(), 1) },
            1
        );
        let namespaces = namespaces_of(u32::try_from(pid).unwrap());
        // SAFETY: the child this test forked, and the pipe it made.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
            libc::close(ready[0]);
            libc::close(ready[1]);
        }
        assert_eq!(namespaces, Namespaces::NotShown);
    }
}
