//! The existing session portal owns `NetworkMonitor`, not the shell backend.
//! No system-bus access or subprocess fallback is needed in an app sandbox.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::watch;
use zbus::zvariant::OwnedValue;

use super::{Connectivity, NetStatus, imp};

const BUS: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.NetworkMonitor";
const DEADLINE: Duration = Duration::from_secs(3);
static STATUS: OnceLock<watch::Receiver<NetStatus>> = OnceLock::new();

fn status() -> &'static watch::Receiver<NetStatus> {
    STATUS.get_or_init(|| {
        let (sender, receiver) = watch::channel(imp::fallback());
        // A dedicated current-thread runtime also works for synchronous callers
        // and callers outside Tokio. One subscription serves the whole process.
        let _ = std::thread::Builder::new()
            .name("network-status".into())
            .spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    runtime.block_on(monitor(&sender));
                }
            });
        receiver
    })
}

pub(super) fn current() -> NetStatus {
    let receiver = status();
    if receiver.has_changed().is_err() {
        imp::fallback()
    } else {
        *receiver.borrow()
    }
}

pub(super) struct Watch {
    portal: watch::Receiver<NetStatus>,
}

impl Watch {
    pub(super) fn new() -> Self {
        Self {
            portal: status().clone(),
        }
    }

    pub(super) async fn wait(&mut self) {
        if self.portal.changed().await.is_err() {
            // A failed thread/runtime start leaves the route-only answer usable.
            tokio::time::sleep(imp::POLL).await;
        }
    }
}

fn publish(sender: &watch::Sender<NetStatus>, status: NetStatus) {
    sender.send_if_modified(|last| {
        if *last == status {
            false
        } else {
            *last = status;
            true
        }
    });
}

async fn monitor(sender: &watch::Sender<NetStatus>) {
    let mut host_changes = super::host().subscribe();
    loop {
        // A rich host owns this process's hints. Do not also keep retrying an
        // unavailable portal or reading routes while that transport is alive.
        loop {
            let hosted = host_changes.borrow_and_update().is_some();
            if !hosted {
                break;
            }
            let _ = host_changes.changed().await;
        }
        tokio::select! {
            () = async {
                if let Ok(Ok(connection)) = tokio::time::timeout(DEADLINE, zbus::Connection::session()).await
                    && let Ok(Ok(proxy)) = tokio::time::timeout(DEADLINE, zbus::Proxy::new(&connection, BUS, PATH, INTERFACE)).await
                { let _ = follow(&proxy, sender).await; }
            } => {},
            _ = host_changes.changed() => {},
        }
        publish(sender, imp::fallback());
        let needs_portal = host_changes.borrow().is_none();
        if needs_portal {
            tokio::select! {
                () = tokio::time::sleep(imp::POLL) => {},
                _ = host_changes.changed() => {},
            }
        }
    }
}

async fn read(proxy: &zbus::Proxy<'_>) -> zbus::Result<NetStatus> {
    let values: HashMap<String, OwnedValue> = proxy.call("GetStatus", &()).await?;
    let available = values
        .get("available")
        .and_then(|value| bool::try_from(value).ok())
        .ok_or_else(|| {
            zbus::Error::Failure("NetworkMonitor status lacks available boolean".into())
        })?;
    let metered = values
        .get("metered")
        .and_then(|value| bool::try_from(value).ok())
        // GLib maps NetworkManager UNKNOWN to false. Only true is lossless.
        .filter(|metered| *metered);
    // Available means a default route: preserve Online's "worth trying"
    // meaning even behind a captive portal or with limited connectivity.
    Ok(NetStatus {
        connectivity: if available {
            Connectivity::Online
        } else {
            Connectivity::Offline
        },
        metered,
    })
}

async fn follow(proxy: &zbus::Proxy<'_>, sender: &watch::Sender<NetStatus>) -> zbus::Result<()> {
    let result = follow_events(proxy, sender).await;
    // Never retain a stale unmetered/online answer after owner or transport loss.
    publish(sender, imp::fallback());
    result
}

async fn follow_events(
    proxy: &zbus::Proxy<'_>,
    sender: &watch::Sender<NetStatus>,
) -> zbus::Result<()> {
    // Subscribe before the first snapshot so an intervening change is not lost.
    let mut changes = tokio::time::timeout(DEADLINE, proxy.receive_signal("changed"))
        .await
        .map_err(|_| zbus::Error::Failure("NetworkMonitor subscription timed out".into()))??;
    let mut owners = if proxy.connection().is_bus() {
        Some(
            tokio::time::timeout(DEADLINE, proxy.receive_owner_changed())
                .await
                .map_err(|_| {
                    zbus::Error::Failure("NetworkMonitor owner subscription timed out".into())
                })??,
        )
    } else {
        None
    };
    loop {
        let status = tokio::time::timeout(DEADLINE, read(proxy))
            .await
            .map_err(|_| zbus::Error::Failure("NetworkMonitor status timed out".into()))??;
        publish(sender, status);
        tokio::select! {
            change = changes.next() => {
                if change.is_none() { return Err(zbus::Error::Failure("NetworkMonitor disconnected".into())); }
            }
            () = async {
                match owners.as_mut() {
                    Some(stream) => { let _ = stream.next().await; }
                    None => std::future::pending::<()>().await,
                }
            } => return Err(zbus::Error::Failure("NetworkMonitor owner changed".into())),
        }
    }
}
