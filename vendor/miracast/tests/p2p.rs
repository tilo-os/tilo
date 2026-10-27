//! Group formation against a simulated NetworkManager over a private D-Bus
//! connection: the calls, the settings that carry the WFD IEs, and what the
//! formed group reports.

#![cfg(feature = "networkmanager")]
#![allow(
    clippy::unused_self,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    reason = "the simulated objects' methods take the shape of their D-Bus signatures"
)]

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use miracast::ie;
use miracast::p2p::{self, ConnectOptions, P2pError};
use zbus::connection::Builder;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

const GUID: &str = "0123456789abcdef0123456789abcdef";
const WIFI: &str = "/org/freedesktop/NetworkManager/Devices/1";
const P2P_DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/2";
const GROUP_DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/3";
const PEER: &str = "/org/freedesktop/NetworkManager/WifiP2PPeer/9";
const ACTIVE: &str = "/org/freedesktop/NetworkManager/ActiveConnection/4";
const IP4: &str = "/org/freedesktop/NetworkManager/IP4Config/5";
const SINK_MAC: &str = "02:00:00:00:00:09";

#[derive(Default)]
struct Record {
    find_started: bool,
    find_stopped: bool,
    settings: HashMap<String, HashMap<String, OwnedValue>>,
    options: HashMap<String, OwnedValue>,
    specific_object: String,
    deactivated: Vec<String>,
}

type Shared = Arc<Mutex<Record>>;

struct Manager {
    record: Shared,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl Manager {
    fn get_devices(&self) -> Vec<OwnedObjectPath> {
        [WIFI, P2P_DEVICE]
            .into_iter()
            .map(|path| OwnedObjectPath::try_from(path).unwrap())
            .collect()
    }

    fn add_and_activate_connection2(
        &self,
        settings: HashMap<String, HashMap<String, OwnedValue>>,
        device: ObjectPath<'_>,
        specific_object: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(
        OwnedObjectPath,
        OwnedObjectPath,
        HashMap<String, OwnedValue>,
    )> {
        assert_eq!(device.as_str(), P2P_DEVICE);
        let mut record = self.record.lock().unwrap();
        record.settings = settings;
        record.options = options;
        record.specific_object = specific_object.to_string();
        Ok((
            OwnedObjectPath::try_from("/org/freedesktop/NetworkManager/Settings/7").unwrap(),
            OwnedObjectPath::try_from(ACTIVE).unwrap(),
            HashMap::new(),
        ))
    }

    fn deactivate_connection(&self, active: ObjectPath<'_>) {
        self.record
            .lock()
            .unwrap()
            .deactivated
            .push(active.to_string());
    }
}

struct Device {
    kind: u32,
    interface: &'static str,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
impl Device {
    #[zbus(property)]
    fn device_type(&self) -> u32 {
        self.kind
    }

    #[zbus(property)]
    fn ip_interface(&self) -> String {
        self.interface.to_owned()
    }

    #[zbus(property)]
    fn ip4_config(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(IP4).unwrap()
    }
}

struct P2pDevice {
    record: Shared,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device.WifiP2P")]
impl P2pDevice {
    fn start_find(&self, options: HashMap<String, OwnedValue>) {
        assert!(options.contains_key("timeout"));
        self.record.lock().unwrap().find_started = true;
    }

    fn stop_find(&self) {
        self.record.lock().unwrap().find_stopped = true;
    }

    #[zbus(property)]
    fn peers(&self) -> Vec<OwnedObjectPath> {
        if self.record.lock().unwrap().find_started {
            vec![OwnedObjectPath::try_from(PEER).unwrap()]
        } else {
            Vec::new()
        }
    }
}

struct Peer;

#[zbus::interface(name = "org.freedesktop.NetworkManager.WifiP2PPeer")]
impl Peer {
    #[zbus(property)]
    fn hw_address(&self) -> String {
        SINK_MAC.to_uppercase()
    }

    #[zbus(property)]
    fn name(&self) -> String {
        "Living Room TV".to_owned()
    }

    #[zbus(property, name = "WfdIEs")]
    fn wfd_ies(&self) -> Vec<u8> {
        // A primary sink on RTSP port 7250.
        vec![0x00, 0x00, 0x06, 0x00, 0x11, 0x1c, 0x52, 0x00, 0x32]
    }
}

struct Active {
    state: u32,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl Active {
    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }

    #[zbus(property)]
    fn devices(&self) -> Vec<OwnedObjectPath> {
        vec![OwnedObjectPath::try_from(GROUP_DEVICE).unwrap()]
    }
}

struct Ip4;

#[zbus::interface(name = "org.freedesktop.NetworkManager.IP4Config")]
impl Ip4 {
    #[zbus(property)]
    fn address_data(&self) -> Vec<HashMap<String, OwnedValue>> {
        vec![HashMap::from([
            (
                "address".to_owned(),
                Value::from("192.168.49.20").try_into().unwrap(),
            ),
            ("prefix".to_owned(), Value::from(24_u32).try_into().unwrap()),
        ])]
    }

    #[zbus(property)]
    fn gateway(&self) -> String {
        "192.168.49.1".to_owned()
    }
}

async fn network_manager(activates: bool) -> (zbus::Connection, zbus::Connection, Shared) {
    let record = Shared::default();
    let (server_socket, client_socket) = tokio::net::UnixStream::pair().unwrap();
    let server = Builder::unix_stream(server_socket)
        .server(GUID)
        .unwrap()
        .p2p()
        .serve_at(
            "/org/freedesktop/NetworkManager",
            Manager {
                record: Arc::clone(&record),
            },
        )
        .unwrap()
        .serve_at(
            WIFI,
            Device {
                kind: 2,
                interface: "wlp1s0",
            },
        )
        .unwrap()
        .serve_at(
            P2P_DEVICE,
            Device {
                kind: 30,
                interface: "p2p-dev-wlp1s0",
            },
        )
        .unwrap()
        .serve_at(
            P2P_DEVICE,
            P2pDevice {
                record: Arc::clone(&record),
            },
        )
        .unwrap()
        .serve_at(PEER, Peer)
        .unwrap()
        .serve_at(
            ACTIVE,
            Active {
                state: if activates { 2 } else { 4 },
            },
        )
        .unwrap()
        .serve_at(
            GROUP_DEVICE,
            Device {
                kind: 30,
                interface: "p2p-wlp1s0-0",
            },
        )
        .unwrap()
        .serve_at(IP4, Ip4)
        .unwrap();
    let client = Builder::unix_stream(client_socket).p2p();
    let (server, client) = tokio::try_join!(server.build(), client.build()).unwrap();
    (server, client, record)
}

fn options() -> ConnectOptions {
    ConnectOptions {
        find_timeout: Duration::from_secs(2),
        activation_timeout: Duration::from_secs(2),
        ..ConnectOptions::default()
    }
}

#[tokio::test]
async fn a_group_forms_with_the_source_ies_and_reports_the_sink() {
    let (_server, client, record) = network_manager(true).await;
    let group = p2p::connect(&client, SINK_MAC, &options()).await.unwrap();
    assert_eq!(group.interface, "p2p-wlp1s0-0");
    assert_eq!(group.local, Ipv4Addr::new(192, 168, 49, 20));
    assert_eq!(group.sink_address(), Some(Ipv4Addr::new(192, 168, 49, 1)));
    assert_eq!(group.sink_rtsp_port, 7250);
    assert_eq!(group.sink_name, "Living Room TV");
    assert!(group.contains(IpAddr::from([192, 168, 49, 77])));
    assert!(!group.contains(IpAddr::from([192, 168, 1, 77])));

    {
        let record = record.lock().unwrap();
        assert!(record.find_started && record.find_stopped);
        assert_eq!(record.specific_object, PEER);
        let p2p = &record.settings["wifi-p2p"];
        assert_eq!(
            String::try_from(p2p["peer"].try_clone().unwrap()).unwrap(),
            SINK_MAC
        );
        let ies: Vec<u8> = p2p["wfd-ies"].try_clone().unwrap().try_into().unwrap();
        assert_eq!(ies, ie::source_ies(7236));
        let connection = &record.settings["connection"];
        assert_eq!(
            String::try_from(connection["type"].try_clone().unwrap()).unwrap(),
            "wifi-p2p"
        );
        assert_eq!(
            String::try_from(record.options["persist"].try_clone().unwrap()).unwrap(),
            "volatile"
        );
        assert_eq!(
            String::try_from(record.options["bind-activation"].try_clone().unwrap()).unwrap(),
            "dbus-client"
        );
    }

    group.close().await.unwrap();
    assert_eq!(record.lock().unwrap().deactivated, vec![ACTIVE.to_owned()]);
}

#[tokio::test]
async fn a_refused_group_is_an_error_and_is_cleaned_up() {
    let (_server, client, record) = network_manager(false).await;
    let error = p2p::connect(&client, SINK_MAC, &options())
        .await
        .unwrap_err();
    assert!(matches!(error, P2pError::Activation(_)), "{error}");
    assert_eq!(record.lock().unwrap().deactivated, vec![ACTIVE.to_owned()]);
}

#[tokio::test]
async fn an_absent_peer_is_reported_by_its_address() {
    let (_server, client, _record) = network_manager(true).await;
    let error = p2p::connect(&client, "02:00:00:00:00:42", &options())
        .await
        .unwrap_err();
    assert!(matches!(error, P2pError::PeerNotFound(ref mac) if mac == "02:00:00:00:00:42"));
}
