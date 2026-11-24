//! Carrying out what the environment's record queued for this device
//! elsewhere: an undo from History, an agent's `move`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::broker::{BrokerState, Shared, poll_fds, pollfd};

/// How often the record is asked whether it changed: how long a file action asked for on
/// another device or in Terminal's History waits at most before this session carries it out.
const ASK_EVERY: Duration = Duration::from_secs(1);

/// Start the thread that carries out the environment's queued file actions
/// for the session's broker, until the broker stops.
pub(crate) fn spawn(
    state: Arc<Mutex<BrokerState>>,
    shared: Arc<Shared>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("tilo-ush-files".into())
        .spawn(move || queued(&state, &shared))
}

/// Carry out the environment's file actions queued for this device, once
/// when the session starts and again whenever the record changed, until the
/// broker stops.
///
/// The workspace answers whether the record moved past the heads last seen
/// without sending it, so asking costs a round trip to the session and no
/// more while nothing changed.
fn queued(state: &Arc<Mutex<BrokerState>>, shared: &Shared) {
    let carry_out = |state: &Arc<Mutex<BrokerState>>| {
        if let Ok(state) = state.lock() {
            super::carry_out_queued(&state);
        }
    };
    let Some((records, key)) = state.lock().ok().map(|state| {
        (
            state.environment.records.clone(),
            state.environment.key.clone(),
        )
    }) else {
        return;
    };
    let mut seen: Vec<[u8; 32]> = Vec::new();
    let mut fds = [pollfd(
        std::os::fd::AsRawFd::as_raw_fd(&shared.stopping.fd()),
        libc::POLLIN,
    )];
    while !shared.stopped() {
        match records.changed_since(&key, &seen) {
            Ok(Some(now)) => {
                seen = now;
                carry_out(state);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(%error, "ush: the record could not be asked whether it changed");
            }
        }
        if poll_fds(&mut fds, Some(ASK_EVERY)).is_err() {
            return;
        }
    }
}
