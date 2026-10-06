//! Scoped renewal of the authenticated connection's existing sleep lease.
//! No explicit hold is set and no worker outlives the build it accompanies.

use std::io::BufReader;
use std::process::{Child, ChildStdin, ChildStdout};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use tilo_vm_protocol::{Client, HelloResult};

use crate::error::CliError;

#[cfg(test)]
mod tests;

pub(super) fn during<T>(
    interval: Duration,
    mut renew: impl FnMut() -> Result<(), CliError> + Send,
    build: impl FnOnce() -> T,
) -> Result<(T, Result<(), CliError>), CliError> {
    std::thread::scope(|scope| {
        let (stop, stopped) = mpsc::sync_channel::<()>(1);
        let worker = std::thread::Builder::new()
            .name("tilo-vm-build-awake".into())
            .spawn_scoped(scope, move || {
                loop {
                    match stopped.recv_timeout(interval) {
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
                        Err(RecvTimeoutError::Timeout) => renew()?,
                    }
                }
            })
            .map_err(CliError::io("start the build sleep-lease worker"))?;
        let result = build();
        drop(stop);
        let renewal = worker.join().map_err(|_| {
            CliError::io("join the build sleep-lease worker")(std::io::Error::other(
                "the renewal worker panicked",
            ))
        })?;
        Ok((result, renewal))
    })
}

pub(super) fn hello(
    client: &mut Client<BufReader<ChildStdout>, ChildStdin>,
    child: &mut Child,
    timeout: Duration,
) -> Result<HelloResult, CliError> {
    std::thread::scope(|scope| {
        let (send, receive) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("tilo-vm-build-hello".into())
            .spawn_scoped(scope, move || {
                let _ = send.send(client.hello(env!("CARGO_PKG_VERSION")));
            })
            .map_err(CliError::io("start the sleep-lease handshake"))?;
        let response = receive.recv_timeout(timeout);
        if response.is_err() {
            // This CLI owns the transport process. Closing it releases the
            // blocked read before the scoped worker is joined.
            let _ = child.kill();
            let _ = child.wait();
        }
        worker.join().map_err(|_| {
            CliError::io("join the sleep-lease handshake")(std::io::Error::other(
                "the handshake worker panicked",
            ))
        })?;
        response
            .map_err(|error| {
                let kind = match error {
                    RecvTimeoutError::Timeout => std::io::ErrorKind::TimedOut,
                    RecvTimeoutError::Disconnected => std::io::ErrorKind::UnexpectedEof,
                };
                CliError::io("renew the device sleep lease")(std::io::Error::new(
                    kind,
                    "the daemon did not complete its handshake",
                ))
            })?
            .map_err(CliError::from)
    })
}
