//! The signals that stop the host: SIGTERM, SIGINT and SIGHUP end its loop as the session closing
//! its door does, so the instances it runs are dropped and every index they keep is closed with
//! them. A process killed by a signal's default action unwinds nothing: each index file is left as
//! its last commit wrote it, and every later start has to build it anew from the record.
//!
//! The handler only stores a flag and writes a byte to a pipe, both async-signal-safe, and the
//! loop waits on the pipe beside its door and its wake ([`crate::wait`]). A second SIGINT while
//! the host is ending ends the process at once, as that signal does.

use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// The signals that stop the host.
const STOPPING: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

/// Whether one of [`STOPPING`] arrived.
static ASKED: AtomicBool = AtomicBool::new(false);
/// The pipe's write end the handler rings the loop through; `-1` until installed.
static RING_FD: AtomicI32 = AtomicI32::new(-1);

/// Write the byte the loop's wait ends on, if the pipe is installed: async-signal-safe, which is
/// all a handler may do.
extern "C" fn on_signal(signal: libc::c_int) {
    if ASKED.swap(true, Ordering::SeqCst) && signal == libc::SIGINT {
        // A second SIGINT: the person means it. The default action ends the process.
        // SAFETY: signal(2) and raise(3) are async-signal-safe.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
        return;
    }
    let fd = RING_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte = 1_u8;
        // SAFETY: write(2) is async-signal-safe; the descriptor stays open for the process's
        // life once installed, and the byte outlives the call.
        unsafe {
            libc::write(fd, (&raw const byte).cast(), 1);
        }
    }
}

/// The stop the host's loop waits on, and the pipe its handler rings it through.
pub struct Stop {
    read: OwnedFd,
    /// Held open for the process's life: the handler writes it, and a stop after it closed would
    /// never end the wait.
    #[allow(dead_code, reason = "held open, never read: the handler writes it")]
    write: OwnedFd,
}

impl Stop {
    /// Take SIGTERM, SIGINT and SIGHUP as the end of the host from now on.
    ///
    /// # Errors
    /// The pipe could not be made, or a handler could not be set.
    pub fn install() -> io::Result<Self> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: pipe2 writes two fresh descriptors into the two-element array it is given.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful pipe2 above transfers sole ownership of both descriptors.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        RING_FD.store(write.as_raw_fd(), Ordering::SeqCst);
        for signal in STOPPING {
            // SAFETY: a zeroed sigaction is a valid starting point; the handler does only
            // async-signal-safe work.
            let set = unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&raw mut action.sa_mask);
                libc::sigaction(signal, &raw const action, std::ptr::null_mut())
            };
            if set != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self { read, write })
    }

    /// The end to wait on: readable once a stop arrived.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.read.as_fd()
    }

    /// Whether the host was asked to stop.
    pub fn asked(&self) -> bool {
        ASKED.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stop signal ends the loop's wait, and a second SIGTERM is the same stop arriving twice:
    /// the host closes what it keeps rather than being killed with it.
    #[test]
    fn a_stop_signal_rings_the_wait_and_asks_the_host_to_end() {
        let stop = Stop::install().unwrap();
        for _ in 0..2 {
            // SAFETY: raise(2) is async-signal-safe, and the handler installed above takes it.
            assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
        }
        let mut watched = libc::pollfd {
            fd: stop.fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live pollfd naming an open descriptor.
        assert_eq!(unsafe { libc::poll(&raw mut watched, 1, 5_000) }, 1);
        assert!(stop.asked());
    }
}
