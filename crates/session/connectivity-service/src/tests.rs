use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use tilo_actions::DoorError;
use tilo_service::{Service, Started};

use crate::device::{Change, Device, Readings};
use crate::model::{Accessory, Mobile, Network, VpnProfile, Wifi, WifiNetwork, Wired};
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
                ..Network::default()
            };
            readings.bluetooth.available = true;
            readings.bluetooth.on = true;
            readings.bluetooth.accessories = vec![Accessory {
                name: "Headphones".into(),
                address: "AA:BB".into(),
                battery: Some(80),
                connected: false,
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
        let mut readings = self.readings.lock().expect("readings");
        match change {
            Change::Wifi(on) => readings.network.wifi.on = *on,
            Change::Airplane(on) => readings.network.airplane_mode = *on,
            Change::Bluetooth(on) => readings.bluetooth.on = *on,
            Change::Hotspot(on) => readings.tethering.hotspot = *on,
            Change::Join { network, .. } => {
                for wifi in &mut readings.network.wifi.networks {
                    wifi.connected = wifi.name == *network;
                }
            }
            Change::Connect { address } => {
                for accessory in &mut readings.bluetooth.accessories {
                    accessory.connected |= accessory.address == *address;
                }
            }
            Change::Vpn { profile, on } => {
                for vpn in &mut readings.vpn.profiles {
                    if vpn.id == *profile {
                        vpn.connected = *on;
                    }
                }
            }
            _ => {}
        }
        Ok(())
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
            password: Some("hunter2".into())
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
        (NETWORK, "toggle-airplane-mode", json!({})),
        (BLUETOOTH, "set-bluetooth", json!({"on": true})),
        (VPN, "set-vpn", json!({"profile": "x", "on": true})),
        (TETHERING, "set-hotspot", json!({"on": true})),
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
