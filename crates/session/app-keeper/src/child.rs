//! The app's process: started as the keeper's child in a process group of its own, ended with
//! the keeper (a kept app without a display is worse than a closed one).

use std::ffi::OsString;
use std::io;
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::Path;
use std::process::{Child, Command};

use crate::wire::End;

pub(crate) struct App {
    child: Child,
    pub pid: u32,
    pub started_at: Option<u64>,
    /// Readable once the process ended.
    pub pidfd: OwnedFd,
}

impl App {
    /// Start `argv` with `socket` as its Wayland display.
    pub(crate) fn spawn(argv: &[OsString], socket: &Path) -> io::Result<Self> {
        let Some((program, rest)) = argv.split_first() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no command to start",
            ));
        };
        let keeper = std::process::id();
        let mut command = Command::new(program);
        command
            .args(rest)
            // An absolute path: it reaches the keeper's socket whatever runtime directory the
            // app (or the sandbox it starts) sees.
            .env("WAYLAND_DISPLAY", socket)
            .env_remove("WAYLAND_SOCKET")
            .process_group(0);
        // SAFETY: only async-signal-safe calls between fork and exec: signal, prctl, getppid and
        // _exit.
        unsafe {
            command.pre_exec(move || {
                // The keeper ignores SIGPIPE; the app starts with the default every program
                // expects.
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(io::Error::last_os_error());
                }
                // The keeper may have died before the request took hold.
                if u32::try_from(libc::getppid()).ok() != Some(keeper) {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        let pid = child.id();
        let pidfd = pidfd_open(pid)?;
        Ok(Self {
            started_at: start_time(pid),
            child,
            pid,
            pidfd,
        })
    }

    /// How the process ended, once it did.
    pub(crate) fn try_end(&mut self) -> Option<End> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(match status.signal() {
                Some(signal) => End::Signalled { signal },
                None => End::Exited {
                    code: status.code().unwrap_or_default(),
                },
            }),
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%error, "the app's process could not be waited for");
                None
            }
        }
    }

    /// Send `signal` to the app's process group.
    pub(crate) fn signal(&self, signal: i32) {
        let Ok(group) = libc::pid_t::try_from(self.pid) else {
            return;
        };
        // SAFETY: kill takes plain integers; a negative pid names the group the app leads.
        if unsafe { libc::kill(group.saturating_neg(), signal) } != 0 {
            tracing::warn!(error = %io::Error::last_os_error(), signal, "the app could not be signalled");
        }
    }
}

fn pidfd_open(pid: u32) -> io::Result<OwnedFd> {
    let pid = libc::pid_t::try_from(pid).map_err(io::Error::other)?;
    // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = i32::try_from(fd).map_err(io::Error::other)?;
    // SAFETY: the kernel just returned this descriptor, owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// When `pid` started, in clock ticks since boot: field 22 of `/proc/<pid>/stat`.
pub(crate) fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is in parentheses and may hold spaces; the fields after it are plain.
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(19)?.parse().ok()
}
