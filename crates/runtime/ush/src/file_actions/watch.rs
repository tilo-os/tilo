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

/// What the thread watches the record's directory for. Writes, not reads:
/// carrying out reads the record, which must not wake this thread again.
/// Reading it still opens the record's lock files for writing, so of these
/// only a write to the document itself counts ([`names_document`]).
pub(super) const WRITES: u32 =
    libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE | libc::IN_MODIFY;

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
    // SAFETY: a valid descriptor and a NUL-terminated path.
    if unsafe { libc::inotify_add_watch(inotify.as_raw_fd(), path.as_ptr(), WRITES) } < 0 {
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
        // Drain every event first; any write to the document is one reason
        // to look.
        let mut written = false;
        loop {
            // SAFETY: reads into a local buffer of the length given.
            let read = unsafe {
                libc::read(
                    inotify.as_raw_fd(),
                    events.as_mut_ptr().cast(),
                    events.len(),
                )
            };
            let Ok(read) = usize::try_from(read) else {
                break;
            };
            if read == 0 {
                break;
            }
            written |= names_document(events.get(..read).unwrap_or_default());
        }
        if written {
            carry_out();
        }
    }
}

/// Whether a batch of inotify events on the record's directory includes a
/// write to its document, or lost events (a full queue) that may have.
pub(super) fn names_document(mut bytes: &[u8]) -> bool {
    const HEADER: usize = std::mem::size_of::<libc::inotify_event>();
    let document = app_state::DOCUMENT_FILE.as_bytes();
    while bytes.len() >= HEADER {
        // SAFETY: at least HEADER bytes remain; read_unaligned copes with the
        // buffer's alignment.
        let event: libc::inotify_event = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast()) };
        if event.mask & libc::IN_Q_OVERFLOW != 0 {
            return true;
        }
        let length = usize::try_from(event.len).unwrap_or(usize::MAX);
        let Some(name) = bytes.get(HEADER..HEADER.saturating_add(length)) else {
            break;
        };
        if name.split(|byte| *byte == 0).next() == Some(document) {
            return true;
        }
        bytes = bytes
            .get(HEADER.saturating_add(length)..)
            .unwrap_or_default();
    }
    false
}
