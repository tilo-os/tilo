//! Supervise a child without waking up while nothing happens.
//!
//! A supervisor that learns of exit or cancellation by calling
//! `Child::try_wait` on a timer is scheduled on every tick for the child's
//! whole life. On Linux a pidfd becomes readable when its process exits, so a
//! supervisor can sleep in one `poll(2)` on that pidfd and a [`WakeFd`] that
//! any thread (or a signal handler) rings, and wake only for an event.

use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};
use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

/// Why [`wait_child_or`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wakeup {
    /// The child exited and has been reaped.
    Exited(ExitStatus),
    /// The wake descriptor became readable. The caller drains it.
    Woken,
    /// The deadline passed before either.
    Deadline,
}

/// Slice of the portable fallback, for kernels without `pidfd_open` and
/// non-Linux hosts: the supervision loop this module replaces.
const FALLBACK_SLICE: Duration = Duration::from_millis(10);

/// Wait until `child` exits, `wake` becomes readable, or `deadline` passes,
/// without polling. With neither a wake descriptor nor a deadline this blocks
/// until the child exits. A deadline already in the past only probes.
///
/// Linux: one `poll(2)` on a pidfd of `child` (pinned to this unreaped child,
/// so it cannot name a recycled pid) and `wake`. Elsewhere, or when the
/// kernel has no `pidfd_open`, the same contract is kept with a short
/// `try_wait` loop.
///
/// # Errors
/// Returns the error of inspecting or reaping the child, or of `poll(2)`.
pub fn wait_child_or(
    child: &mut Child,
    wake: Option<BorrowedFd<'_>>,
    deadline: Option<Instant>,
) -> io::Result<Wakeup> {
    if let Some(status) = child.try_wait()? {
        return Ok(Wakeup::Exited(status));
    }
    match pidfd(child) {
        Some(pidfd) => wait_pidfd(child, &pidfd, wake, deadline),
        None => wait_fallback(child, wake, deadline),
    }
}

#[cfg(target_os = "linux")]
fn pidfd(child: &Child) -> Option<OwnedFd> {
    let pid = libc::pid_t::try_from(child.id()).ok()?;
    // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor
    // or -1. The child is unreaped, so its pid still names exactly it.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    let fd = i32::try_from(fd).ok().filter(|fd| *fd >= 0)?;
    // SAFETY: the kernel returned a fresh descriptor this call now owns.
    Some(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(not(target_os = "linux"))]
fn pidfd(_: &Child) -> Option<OwnedFd> {
    None
}

fn wait_pidfd(
    child: &mut Child,
    pidfd: &OwnedFd,
    wake: Option<BorrowedFd<'_>>,
    deadline: Option<Instant>,
) -> io::Result<Wakeup> {
    loop {
        let mut fds = [
            pollin(pidfd.as_raw_fd()),
            // poll(2) ignores a negative descriptor.
            pollin(wake.map_or(-1, |fd| fd.as_raw_fd())),
        ];
        poll(&mut fds, remaining(deadline))?;
        let [exited, woken] = fds;
        if exited.revents != 0 {
            return child.wait().map(Wakeup::Exited);
        }
        if woken.revents != 0 {
            return Ok(Wakeup::Woken);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(Wakeup::Deadline);
        }
    }
}

fn wait_fallback(
    child: &mut Child,
    wake: Option<BorrowedFd<'_>>,
    deadline: Option<Instant>,
) -> io::Result<Wakeup> {
    loop {
        let slice = deadline.map_or(FALLBACK_SLICE, |deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .min(FALLBACK_SLICE)
        });
        let mut fds = [pollin(wake.map_or(-1, |fd| fd.as_raw_fd()))];
        poll(&mut fds, Some(slice))?;
        if let Some(status) = child.try_wait()? {
            return Ok(Wakeup::Exited(status));
        }
        if fds.iter().any(|fd| fd.revents != 0) {
            return Ok(Wakeup::Woken);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(Wakeup::Deadline);
        }
    }
}

fn pollin(fd: i32) -> libc::pollfd {
    libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }
}

/// Time left until `deadline`, or `None` to wait without one.
fn remaining(deadline: Option<Instant>) -> Option<Duration> {
    deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()))
}

/// `poll(2)` that retries an interrupted call (the caller recomputes nothing:
/// a signal that matters rings a wake descriptor, which the retry observes).
fn poll(fds: &mut [libc::pollfd], timeout: Option<Duration>) -> io::Result<()> {
    let count = libc::nfds_t::try_from(fds.len()).map_err(io::Error::other)?;
    let started = Instant::now();
    loop {
        let remaining = timeout.map(|timeout| timeout.saturating_sub(started.elapsed()));
        // Round up, so a timed-out poll means the deadline really passed.
        let millis = remaining.map_or(-1, |remaining| {
            let micros = remaining.as_micros();
            let millis = micros.div_ceil(1000);
            i32::try_from(millis).unwrap_or(i32::MAX)
        });
        // SAFETY: `fds` is a valid, exclusively borrowed array of `count`
        // pollfd entries for the duration of the call.
        if unsafe { libc::poll(fds.as_mut_ptr(), count, millis) } >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// A level-triggered wake descriptor: readable from the first [`ring`] until
/// [`drain`]. A supervisor passes [`fd`] to [`wait_child_or`]; any thread
/// rings it after changing what the supervisor must re-check.
///
/// [`ring`]: WakeFd::ring
/// [`drain`]: WakeFd::drain
/// [`fd`]: WakeFd::fd
#[derive(Debug)]
pub struct WakeFd {
    read: OwnedFd,
    /// The pipe's write end; an eventfd reads and writes one descriptor.
    #[cfg(not(target_os = "linux"))]
    write: OwnedFd,
}

impl WakeFd {
    /// # Errors
    /// Returns the error of creating the eventfd (Linux) or pipe.
    #[cfg(target_os = "linux")]
    pub fn new() -> io::Result<Self> {
        // SAFETY: eventfd takes an initial count and flags and returns a new
        // descriptor or -1.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the kernel returned a fresh descriptor this call now owns.
        Ok(Self {
            read: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }

    /// # Errors
    /// Returns the error of creating the eventfd (Linux) or pipe.
    #[cfg(not(target_os = "linux"))]
    pub fn new() -> io::Result<Self> {
        let mut fds = [0; 2];
        // SAFETY: pipe writes two new descriptors into the two-element array.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let [read, write] = fds;
        // SAFETY: pipe returned two fresh descriptors this call now owns.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(read), OwnedFd::from_raw_fd(write)) };
        for fd in [&read, &write] {
            let raw = fd.as_raw_fd();
            // SAFETY: fcntl only changes flags of a descriptor owned above.
            if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } != 0
                || unsafe { libc::fcntl(raw, libc::F_SETFL, libc::O_NONBLOCK) } != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self { read, write })
    }

    /// Make the descriptor readable. Only a `write(2)`: no allocation or lock,
    /// so a signal handler may call it. A full counter or pipe is already
    /// readable, so a failed write loses nothing.
    pub fn ring(&self) {
        let one = 1u64.to_ne_bytes();
        // SAFETY: writes from a valid local buffer to a descriptor owned by
        // this value; the result is deliberately ignored (see above).
        unsafe {
            libc::write(
                self.ring_fd().as_raw_fd(),
                one.as_ptr().cast(),
                ring_len(one.len()),
            )
        };
    }

    /// The descriptor to wait on; readable while rung and not drained.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.read.as_fd()
    }

    /// Consume every ring so far, making the descriptor unreadable again.
    /// Drain before re-checking state, so a ring racing the check is kept.
    pub fn drain(&self) {
        let mut buffer = [0u8; 64];
        // SAFETY: reads into a valid local buffer from a nonblocking
        // descriptor owned by this value; stops at EAGAIN or end.
        while unsafe {
            libc::read(
                self.read.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        } > 0
        {}
    }

    #[cfg(target_os = "linux")]
    fn ring_fd(&self) -> BorrowedFd<'_> {
        self.read.as_fd()
    }

    #[cfg(not(target_os = "linux"))]
    fn ring_fd(&self) -> BorrowedFd<'_> {
        self.write.as_fd()
    }
}

/// An eventfd counts in 8-byte writes; a pipe needs only one byte.
#[cfg(target_os = "linux")]
fn ring_len(len: usize) -> usize {
    len
}

#[cfg(not(target_os = "linux"))]
fn ring_len(_: usize) -> usize {
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn sleeper(seconds: &str) -> Child {
        Command::new("sleep").arg(seconds).spawn().unwrap()
    }

    #[cfg(target_os = "linux")]
    fn context_switches() -> u64 {
        let tid = tilo_test_env::idle::current_thread_id().unwrap();
        tilo_test_env::idle::context_switches(&tid.to_string())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn wait_child_or_returns_on_exit_without_polling() {
        let mut child = sleeper("0.2");
        let before = context_switches();
        let began = Instant::now();
        let woke = wait_child_or(&mut child, None, None).unwrap();
        let switches = context_switches().saturating_sub(before);
        assert!(matches!(woke, Wakeup::Exited(status) if status.success()));
        assert!(began.elapsed() >= Duration::from_millis(150));
        // The 10 ms loop this replaces switched about 20 times here.
        assert!(
            switches <= 3,
            "waiting 200 ms for exit took {switches} context switches"
        );
    }

    #[test]
    fn wait_child_or_returns_on_wake() {
        let mut child = sleeper("3600");
        let wake = std::sync::Arc::new(WakeFd::new().unwrap());
        let ringer = wake.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            ringer.ring();
        });
        let first = wait_child_or(&mut child, Some(wake.fd()), None).unwrap();
        thread.join().unwrap();
        assert_eq!(first, Wakeup::Woken);
        // Level-triggered until drained, then quiet again.
        let soon = Instant::now()
            .checked_add(Duration::from_millis(30))
            .unwrap();
        assert_eq!(
            wait_child_or(&mut child, Some(wake.fd()), Some(soon)).unwrap(),
            Wakeup::Woken
        );
        wake.drain();
        let soon = Instant::now()
            .checked_add(Duration::from_millis(30))
            .unwrap();
        assert_eq!(
            wait_child_or(&mut child, Some(wake.fd()), Some(soon)).unwrap(),
            Wakeup::Deadline
        );
        child.kill().unwrap();
        assert!(matches!(
            wait_child_or(&mut child, Some(wake.fd()), None).unwrap(),
            Wakeup::Exited(_)
        ));
    }

    #[test]
    fn wait_child_or_honours_deadline() {
        let mut child = sleeper("3600");
        let began = Instant::now();
        let deadline = began.checked_add(Duration::from_millis(100)).unwrap();
        assert_eq!(
            wait_child_or(&mut child, None, Some(deadline)).unwrap(),
            Wakeup::Deadline
        );
        assert!(Instant::now() >= deadline);
        // A past deadline only probes a live child.
        assert_eq!(
            wait_child_or(&mut child, None, Some(began)).unwrap(),
            Wakeup::Deadline
        );
        child.kill().unwrap();
        child.wait().unwrap();
        // An already reaped child reports its status without waiting.
        assert!(matches!(
            wait_child_or(&mut child, None, Some(began)).unwrap(),
            Wakeup::Exited(_)
        ));
    }
}
