//! The signals that stop the host: SIGTERM, SIGINT and SIGHUP end its loop as the session closing
//! its api does, so the instances it runs are dropped and every index they keep is closed with
//! them. A process killed by a signal's default action unwinds nothing: each index file is left as
//! its last commit wrote it, and every later start has to build it anew from the document.
//!
//! The handler only stores a flag and writes a byte to a pipe, both async-signal-safe, and the
//! loop waits on the pipe beside its api and its wake ([`crate::wait`]). A second SIGINT while
//! the host is ending ends the process at once, as that signal does.
//!
//! On Windows the console's control events stand for the signals: Ctrl+C, Ctrl+Break, the console
//! closing, the person logging off and the system shutting down. The handler, which runs on a
//! thread of its own, stores the flag and sets an event the loop's wait watches.

use std::io;
#[cfg(unix)]
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd};
#[cfg(unix)]
use std::sync::atomic::AtomicI32;
use std::sync::atomic::{AtomicBool, Ordering};

/// The signals that stop the host.
#[cfg(unix)]
const STOPPING: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

/// Whether a stop arrived.
static ASKED: AtomicBool = AtomicBool::new(false);
/// The pipe's write end the handler rings the loop through; `-1` until installed.
#[cfg(unix)]
static RING_FD: AtomicI32 = AtomicI32::new(-1);

/// Write the byte the loop's wait ends on, if the pipe is installed: async-signal-safe, which is
/// all a handler may do.
#[cfg(unix)]
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
#[cfg(unix)]
pub struct Stop {
    read: OwnedFd,
    /// Held open for the process's life: the handler writes it, and a stop after it closed would
    /// never end the wait.
    #[allow(dead_code, reason = "held open, never read: the handler writes it")]
    write: OwnedFd,
}

#[cfg(unix)]
impl Stop {
    /// Take SIGTERM, SIGINT and SIGHUP as the end of the host from now on.
    ///
    /// # Errors
    /// The pipe could not be made, or a handler could not be set.
    pub fn install() -> io::Result<Self> {
        let (read, write) = io::pipe()?;
        let (read, write) = (OwnedFd::from(read), OwnedFd::from(write));
        // Neither end ever waits: the handler's write into a full pipe is a stop already rung.
        for end in [&read, &write] {
            let fd = end.as_raw_fd();
            // SAFETY: fcntl with integer arguments on a descriptor this function owns.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            // SAFETY: as above.
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
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

/// The event the console's handler sets; null until installed.
#[cfg(windows)]
static RING: std::sync::atomic::AtomicPtr<std::ffi::c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Take a control event as the end of the host: set the flag and the event the loop's wait
/// watches. A second Ctrl+C while the host is ending is left to the default handler, which ends
/// the process, as a second SIGINT does.
#[cfg(windows)]
unsafe extern "system" fn on_control(event: u32) -> windows_sys::core::BOOL {
    use windows_sys::Win32::System::Console::CTRL_C_EVENT;
    if ASKED.swap(true, Ordering::SeqCst) && event == CTRL_C_EVENT {
        return 0;
    }
    let ring = RING.load(Ordering::SeqCst);
    if !ring.is_null() {
        // SAFETY: the event `Stop::install` made, held open for the process's life.
        unsafe { windows_sys::Win32::System::Threading::SetEvent(ring) };
    }
    1
}

/// The stop the host's loop waits on: an event the console's handler sets.
#[cfg(windows)]
pub struct Stop {
    event: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
impl Stop {
    /// Take the console's control events as the end of the host from now on.
    ///
    /// # Errors
    /// The event could not be made, or the handler could not be set.
    pub fn install() -> io::Result<Self> {
        use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
        // SAFETY: no attributes and no name; manual reset, unset.
        let event = unsafe {
            windows_sys::Win32::System::Threading::CreateEventW(
                std::ptr::null(),
                1,
                0,
                std::ptr::null(),
            )
        };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: an event CreateEventW just returned, owned from here and never closed while
        // the handler may set it: a stop lives for the process's life.
        let event = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(event) };
        RING.store(event.as_raw_handle(), Ordering::SeqCst);
        // SAFETY: a handler that only stores a flag and sets an event.
        if unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(on_control), 1)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { event })
    }

    /// The event to wait on: set once a stop arrived.
    pub fn handle(&self) -> std::os::windows::io::BorrowedHandle<'_> {
        use std::os::windows::io::AsHandle as _;
        self.event.as_handle()
    }

    /// Whether the host was asked to stop.
    pub fn asked(&self) -> bool {
        ASKED.load(Ordering::SeqCst)
    }
}

#[cfg(all(test, unix))]
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
