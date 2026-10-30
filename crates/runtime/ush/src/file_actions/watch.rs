//! Carrying out what the environment's record queued for this device
//! elsewhere: an undo from History, an agent's `move`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::broker::{BrokerState, Shared, poll_fds, pollfd};

/// Start the thread that carries out the environment's queued file actions
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
        .spawn(move || queued(&state, &shared))
}

/// Carry out the environment's file actions queued for this device, once
/// when the session starts and again whenever the record's document is
/// written, until the broker stops.
///
/// The watch reports writes to the document itself, not to the lock files
/// carrying out a batch opens: those would wake it forever.
fn queued(state: &Arc<Mutex<BrokerState>>, shared: &Shared) {
    let carry_out = |state: &Arc<Mutex<BrokerState>>| {
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
    let Some(dir) = dir else {
        carry_out(state);
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        carry_out(state);
        return;
    }
    let watched = {
        let state = Arc::clone(state);
        tilo_files::native::watch_changes(&dir, is_document, move || carry_out(&state))
    };
    let Ok(_watch) = watched else {
        tracing::warn!(
            "no watch: file actions asked for elsewhere wait for this session's next command"
        );
        carry_out(state);
        return;
    };
    carry_out(state);
    let mut fds = [pollfd(
        std::os::fd::AsRawFd::as_raw_fd(&shared.stopping.fd()),
        libc::POLLIN,
    )];
    while !shared.stopped() {
        if poll_fds(&mut fds, None).is_err() {
            return;
        }
    }
}

/// Whether a changed entry is the record's document.
pub(super) fn is_document(path: &Path) -> bool {
    path.file_name() == Some(std::ffi::OsStr::new(app_state::DOCUMENT_FILE))
}
