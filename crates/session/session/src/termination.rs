//! The signals that end a session: SIGTERM, SIGINT and SIGHUP end its loop, so the session
//! returns and drops what it holds as it does when it ends on its own: the files people choose
//! for apps are unmounted, its sockets removed, its apps ended. A process killed by the signal's
//! default action unwinds nothing, and left all of that behind.
//!
//! The handler only stores a flag and writes a byte to a pipe, both async-signal-safe; a thread
//! of its own reads the pipe and wakes the loop, which reads the flag ([`asked`]) on its pass.
//! A second signal while the session is ending ends the process at once, as that signal does.

use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use tilo_service::Wake;

/// The signals that end the session.
const ENDING: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

/// Whether one of [`ENDING`] arrived.
static ASKED: AtomicBool = AtomicBool::new(false);
/// The pipe's write end the handler wakes the reading thread through; `-1` until installed.
static WAKE_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(signal: libc::c_int) {
    if ASKED.swap(true, Ordering::SeqCst) {
        // The second: the person means it. The default action ends the process.
        // SAFETY: signal(2) and raise(3) are async-signal-safe.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
        return;
    }
    let fd = WAKE_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte = 1_u8;
        // SAFETY: write(2) is async-signal-safe; the descriptor stays open for the process's
        // life once installed, and the byte outlives the call.
        unsafe {
            libc::write(fd, (&raw const byte).cast(), 1);
        }
    }
}

/// Whether the session was asked to end by a signal.
#[must_use]
pub fn asked() -> bool {
    ASKED.load(Ordering::SeqCst)
}

/// End the loop on SIGTERM, SIGINT and SIGHUP from now on, waking it through `wake`.
///
/// # Errors
/// No pipe could be made, the waking thread could not start, or a handler could not be set.
pub fn install(wake: Wake) -> io::Result<()> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: pipe2 writes two fresh descriptors into the two-element array it is given.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful pipe2 above transfers sole ownership of both descriptors.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    std::thread::Builder::new()
        .name("tilo-session-ending".into())
        .spawn(move || {
            let mut byte = [0_u8; 1];
            loop {
                // SAFETY: reads at most one byte into a buffer of one, from a descriptor this
                // thread owns.
                let read = unsafe { libc::read(read.as_raw_fd(), byte.as_mut_ptr().cast(), 1) };
                if read > 0 {
                    wake();
                } else if read == 0
                    || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
                {
                    return;
                }
            }
        })?;
    // The write end lives for the process: the handler may run at any time from here on.
    WAKE_FD.store(write.as_raw_fd(), Ordering::SeqCst);
    std::mem::forget(write);
    for signal in ENDING {
        // SAFETY: a zeroed sigaction is a valid starting point; the handler is an
        // `extern "C" fn(c_int)` that does only async-signal-safe work.
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
    Ok(())
}
