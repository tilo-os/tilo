//! `org.kde.StatusNotifierWatcher`: where applications announce their
//! tray items. The shell is both the watcher and the only host.

use std::sync::Arc;

use zbus::message::Header;
use zbus::object_server::SignalEmitter;

use super::{Tray, WATCHER_BUS_NAME, WATCHER_PATH, item};

pub struct Watcher {
    tray: Arc<Tray>,
}

impl Watcher {
    pub fn new(tray: Arc<Tray>) -> Self {
        Self { tray }
    }
}

#[allow(
    clippy::unused_self,
    clippy::unused_async,
    reason = "the D-Bus interface fixes these signatures: properties are methods on the served object"
)]
#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    /// An application announces an item: by bus name, or by object path
    /// on its own connection. Answered at once — a client may be blocked
    /// on this reply and unable to answer the reads that follow — and the
    /// item is read in the background.
    async fn register_status_notifier_item(
        &self,
        service: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        let sender = header.sender().map(ToString::to_string);
        let (destination, path) = item::locate(service, sender.as_deref());
        let tray = Arc::clone(&self.tray);
        let connection = connection.clone();
        let emitter = emitter.into_owned();
        drop(tokio::spawn(async move {
            match item::track(tray, connection, destination, path).await {
                Ok(key) => {
                    let _ = Self::status_notifier_item_registered(&emitter, &key).await;
                }
                Err(error) => tracing::debug!(
                    target: "shell::tray",
                    %error,
                    "a registered tray item could not be read"
                ),
            }
        }));
    }

    /// Hosts other than the shell are accepted and ignored; there is one
    /// bar.
    async fn register_status_notifier_host(
        &self,
        service: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        // Any host may register; the shell's bar is the one that draws.
        let _ = service;
        let _ = Self::status_notifier_host_registered(&emitter).await;
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.tray.keys()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    pub async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Keeps the watcher served while held.
pub struct WatcherHandle {
    connection: zbus::Connection,
}

impl WatcherHandle {
    pub fn connection(&self) -> &zbus::Connection {
        &self.connection
    }
}

/// Build the connection with the watcher for `tray` served on it and, on a
/// message bus, claim its well-known name. Requests to items are sent from
/// the calling runtime.
///
/// The watcher is served while the connection is built, so it answers the
/// first message that arrives. Added to a running connection, it would only
/// start listening in a task of its own, and a registration that raced it
/// would get no reply.
///
/// # Errors
///
/// When the connection cannot be built, or another watcher owns the name.
pub async fn serve(
    builder: zbus::connection::Builder<'_>,
    tray: Arc<Tray>,
) -> Result<WatcherHandle, zbus::Error> {
    tray.attach_runtime(tokio::runtime::Handle::current());
    let connection = builder
        .serve_at(WATCHER_PATH, Watcher::new(tray))?
        .build()
        .await?;
    if connection.is_bus() {
        connection.request_name(WATCHER_BUS_NAME).await?;
    }
    Ok(WatcherHandle { connection })
}
