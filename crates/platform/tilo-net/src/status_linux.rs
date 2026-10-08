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

pub(super) struct Watch(watch::Receiver<NetStatus>);

impl Watch {
    pub(super) fn new() -> Self {
        Self(status().clone())
    }

    pub(super) async fn wait(&mut self) {
        if self.0.changed().await.is_err() {
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
    loop {
        if let Ok(Ok(connection)) =
            tokio::time::timeout(DEADLINE, zbus::Connection::session()).await
            && let Ok(Ok(proxy)) = tokio::time::timeout(
                DEADLINE,
                zbus::Proxy::new(&connection, BUS, PATH, INTERFACE),
            )
            .await
        {
            let _ = follow(&proxy, sender).await;
        }
        publish(sender, imp::fallback());
        // Only the unavailable-portal path checks routes/retries. A working
        // portal sleeps on signals and does no periodic reads or active probes.
        tokio::time::sleep(imp::POLL).await;
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
        .and_then(|value| bool::try_from(value).ok());
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use zbus::connection::Builder;
    use zbus::object_server::SignalEmitter;

    struct Portal {
        available: Option<bool>,
        metered: Option<bool>,
        denied: bool,
        reads: Arc<AtomicUsize>,
    }

    #[zbus::interface(name = "org.freedesktop.portal.NetworkMonitor")]
    impl Portal {
        fn get_status(&self) -> zbus::fdo::Result<HashMap<String, OwnedValue>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.denied {
                return Err(zbus::fdo::Error::AccessDenied("network not allowed".into()));
            }
            let mut values = HashMap::new();
            if let Some(available) = self.available {
                values.insert("available".into(), OwnedValue::from(available));
            }
            if let Some(metered) = self.metered {
                values.insert("metered".into(), OwnedValue::from(metered));
            }
            // Captive portal still has a route: it remains worth trying.
            values.insert("connectivity".into(), OwnedValue::from(3_u32));
            Ok(values)
        }

        #[zbus(signal, name = "changed")]
        async fn changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    }

    async fn serve(portal: Portal) -> (zbus::Connection, zbus::Connection) {
        let (server, client) = tokio::net::UnixStream::pair().unwrap();
        let server = Builder::unix_stream(server)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p()
            .serve_at(PATH, portal)
            .unwrap()
            .build();
        let client = Builder::unix_stream(client).p2p().build();
        let (server, client) = tokio::join!(server, client);
        (server.unwrap(), client.unwrap())
    }

    async fn next(receiver: &mut watch::Receiver<NetStatus>) -> NetStatus {
        tokio::time::timeout(Duration::from_secs(2), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        *receiver.borrow_and_update()
    }

    #[tokio::test]
    async fn portal_transitions_idle_dedup_and_disconnect_use_the_production_subscription() {
        let reads = Arc::new(AtomicUsize::new(0));
        let (server, client) = serve(Portal {
            available: Some(true),
            metered: Some(false),
            denied: false,
            reads: reads.clone(),
        })
        .await;
        let proxy = zbus::Proxy::new(&client, BUS, PATH, INTERFACE)
            .await
            .unwrap();
        let (sender, mut receiver) = watch::channel(NetStatus {
            connectivity: Connectivity::Unknown,
            metered: None,
        });
        let task = tokio::spawn(async move { follow(&proxy, &sender).await });
        assert_eq!(
            next(&mut receiver).await,
            NetStatus {
                connectivity: Connectivity::Online,
                metered: Some(false)
            }
        );
        // Longer than the old five-second poll: an idle portal causes no reads.
        assert!(
            tokio::time::timeout(Duration::from_millis(5200), receiver.changed())
                .await
                .is_err()
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        let iface = server
            .object_server()
            .interface::<_, Portal>(PATH)
            .await
            .unwrap();
        Portal::changed(iface.signal_emitter()).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), receiver.changed())
                .await
                .is_err()
        );
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        iface.get_mut().await.metered = Some(true);
        Portal::changed(iface.signal_emitter()).await.unwrap();
        assert_eq!(next(&mut receiver).await.metered, Some(true));
        iface.get_mut().await.available = Some(false);
        Portal::changed(iface.signal_emitter()).await.unwrap();
        assert_eq!(
            next(&mut receiver).await.connectivity,
            Connectivity::Offline
        );
        server.close().await.unwrap();
        assert_eq!(next(&mut receiver).await.metered, None);
        assert!(
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn refusal_and_malformed_status_discard_stale_metering() {
        for (available, denied) in [(Some(true), true), (None, false)] {
            let (server, client) = serve(Portal {
                available,
                metered: Some(false),
                denied,
                reads: Arc::new(AtomicUsize::new(0)),
            })
            .await;
            let proxy = zbus::Proxy::new(&client, BUS, PATH, INTERFACE)
                .await
                .unwrap();
            let (sender, receiver) = watch::channel(NetStatus {
                connectivity: Connectivity::Online,
                metered: Some(false),
            });
            assert!(follow(&proxy, &sender).await.is_err());
            assert_eq!(receiver.borrow().metered, None);
            server.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn absent_metering_stays_unknown_instead_of_claiming_unmetered() {
        let (server, client) = serve(Portal {
            available: Some(true),
            metered: None,
            denied: false,
            reads: Arc::new(AtomicUsize::new(0)),
        })
        .await;
        let proxy = zbus::Proxy::new(&client, BUS, PATH, INTERFACE)
            .await
            .unwrap();
        assert_eq!(
            read(&proxy).await.unwrap(),
            NetStatus {
                connectivity: Connectivity::Online,
                metered: None
            }
        );
        server.close().await.unwrap();
    }
}
