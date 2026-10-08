//! Receiver discovery: Cast, `FCast` and `AirPlay` over mDNS, Miracast over
//! `NetworkManager`'s Wi-Fi Direct peers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use fcast_sender_sdk::device::{DeviceInfo, ProtocolType};
use fcast_sender_sdk::{DeviceDiscovererEventHandler, url_format_ip_addr};
use mdns_sd::ServiceEvent;
use tilo_projector::{ProjectionDevice, ProjectionMode, ProjectionProtocol};

use crate::error::ProjectorError;

pub(crate) const DEFAULT_DISCOVERY_MS: u64 = 1_500;
pub(crate) const AIRPLAY_SERVICE: &str = "_airplay._tcp.local.";
const NM_DESTINATION: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_INTERFACE: &str = "org.freedesktop.NetworkManager";
const NM_DEVICE_INTERFACE: &str = "org.freedesktop.NetworkManager.Device";
const NM_P2P_INTERFACE: &str = "org.freedesktop.NetworkManager.Device.WifiP2P";
const NM_P2P_PEER_INTERFACE: &str = "org.freedesktop.NetworkManager.WifiP2PPeer";
const NM_DEVICE_TYPE_WIFI_P2P: u32 = 30;

#[derive(Default)]
struct DiscoverySink {
    devices: Mutex<HashMap<String, DeviceInfo>>,
    changed: Mutex<Option<mpsc::Sender<()>>>,
}

impl DiscoverySink {
    fn with_signal(tx: mpsc::Sender<()>) -> Self {
        Self {
            devices: Mutex::new(HashMap::new()),
            changed: Mutex::new(Some(tx)),
        }
    }

    fn insert(&self, device: DeviceInfo) {
        let key = discovery_key(&device);
        let Ok(mut devices) = self.devices.lock() else {
            return;
        };
        devices.insert(key, device);
        drop(devices);
        let Ok(changed) = self.changed.lock() else {
            return;
        };
        if let Some(tx) = changed.as_ref() {
            let _ = tx.send(());
        }
    }
}

impl DeviceDiscovererEventHandler for DiscoverySink {
    fn device_available(&self, device_info: DeviceInfo) {
        self.insert(device_info);
    }

    fn device_removed(&self, device_name: String) {
        let Ok(mut devices) = self.devices.lock() else {
            return;
        };
        devices.retain(|_, device| device.name != device_name);
    }

    fn device_changed(&self, device_info: DeviceInfo) {
        self.insert(device_info);
    }
}

fn protocol(info: &DeviceInfo) -> ProjectionProtocol {
    match info.protocol {
        ProtocolType::FCast => ProjectionProtocol::FCast,
        ProtocolType::Chromecast => ProjectionProtocol::Chromecast,
    }
}

fn discovery_key(info: &DeviceInfo) -> String {
    let kind = protocol(info);
    let receiver_id = info
        .txt_records
        .get("id")
        .or_else(|| info.txt_records.get("deviceid"))
        .map_or(info.name.as_str(), String::as_str);
    format!("{}:{receiver_id}", kind.label())
}

fn into_projection_device(info: DeviceInfo) -> ProjectionDevice {
    let protocol = protocol(&info);
    let addresses: Vec<String> = info.addresses.iter().map(url_format_ip_addr).collect();
    let detail = addresses.first().map_or_else(
        || "Local network".to_owned(),
        |address| format!("{address}:{}", info.port),
    );
    ProjectionDevice {
        id: discovery_key(&info),
        name: info.name,
        protocol,
        detail,
        // A remote receiver is not a compositor output yet. Advertising only
        // mirror prevents the UI promising Extend/Only-there before the KMS
        // virtual-output path is attached.
        modes: vec![ProjectionMode::Mirror],
        addresses,
        port: info.port,
    }
}

fn airplay_projection_device(info: &mdns_sd::ResolvedService) -> ProjectionDevice {
    let name = info
        .get_fullname()
        .strip_suffix(&format!(".{AIRPLAY_SERVICE}"))
        .unwrap_or_else(|| info.get_fullname())
        .to_owned();
    let receiver_id = info
        .get_property_val_str("deviceid")
        .or_else(|| info.get_property_val_str("pi"))
        .unwrap_or_else(|| info.get_fullname());
    let mut addresses: Vec<String> = info
        .get_addresses()
        .iter()
        .map(|address| match address.to_ip_addr() {
            std::net::IpAddr::V4(address) => address.to_string(),
            std::net::IpAddr::V6(address) => format!("[{address}]"),
        })
        .collect();
    addresses.sort();
    let endpoint = addresses.first().map_or_else(
        || "Local network".to_owned(),
        |address| format!("{address}:{}", info.get_port()),
    );
    let detail = info
        .get_property_val_str("model")
        .map(|model| format!("{model} · {endpoint}"))
        .unwrap_or(endpoint);
    ProjectionDevice {
        id: format!("AirPlay:{receiver_id}"),
        name,
        protocol: ProjectionProtocol::AirPlay,
        detail,
        modes: vec![ProjectionMode::Mirror],
        addresses,
        port: info.get_port(),
    }
}

async fn discover_miracast_on_connection(
    connection: &zbus::Connection,
    timeout: Duration,
) -> zbus::Result<Vec<ProjectionDevice>> {
    use zbus::zvariant::OwnedObjectPath;

    let manager = zbus::Proxy::new(connection, NM_DESTINATION, NM_PATH, NM_INTERFACE).await?;
    let devices_reply = manager.call_method("GetDevices", &()).await?;
    let device_paths: Vec<OwnedObjectPath> = devices_reply.body().deserialize()?;
    let mut p2p_paths = Vec::new();
    for path in device_paths {
        let device = zbus::Proxy::new(
            connection,
            NM_DESTINATION,
            path.as_str(),
            NM_DEVICE_INTERFACE,
        )
        .await?;
        if device.get_property::<u32>("DeviceType").await? == NM_DEVICE_TYPE_WIFI_P2P {
            let p2p = zbus::Proxy::new(connection, NM_DESTINATION, path.as_str(), NM_P2P_INTERFACE)
                .await?;
            let options = HashMap::from([(
                "timeout",
                zbus::zvariant::Value::from(timeout.as_secs().clamp(1, 600) as i32),
            )]);
            p2p.call_method("StartFind", &options).await?;
            p2p_paths.push(path);
        }
    }

    tokio::time::sleep(timeout).await;
    let mut receivers = Vec::new();
    for path in p2p_paths {
        let p2p =
            zbus::Proxy::new(connection, NM_DESTINATION, path.as_str(), NM_P2P_INTERFACE).await?;
        let peers: Vec<OwnedObjectPath> = p2p.get_property("Peers").await?;
        for peer_path in peers {
            let peer = zbus::Proxy::new(
                connection,
                NM_DESTINATION,
                peer_path.as_str(),
                NM_P2P_PEER_INTERFACE,
            )
            .await?;
            let wfd_ies: Vec<u8> = peer.get_property("WfdIEs").await?;
            if wfd_ies.is_empty() {
                continue;
            }
            let name: String = peer.get_property("Name").await?;
            let address: String = peer.get_property("HwAddress").await?;
            let manufacturer: String = peer.get_property("Manufacturer").await?;
            let model: String = peer.get_property("Model").await?;
            let strength: u8 = peer.get_property("Strength").await?;
            let hardware = [manufacturer.as_str(), model.as_str()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let detail = if hardware.is_empty() {
                format!("Wi-Fi Direct · {strength}%")
            } else {
                format!("{hardware} · {strength}%")
            };
            receivers.push(ProjectionDevice {
                id: format!("Miracast:{address}"),
                name,
                protocol: ProjectionProtocol::Miracast,
                detail,
                modes: vec![ProjectionMode::Mirror],
                // Wi-Fi Direct has not formed a group yet; the peer MAC, not
                // an IP endpoint, identifies the receiver at this stage.
                addresses: vec![address],
                port: 0,
            });
        }
        let _ = p2p.call_method("StopFind", &()).await;
    }
    Ok(receivers)
}

fn discover_miracast(timeout: Duration) -> Vec<ProjectionDevice> {
    tilo_rt::block_on(async {
        let Ok(connection) = zbus::Connection::system().await else {
            return Vec::new();
        };
        discover_miracast_on_connection(&connection, timeout)
            .await
            .unwrap_or_default()
    })
    .unwrap_or_default()
}

pub(crate) fn discover(timeout: Duration) -> Result<Vec<ProjectionDevice>, ProjectorError> {
    let miracast = std::thread::Builder::new()
        .name("tilo-miracast-discovery".to_owned())
        .spawn(move || discover_miracast(timeout))
        .map_err(ProjectorError::io("cannot start Miracast discovery"))?;
    let context = fcast_sender_sdk::context::CastContext::new().map_err(|error| {
        ProjectorError::Unavailable(format!("cannot initialise Cast discovery: {error}"))
    })?;
    let (tx, rx) = mpsc::channel();
    let sink = Arc::new(DiscoverySink::with_signal(tx));
    context.start_discovery(sink.clone());
    let airplay_daemon = mdns_sd::ServiceDaemon::new().map_err(|error| {
        ProjectorError::Unavailable(format!("cannot initialise AirPlay discovery: {error}"))
    })?;
    let airplay_rx = airplay_daemon.browse(AIRPLAY_SERVICE).map_err(|error| {
        ProjectorError::Unavailable(format!("cannot browse AirPlay receivers: {error}"))
    })?;
    let mut airplay = HashMap::new();

    // Give both service browsers the full bounded collection window. The two
    // protocols have independent mDNS streams, so returning on the first quiet
    // gap can consistently lose whichever browser resolves second.
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        ProjectorError::Protocol("requested discovery timeout is too large".to_owned())
    })?;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if matches!(
            rx.recv_timeout(Duration::from_millis(100).min(remaining)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ) {
            break;
        }
        while let Ok(event) = airplay_rx.try_recv() {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    let device = airplay_projection_device(&info);
                    airplay.insert(device.id.clone(), device);
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    airplay.retain(|_, device| !fullname.starts_with(&device.name));
                }
                _ => {}
            }
        }
    }
    let _ = airplay_daemon.shutdown();

    let mut devices: Vec<_> = sink
        .devices
        .lock()
        .map_err(|_| {
            ProjectorError::Unavailable("projection discovery cache was poisoned".to_owned())
        })?
        .values()
        .cloned()
        .map(into_projection_device)
        .collect();
    devices.extend(airplay.into_values());
    devices.extend(miracast.join().unwrap_or_default());
    devices.sort_by_key(|device| device.name.to_lowercase());
    Ok(devices)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::used_underscore_binding,
        reason = "zbus::interface's generated dispatcher binds start_find's _options parameter \
                  by name in dispatch code that lands in this module but outside the annotated \
                  impl, so neither an impl- nor fn-level allow reaches it; the mock ignores it"
    )]
    use super::*;
    use fcast_sender_sdk::IpAddr;
    use mdns_sd::{ServiceDaemon, ServiceInfo};
    use zbus::connection::Builder;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue};

    const TEST_GUID: &str = "0123456789abcdef0123456789abcdef";
    const TEST_P2P_PATH: &str = "/org/freedesktop/NetworkManager/Devices/7";
    const TEST_PEER_PATH: &str = "/org/freedesktop/NetworkManager/WifiP2PPeer/9";

    struct SimulatedNetworkManager;

    #[allow(
        clippy::unused_self,
        reason = "zbus::interface dispatches by instance method, even for a stateless mock reply"
    )]
    #[zbus::interface(name = "org.freedesktop.NetworkManager")]
    impl SimulatedNetworkManager {
        fn get_devices(&self) -> Vec<OwnedObjectPath> {
            vec![OwnedObjectPath::try_from(TEST_P2P_PATH).expect("device path")]
        }
    }

    struct SimulatedDevice;

    #[allow(
        clippy::unused_self,
        reason = "zbus::interface dispatches by instance method, even for a stateless mock reply"
    )]
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
    impl SimulatedDevice {
        #[zbus(property)]
        fn device_type(&self) -> u32 {
            NM_DEVICE_TYPE_WIFI_P2P
        }
    }

    struct SimulatedP2pDevice;

    #[allow(
        clippy::unused_self,
        reason = "zbus::interface dispatches by instance method, even for a stateless mock reply"
    )]
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Device.WifiP2P")]
    impl SimulatedP2pDevice {
        fn start_find(&self, _options: HashMap<String, OwnedValue>) {}

        fn stop_find(&self) {}

        #[zbus(property)]
        fn peers(&self) -> Vec<OwnedObjectPath> {
            vec![OwnedObjectPath::try_from(TEST_PEER_PATH).expect("peer path")]
        }
    }

    struct SimulatedMiracastPeer;

    #[allow(
        clippy::unused_self,
        reason = "zbus::interface dispatches by instance method, even for a stateless mock reply"
    )]
    #[zbus::interface(name = "org.freedesktop.NetworkManager.WifiP2PPeer")]
    impl SimulatedMiracastPeer {
        #[zbus(property)]
        fn name(&self) -> &'static str {
            "Conference display"
        }

        #[zbus(property)]
        fn hw_address(&self) -> &'static str {
            "02:00:00:00:00:09"
        }

        #[zbus(property)]
        fn manufacturer(&self) -> &'static str {
            "Tilo Labs"
        }

        #[zbus(property)]
        fn model(&self) -> &'static str {
            "WFD Simulator"
        }

        #[zbus(property)]
        fn strength(&self) -> u8 {
            87
        }

        #[zbus(property, name = "WfdIEs")]
        fn wfd_ies(&self) -> Vec<u8> {
            // Presence is the NetworkManager contract for a Wi-Fi Display
            // capable P2P peer. Parsing the WFD subelements belongs to the
            // RTSP session driver, not the picker.
            vec![0x00, 0x00, 0x06, 0x00, 0x01, 0x1c, 0x44, 0x00, 0x00]
        }
    }

    async fn simulated_network_manager() -> (zbus::Connection, zbus::Connection) {
        let (server_socket, client_socket) = tokio::net::UnixStream::pair().expect("socket pair");
        let server = Builder::unix_stream(server_socket)
            .server(TEST_GUID)
            .expect("server GUID")
            .p2p()
            .serve_at(NM_PATH, SimulatedNetworkManager)
            .expect("manager service")
            .serve_at(TEST_P2P_PATH, SimulatedDevice)
            .expect("base device service")
            .serve_at(TEST_P2P_PATH, SimulatedP2pDevice)
            .expect("P2P device service")
            .serve_at(TEST_PEER_PATH, SimulatedMiracastPeer)
            .expect("peer service");
        let client = Builder::unix_stream(client_socket).p2p();
        futures_util::try_join!(server.build(), client.build()).expect("D-Bus simulator pair")
    }

    fn info(protocol: ProtocolType) -> DeviceInfo {
        DeviceInfo {
            name: "Studio TV".to_owned(),
            protocol,
            addresses: vec![IpAddr::v4(192, 0, 2, 40)],
            port: 46899,
            txt_records: HashMap::from([("id".to_owned(), "receiver-42".to_owned())]),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn discovery_records_share_the_broker_contract_and_stay_bounded() {
        // Phase `fcast_discovery_record_preserves_endpoint_and_identity`.
        {
            let device = into_projection_device(info(ProtocolType::FCast));
            assert_eq!(device.protocol, ProjectionProtocol::FCast);
            assert_eq!(device.id, "FCast:receiver-42");
            assert_eq!(device.addresses, ["192.0.2.40"]);
            assert_eq!(device.port, 46899);
            assert_eq!(device.modes, [ProjectionMode::Mirror]);
        }
        // Phase `chromecast_discovery_record_uses_the_same_broker_contract`.
        {
            let mut cast = info(ProtocolType::Chromecast);
            cast.port = 8009;
            let device = into_projection_device(cast);
            assert_eq!(device.protocol, ProjectionProtocol::Chromecast);
            assert_eq!(device.id, "Google Cast:receiver-42");
            assert_eq!(device.port, 8009);
        }
        // Phase `zero_length_discovery_is_bounded_and_json_serialisable`.
        {
            let devices = discover(Duration::ZERO).expect("empty bounded discovery");
            serde_json::to_string(&devices).expect("projection discovery JSON");
        }
        // Phase `local_mdns_receivers_exercise_three_real_discovery_browsers`.
        {
            let daemon = ServiceDaemon::new().expect("mDNS simulator daemon");
            let suffix = std::process::id();
            let fcast_name = format!("Tilo FCast simulator {suffix}");
            let cast_name = format!("Tilo Cast simulator {suffix}");
            let airplay_name = format!("Tilo AirPlay simulator {suffix}");

            let fcast = ServiceInfo::new(
                "_fcast._tcp.local.",
                &fcast_name,
                &format!("tilo-fcast-{suffix}.local."),
                "",
                46899,
                HashMap::from([("id".to_owned(), format!("fcast-{suffix}"))]),
            )
            .expect("FCast simulator record")
            .enable_addr_auto();
            let cast = ServiceInfo::new(
                "_googlecast._tcp.local.",
                &format!("tilo-cast-{suffix}"),
                &format!("tilo-cast-{suffix}.local."),
                "",
                8009,
                HashMap::from([
                    ("id".to_owned(), format!("cast-{suffix}")),
                    ("fn".to_owned(), cast_name.clone()),
                ]),
            )
            .expect("Google Cast simulator record")
            .enable_addr_auto();
            let airplay = ServiceInfo::new(
                AIRPLAY_SERVICE,
                &airplay_name,
                &format!("tilo-airplay-{suffix}.local."),
                "",
                7000,
                HashMap::from([
                    ("deviceid".to_owned(), format!("airplay-{suffix}")),
                    ("model".to_owned(), "AudioAccessory6,1".to_owned()),
                    ("features".to_owned(), "0x5A7FFFF7,0x1E".to_owned()),
                ]),
            )
            .expect("AirPlay simulator record")
            .enable_addr_auto();
            daemon.register(fcast).expect("register FCast simulator");
            daemon.register(cast).expect("register Cast simulator");
            daemon
                .register(airplay)
                .expect("register AirPlay simulator");
            std::thread::sleep(Duration::from_millis(300));

            let devices = discover(Duration::from_secs(3)).expect("discover simulated receivers");
            let _ = daemon.shutdown();

            assert!(
                devices.iter().any(|device| {
                    device.name == fcast_name
                        && device.protocol == ProjectionProtocol::FCast
                        && device.port == 46899
                }),
                "FCast simulator missing from {devices:?}"
            );
            assert!(
                devices.iter().any(|device| {
                    device.name == cast_name
                        && device.protocol == ProjectionProtocol::Chromecast
                        && device.port == 8009
                }),
                "Google Cast simulator missing from {devices:?}"
            );
            assert!(
                devices.iter().any(|device| {
                    device.name == airplay_name
                        && device.protocol == ProjectionProtocol::AirPlay
                        && device.port == 7000
                }),
                "AirPlay simulator missing from {devices:?}"
            );
        }
    }

    #[tokio::test]
    async fn simulated_networkmanager_peer_exercises_miracast_discovery() {
        let (_server, client) = simulated_network_manager().await;
        let devices = discover_miracast_on_connection(&client, Duration::ZERO)
            .await
            .expect("simulated Miracast discovery");
        assert_eq!(devices.len(), 1);
        let receiver = &devices[0];
        assert_eq!(receiver.protocol, ProjectionProtocol::Miracast);
        assert_eq!(receiver.name, "Conference display");
        assert_eq!(receiver.id, "Miracast:02:00:00:00:00:09");
        assert_eq!(receiver.addresses, ["02:00:00:00:00:09"]);
        assert!(receiver.detail.contains("87%"));
    }
}
