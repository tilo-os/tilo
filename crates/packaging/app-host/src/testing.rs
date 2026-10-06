//! Explicit connected-service fixtures, with an owned and joined RPC driver.

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::thread::JoinHandle;

use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

use crate::app_host_capnp::app_host;

/// A test's server transport. Dropping it stops and joins only its own driver.
pub struct ConnectedService {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    driver: Option<JoinHandle<Result<(), String>>>,
}

impl ConnectedService {
    /// Supply an explicit service dependency without mutating process environment.
    ///
    /// # Errors
    /// Returns why the socket pair, runtime or server thread could not start.
    pub fn start<S>(server: S) -> Result<(Self, OwnedFd), String>
    where
        S: app_host::Server + Send + 'static,
    {
        let (client, connection) = UnixStream::pair().map_err(|error| error.to_string())?;
        connection
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let driver = std::thread::Builder::new()
            .name("app-host-test-service".into())
            .spawn(move || serve(server, connection, stopped))
            .map_err(|error| error.to_string())?;
        Ok((
            Self {
                shutdown: Some(shutdown),
                driver: Some(driver),
            },
            client.into(),
        ))
    }

    /// Stop the fixture and assert its driver result through the caller's test.
    ///
    /// # Errors
    /// Returns a runtime failure or a panicked server driver.
    pub fn shutdown(mut self) -> Result<(), String> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), String> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.driver.take().map_or(Ok(()), |driver| {
            driver
                .join()
                .map_err(|_| "app-host test service panicked".to_owned())?
        })
    }
}

impl Drop for ConnectedService {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn serve<S>(
    server: S,
    connection: UnixStream,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), String>
where
    S: app_host::Server + 'static,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    tokio::task::LocalSet::new().block_on(&runtime, async move {
        let connection =
            tokio::net::UnixStream::from_std(connection).map_err(|error| error.to_string())?;
        let (read, write) = tokio::io::split(connection);
        let network = Box::new(twoparty::VatNetwork::new(
            read.compat(),
            write.compat_write(),
            rpc_twoparty_capnp::Side::Server,
            capnp::message::ReaderOptions::default(),
        ));
        let service: app_host::Client = capnp_rpc::new_client(server);
        let rpc = RpcSystem::new(network, Some(service.client));
        tokio::select! {
            _ = &mut shutdown => Ok(()),
            result = rpc => match result {
                Ok(()) => Ok(()),
                Err(error) if error.kind == capnp::ErrorKind::Disconnected => Ok(()),
                Err(error) => Err(error.to_string()),
            },
        }
    })
}
