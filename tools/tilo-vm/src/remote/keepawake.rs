//! Renewing a device's sleep lease while work on this machine runs for it. No worker outlives
//! the work it accompanies.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use crate::error::CliError;

pub(super) fn during<T>(
    interval: Duration,
    mut renew: impl FnMut() -> Result<(), CliError> + Send,
    work: impl FnOnce() -> T,
) -> Result<(T, Result<(), CliError>), CliError> {
    std::thread::scope(|scope| {
        let (stop, stopped) = mpsc::sync_channel::<()>(1);
        let worker = std::thread::Builder::new()
            .name("tilo-vm-awake".into())
            .spawn_scoped(scope, move || {
                loop {
                    match stopped.recv_timeout(interval) {
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
                        Err(RecvTimeoutError::Timeout) => renew()?,
                    }
                }
            })
            .map_err(CliError::io("start the sleep-lease worker"))?;
        let result = work();
        drop(stop);
        let renewal = worker.join().map_err(|_| {
            CliError::io("join the sleep-lease worker")(std::io::Error::other(
                "the renewal worker panicked",
            ))
        })?;
        Ok((result, renewal))
    })
}
