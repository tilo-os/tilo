use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use tilo_actions::DoorError;
use tilo_service::{Service, Started};

use crate::device::{Change, Device, Readings};
use crate::model::{
    Accessory, Band, Bluetooth, Client, Metered, Mobile, MobileProfile, Nearby, Network, SavedKind,
    SavedNetwork, Tethering, Vpn, VpnProfile, Wifi, WifiNetwork, Wired,
};
use crate::{BLUETOOTH, Connectivity, NETWORK, TETHERING, VPN};

/// A device in memory: changes recorded and applied to what it reads.
#[derive(Default)]
struct Fake {
    readings: Mutex<Readings>,
    changes: Mutex<Vec<Change>>,
    refuse: Mutex<Option<String>>,
}

impl Fake {
    fn laptop() -> Arc<Self> {
        let fake = Self::default();
        {
            let mut readings = fake.readings.lock().expect("readings");
            readings.network = Network {
                wifi: Wifi {
                    available: true,
                    on: true,
                    hardware_blocked: false,
                    networks: vec![
                        network("Home", true, true, true),
                        network("Cafe", false, false, false),
                        network("Office", false, true, false),
                    ],
                },
                wired: Some(Wired::default()),
                mobile: Mobile::default(),
                saved: vec![
                    saved("Home", "uuid-home", true),
                    saved("Office", "uuid-office", false),
                ],
                ..Network::default()
            };
            readings.network.saved[0].proxy = Some("proxy.local:3128".into());
            readings.network.saved[0].metered = Metered::Metered;
            readings.network.saved[0].priority = 10;
            readings.bluetooth.available = true;
            readings.bluetooth.on = true;
            readings.bluetooth.accessories = vec![Accessory {
                name: "Headphones".into(),
                address: "AA:BB".into(),
                battery: Some(80),
                connected: false,
            }];
            readings.bluetooth.nearby = vec![Nearby {
                name: "Speaker".into(),
                address: "AA:CC".into(),
                signal: Some(-60),
            }];
            readings.vpn.profiles = vec![VpnProfile {
                name: "Work".into(),
                id: "uuid-work".into(),
                kind: "wireguard".into(),
                connected: false,
            }];
            readings.tethering.available = true;
        }
        Arc::new(fake)
    }

    /// A device with a modem instead of a wired link.
    fn phone() -> Arc<Self> {
        let fake = Self::default();
        {
            let mut readings = fake.readings.lock().expect("readings");
            readings.network = Network {
                wifi: Wifi {
                    available: true,
                    on: true,
                    hardware_blocked: false,
                    networks: vec![network("Home", true, true, false)],
                },
                mobile: Mobile {
                    available: true,
                    on: true,
                    profile: Some(MobileProfile {
                        id: "uuid-mobile".into(),
                        name: "Mobile".into(),
                        apn: None,
                        roaming: false,
                        metered: Metered::Metered,
                        autoconnect: true,
                    }),
                },
                ..Network::default()
            };
            readings.tethering.available = true;
        }
        Arc::new(fake)
    }

    fn changes(&self) -> Vec<Change> {
        self.changes.lock().expect("changes").clone()
    }
}

fn network(name: &str, known: bool, secure: bool, connected: bool) -> WifiNetwork {
    WifiNetwork {
        name: name.into(),
        signal: 3,
        secure,
        known,
        connected,
    }
}

/// A saved Wi-Fi network.
fn saved(name: &str, id: &str, autoconnect: bool) -> SavedNetwork {
    SavedNetwork {
        name: name.into(),
        id: id.into(),
        kind: SavedKind::Wifi,
        autoconnect,
        ..SavedNetwork::default()
    }
}

impl Device for Fake {
    fn read(&self) -> Readings {
        self.readings.lock().expect("readings").clone()
    }

    fn change(&self, change: &Change) -> Result<(), String> {
        self.changes.lock().expect("changes").push(change.clone());
        let refusal = self.refuse.lock().expect("refuse").clone();
        if let Some(refusal) = refusal {
            return Err(refusal);
        }
        {
            let mut readings = self.readings.lock().expect("readings");
            apply_network(&mut readings.network, change);
            apply_bluetooth(&mut readings.bluetooth, change);
            apply_vpn(&mut readings.vpn, change);
            apply_tethering(&mut readings.tethering, change);
        }
        Ok(())
    }
}

/// `change` to the network as the device then reads it.
fn apply_network(network: &mut Network, change: &Change) {
    match change {
        Change::Wifi(on) => network.wifi.on = *on,
        Change::Wired(on) => {
            if let Some(wired) = &mut network.wired {
                wired.connected = *on;
            }
        }
        Change::Airplane(on) => network.airplane_mode = *on,
        Change::MobileData(on) => network.mobile.on = *on,
        Change::Join { network: name, .. } => {
            for wifi in &mut network.wifi.networks {
                wifi.connected = wifi.name == *name;
            }
        }
        Change::Leave { network: name } => {
            for wifi in &mut network.wifi.networks {
                if wifi.name == *name {
                    wifi.connected = false;
                }
            }
        }
        Change::Forget { profile } => {
            network.saved.retain(|saved| saved.id != *profile);
        }
        Change::Autoconnect { profile, on } => {
            for saved in &mut network.saved {
                if saved.id == *profile {
                    saved.autoconnect = *on;
                }
            }
        }
        Change::Proxy { profile, server } => {
            for saved in &mut network.saved {
                if saved.id == *profile {
                    saved.proxy.clone_from(server);
                }
            }
        }
        Change::MobileApn { apn } => {
            if let Some(profile) = &mut network.mobile.profile {
                profile.apn.clone_from(apn);
            }
        }
        Change::MobileRoaming(on) => {
            if let Some(profile) = &mut network.mobile.profile {
                profile.roaming = *on;
            }
        }
        _ => {}
    }
}

/// `change` to Bluetooth as the device then reads it.
fn apply_bluetooth(bluetooth: &mut Bluetooth, change: &Change) {
    match change {
        Change::Bluetooth(on) => bluetooth.on = *on,
        Change::ScanBluetooth(on) => bluetooth.discovering = *on,
        Change::Connect { address } => {
            for accessory in &mut bluetooth.accessories {
                accessory.connected |= accessory.address == *address;
            }
        }
        Change::Disconnect { address } => {
            for accessory in &mut bluetooth.accessories {
                if accessory.address == *address {
                    accessory.connected = false;
                }
            }
        }
        Change::Pair { address } => {
            let paired = bluetooth
                .nearby
                .iter()
                .position(|nearby| nearby.address == *address);
            if let Some(paired) = paired {
                let nearby = bluetooth.nearby.remove(paired);
                bluetooth.accessories.push(Accessory {
                    name: nearby.name,
                    address: nearby.address,
                    battery: None,
                    connected: true,
                });
            }
        }
        Change::ForgetBluetooth { address } => {
            bluetooth
                .accessories
                .retain(|accessory| accessory.address != *address);
        }
        _ => {}
    }
}

/// `change` to the VPN profiles as the device then reads them.
fn apply_vpn(vpn: &mut Vpn, change: &Change) {
    match change {
        Change::Vpn { profile, on } => {
            for saved in &mut vpn.profiles {
                if saved.id == *profile {
                    saved.connected = *on;
                }
            }
        }
        Change::ImportVpn { kind, name, .. } => {
            vpn.profiles.push(VpnProfile {
                name: name.clone(),
                id: format!("uuid-{name}"),
                kind: kind.clone(),
                connected: false,
            });
        }
        Change::ForgetVpn { profile } => {
            vpn.profiles.retain(|saved| saved.id != *profile);
        }
        _ => {}
    }
}

/// `change` to the hotspot as the device then reads it.
fn apply_tethering(tethering: &mut Tethering, change: &Change) {
    match change {
        Change::Hotspot(on) => tethering.hotspot = *on,
        Change::ConfigureHotspot { name, band, .. } => {
            tethering.name = Some(name.clone());
            tethering.band = *band;
            tethering.hotspot = true;
        }
        _ => {}
    }
}

fn service(fake: &Arc<Fake>) -> Connectivity {
    let device: Arc<dyn Device> = Arc::clone(fake) as Arc<dyn Device>;
    let mut service = Connectivity::new(device, None).expect("service");
    service.settle();
    service
}

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn read(service: &Connectivity, resource: &str) -> Value {
    service.read(resource).expect("published")
}

/// A switch is taken at once, pending while the device works, and lands in
/// the reading.
#[test]
fn turning_wifi_off_lands_in_the_reading() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    assert_eq!(read(&service, NETWORK)["wifi"]["on"], json!(true));
    let started = service
        .invoke(NETWORK, "set-wifi", &arguments(json!({"on": false})))
        .expect("taken");
    assert_eq!(started, Started::Accepted);
    assert_eq!(read(&service, NETWORK)["pending"], json!(["set-wifi"]));
    service.settle();
    let network = read(&service, NETWORK);
    assert_eq!(network["wifi"]["on"], json!(false));
    assert_eq!(network["pending"], json!([]));
    assert_eq!(fake.changes(), [Change::Wifi(false)]);

    // A toggle reads what the switch is now.
    service
        .invoke(NETWORK, "toggle-wifi", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(fake.changes().last(), Some(&Change::Wifi(true)));
}

/// What the device refused is the resource's problem until the next change.
#[test]
fn a_refusal_is_the_problem() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    *fake.refuse.lock().expect("refuse") = Some("a switch holds it off".into());
    service
        .invoke(
            NETWORK,
            "set-airplane-mode",
            &arguments(json!({"on": true})),
        )
        .expect("taken");
    service.settle();
    assert_eq!(
        read(&service, NETWORK)["problem"],
        json!("a switch holds it off")
    );
    *fake.refuse.lock().expect("refuse") = None;
    service
        .invoke(
            NETWORK,
            "set-airplane-mode",
            &arguments(json!({"on": true})),
        )
        .expect("taken");
    service.settle();
    let network = read(&service, NETWORK);
    assert_eq!(network["problem"], Value::Null);
    assert_eq!(network["airplane-mode"], json!(true));
}

/// Each network in reach is a child offering to join it (or leave the one
/// connected); a protected one this device does not know asks for its
/// password, which never shows in a reading.
#[test]
fn networks_are_joined_from_their_children() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    let resources = service.resources();
    let network = resources
        .iter()
        .find(|r| r.id.as_str() == NETWORK)
        .expect("network");
    let children: Vec<(&str, &str)> = network
        .children
        .iter()
        .map(|child| (child.id.as_str(), child.actions[0].id.as_str()))
        .collect();
    assert_eq!(
        children,
        [
            ("wifi-network:Home", "disconnect-wifi"),
            ("wifi-network:Cafe", "join-wifi"),
            ("wifi-network:Office", "join-wifi"),
            ("saved-network:uuid-home", "set-network-autoconnect"),
            ("saved-network:uuid-office", "set-network-autoconnect"),
        ]
    );
    assert!(
        network.actions.iter().all(|spec| spec.id != "join-wifi"),
        "joining is offered on the networks only"
    );

    service
        .invoke("wifi-network:Cafe", "join-wifi", &Map::new())
        .expect("an open network joins");
    service.settle();
    assert_eq!(
        read(&service, NETWORK)["wifi"]["networks"][1]["connected"],
        json!(true)
    );

    let refused = service.invoke("wifi-network:Office", "join-wifi", &Map::new());
    assert!(
        matches!(&refused, Err(DoorError::Unavailable { code, .. }) if code == "password_needed"),
        "{refused:?}"
    );
    service
        .invoke(
            NETWORK,
            "connect-wifi",
            &arguments(json!({"network": "Office", "password": "hunter2"})),
        )
        .expect("taken with its password");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::Join {
            network: "Office".into(),
            password: Some("hunter2".into()),
            hidden: false,
        })
    );
    for resource in [NETWORK, BLUETOOTH, VPN, TETHERING] {
        assert!(!read(&service, resource).to_string().contains("hunter2"));
    }
}

/// What the device has not got is published unavailable, and refused.
#[test]
fn what_the_device_lacks_is_unavailable() {
    let fake = Arc::new(Fake::default());
    let mut service = service(&fake);
    for (resource, action, arguments) in [
        (NETWORK, "set-wifi", json!({"on": true})),
        (NETWORK, "set-wired", json!({"on": true})),
        (NETWORK, "set-mobile-data", json!({"on": true})),
        (NETWORK, "set-mobile-apn", json!({"apn": "internet"})),
        (NETWORK, "set-mobile-roaming", json!({"on": true})),
        (NETWORK, "connect-hidden-wifi", json!({"network": "Loft"})),
        (NETWORK, "toggle-airplane-mode", json!({})),
        (BLUETOOTH, "set-bluetooth", json!({"on": true})),
        (BLUETOOTH, "scan", json!({"on": true})),
        (BLUETOOTH, "pair", json!({"address": "AA:CC"})),
        (VPN, "set-vpn", json!({"profile": "x", "on": true})),
        (TETHERING, "set-hotspot", json!({"on": true})),
        (
            TETHERING,
            "configure-hotspot",
            json!({"name": "Loft", "password": "hunter2", "band": "five"}),
        ),
    ] {
        let refused = service.invoke(resource, action, &self::arguments(arguments));
        assert!(
            matches!(refused, Err(DoorError::Unavailable { .. })),
            "{resource} {action}: {refused:?}"
        );
    }
    assert!(fake.changes().is_empty());
    assert!(matches!(
        service.invoke(NETWORK, "set-wifi", &arguments(json!({}))),
        Err(DoorError::Unavailable { .. } | DoorError::InvalidArguments(_))
    ));
}

/// Accessories and VPN profiles are children too, each bound to what
/// pressing it does now.
#[test]
fn accessories_and_profiles_connect_from_their_children() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    service
        .invoke("bluetooth-accessory:AA:BB", "connect", &Map::new())
        .expect("taken");
    service
        .invoke("vpn-profile:uuid-work", "set-vpn", &Map::new())
        .expect("taken");
    service
        .invoke(TETHERING, "set-hotspot", &arguments(json!({"on": true})))
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes(),
        [
            Change::Connect {
                address: "AA:BB".into()
            },
            Change::Vpn {
                profile: "uuid-work".into(),
                on: true
            },
            Change::Hotspot(true),
        ]
    );
    assert_eq!(
        read(&service, BLUETOOTH)["accessories"][0]["connected"],
        json!(true)
    );
    assert_eq!(read(&service, VPN)["profiles"][0]["connected"], json!(true));
    assert_eq!(read(&service, TETHERING)["hotspot"], json!(true));
    let resources = service.resources();
    let vpn = resources
        .iter()
        .find(|r| r.id.as_str() == VPN)
        .expect("vpn");
    assert!(
        vpn.children[0].actions[0]
            .bound()
            .contains(&("on".to_owned(), json!(false))),
        "once connected, its child disconnects it"
    );
}

/// The actions a published child offers, by id; empty when it is gone.
fn child_actions(service: &Connectivity, id: &str) -> Vec<String> {
    service
        .resources()
        .iter()
        .flat_map(|resource| resource.children.iter())
        .find(|child| child.id.as_str() == id)
        .map(|child| {
            child
                .actions
                .iter()
                .map(|action| action.id.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// A network nobody announces is joined by the name typed in, with its
/// password when it has one.
#[test]
fn a_hidden_network_joins_by_the_name_it_does_not_announce() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    service
        .invoke(
            NETWORK,
            "connect-hidden-wifi",
            &arguments(json!({"network": "Loft", "password": "hunter2"})),
        )
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::Join {
            network: "Loft".into(),
            password: Some("hunter2".into()),
            hidden: true,
        })
    );
    // An open network nobody announces asks for no password.
    service
        .invoke(
            NETWORK,
            "connect-hidden-wifi",
            &arguments(json!({"network": "Cafe"})),
        )
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::Join {
            network: "Cafe".into(),
            password: None,
            hidden: true,
        })
    );
    assert!(!read(&service, NETWORK).to_string().contains("hunter2"));
}

/// The wired link is switched from the resource, and says what it carries.
#[test]
fn the_wired_link_is_switched_from_the_resource() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    service
        .invoke(NETWORK, "set-wired", &arguments(json!({"on": true})))
        .expect("taken");
    service.settle();
    assert_eq!(fake.changes(), [Change::Wired(true)]);
    assert_eq!(read(&service, NETWORK)["wired"]["connected"], json!(true));
    service
        .invoke(NETWORK, "set-wired", &arguments(json!({"on": false})))
        .expect("taken");
    service.settle();
    assert_eq!(read(&service, NETWORK)["wired"]["connected"], json!(false));
}

/// The networks this device keeps are children: each joins by itself
/// again or is forgotten, and one going through a proxy offers to stop.
#[test]
fn saved_networks_are_kept_and_forgotten_from_their_children() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    assert_eq!(
        child_actions(&service, "saved-network:uuid-home"),
        [
            "set-network-autoconnect",
            "forget-network",
            "set-network-proxy"
        ]
    );
    assert_eq!(
        child_actions(&service, "saved-network:uuid-office"),
        ["set-network-autoconnect", "forget-network"],
        "a network with no proxy offers no proxy to stop"
    );
    let saved = &read(&service, NETWORK)["saved"];
    assert_eq!(saved[0]["id"], json!("uuid-home"));
    assert_eq!(saved[0]["proxy"], json!("proxy.local:3128"));
    assert_eq!(saved[0]["metered"], json!("metered"));
    assert_eq!(saved[0]["priority"], json!(10));

    service
        .invoke(
            "saved-network:uuid-office",
            "set-network-autoconnect",
            &Map::new(),
        )
        .expect("taken");
    service
        .invoke("saved-network:uuid-home", "set-network-proxy", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes(),
        [
            Change::Autoconnect {
                profile: "uuid-office".into(),
                on: true,
            },
            Change::Proxy {
                profile: "uuid-home".into(),
                server: None,
            },
        ]
    );
    let saved = &read(&service, NETWORK)["saved"];
    assert_eq!(saved[1]["autoconnect"], json!(true));
    assert_eq!(saved[0]["proxy"], Value::Null);

    // A proxy is typed in as `host:port`; one that is not is refused.
    let refused = service.invoke(
        NETWORK,
        "set-network-proxy",
        &arguments(json!({"profile": "uuid-home", "server": "proxy.local"})),
    );
    assert!(
        matches!(refused, Err(DoorError::InvalidArguments(_))),
        "{refused:?}"
    );
    service
        .invoke(
            NETWORK,
            "set-network-proxy",
            &arguments(json!({"profile": "uuid-home", "server": "proxy.local:8080"})),
        )
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::Proxy {
            profile: "uuid-home".into(),
            server: Some("proxy.local:8080".into()),
        })
    );
    assert_eq!(
        read(&service, NETWORK)["saved"][0]["proxy"],
        json!("proxy.local:8080")
    );

    service
        .invoke("saved-network:uuid-office", "forget-network", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::Forget {
            profile: "uuid-office".into(),
        })
    );
    assert_eq!(
        read(&service, NETWORK)["saved"].as_array().map(Vec::len),
        Some(1)
    );
    assert!(child_actions(&service, "saved-network:uuid-office").is_empty());
}

/// Mobile data takes the access point name typed in, or the SIM's
/// operator's again, and whether it may roam.
#[test]
fn mobile_data_takes_its_access_point_and_roaming() {
    let fake = Fake::phone();
    let mut service = service(&fake);
    assert_eq!(
        read(&service, NETWORK)["mobile"]["profile"]["metered"],
        json!("metered")
    );
    service
        .invoke(
            NETWORK,
            "set-mobile-apn",
            &arguments(json!({"apn": "internet.apn"})),
        )
        .expect("taken");
    service
        .invoke(
            NETWORK,
            "set-mobile-roaming",
            &arguments(json!({"on": true})),
        )
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes(),
        [
            Change::MobileApn {
                apn: Some("internet.apn".into()),
            },
            Change::MobileRoaming(true),
        ]
    );
    let mobile = &read(&service, NETWORK)["mobile"];
    assert_eq!(mobile["profile"]["apn"], json!("internet.apn"));
    assert_eq!(mobile["profile"]["roaming"], json!(true));

    // Nothing typed means the operator chooses it again.
    service
        .invoke(NETWORK, "set-mobile-apn", &arguments(json!({"apn": "  "})))
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::MobileApn { apn: None })
    );
    assert_eq!(
        read(&service, NETWORK)["mobile"]["profile"]["apn"],
        Value::Null
    );
}

/// The hotspot takes the name and band it is set up with, and says who is
/// sharing the connection; its password never shows.
#[test]
fn the_hotspot_takes_a_name_a_band_and_shows_who_is_on_it() {
    let fake = Fake::laptop();
    fake.readings.lock().expect("readings").tethering.clients = vec![Client {
        address: "11:22:33:44:55:66".into(),
        signal: Some(-40),
    }];
    let mut service = service(&fake);
    service
        .invoke(
            TETHERING,
            "configure-hotspot",
            &arguments(json!({"name": "Loft", "password": "hunter2", "band": "five"})),
        )
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::ConfigureHotspot {
            name: "Loft".into(),
            password: "hunter2".into(),
            band: Band::Five,
        })
    );
    let tethering = read(&service, TETHERING);
    assert_eq!(tethering["name"], json!("Loft"));
    assert_eq!(tethering["band"], json!("five"));
    assert_eq!(tethering["hotspot"], json!(true));
    assert_eq!(
        tethering["clients"][0]["address"],
        json!("11:22:33:44:55:66")
    );
    assert!(!tethering.to_string().contains("hunter2"));
}

/// Bluetooth looks for devices in reach, pairs them and forgets them; a
/// radio that is off does neither.
#[test]
fn bluetooth_looks_for_devices_pairs_and_forgets_them() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    assert_eq!(child_actions(&service, "bluetooth-nearby:AA:CC"), ["pair"]);
    assert_eq!(
        child_actions(&service, "bluetooth-accessory:AA:BB"),
        ["connect", "forget"]
    );
    service
        .invoke(BLUETOOTH, "scan", &arguments(json!({"on": true})))
        .expect("taken");
    service.settle();
    assert_eq!(read(&service, BLUETOOTH)["discovering"], json!(true));

    service
        .invoke("bluetooth-nearby:AA:CC", "pair", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::Pair {
            address: "AA:CC".into(),
        })
    );
    let bluetooth = read(&service, BLUETOOTH);
    assert_eq!(bluetooth["accessories"][1]["name"], json!("Speaker"));
    assert_eq!(bluetooth["nearby"], json!([]));

    service
        .invoke("bluetooth-accessory:AA:CC", "forget", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::ForgetBluetooth {
            address: "AA:CC".into(),
        })
    );
    assert_eq!(
        read(&service, BLUETOOTH)["accessories"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    service
        .invoke(BLUETOOTH, "set-bluetooth", &arguments(json!({"on": false})))
        .expect("taken");
    service.settle();
    for action in ["scan", "pair"] {
        let arguments = arguments(json!({"on": true, "address": "AA:CC"}));
        let refused = service.invoke(BLUETOOTH, action, &arguments);
        assert!(
            matches!(&refused, Err(DoorError::Unavailable { code, .. }) if code == "bluetooth_off"),
            "{action}: {refused:?}"
        );
    }
}

/// A VPN is added from a configuration's text and removed by its child;
/// the text never shows in a reading.
#[test]
fn vpn_profiles_are_imported_and_forgotten() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    let text = "[Interface]\nPrivateKey = very-secret";
    service
        .invoke(
            VPN,
            "import-vpn",
            &arguments(json!({"kind": "wireguard", "name": "Home", "text": text})),
        )
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::ImportVpn {
            kind: "wireguard".into(),
            name: "Home".into(),
            text: text.into(),
        })
    );
    assert_eq!(read(&service, VPN)["profiles"][1]["name"], json!("Home"));
    assert!(!read(&service, VPN).to_string().contains("very-secret"));

    assert_eq!(
        child_actions(&service, "vpn-profile:uuid-Home"),
        ["set-vpn", "forget-vpn"]
    );
    service
        .invoke("vpn-profile:uuid-Home", "forget-vpn", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::ForgetVpn {
            profile: "uuid-Home".into(),
        })
    );
    assert!(child_actions(&service, "vpn-profile:uuid-Home").is_empty());
}
