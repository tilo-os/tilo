//! Waiting on a child process without polling it.
//!
//! An [`ExitWatch`] sleeps until the child exits or its [`Bell`] rings, so a
//! supervisor reacts to other events without looking at the child on a timer.
//! On Linux it waits on a pidfd, which becomes readable when the child exits
//! without reaping it; where no such handle exists, [`ExitWatch::wait`] returns
//! after at most 100 ms.

use std::process::Child;
use std::time::Duration;

const FALLBACK_PAUSE: Duration = Duration::from_millis(100);

/// Sleeps until a child exits, its [`Bell`] rings or a deadline passes.
pub struct ExitWatch(backend::Watch);

#[derive(Clone)]
pub struct Bell(backend::Bell);

impl ExitWatch {
    /// Watches `child`, and returns the bell that wakes [`Self::wait`]. `child` must
    /// not have been waited for yet: once reaped, its process id can name another
    /// process.
    #[must_use]
    pub fn new(child: &Child) -> (Self, Bell) {
        let (watch, bell) = backend::watch(child);
        (Self(watch), Bell(bell))
    }

    /// Sleeps until the child exits, the bell rings or `deadline` (from now) passes;
    /// `None` waits without a deadline. It never reaps the child.
    pub fn wait(&self, deadline: Option<Duration>) {
        self.0.wait(deadline);
    }

    /// Whether the system lets this watch sleep until the child exits, rather
    /// than returning every 100 ms.
    #[must_use]
    pub fn watches_the_process(&self) -> bool {
        self.0.watches_the_process()
    }
}

impl Bell {
    pub fn ring(&self) {
        self.0.ring();
    }
}

fn fallback_pause(deadline: Option<Duration>) -> Duration {
    deadline.map_or(FALLBACK_PAUSE, |left| left.min(FALLBACK_PAUSE))
}

#[cfg(target_os = "linux")]
mod backend {
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::process::Child;
    use std::sync::Arc;
    use std::time::Duration;

    pub struct Watch {
        fds: Option<(OwnedFd, UnixStream)>,
    }

    #[derive(Clone)]
    pub struct Bell(Option<Arc<UnixStream>>);

    fn pidfd(child: &Child) -> Option<OwnedFd> {
        let pid = libc::pid_t::try_from(child.id()).ok()?;
        // SAFETY: pidfd_open takes a pid and flags and returns a new
        // descriptor or -1. The caller has not reaped the child, so the pid
        // still names it.
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        let fd = i32::try_from(raw).ok().filter(|fd| *fd >= 0)?;
        // SAFETY: pidfd_open returned a new descriptor nothing else owns.
        Some(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn bell() -> std::io::Result<(UnixStream, UnixStream)> {
        let (ring, listen) = UnixStream::pair()?;
        ring.set_nonblocking(true)?;
        listen.set_nonblocking(true)?;
        Ok((ring, listen))
    }

    pub fn watch(child: &Child) -> (Watch, Bell) {
        match (pidfd(child), bell()) {
            (Some(pidfd), Ok((ring, listen))) => (
                Watch {
                    fds: Some((pidfd, listen)),
                },
                Bell(Some(Arc::new(ring))),
            ),
            _ => (Watch { fds: None }, Bell(None)),
        }
    }

    impl Watch {
        pub fn wait(&self, deadline: Option<Duration>) {
            let Some((pidfd, listen)) = &self.fds else {
                std::thread::sleep(super::fallback_pause(deadline));
                return;
            };
            let mut fds = [
                libc::pollfd {
                    fd: pidfd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: listen.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let timeout = deadline.map_or(-1, |left| {
                i32::try_from(left.as_millis().saturating_add(1)).unwrap_or(i32::MAX)
            });
            // SAFETY: two initialised pollfds over descriptors this watch
            // owns; poll writes only their `revents`.
            let _ = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
            // Empty the bell: the caller looks at everything after a wait.
            let mut drain = [0u8; 64];
            while matches!((&*listen).read(&mut drain), Ok(n) if n > 0) {}
        }

        pub const fn watches_the_process(&self) -> bool {
            self.fds.is_some()
        }
    }

    impl Bell {
        pub fn ring(&self) {
            if let Some(ring) = &self.0 {
                let _ = (&**ring).write(&[1]);
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod backend {
    use std::process::Child;
    use std::time::Duration;

    pub struct Watch;

    #[derive(Clone)]
    pub struct Bell;

    pub const fn watch(_child: &Child) -> (Watch, Bell) {
        (Watch, Bell)
    }

    impl Watch {
        #[allow(
            clippy::unused_self,
            reason = "the signature is shared with the Linux backend, which waits on its descriptors"
        )]
        pub fn wait(&self, deadline: Option<Duration>) {
            std::thread::sleep(super::fallback_pause(deadline));
        }

        #[allow(
            clippy::unused_self,
            reason = "the signature is shared with the Linux backend, which has a process to watch"
        )]
        pub const fn watches_the_process(&self) -> bool {
            false
        }
    }

    impl Bell {
        #[allow(
            clippy::unused_self,
            reason = "the signature is shared with the Linux backend, whose bell wakes a watch"
        )]
        pub const fn ring(&self) {}
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::Instant;
    use tilo_test_env::SpawnUnderEnvLock as _;

    #[allow(clippy::unwrap_used, reason = "a test helper fails loudly")]
    fn sleeper(seconds: &str) -> Child {
        Command::new("sleep")
            .arg(seconds)
            .stdin(Stdio::null())
            .spawn_locked()
            .unwrap()
    }

    #[test]
    fn a_running_child_keeps_the_watch_asleep_until_the_deadline() {
        let mut child = sleeper("5");
        let (watch, _bell) = ExitWatch::new(&child);
        assert_eq!(watch.watches_the_process(), cfg!(target_os = "linux"));
        if watch.watches_the_process() {
            let began = Instant::now();
            watch.wait(Some(Duration::from_millis(400)));
            assert!(began.elapsed() >= Duration::from_millis(400));
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Block on `watch` on its own thread, and fail as hung if nothing wakes
    /// it within the shared hang bound.
    fn woken(watch: ExitWatch, what: &str) {
        let (woke, woken) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            watch.wait(None);
            let _ = woke.send(());
        });
        tilo_test_env::recv(what, &woken);
    }

    #[test]
    fn an_exit_or_a_ring_wakes_the_watch_at_once() {
        // The child outlives the test, so only the ring can wake the watch.
        let mut child = sleeper("3600");
        let (watch, bell) = ExitWatch::new(&child);
        if !watch.watches_the_process() {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        bell.ring();
        woken(watch, "the ring to wake the watch");

        let mut quick = sleeper("0");
        let (watch, _bell) = ExitWatch::new(&quick);
        woken(watch, "the exit to wake the watch");
        assert!(quick.wait().unwrap().success());
        let _ = child.kill();
        let _ = child.wait();
    }
}
