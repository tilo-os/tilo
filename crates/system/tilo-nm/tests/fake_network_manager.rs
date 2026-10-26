//! `tilo-nm` against a fake `NetworkManager` served over a socket pair: the
//! production client code, a real D-Bus wire, no bus daemon.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tilo_nm::{
    ActiveState, ActiveVpn, LinkKind, NetworkState, NmClient, NmConnectivity, NmState, PrimaryLink,
};
use zbus::connection::Builder;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{Connection, Guid};

const ACTIVE: &str = "/org/freedesktop/NetworkManager/ActiveConnection/7";
const DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/3";
const VPN: &str = "/org/freedesktop/NetworkManager/ActiveConnection/9";

struct FakeNm {
    state: u32,
    connectivity: u32,
    metered: u32,
    primary: OwnedObjectPath,
    primary_type: String,
    check_uri: String,
    checks: Arc<AtomicUsize>,
    devices: Vec<OwnedObjectPath>,
    active: Vec<OwnedObjectPath>,
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

    #[zbus(property)]
    fn active_connections(&self) -> Vec<OwnedObjectPath> {
        self.active.clone()
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
    kind: String,
    uuid: String,
    state: u32,
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

    #[zbus(property, name = "Type")]
    fn kind(&self) -> String {
        self.kind.clone()
    }

    #[zbus(property)]
    fn uuid(&self) -> String {
        self.uuid.clone()
    }

    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }

    #[zbus(signal, name = "StateChanged")]
    async fn state_signal(emitter: &SignalEmitter<'_>, state: u32, reason: u32)
    -> zbus::Result<()>;
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

    #[zbus(signal, name = "StateChanged")]
    async fn state_signal(
        emitter: &SignalEmitter<'_>,
        new: u32,
        old: u32,
        reason: u32,
    ) -> zbus::Result<()>;
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
        active: vec![path(ACTIVE)],
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
                kind: "802-11-wireless".into(),
                uuid: "home-uuid".into(),
                state: 2,
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

/// Wi-Fi up and the "Work" VPN in `state`, both active connections.
async fn serve_vpn(state: u32) -> (Connection, Connection) {
    let mut nm = wifi_nm(2, Arc::default());
    nm.active.push(path(VPN));
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
                kind: "802-11-wireless".into(),
                uuid: "home-uuid".into(),
                state: 2,
            },
        )
        .unwrap()
        .serve_at(
            VPN,
            FakeActive {
                id: "Work".into(),
                devices: Vec::new(),
                kind: "vpn".into(),
                uuid: "work-uuid".into(),
                state,
            },
        )
        .unwrap()
        .build();
    let client = Builder::unix_stream(client).p2p().build();
    let (server, client) = tokio::join!(server, client);
    (server.unwrap(), client.unwrap())
}

/// The VPN goes to `state` for `reason`, signalled as `NetworkManager` does.
async fn vpn_goes(server: &Connection, state: u32, reason: u32) {
    let iface = server
        .object_server()
        .interface::<_, FakeActive>(VPN)
        .await
        .unwrap();
    iface.get_mut().await.state = state;
    let emitter = iface.signal_emitter();
    FakeActive::state_signal(emitter, state, reason)
        .await
        .unwrap();
    iface.get().await.state_changed(emitter).await.unwrap();
}

/// The VPN's active connection goes away, as `NetworkManager` drops it.
async fn vpn_gone(server: &Connection) {
    let iface = server
        .object_server()
        .interface::<_, FakeNm>(tilo_nm::ROOT_PATH)
        .await
        .unwrap();
    iface
        .get_mut()
        .await
        .active
        .retain(|active| active.as_str() != VPN);
    iface
        .get()
        .await
        .active_connections_changed(iface.signal_emitter())
        .await
        .unwrap();
}

fn work(state: ActiveState) -> Vec<ActiveVpn> {
    vec![ActiveVpn {
        uuid: "work-uuid".into(),
        state,
    }]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_vpn_watch_follows_active_vpns_and_counts_turning_off_on_purpose() {
    let (server, client) = serve_vpn(1).await;
    let mut watch = NmClient::new(client).watch_vpns().await.unwrap();
    let first = watch.current();
    assert!(first.known);
    assert_eq!(first.active, work(ActiveState::Activating), "only the VPN");

    vpn_goes(&server, 2, 1).await;
    let mut reading = tilo_test_env::within("the VPN connected", watch.changed())
        .await
        .unwrap();
    while reading.active != work(ActiveState::Activated) {
        reading = tilo_test_env::within("the VPN connected", watch.changed())
            .await
            .unwrap();
    }
    assert!(reading.ended_on_purpose.is_empty());

    // Turned off: deactivating, deactivated, gone; counted once.
    vpn_goes(&server, 3, 2).await;
    vpn_goes(&server, 4, 2).await;
    vpn_gone(&server).await;
    loop {
        let reading = tilo_test_env::within("the VPN went away", watch.changed())
            .await
            .unwrap();
        if reading.active.is_empty() {
            assert_eq!(reading.ended_on_purpose.get("work-uuid"), Some(&1));
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vpn_that_drops_is_not_counted_as_turned_off() {
    let (server, client) = serve_vpn(2).await;
    let mut watch = NmClient::new(client).watch_vpns().await.unwrap();
    assert_eq!(watch.current().active, work(ActiveState::Activated));
    // The service stopped (4): not the person's doing.
    vpn_goes(&server, 4, 4).await;
    vpn_gone(&server).await;
    loop {
        let reading = tilo_test_env::within("the VPN went away", watch.changed())
            .await
            .unwrap();
        if reading.active.is_empty() {
            assert!(reading.ended_on_purpose.is_empty());
            break;
        }
    }
}

/// The change watch counts what network settings shows (a device changing
/// state, the root's properties) and not a Wi-Fi device's scan bookkeeping,
/// and ends with the connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_change_watch_counts_what_settings_shows() {
    let (server, client) = serve(wifi_nm(2, Arc::default())).await;
    let mut changes = NmClient::new(client).watch_changes().await.unwrap();
    assert_eq!(changes.count(), 0);

    // A finished scan first, then the cable going out: only the second is
    // a change.
    let wireless = server
        .object_server()
        .interface::<_, FakeWireless>(DEVICE)
        .await
        .unwrap();
    wireless.get_mut().await.last_scan = 2_000;
    wireless
        .get()
        .await
        .last_scan_changed(wireless.signal_emitter())
        .await
        .unwrap();
    let device = server
        .object_server()
        .interface::<_, FakeDevice>(DEVICE)
        .await
        .unwrap();
    FakeDevice::state_signal(device.signal_emitter(), 30, 100, 40)
        .await
        .unwrap();
    let counted = tilo_test_env::within("a device changing state is a change", changes.changed())
        .await
        .expect("the follower is still running");
    assert_eq!(counted, 1);
    // The scan came first on the wire: had it counted, it would be by now.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(changes.count(), 1, "a finished scan is not a change");

    // Metered and the primary connection: two root properties.
    update(&server, |nm| nm.metered = 1).await;
    while changes.count() < 3 {
        tilo_test_env::within("the root's properties are changes", changes.changed())
            .await
            .expect("the follower is still running");
    }

    // The interfaces hold the server's connection too.
    drop((wireless, device, server));
    tilo_test_env::within("the watch ends with the connection", async {
        while changes.changed().await.is_some() {}
    })
    .await;
}
