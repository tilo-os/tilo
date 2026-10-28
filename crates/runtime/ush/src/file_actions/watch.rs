//! Carrying out what the environment's record queued for this device
//! elsewhere: an undo from History, an agent's `move`.

use std::os::fd::{AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};
use std::sync::{Arc, Mutex};

use crate::broker::{BrokerState, Shared, poll_fds, pollfd};

/// Start the thread that carries the environment's queued file actions out
/// for the session's broker, until the broker stops.
///
/// # Errors
///
/// The thread could not be started.
pub(crate) fn spawn(
    state: Arc<Mutex<BrokerState>>,
    shared: Arc<Shared>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("tilo-ush-files".into())
        .spawn(move || queued(&state, shared.stopping.fd(), &|| shared.stopped()))
}

/// Carry out the environment's file actions queued for this device, once
/// when the session starts and again whenever the record's directory is
/// written, until the broker stops. Nothing polls: the thread sleeps on
/// inotify and on the broker's stop.
pub fn queued(state: &Mutex<BrokerState>, stopping: BorrowedFd<'_>, stopped: &dyn Fn() -> bool) {
    let carry_out = || {
        if let Ok(state) = state.lock() {
            super::carry_out_queued(&state);
        }
    };
    let dir = state.lock().ok().and_then(|state| {
        state
            .environment
            .records
            .record_dir(&state.environment.key)
            .ok()
    });
    carry_out();
    let Some(dir) = dir else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    // SAFETY: inotify_init1 takes flags and returns a new descriptor or -1.
    let raw = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
    if raw < 0 {
        tracing::warn!(
            "no inotify: file actions asked for elsewhere wait for this session's next command"
        );
        return;
    }
    // SAFETY: the kernel returned a fresh descriptor this call now owns.
    let inotify = unsafe { OwnedFd::from_raw_fd(raw) };
    let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()) else {
        return;
    };
    // Writes, not reads: carrying out reads the record, which must not wake
    // this thread again.
    let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE | libc::IN_MODIFY;
    // SAFETY: a valid descriptor and a NUL-terminated path.
    if unsafe { libc::inotify_add_watch(inotify.as_raw_fd(), path.as_ptr(), mask) } < 0 {
        return;
    }
    let mut events = [0u8; 4096];
    while !stopped() {
        let mut fds = [
            pollfd(inotify.as_raw_fd(), libc::POLLIN),
            pollfd(stopping.as_raw_fd(), libc::POLLIN),
        ];
        if poll_fds(&mut fds, None).is_err() {
            return;
        }
        let [changed, woken] = fds;
        if woken.revents != 0 || stopped() {
            return;
        }
        if changed.revents == 0 {
            continue;
        }
        // Every event so far is one reason to look: drain them all first.
        loop {
            // SAFETY: reads into a local buffer of the length given.
            let read = unsafe {
                libc::read(
                    inotify.as_raw_fd(),
                    events.as_mut_ptr().cast(),
                    events.len(),
                )
            };
            if read <= 0 {
                break;
            }
        }
        carry_out();
    }
}
