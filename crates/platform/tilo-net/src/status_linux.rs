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
static HOST: OnceLock<watch::Sender<Option<NetStatus>>> = OnceLock::new();

fn host() -> &'static watch::Sender<Option<NetStatus>> {
    HOST.get_or_init(|| watch::channel(None).0)
}

pub(super) fn set_host(available: Option<bool>, metered: Option<bool>) {
    host().send_replace(Some(NetStatus {
        connectivity: match available {
            Some(true) => Connectivity::Online,
            Some(false) => Connectivity::Offline,
            None => Connectivity::Unknown,
        },
        metered,
    }));
}

pub(super) fn clear_host() {
    host().send_replace(None);
}

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
    let hosted = *host().borrow();
    if let Some(status) = hosted {
        return status;
    }
    let receiver = status();
    if receiver.has_changed().is_err() {
        imp::fallback()
    } else {
        *receiver.borrow()
    }
}

pub(super) struct Watch {
    portal: watch::Receiver<NetStatus>,
    host: watch::Receiver<Option<NetStatus>>,
}

impl Watch {
    pub(super) fn new() -> Self {
        Self {
            portal: status().clone(),
            host: host().subscribe(),
        }
    }

    pub(super) async fn wait(&mut self) {
        let failed = tokio::select! {
            result = self.portal.changed() => result.is_err(),
            result = self.host.changed() => result.is_err(),
        };
        if failed {
            // A failed thread/runtime start leaves the route-only answer usable.
            let _ = tokio::time::timeout(imp::POLL, self.host.changed()).await;
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
    let mut host_changes = host().subscribe();
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use zbus::connection::Builder;
    use zbus::object_server::SignalEmitter;

    #[tokio::test]
    async fn authenticated_host_preserves_unknown_and_unmetered_and_clears_on_loss() {
        set_host(None, None);
        assert_eq!(
            current(),
            NetStatus {
                connectivity: Connectivity::Unknown,
                metered: None
            }
        );
        let (portal_sender, mut portable_updates) = watch::channel(NetStatus {
            connectivity: Connectivity::Online,
            metered: Some(false),
        });
        let monitor_task = tokio::spawn(async move { monitor(&portal_sender).await });
        // Past the fallback's poll on the runtime's clock, which runs ahead
        // while every task waits.
        tokio::time::pause();
        assert!(
            tokio::time::timeout(imp::POLL * 2, portable_updates.changed())
                .await
                .is_err(),
            "a supported rich host must suspend fallback route reads and portal retries"
        );
        tokio::time::resume();
        let (_portal, portal) = watch::channel(imp::fallback());
        let mut changes = Watch {
            portal,
            host: host().subscribe(),
        };
        set_host(Some(true), Some(true));
        changes.wait().await;
        assert_eq!(
            current(),
            NetStatus {
                connectivity: Connectivity::Online,
                metered: Some(true)
            }
        );
        set_host(Some(true), Some(false));
        changes.wait().await;
        assert_eq!(current().metered, Some(false));
        set_host(Some(false), None);
        changes.wait().await;
        assert_eq!(
            current(),
            NetStatus {
                connectivity: Connectivity::Offline,
                metered: None
            }
        );
        clear_host();
        changes.wait().await;
        monitor_task.abort();
        assert!(monitor_task.await.unwrap_err().is_cancelled());
        assert_eq!(
            *host().borrow(),
            None,
            "host loss must release the rich snapshot"
        );
        assert_ne!(
            current().metered,
            Some(false),
            "portable fallback cannot promise unmetered"
        );
    }

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
        tilo_test_env::within("the next status", receiver.changed())
            .await
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
                metered: None
            }
        );
        // Longer than the old five-second poll, on the runtime's clock, which
        // runs ahead while every task waits: an idle portal causes no reads.
        tokio::time::pause();
        assert!(
            tokio::time::timeout(imp::POLL * 2, receiver.changed())
                .await
                .is_err()
        );
        tokio::time::resume();
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        let iface = server
            .object_server()
            .interface::<_, Portal>(PATH)
            .await
            .unwrap();
        Portal::changed(iface.signal_emitter()).await.unwrap();
        // The signal is read again; the unchanged answer publishes nothing,
        // so the next status seen below is the metered one.
        tilo_test_env::within("the changed signal to be read", async {
            while reads.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
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
            tilo_test_env::within("the follower to end", task)
                .await
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
