//! Explicit connected-service fixtures, with an owned and joined RPC driver.

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::thread::JoinHandle;

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
    // Shutdown the owned socket, then keep driving RPC long enough to retire
    // its pending server calls. Dropping RpcSystem alone can retain call cycles.
    let disconnect = connection.try_clone().map_err(|error| error.to_string())?;
    tokio::task::LocalSet::new().block_on(&runtime, async move {
        let connection =
            tokio::net::UnixStream::from_std(connection).map_err(|error| error.to_string())?;
        let service: app_host::Client = capnp_rpc::new_client(server);
        let mut rpc = Box::pin(runtime_portal::serve_connected_io(
            connection,
            service.client,
        ));
        tokio::select! {
            biased;
            result = &mut rpc => driver_result(result),
            _ = &mut shutdown => {
                // EOF takes the ordinary connection-disconnect path, canceling
                // pending server calls before we join. A separate Capnp
                // disconnector can release state ahead of the message loop.
                disconnect.shutdown(std::net::Shutdown::Both)
                    .map_err(|error| error.to_string())?;
                let result = tokio::time::timeout(std::time::Duration::from_secs(2), &mut rpc)
                    .await.map_err(|_| "app-host test transport did not disconnect".to_owned())?;
                driver_result(result)
            },
        }
    })
}

fn driver_result(result: Result<(), capnp::Error>) -> Result<(), String> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind == capnp::ErrorKind::Disconnected => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EmptyHost;

    impl app_host::Server for EmptyHost {
        fn list(
            self: capnp::capability::Rc<Self>,
            _params: app_host::ListParams,
            mut results: app_host::ListResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            results.get().init_entries(0);
            std::future::ready(Ok(()))
        }
    }

    #[test]
    fn connected_service_stops_and_joins_after_peer_disconnect() {
        for iteration in 0..128 {
            let (service, fd) = ConnectedService::start(EmptyHost).unwrap();
            let client = crate::client::BlockingClient::from_connected_fd(fd).unwrap();
            assert!(client.list().unwrap().is_empty());
            drop(client);
            assert_eq!(
                service.shutdown(),
                Ok(()),
                "owned driver iteration {iteration}"
            );
        }
    }

    #[test]
    fn connected_service_stops_and_joins_with_a_live_idle_peer() {
        let (service, fd) = ConnectedService::start(EmptyHost).unwrap();
        let client = crate::client::BlockingClient::from_connected_fd(fd).unwrap();
        assert!(client.list().unwrap().is_empty());
        assert_eq!(service.shutdown(), Ok(()));
        drop(client);
    }

    #[test]
    fn peer_close_races_owned_driver_shutdown() {
        for iteration in 0..512 {
            let (service, fd) = ConnectedService::start(EmptyHost).unwrap();
            let client = crate::client::BlockingClient::from_connected_fd(fd).unwrap();
            assert!(client.list().unwrap().is_empty());
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let stop_barrier = barrier.clone();
            let stop = std::thread::spawn(move || {
                stop_barrier.wait();
                service.shutdown()
            });
            barrier.wait();
            drop(client);
            assert_eq!(
                stop.join().unwrap(),
                Ok(()),
                "concurrent close iteration {iteration}"
            );
        }
    }

    /// Lists one system package the way the service does: its signed
    /// manifest's text and its measured size.
    struct ListingHost;

    const MAIL_MANIFEST: &str = concat!(
        "[package]\nid = \"so.tilo.mail\"\nname = \"Mail\"\ncategory = \"Office\"\n",
        "version = \"0.4.0\"\nkind = \"app\"\n",
        "[package.names]\nde = \"E-Mail\"\n",
        "[publisher]\nidentity = \"ed25519:",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "\"\nsignature = \"\"\n",
        "[sandbox]\nnamespaces = []\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n",
        "[sandbox.net]\nmode = \"off\"\n[entrypoint]\nbin = \"bin/mail\"\n",
    );

    impl app_host::Server for ListingHost {
        fn list(
            self: capnp::capability::Rc<Self>,
            _params: app_host::ListParams,
            mut results: app_host::ListResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let mut entry = results.get().init_entries(1).get(0);
            entry.set_id(&[7; 32]);
            entry.set_payload_root("/usr/share/tilo-apps/payloads/so.tilo.mail");
            entry.set_manifest_path("/usr/share/tilo-apps/payloads/so.tilo.mail/manifest.toml");
            entry.set_manifest_text(MAIL_MANIFEST);
            entry.set_size_bytes(2_400_000);
            entry.set_user_managed(false);
            std::future::ready(Ok(()))
        }
    }

    #[test]
    fn a_listed_package_carries_its_manifest_name_category_version_and_size() {
        let (service, fd) = ConnectedService::start(ListingHost).unwrap();
        let client = crate::client::BlockingClient::from_connected_fd(fd).unwrap();
        let listed = client.list().unwrap();
        let [mail] = listed.as_slice() else {
            panic!("one listed package, got {listed:?}");
        };
        assert_eq!(mail.package.id, "so.tilo.mail");
        assert_eq!(mail.package.version, "0.4.0");
        assert_eq!(mail.package.payload_bytes, 2_400_000);
        assert!(!mail.package.user_managed());
        assert_eq!(mail.about.name, "Mail");
        assert_eq!(mail.about.category.as_deref(), Some("Office"));
        assert_eq!(mail.about.name_for("de-AT"), "E-Mail");
        assert_eq!(mail.about.name_for("fr"), "Mail");
        drop(client);
        assert_eq!(service.shutdown(), Ok(()));
    }

    struct PanickingHost;
    impl app_host::Server for PanickingHost {
        fn list(
            self: capnp::capability::Rc<Self>,
            _params: app_host::ListParams,
            _results: app_host::ListResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            std::future::poll_fn(|_| panic!("injected owned service failure"))
        }
    }

    #[test]
    fn connected_service_shutdown_still_reports_driver_panics() {
        let (service, fd) = ConnectedService::start(PanickingHost).unwrap();
        let client = crate::client::BlockingClient::from_connected_fd(fd).unwrap();
        assert!(client.list().is_err());
        drop(client);
        assert_eq!(
            service.shutdown(),
            Err("app-host test service panicked".into())
        );
    }

    struct PendingCall(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for PendingCall {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    struct PendingHost {
        entered: std::sync::mpsc::SyncSender<()>,
        canceled: std::sync::Arc<std::sync::atomic::AtomicBool>,
        waiter: std::sync::Arc<std::sync::Mutex<Option<std::task::Waker>>>,
    }

    impl app_host::Server for PendingHost {
        fn list(
            self: capnp::capability::Rc<Self>,
            _params: app_host::ListParams,
            _results: app_host::ListResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let pending = PendingCall(std::sync::Arc::clone(&self.canceled));
            let entered = self.entered.clone();
            let waiter = std::sync::Arc::clone(&self.waiter);
            async move {
                let _pending = pending;
                entered.send(()).unwrap();
                std::future::poll_fn(|cx| {
                    *waiter.lock().unwrap() = Some(cx.waker().clone());
                    std::task::Poll::<()>::Pending
                })
                .await;
                Ok(())
            }
        }
    }

    #[test]
    fn connected_service_shutdown_cancels_an_entered_server_call() {
        let (entered, observed) = std::sync::mpsc::sync_channel(1);
        let canceled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let waiter = std::sync::Arc::new(std::sync::Mutex::new(None));
        let (service, fd) = ConnectedService::start(PendingHost {
            entered,
            canceled: std::sync::Arc::clone(&canceled),
            waiter: std::sync::Arc::clone(&waiter),
        })
        .unwrap();
        let client = std::thread::spawn(move || {
            crate::client::BlockingClient::from_connected_fd(fd)
                .unwrap()
                .list()
        });
        observed
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(service.shutdown(), Ok(()));
        // Like the Store fixture, release the saved task waker after joining.
        drop(waiter.lock().unwrap().take());
        assert!(
            client.join().unwrap().is_err(),
            "pending RPC reports disconnect"
        );
        assert!(
            canceled.load(std::sync::atomic::Ordering::SeqCst),
            "the real entered service future is retired by shutdown"
        );
    }
}
