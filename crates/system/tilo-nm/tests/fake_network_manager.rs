//! `tilo-nm` against a fake `NetworkManager` served over a socket pair: the
//! production client code, a real D-Bus wire, no bus daemon.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tilo_nm::{LinkKind, NetworkState, NmClient, NmConnectivity, NmState, PrimaryLink};
use zbus::connection::Builder;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{Connection, Guid};

const ACTIVE: &str = "/org/freedesktop/NetworkManager/ActiveConnection/7";
const DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/3";

struct FakeNm {
    state: u32,
    connectivity: u32,
    metered: u32,
    primary: OwnedObjectPath,
    primary_type: String,
    check_uri: String,
    checks: Arc<AtomicUsize>,
    devices: Vec<OwnedObjectPath>,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl FakeNm {
    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }

    #[zbus(property)]
    fn connectivity(&self) -> u32 {
        self.connectivity
    }

    #[zbus(property)]
    fn metered(&self) -> u32 {
        self.metered
    }

    #[zbus(property)]
    fn primary_connection(&self) -> OwnedObjectPath {
        self.primary.clone()
    }

    #[zbus(property)]
    fn primary_connection_type(&self) -> String {
        self.primary_type.clone()
    }

    #[zbus(property)]
    fn connectivity_check_uri(&self) -> String {
        self.check_uri.clone()
    }

    #[zbus(property)]
    fn devices(&self) -> Vec<OwnedObjectPath> {
        self.devices.clone()
    }

    fn check_connectivity(&mut self) -> u32 {
        self.checks.fetch_add(1, Ordering::SeqCst);
        self.connectivity = 2;
        self.connectivity
    }
}

struct FakeActive {
    id: String,
    devices: Vec<OwnedObjectPath>,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl FakeActive {
    #[zbus(property)]
    fn id(&self) -> String {
        self.id.clone()
    }

    #[zbus(property)]
    fn devices(&self) -> Vec<OwnedObjectPath> {
        self.devices.clone()
    }
}

struct FakeDevice {
    interface: String,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
impl FakeDevice {
    #[zbus(property)]
    fn interface(&self) -> String {
        self.interface.clone()
    }
}

/// A Wi-Fi device whose scans finish when the test says so.
struct FakeWireless {
    last_scan: i64,
    refusal: Option<String>,
    requests: tokio::sync::mpsc::UnboundedSender<()>,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
impl FakeWireless {
    #[zbus(property)]
    fn last_scan(&self) -> i64 {
        self.last_scan
    }

    fn request_scan(&self, options: HashMap<String, OwnedValue>) -> zbus::fdo::Result<()> {
        // A plain "scan now": no SSIDs to probe for.
        if let Some(option) = options.into_keys().next() {
            return Err(zbus::fdo::Error::InvalidArgs(option));
        }
        if let Some(refusal) = &self.refusal {
            return Err(zbus::fdo::Error::NotSupported(refusal.clone()));
        }
        let _ = self.requests.send(());
        Ok(())
    }
}

fn path(path: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(path).unwrap()
}

/// A connected Wi-Fi primary with full connectivity, metering as given.
fn wifi_nm(metered: u32, checks: Arc<AtomicUsize>) -> FakeNm {
    FakeNm {
        state: 70,
        connectivity: 4,
        metered,
        primary: path(ACTIVE),
        primary_type: "802-11-wireless".into(),
        check_uri: "http://check.example/".into(),
        checks,
        devices: vec![path(DEVICE)],
    }
}

async fn serve(nm: FakeNm) -> (Connection, Connection) {
    let (requests, _) = tokio::sync::mpsc::unbounded_channel();
    serve_with_wifi(
        nm,
        FakeWireless {
            last_scan: 1_000,
            refusal: None,
            requests,
        },
    )
    .await
}

async fn serve_with_wifi(nm: FakeNm, wireless: FakeWireless) -> (Connection, Connection) {
    let (server, client) = tokio::net::UnixStream::pair().unwrap();
    let server = Builder::unix_stream(server)
        .server(Guid::generate())
        .unwrap()
        .p2p()
        .serve_at(tilo_nm::ROOT_PATH, nm)
        .unwrap()
        .serve_at(
            ACTIVE,
            FakeActive {
                id: "Home".into(),
                devices: vec![path(DEVICE)],
            },
        )
        .unwrap()
        .serve_at(
            DEVICE,
            FakeDevice {
                interface: "wlp2s0".into(),
            },
        )
        .unwrap()
        .serve_at(DEVICE, wireless)
        .unwrap()
        .build();
    let client = Builder::unix_stream(client).p2p().build();
    let (server, client) = tokio::join!(server, client);
    (server.unwrap(), client.unwrap())
}

async fn update(server: &Connection, change: impl FnOnce(&mut FakeNm)) {
    let iface = server
        .object_server()
        .interface::<_, FakeNm>(tilo_nm::ROOT_PATH)
        .await
        .unwrap();
    {
        let mut nm = iface.get_mut().await;
        change(&mut nm);
    }
    let emitter: &SignalEmitter<'static> = iface.signal_emitter();
    let nm = iface.get().await;
    nm.metered_changed(emitter).await.unwrap();
    nm.primary_connection_changed(emitter).await.unwrap();
    drop(nm);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reading_maps_the_primary_connection() {
    let (_server, client) = serve(wifi_nm(4, Arc::default())).await;
    let state = NmClient::new(client).state().await.unwrap();
    assert_eq!(
        state,
        NetworkState {
            state: NmState::ConnectedGlobal,
            connectivity: NmConnectivity::Full,
            metered: Some(false),
            primary: Some(PrimaryLink {
                kind: LinkKind::Wifi,
                name: Some("Home".into()),
                device: Some("wlp2s0".into()),
            }),
        }
    );
    assert!(state.allows_speculative_traffic());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_watch_follows_properties_changed() {
    let (server, client) = serve(wifi_nm(2, Arc::default())).await;
    let mut watch = NmClient::new(client).watch().await.unwrap();
    assert_eq!(watch.current().metered, Some(false));

    update(&server, |nm| nm.metered = 1).await;
    let changed = tilo_test_env::within(
        "PropertiesChanged{Metered: 1} reaches the watch",
        watch.changed(),
    )
    .await
    .expect("the follower is still running");
    assert_eq!(changed.metered, Some(true));
    assert!(!changed.allows_speculative_traffic());

    update(&server, |nm| nm.primary = path("/")).await;
    let changed = tilo_test_env::within(
        "losing the primary connection reaches the watch",
        watch.changed(),
    )
    .await
    .expect("the follower is still running");
    assert_eq!(changed.primary, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_connection_reads_as_unknown() {
    let (server, client) = serve(wifi_nm(2, Arc::default())).await;
    let mut watch = NmClient::new(client).watch().await.unwrap();
    assert!(watch.current().allows_speculative_traffic());
    drop(server);
    let last =
        tilo_test_env::within("a closed connection reaches the watch", watch.changed()).await;
    assert_eq!(last, Some(NetworkState::unknown()));
    assert!(!watch.current().allows_speculative_traffic());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reading_never_checks_and_checking_is_explicit() {
    let checks = Arc::new(AtomicUsize::new(0));
    let (_server, client) = serve(wifi_nm(2, Arc::clone(&checks))).await;
    let nm = NmClient::new(client);
    let _ = nm.state().await.unwrap();
    let _watch = nm.watch().await.unwrap();
    assert_eq!(
        checks.load(Ordering::SeqCst),
        0,
        "a reading must not make NetworkManager probe the internet"
    );
    assert_eq!(
        nm.check_connectivity_now().await.unwrap(),
        NmConnectivity::Portal
    );
    assert_eq!(checks.load(Ordering::SeqCst), 1);
    assert_eq!(
        nm.captive_portal_uri().await.unwrap(),
        "http://check.example/"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_http_check_uri_is_refused() {
    let mut nm = wifi_nm(2, Arc::default());
    nm.check_uri = "file:///etc/shadow".into();
    let (_server, client) = serve(nm).await;
    assert!(matches!(
        NmClient::new(client).captive_portal_uri().await,
        Err(tilo_nm::NmError::UnsafeUri)
    ));
}

/// A scan returns when the device's `LastScan` moves, not when the request is
/// accepted: the list read after it is what the radio saw.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wifi_scan_returns_when_the_device_reports_it_finished() {
    let (requests, mut requested) = tokio::sync::mpsc::unbounded_channel();
    let (server, client) = serve_with_wifi(
        wifi_nm(2, Arc::default()),
        FakeWireless {
            last_scan: 1_000,
            refusal: None,
            requests,
        },
    )
    .await;
    let scan = tokio::spawn(async move { NmClient::new(client).scan_wifi().await });
    tilo_test_env::within("the scan is requested", requested.recv())
        .await
        .unwrap();
    // Requested but not finished: the scan is still waiting.
    tokio::task::yield_now().await;
    assert!(
        !scan.is_finished(),
        "an accepted request is not a finished scan"
    );

    let wireless = server
        .object_server()
        .interface::<_, FakeWireless>(DEVICE)
        .await
        .unwrap();
    wireless.get_mut().await.last_scan = 14_600;
    wireless
        .get()
        .await
        .last_scan_changed(wireless.signal_emitter())
        .await
        .unwrap();
    tilo_test_env::within("LastScan moving finishes the scan", scan)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_or_impossible_wifi_scan_says_why() {
    let (requests, _) = tokio::sync::mpsc::unbounded_channel();
    let (_server, client) = serve_with_wifi(
        wifi_nm(2, Arc::default()),
        FakeWireless {
            last_scan: 1_000,
            refusal: Some("Scanning not allowed while unavailable".into()),
            requests,
        },
    )
    .await;
    let refused = NmClient::new(client).scan_wifi().await.unwrap_err();
    assert!(
        refused.to_string().contains("Scanning not allowed"),
        "{refused}"
    );

    let mut nm = wifi_nm(2, Arc::default());
    nm.devices = Vec::new();
    let (_server, client) = serve(nm).await;
    assert!(matches!(
        NmClient::new(client).scan_wifi().await,
        Err(tilo_nm::NmError::NoWifiDevice)
    ));
}
