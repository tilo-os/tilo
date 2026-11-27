//! The process at the other end of a local connection, pinned: held so that it names that one
//! process for as long as it is kept, never another that later runs under the same pid.
//!
//! Each platform pins it its own way, and every pin answers the same questions: which process
//! it is ([`Pinned::pid`]), whether it still runs ([`Pinned::alive`]), whether a pid and start
//! another part of the system names are it ([`Pinned::is`]), and, for a tokio caller, when it
//! ends ([`Pinned::exited`]).
//!
//! - Linux: the pidfd the kernel hands for the socket's peer (`SO_PEERPIDFD`), which refers to
//!   that task alone and becomes readable when it exits.
//! - macOS: the peer's pid (`LOCAL_PEERPID`), with its start time read before and after a
//!   kqueue `NOTE_EXIT` watch is set on it: the same start on both sides means the watch is on
//!   the process that connected, not one that took its pid after it.
//! - Windows: the pipe's client process (`GetNamedPipeClientProcessId`), opened as a handle and
//!   then checked against the pipe: while the pipe is still connected the process holding its end
//!   runs, so the handle is that process and not one that reused its pid.
//! - Android, whose kernel pins no process behind a socket: only this process, which runs while it
//!   asks. A peer that is any other process is pinned by nothing.

use std::io;
use std::sync::Arc;

/// A process pinned at the other end of a connection. Clones share the one pin.
#[derive(Clone, Debug)]
pub struct Pinned(Arc<imp::Pin>);

impl Pinned {
    /// The process at the other end of `stream`, pinned as it is now.
    ///
    /// # Errors
    /// The system names no process for the peer, or it ended while it was being pinned.
    pub fn of(stream: &crate::Stream) -> io::Result<Self> {
        #[cfg(unix)]
        let pin = imp::Pin::of(std::os::fd::AsFd::as_fd(stream));
        #[cfg(windows)]
        let pin = imp::Pin::of(
            std::os::windows::io::AsRawHandle::as_raw_handle(stream),
            stream.is_server_end(),
        );
        pin.map(|pin| Self(Arc::new(pin)))
    }

    /// [`Pinned::of`] for a stream on the tokio runtime.
    ///
    /// # Errors
    /// As [`Pinned::of`].
    #[cfg(feature = "tokio")]
    pub fn of_tokio(stream: &crate::tokio::Stream) -> io::Result<Self> {
        #[cfg(unix)]
        let pin = imp::Pin::of(std::os::fd::AsFd::as_fd(stream));
        #[cfg(windows)]
        let pin = {
            let (pipe, server_end) = stream.raw_end();
            imp::Pin::of(pipe, server_end)
        };
        pin.map(|pin| Self(Arc::new(pin)))
    }

    /// The process `pidfd` refers to, as a launcher that pinned it hands it over.
    ///
    /// # Errors
    /// The process has already ended.
    #[cfg(target_os = "linux")]
    pub fn from_pidfd(pidfd: std::os::fd::OwnedFd) -> io::Result<Self> {
        imp::Pin::from_pidfd(pidfd).map(|pin| Self(Arc::new(pin)))
    }

    /// The pidfd pinning the process, to hand over to another process.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn pidfd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&*self.0.pidfd)
    }

    /// The pinned process's pid.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.0.pid
    }

    /// Whether the pinned process still runs.
    #[must_use]
    pub fn alive(&self) -> bool {
        self.0.alive()
    }

    /// Whether the process another part of the system names by `pid` and `start` (its start
    /// time as this platform counts it: clock ticks since boot on Linux, microseconds since the
    /// epoch on macOS, the creation `FILETIME` on Windows) is the pinned one, still running.
    #[must_use]
    pub fn is(&self, pid: u32, start: u64) -> bool {
        self.0.pid == pid && imp::start_of(pid) == Some(start) && self.0.is_started(start)
    }

    /// Resolves once the pinned process has ended, or at once when its end cannot be watched:
    /// a process this cannot watch is treated as gone, never as running forever.
    #[cfg(feature = "tokio")]
    pub async fn exited(&self) {
        self.0.exited().await;
    }
}

/// Whether `fd` has nothing to read now: a pidfd, or a kqueue watching an exit, is readable once
/// its process has ended.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn quiet(fd: std::os::fd::BorrowedFd<'_>) -> bool {
    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
    poll(&mut [PollFd::new(fd, PollFlags::POLLIN)], PollTimeout::ZERO) == Ok(0)
}

/// Resolves once `fd` turns readable, or at once when it cannot be watched.
#[cfg(all(feature = "tokio", any(target_os = "linux", target_os = "macos")))]
async fn readable(fd: &std::sync::Arc<std::os::fd::OwnedFd>) {
    #[derive(Debug)]
    struct Watched(std::sync::Arc<std::os::fd::OwnedFd>);
    impl std::os::fd::AsRawFd for Watched {
        fn as_raw_fd(&self) -> std::os::fd::RawFd {
            self.0.as_raw_fd()
        }
    }
    // SAFETY: the AsyncFd owns `Watched` and so one strong reference to the descriptor, which
    // keeps it open until the AsyncFd deregisters it on drop.
    if let Ok(watched) = unsafe {
        tokio::io::unix::AsyncFd::register_with_interest(
            Watched(std::sync::Arc::clone(fd)),
            tokio::io::Interest::READABLE,
        )
    } {
        let _ = watched.readable().await;
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::io;
    use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd};
    use std::sync::Arc;

    #[derive(Debug)]
    pub(super) struct Pin {
        pub(super) pidfd: Arc<OwnedFd>,
        pub(super) pid: u32,
    }

    impl Pin {
        pub(super) fn of(socket: BorrowedFd<'_>) -> io::Result<Self> {
            Self::from_pidfd(nix::sys::socket::getsockopt(
                &socket,
                nix::sys::socket::sockopt::PeerPidfd,
            )?)
        }

        pub(super) fn from_pidfd(pidfd: OwnedFd) -> io::Result<Self> {
            let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", pidfd.as_raw_fd()))?;
            // A pidfd whose process has ended reads `Pid: -1`, which parses as no pid.
            let pid = info
                .lines()
                .find_map(|line| line.strip_prefix("Pid:"))
                .and_then(|pid| pid.trim().parse::<u32>().ok());
            match pid {
                Some(pid) if super::quiet(pidfd.as_fd()) => Ok(Self {
                    pidfd: Arc::new(pidfd),
                    pid,
                }),
                _ => Err(io::Error::other("the pinned process has already ended")),
            }
        }

        pub(super) fn alive(&self) -> bool {
            super::quiet(self.pidfd.as_fd())
        }

        /// The pidfd pins the task itself: a start read for its pid while it runs is its own.
        pub(super) fn is_started(&self, _: u64) -> bool {
            self.alive()
        }

        #[cfg(feature = "tokio")]
        pub(super) async fn exited(&self) {
            super::readable(&self.pidfd).await;
        }
    }

    /// The start of the process `pid` now, in clock ticks since boot (`/proc/<pid>/stat`'s 22nd
    /// field).
    pub(super) fn start_of(pid: u32) -> Option<u64> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (_, fields) = stat.rsplit_once(") ")?;
        fields.split_whitespace().nth(19)?.parse().ok()
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
    use std::io;
    use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
    use std::sync::Arc;

    #[derive(Debug)]
    pub(super) struct Pin {
        pub(super) pid: u32,
        start: u64,
        /// A kqueue watching the process's exit: readable once it has ended.
        exit: Arc<OwnedFd>,
    }

    impl Pin {
        pub(super) fn of(socket: BorrowedFd<'_>) -> io::Result<Self> {
            let pid =
                nix::sys::socket::getsockopt(&socket, nix::sys::socket::sockopt::LocalPeerPid)?;
            let pid = u32::try_from(pid)
                .ok()
                .filter(|pid| *pid > 0)
                .ok_or_else(|| io::Error::other("the system named no process for the peer"))?;
            let ended = || io::Error::other("the peer's process ended while it was pinned");
            let start = start_of(pid).ok_or_else(ended)?;
            let exit = Kqueue::new()?;
            let watch = KEvent::new(
                usize::try_from(pid).map_err(io::Error::other)?,
                EventFilter::EVFILT_PROC,
                EvFlags::EV_ADD | EvFlags::EV_CLEAR,
                FilterFlag::NOTE_EXIT,
                0,
                0,
            );
            exit.kevent(&[watch], &mut [], None)?;
            // The watch is set on whatever runs as `pid` now: the same start as before it means
            // that is the process that connected.
            if start_of(pid) != Some(start) {
                return Err(ended());
            }
            Ok(Self {
                pid,
                start,
                exit: Arc::new(exit.into()),
            })
        }

        pub(super) fn alive(&self) -> bool {
            super::quiet(self.exit.as_fd())
        }

        pub(super) fn is_started(&self, start: u64) -> bool {
            self.start == start && self.alive()
        }

        #[cfg(feature = "tokio")]
        pub(super) async fn exited(&self) {
            super::readable(&self.exit).await;
        }
    }

    /// The start of the process `pid` now, in microseconds since the epoch. Nothing in the
    /// platform crates wraps `proc_pidinfo`, so it is called here.
    pub(super) fn start_of(pid: u32) -> Option<u64> {
        // SAFETY: proc_bsdinfo is plain integers and arrays of them, for which zero is valid.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: a writable buffer of `size` bytes for the flavour that fills a proc_bsdinfo.
        let read = unsafe {
            libc::proc_pidinfo(
                libc::c_int::try_from(pid).ok()?,
                libc::PROC_PIDTBSDINFO,
                0,
                (&raw mut info).cast(),
                size,
            )
        };
        (read == size).then(|| {
            info.pbi_start_tvsec
                .saturating_mul(1_000_000)
                .saturating_add(info.pbi_start_tvusec)
        })
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle};
    use windows_sys::Win32::Foundation::{FILETIME, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        WaitForSingleObject,
    };

    #[derive(Debug)]
    pub(super) struct Pin {
        pub(super) pid: u32,
        start: u64,
        process: OwnedHandle,
    }

    impl Pin {
        pub(super) fn of(pipe: RawHandle, server_end: bool) -> io::Result<Self> {
            let pid = crate::windows::peer_pid(pipe, server_end)?;
            // SAFETY: opens a handle the OwnedHandle takes; a null one is the failure.
            let process = unsafe {
                OpenProcess(
                    PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    pid,
                )
            };
            if process.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: a fresh process handle this call owns.
            let process = unsafe { OwnedHandle::from_raw_handle(process) };
            // While the pipe is connected the process holding its other end runs: the handle
            // opened for its pid is that process, not one that took the pid after it.
            if !crate::windows::pipe_connected(pipe) {
                return Err(io::Error::other(
                    "the peer's process ended while it was pinned",
                ));
            }
            let start = created(process.as_raw_handle())
                .ok_or_else(|| io::Error::other("the system names no start for the peer"))?;
            Ok(Self {
                pid,
                start,
                process,
            })
        }

        pub(super) fn alive(&self) -> bool {
            // SAFETY: a live process handle, without waiting.
            unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) == WAIT_TIMEOUT }
        }

        pub(super) fn is_started(&self, start: u64) -> bool {
            self.start == start && self.alive()
        }

        /// A wait on a duplicate of the handle, on the runtime's blocking pool: one the caller
        /// stops awaiting keeps its thread until the process ends.
        #[cfg(feature = "tokio")]
        pub(super) async fn exited(&self) {
            let Ok(process) = self.process.try_clone() else {
                return;
            };
            let _ = tokio::task::spawn_blocking(move || {
                // SAFETY: a live process handle the closure owns.
                unsafe {
                    WaitForSingleObject(
                        process.as_raw_handle(),
                        windows_sys::Win32::System::Threading::INFINITE,
                    )
                }
            })
            .await;
        }
    }

    /// The creation time of the process `process` handles, as a `FILETIME`'s 100 ns since 1601.
    fn created(process: RawHandle) -> Option<u64> {
        let mut times = [FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        }; 4];
        let [creation, exit, kernel, user] = &mut times;
        // SAFETY: a live process handle and four writable FILETIMEs.
        let read = unsafe { GetProcessTimes(process, creation, exit, kernel, user) };
        (read != 0)
            .then(|| (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
    }

    /// The creation time of the process `pid` now.
    pub(super) fn start_of(pid: u32) -> Option<u64> {
        // SAFETY: opens a handle the OwnedHandle takes; a null one is the failure.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return None;
        }
        // SAFETY: a fresh process handle this call owns.
        let process = unsafe { OwnedHandle::from_raw_handle(process) };
        created(process.as_raw_handle())
    }
}

/// A phone's kernel, which pins no process behind a socket: only this process, which runs while
/// it asks.
#[cfg(target_os = "android")]
mod imp {
    use std::io;

    #[derive(Debug)]
    pub(super) struct Pin {
        pub(super) pid: u32,
    }

    impl Pin {
        pub(super) fn of(socket: std::os::fd::BorrowedFd<'_>) -> io::Result<Self> {
            let peer =
                nix::sys::socket::getsockopt(&socket, nix::sys::socket::sockopt::PeerCredentials)?;
            let pid = u32::try_from(peer.pid())
                .map_err(|_| io::Error::other("the system named no process for the peer"))?;
            if pid == std::process::id() {
                Ok(Self { pid })
            } else {
                Err(io::Error::other(
                    "this kernel pins no process behind a socket but this one",
                ))
            }
        }

        #[expect(
            clippy::unused_self,
            reason = "this process runs while it asks; the other platforms read their pin"
        )]
        pub(super) const fn alive(&self) -> bool {
            true
        }

        pub(super) fn is_started(&self, _: u64) -> bool {
            self.alive()
        }

        #[cfg(feature = "tokio")]
        pub(super) async fn exited(&self) {
            std::future::pending::<()>().await;
        }
    }

    /// No start of a process is read here: a caller's pid and start never match.
    pub(super) const fn start_of(_: u32) -> Option<u64> {
        None
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
#[path = "peer_tests.rs"]
mod tests;
