use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use tilo_actions::ApiError;
use tilo_service::{Service, Started};

use os_actions::{EnterpriseEapMethod, EnterpriseWifiConfiguration};

use crate::device::{Change, Device, Readings};
use crate::model::{
    Accessory, Band, Bluetooth, Client, Link, LinkKind, Metered, Mobile, MobileProfile, Modem,
    ModemUnit, Nearby, Network, SavedKind, SavedNetwork, Tethering, Vpn, VpnProfile, Wifi,
    WifiNetwork, Wired,
};
use crate::{BLUETOOTH, Connectivity, MODEM, NETWORK, Preferences, TETHERING, VPN};

/// A device in memory: changes recorded and applied to what it reads.
#[derive(Default)]
struct Fake {
    readings: Mutex<Readings>,
    changes: Mutex<Vec<Change>>,
    refuse: Mutex<Option<String>>,
    /// The operators a scan finds.
    operators: Mutex<Vec<os_actions::Operator>>,
    /// What the network answers the next code, and what it was asked, in order.
    answers: Mutex<Vec<os_actions::CodeAnswer>>,
    asked: Mutex<Vec<String>>,
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
                link: Some(Link {
                    kind: LinkKind::Wifi,
                    name: Some("Home".into()),
                    signal: 3,
                }),
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
                ..Accessory::default()
            }];
            readings.bluetooth.nearby = vec![Nearby {
                name: "Speaker".into(),
                address: "AA:CC".into(),
                signal: Some(-60),
                ..Nearby::default()
            }];
            readings.vpn.profiles = vec![VpnProfile {
                name: "Work".into(),
                id: "uuid-work".into(),
                kind: "wireguard".into(),
                connected: false,
                ..VpnProfile::default()
            }];
            readings.tethering.available = true;
            readings.tethering.clients = vec![Client {
                address: "11:22:33:44:55:66".into(),
                signal: Some(-40),
            }];
        }
        Arc::new(fake)
    }

    /// A device with a modem: mobile data, and a SIM asking for its PIN.
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
            readings.modem = Modem {
                available: true,
                modems: vec![ModemUnit {
                    path: "/mm/modem0".into(),
                    name: "Phone modem".into(),
                    lock: crate::model::SimLock::Pin,
                    retries: crate::model::Retries {
                        pin: Some(2),
                        puk: None,
                    },
                    network_types: vec!["5g-4g-3g".into(), "4g-3g".into()],
                    network_type: Some("5g-4g-3g".into()),
                    sims: vec![crate::model::ModemSim {
                        path: "/mm/sim0".into(),
                        slot: Some(1),
                        active: true,
                        operator: Some("Tilo Mobile".into()),
                        label: "Tilo Mobile".into(),
                        kind: crate::model::SimKind::Physical,
                    }],
                    ..ModemUnit::default()
                }],
                ..Modem::default()
            };
        }
        Arc::new(fake)
    }

    fn changes(&self) -> Vec<Change> {
        self.changes.lock().expect("changes").clone()
    }

    /// What the network answers next: the next one queued, else a plain confirmation.
    fn answer(&self) -> os_actions::CodeAnswer {
        let mut answers = self.answers.lock().expect("answers");
        if answers.is_empty() {
            os_actions::CodeAnswer {
                text: "Done".into(),
                awaiting: false,
            }
        } else {
            answers.remove(0)
        }
    }

    /// The codes and answers the network was given, in order.
    fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("asked").clone()
    }
}

fn network(name: &str, known: bool, secure: bool, connected: bool) -> WifiNetwork {
    WifiNetwork {
        name: name.into(),
        signal: 3,
        secure,
        known,
        connected,
        ..WifiNetwork::default()
    }
}

/// A saved Wi-Fi network.
fn saved(name: &str, id: &str, autoconnect: bool) -> SavedNetwork {
    SavedNetwork {
        name: name.into(),
        profile: id.into(),
        kind: SavedKind::Wifi,
        autoconnect,
        ..SavedNetwork::default()
    }
}

impl Device for Fake {
    fn read(&self) -> Readings {
        self.readings.lock().expect("readings").clone()
    }

    fn scan_operators(&self, modem: &str) -> Result<Vec<os_actions::Operator>, String> {
        let readings = self.readings.lock().expect("readings");
        assert!(
            readings.modem.modems.iter().any(|held| held.path == modem),
            "a modem of this device: {modem}"
        );
        Ok(self.operators.lock().expect("operators").clone())
    }

    fn send_code(&self, _modem: &str, command: &str) -> Result<os_actions::CodeAnswer, String> {
        self.asked.lock().expect("asked").push(command.to_owned());
        if let Some(refusal) = self.refuse.lock().expect("refuse").clone() {
            return Err(refusal);
        }
        Ok(self.answer())
    }

    fn answer_code(&self, _modem: &str, response: &str) -> Result<os_actions::CodeAnswer, String> {
        self.asked
            .lock()
            .expect("asked")
            .push(format!("answer {response}"));
        Ok(self.answer())
    }

    fn cancel_code(&self, _modem: &str) -> Result<(), String> {
        self.asked.lock().expect("asked").push("cancel".into());
        Ok(())
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
            apply_modem(&mut readings.modem, change);
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
        Change::Enterprise { config, .. } => {
            for wifi in &mut network.wifi.networks {
                wifi.connected = wifi.name == config.ssid;
            }
        }
        Change::Forget { profile } => {
            network.saved.retain(|saved| saved.profile != *profile);
        }
        Change::Autoconnect { profile, on } => {
            for saved in &mut network.saved {
                if saved.profile == *profile {
                    saved.autoconnect = *on;
                }
            }
        }
        Change::OpenNetworks { ask } => {
            for saved in &mut network.saved {
                if saved.open {
                    saved.autoconnect = !*ask;
                }
            }
        }
        Change::Proxy { profile, server } => {
            for saved in &mut network.saved {
                if saved.profile == *profile {
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
        Change::Wake { profile, on } => {
            for saved in &mut network.saved {
                if saved.profile == *profile {
                    saved.wake = Some(crate::model::NetworkWake {
                        on: *on,
                        ..crate::model::NetworkWake::default()
                    });
                }
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
                    ..Accessory::default()
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
                ..VpnProfile::default()
            });
        }
        Change::ForgetVpn { profile } => {
            vpn.profiles.retain(|saved| saved.id != *profile);
        }
        Change::EditVpn { profile, patch } => {
            for saved in &mut vpn.profiles {
                if saved.id != *profile {
                    continue;
                }
                if let Some(name) = &patch.name {
                    saved.name.clone_from(name);
                }
                if let Some(on) = patch.autoconnect {
                    saved.autoconnect = on;
                }
                if let Some(on) = patch.all_traffic {
                    saved.all_traffic = on;
                }
                if let Some(dns) = &patch.dns {
                    saved.dns.clone_from(dns);
                }
                if let Some(mtu) = patch.mtu {
                    saved.mtu = mtu;
                }
                if let Some(endpoint) = &patch.endpoint {
                    saved.endpoint = Some(endpoint.clone());
                }
                if let Some(routes) = &patch.allowed_ips {
                    saved.allowed_ips.clone_from(routes);
                }
                if let Some(keepalive) = patch.keepalive {
                    saved.keepalive = keepalive;
                }
            }
        }
        _ => {}
    }
}

/// `change` to the modems as the device then reads them.
fn apply_modem(modem: &mut Modem, change: &Change) {
    match change {
        Change::ModemPin { sim, .. } => {
            for held in &mut modem.modems {
                if held.sims.iter().any(|held| held.path == *sim) {
                    held.lock = crate::model::SimLock::None;
                }
            }
        }
        Change::ModemPuk { sim, .. } => {
            for held in &mut modem.modems {
                if held.sims.iter().any(|held| held.path == *sim) {
                    held.lock = crate::model::SimLock::None;
                }
            }
        }
        Change::ModemPinLock { sim, on, .. } => {
            for held in &mut modem.modems {
                if held.sims.iter().any(|held| held.path == *sim) {
                    held.pin_enabled = Some(*on);
                }
            }
        }
        Change::ModemModes { modem: path, modes } => {
            for held in &mut modem.modems {
                if held.path == *path {
                    held.network_type = Some(modes.key());
                }
            }
        }
        Change::ModemUseSlot { modem: path, slot } => {
            for held in &mut modem.modems {
                if held.path == *path {
                    for sim in &mut held.sims {
                        sim.active = sim.slot == Some(*slot);
                    }
                }
            }
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

/// This device's policy as a test keeps it, in memory.
#[derive(Default)]
struct Policy(Mutex<bool>);

impl Preferences for Policy {
    fn ask_before_open_networks(&self) -> bool {
        *self.0.lock().expect("policy")
    }

    fn set_ask_before_open_networks(&self, on: bool) -> Result<(), String> {
        *self.0.lock().expect("policy") = on;
        Ok(())
    }
}

/// [`service`] over a policy of `ask`, which the test keeps.
fn service_asking(fake: &Arc<Fake>, ask: bool) -> (Connectivity, Arc<Policy>) {
    let policy = Arc::new(Policy(Mutex::new(ask)));
    let device: Arc<dyn Device> = Arc::clone(fake) as Arc<dyn Device>;
    let held: Arc<dyn Preferences> = Arc::clone(&policy) as Arc<dyn Preferences>;
    let mut service = Connectivity::over(device, None, held).expect("service");
    service.settle();
    (service, policy)
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

/// A switch is taken at once, pending while the device works, and lands in
/// the reading; the networks in reach and the networks this device keeps
/// join, leave and are forgotten from the children the resource publishes,
/// with a password that never shows.
#[test]
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
fn wifi_switches_and_joins_from_the_network_resource() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    let network = read(&service, NETWORK);
    assert_eq!(network["wifi"]["on"], json!(true));
    assert_eq!(
        network["link"]["kind"],
        json!("wifi"),
        "the connection traffic leaves on"
    );
    assert_eq!(network["link"]["name"], json!("Home"));
    assert_eq!(network["link"]["signal"], json!(3));

    let started = service
        .invoke(NETWORK, "set-wifi", &arguments(json!({"on": false})))
        .expect("taken");
    let Started::Later(answered) = started else {
        panic!("a change is answered once the device made it: {started:?}");
    };
    assert_eq!(read(&service, NETWORK)["pending"], json!(["set-wifi"]));
    service.settle();
    assert!(
        matches!(
            answered.wait_timeout(std::time::Duration::from_secs(5)),
            Ok(Some(Ok(_)))
        ),
        "whoever asked hears the change was made"
    );
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

    // What the device refused is the resource's problem until the next change.
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
    service
        .invoke(
            NETWORK,
            "set-airplane-mode",
            &arguments(json!({"on": false})),
        )
        .expect("taken");
    service.settle();

    // Each network in reach is a child offering to join it (or leave the one
    // connected); a protected one this device does not know asks for its
    // password.
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
            ("wifi-network:Home", "leave"),
            ("wifi-network:Cafe", "join"),
            ("wifi-network:Office", "join"),
            ("saved-network:uuid-home", "join"),
            ("saved-network:uuid-office", "join"),
        ]
    );
    assert!(
        network.actions.iter().all(|spec| spec.id != "join"),
        "joining is offered on the networks only"
    );
    // Each row of the reading carries the address its own actions are called on.
    let reading = read(&service, NETWORK);
    assert_eq!(
        reading["wifi"]["networks"][1]["$address"],
        json!("wifi-network:Cafe")
    );
    assert_eq!(
        reading["saved"][0]["$address"],
        json!("saved-network:uuid-home")
    );

    service
        .invoke("wifi-network:Cafe", "join", &Map::new())
        .expect("an open network joins");
    service.settle();
    assert_eq!(
        read(&service, NETWORK)["wifi"]["networks"][1]["connected"],
        json!(true)
    );
    let refused = service.invoke("wifi-network:Office", "join", &Map::new());
    assert!(
        matches!(&refused, Err(ApiError::Unavailable { code, .. }) if code == "password_needed"),
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

    // A network nobody announces is joined by the name typed in, with its
    // password when it has one.
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
    for resource in [NETWORK, BLUETOOTH, VPN, TETHERING] {
        assert!(!read(&service, resource).to_string().contains("hunter2"));
    }

    // A work or school network joins in one change carrying the identity, the
    // method and the certificates it was given.
    let mut enterprise = campus();
    enterprise["profile"] = json!("Campus");
    enterprise["anonymous_identity"] = json!("anonymous@example.edu");
    enterprise["method"] = json!("TTLS");
    enterprise["inner_auth"] = json!("PAP");
    enterprise["client_certificate"] = json!("/etc/ssl/certs/ada.pem");
    enterprise["private_key"] = json!("/etc/ssl/private/ada.key");
    service
        .invoke(NETWORK, "connect-enterprise-wifi", &arguments(enterprise))
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::Enterprise {
            config: EnterpriseWifiConfiguration {
                profile_name: "Campus".into(),
                ssid: "Campus".into(),
                identity: "ada@example.edu".into(),
                anonymous_identity: Some("anonymous@example.edu".into()),
                ca_certificate: "/etc/ssl/certs/campus.pem".into(),
                method: EnterpriseEapMethod::Ttls,
                phase2_auth: Some("pap".into()),
                client_certificate: Some("/etc/ssl/certs/ada.pem".into()),
                private_key: Some("/etc/ssl/private/ada.key".into()),
            },
            password: Some("hunter2".into()),
        })
    );
    assert!(!read(&service, NETWORK).to_string().contains("hunter2"));

    // An unnamed method, profile and inner authentication take the setup's
    // defaults.
    service
        .invoke(NETWORK, "connect-enterprise-wifi", &arguments(campus()))
        .expect("taken");
    service.settle();
    let Some(Change::Enterprise { config, .. }) = fake.changes().last().cloned() else {
        panic!("the join reached the device: {:?}", fake.changes());
    };
    assert_eq!(config.profile_name, "Campus", "the profile is the network");
    assert_eq!(config.method, EnterpriseEapMethod::Peap);
    assert_eq!(config.phase2_auth.as_deref(), Some("mschapv2"));

    // What a work or school network lacks, or a method nobody has, is refused
    // by name before anything reaches the device.
    let reached = fake.changes().len();
    let without = |field: &str| {
        let mut enterprise = campus();
        enterprise.as_object_mut().expect("an object").remove(field);
        enterprise
    };
    let with = |field: &str, value: &str| {
        let mut enterprise = campus();
        enterprise[field] = json!(value);
        enterprise
    };
    for (enterprise, field) in [
        (with("method", "LEAP"), "method"),
        (with("inner_auth", "GTC"), "inner authentication"),
        (without("identity"), "identity"),
        (without("ssid"), "ssid"),
        (without("ca_certificate"), "CA certificate"),
    ] {
        let refused = service.invoke(NETWORK, "connect-enterprise-wifi", &arguments(enterprise));
        assert!(
            matches!(&refused, Err(ApiError::InvalidArguments(reason)) if reason.contains(field)),
            "{field}: {refused:?}"
        );
    }
    assert_eq!(fake.changes().len(), reached, "nothing reached the device");

    // What the device refuses is the resource's problem.
    *fake.refuse.lock().expect("refuse") = Some("no certificate agent".into());
    service
        .invoke(NETWORK, "connect-enterprise-wifi", &arguments(campus()))
        .expect("taken");
    service.settle();
    assert_eq!(
        read(&service, NETWORK)["problem"],
        json!("no certificate agent")
    );
}

/// The arguments a work or school network joins with.
fn campus() -> Value {
    json!({
        "ssid": "Campus",
        "identity": "ada@example.edu",
        "ca_certificate": "/etc/ssl/certs/campus.pem",
        "password": "hunter2",
    })
}

/// The wired link is switched from the resource, and the networks this
/// device keeps are children that join by themselves again, take a proxy or
/// are forgotten.
#[test]
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
fn the_wired_link_and_saved_networks_are_controlled_from_their_resources() {
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

    assert_eq!(
        child_actions(&service, "saved-network:uuid-home"),
        [
            "join",
            "set-autoconnect",
            "forget",
            "set-proxy",
            "set-wake",
            "share-with-devices"
        ]
    );
    assert_eq!(
        child_actions(&service, "saved-network:uuid-office"),
        [
            "join",
            "set-autoconnect",
            "forget",
            "set-proxy",
            "set-wake",
            "share-with-devices"
        ],
        "a network with no proxy is given one by its own action"
    );
    let saved = &read(&service, NETWORK)["saved"];
    assert_eq!(saved[0]["profile"], json!("uuid-home"));
    assert_eq!(saved[0]["proxy"], json!("proxy.local:3128"));
    assert_eq!(saved[0]["metered"], json!("metered"));
    assert_eq!(saved[0]["priority"], json!(10));

    service
        .invoke(
            "saved-network:uuid-office",
            "set-autoconnect",
            &arguments(json!({"on": true})),
        )
        .expect("taken");
    service
        .invoke("saved-network:uuid-home", "set-proxy", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes()[2..].to_vec(),
        [
            Change::Autoconnect {
                profile: "uuid-office".into(),
                on: true,
            },
            Change::Proxy {
                profile: "uuid-home".into(),
                server: None,
            },
        ],
        "the wired link was switched first"
    );
    let saved = &read(&service, NETWORK)["saved"];
    assert_eq!(saved[1]["autoconnect"], json!(true));
    assert_eq!(saved[0]["proxy"], Value::Null);

    // A proxy is typed in as `host:port`; one that is not is refused.
    let refused = service.invoke(
        "saved-network:uuid-home",
        "set-proxy",
        &arguments(json!({"server": "proxy.local"})),
    );
    assert!(
        matches!(refused, Err(ApiError::InvalidArguments(_))),
        "{refused:?}"
    );
    service
        .invoke(
            "saved-network:uuid-home",
            "set-proxy",
            &arguments(json!({"server": "proxy.local:8080"})),
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
        .invoke("saved-network:uuid-office", "forget", &Map::new())
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

/// Accessories, VPN profiles, the hotspot and the modem are each bound to
/// what pressing them does now, and what the device cannot do is refused by
/// name.
#[test]
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
fn accessories_profiles_the_hotspot_and_the_modem_act_from_their_children() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    assert_eq!(child_actions(&service, "bluetooth-nearby:AA:CC"), ["pair"]);
    assert_eq!(
        child_actions(&service, "bluetooth-accessory:AA:BB"),
        ["connect", "forget"]
    );
    assert_eq!(
        child_actions(&service, "vpn-profile:uuid-work"),
        ["connect", "edit", "forget"]
    );
    service
        .invoke("bluetooth-accessory:AA:BB", "connect", &Map::new())
        .expect("taken");
    service
        .invoke("vpn-profile:uuid-work", "connect", &Map::new())
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
    assert_eq!(
        vpn.children[0].actions[0].id, "disconnect",
        "once connected, its child disconnects it"
    );

    // Bluetooth looks for devices in reach, pairs them and forgets them; a
    // radio that is off does neither.
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
    let refused = service.invoke(BLUETOOTH, "scan", &arguments(json!({"on": true})));
    assert!(
        matches!(&refused, Err(ApiError::Unavailable { code, .. }) if code == "bluetooth_off"),
        "scan: {refused:?}"
    );
    assert!(
        child_actions(&service, "bluetooth-nearby:AA:CC").is_empty(),
        "with the radio off nothing is in reach to pair"
    );

    // A VPN is added from a configuration's text and removed by its child;
    // the text never shows in a reading.
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
        ["connect", "edit", "forget"]
    );
    service
        .invoke("vpn-profile:uuid-Home", "forget", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::ForgetVpn {
            profile: "uuid-Home".into(),
        })
    );
    assert!(child_actions(&service, "vpn-profile:uuid-Home").is_empty());

    // The hotspot takes the name and band it is set up with, and says who is
    // sharing the connection; its password never shows.
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

    // Mobile data takes the access point name typed in, or the SIM's
    // operator's again, and whether it may roam.
    let phone = Fake::phone();
    let mut service = self::service(&phone);
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
        phone.changes(),
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
    service
        .invoke(NETWORK, "set-mobile-apn", &arguments(json!({"apn": "  "})))
        .expect("taken");
    service.settle();
    assert_eq!(
        phone.changes().last(),
        Some(&Change::MobileApn { apn: None })
    );
    assert_eq!(
        read(&service, NETWORK)["mobile"]["profile"]["apn"],
        Value::Null
    );
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
        (NETWORK, "connect-enterprise-wifi", json!({})),
        (NETWORK, "toggle-airplane-mode", json!({})),
        (BLUETOOTH, "set-bluetooth", json!({"on": true})),
        (BLUETOOTH, "scan", json!({"on": true})),
        (TETHERING, "set-hotspot", json!({"on": true})),
        (
            TETHERING,
            "configure-hotspot",
            json!({"name": "Loft", "password": "hunter2", "band": "five"}),
        ),
    ] {
        let refused = service.invoke(resource, action, &self::arguments(arguments));
        assert!(
            matches!(refused, Err(ApiError::Unavailable { .. })),
            "{resource} {action}: {refused:?}"
        );
    }
    assert!(fake.changes().is_empty());
    assert!(matches!(
        service.invoke(NETWORK, "set-wifi", &arguments(json!({}))),
        Err(ApiError::Unavailable { .. } | ApiError::InvalidArguments(_))
    ));
}

/// A locked SIM is unlocked with its PIN: the code is what the change
/// carries, and the reading says the lock is gone.
#[test]
fn a_locked_sim_unlocks_with_its_pin() {
    let fake = Fake::phone();
    let mut service = service(&fake);
    assert_eq!(read(&service, MODEM)["modems"][0]["lock"], json!("pin"));
    service
        .invoke(MODEM, "unlock-sim", &arguments(json!({"pin": "1234"})))
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::ModemPin {
            sim: "/mm/sim0".into(),
            pin: "1234".into(),
        })
    );
    assert_eq!(read(&service, MODEM)["modems"][0]["lock"], json!("none"));
    assert!(!read(&service, MODEM).to_string().contains("1234"));
}

/// A modem's network types are chosen by the key it lists, and a key it does
/// not list is refused.
#[test]
fn a_modem_takes_the_network_types_it_lists() {
    let fake = Fake::phone();
    let mut service = service(&fake);
    assert_eq!(
        read(&service, MODEM)["modems"][0]["network-types"],
        json!(["5g-4g-3g", "4g-3g"])
    );
    service
        .invoke(
            MODEM,
            "set-preferred-network",
            &arguments(json!({"network": "4g-3g"})),
        )
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::ModemModes {
            modem: "/mm/modem0".into(),
            modes: os_actions::NetworkModes {
                allowed: 1 << 3 | 1 << 2,
                preferred: 0,
            },
        })
    );
    let refused = service.invoke(
        MODEM,
        "set-preferred-network",
        &arguments(json!({"network": "2g"})),
    );
    assert!(matches!(refused, Err(ApiError::Unavailable { .. })));
}

/// A VPN profile's own page changes its settings one field at a time, over
/// the settings the reading holds.
#[test]
fn a_vpn_profile_changes_its_settings() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    let edit = |service: &mut Connectivity, patch: Value| {
        service
            .invoke("vpn-profile:uuid-work", "edit", &arguments(patch))
            .expect("taken");
        service.settle();
    };
    edit(&mut service, json!({"name": "Office"}));
    edit(&mut service, json!({"dns": ["10.0.0.1"]}));
    let vpn = read(&service, VPN);
    assert_eq!(vpn["profiles"][0]["name"], json!("Office"));
    assert_eq!(vpn["profiles"][0]["dns"], json!(["10.0.0.1"]));
    assert_eq!(
        vpn["profiles"][0]["$address"],
        json!("vpn-profile:uuid-work"),
        "the row carries the address its actions are called on"
    );

    edit(&mut service, json!({"dns": []}));
    assert_eq!(read(&service, VPN)["profiles"][0]["dns"], json!([]));
}

/// Every connectivity resource is published as a surface a view compiles
/// against, the modem and the VPN included.
#[test]
fn the_connectivity_resources_are_surfaces() {
    for (surface, kind, label) in [
        (crate::network_surface(), "network", "Network"),
        (crate::bluetooth_surface(), "bluetooth", "Bluetooth"),
        (crate::modem_surface(), "modem", "Modem"),
        (crate::vpn_surface(), "vpn", "VPN"),
        (crate::tethering_surface(), "tethering", "Tethering"),
    ] {
        assert_eq!(surface.kind.as_str(), kind);
        assert_eq!(surface.label, label);
    }
}

/// A saved network wakes this device when asked to, and sharing its password with the person's
/// other devices is asked of the device, which says why when it cannot.
#[test]
fn a_saved_network_wakes_the_device_and_its_password_is_shared_by_the_device() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    assert_eq!(read(&service, NETWORK)["saved"][0]["wake"], Value::Null);
    service
        .invoke(
            "saved-network:uuid-home",
            "set-wake",
            &arguments(json!({"on": true})),
        )
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::Wake {
            profile: "uuid-home".into(),
            on: true,
        })
    );
    assert_eq!(
        read(&service, NETWORK)["saved"][0]["wake"]["on"],
        json!(true)
    );

    let before = fake.changes().len();
    service
        .invoke("saved-network:uuid-home", "share-with-devices", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes()[before..].to_vec(),
        [Change::ShareWifi {
            profile: "uuid-home".into(),
        }],
        "the device is asked to share it"
    );
}

/// A laptop whose Wi-Fi keeps a network that asks for no password, which `NetworkManager` would
/// join by itself.
fn laptop_with_open_network() -> Arc<Fake> {
    let fake = Fake::default();
    {
        let mut readings = fake.readings.lock().expect("readings");
        readings.network = Network {
            wifi: Wifi {
                available: true,
                on: true,
                hardware_blocked: false,
                networks: vec![network("Cafe", false, false, false)],
            },
            saved: vec![SavedNetwork {
                name: "Cafe".into(),
                profile: "uuid-cafe".into(),
                kind: SavedKind::Wifi,
                autoconnect: true,
                open: true,
                ..SavedNetwork::default()
            }],
            ..Network::default()
        };
    }
    Arc::new(fake)
}

/// An open network this device keeps does not join by itself while it asks first: the policy is
/// kept, the device's profile follows it, and a network saved later is kept too.
#[test]
fn an_open_network_waits_for_the_person_while_the_device_asks() {
    let fake = laptop_with_open_network();
    let (mut service, policy) = service_asking(&fake, false);
    service.settle();
    assert!(
        fake.changes().is_empty(),
        "the device keeps joining it by itself until the person asks first: {:?}",
        fake.changes()
    );
    assert_eq!(
        read(&service, NETWORK)["saved"][0]["open"],
        json!(true),
        "the pane knows which networks ask for no password"
    );
    assert_eq!(
        read(&service, NETWORK)["ask-before-open-networks"],
        json!(false)
    );

    service
        .invoke(
            NETWORK,
            "set-ask-before-open-networks",
            &self::arguments(json!({"on": true})),
        )
        .expect("taken");
    service.settle();
    assert!(policy.ask_before_open_networks(), "the policy is kept");
    assert_eq!(
        fake.changes().last(),
        Some(&Change::OpenNetworks { ask: true }),
        "the open profile follows the policy"
    );
    let network = read(&service, NETWORK);
    assert_eq!(network["ask-before-open-networks"], json!(true));
    assert_eq!(network["saved"][0]["autoconnect"], json!(false));

    // A network saved after the policy was set, as a join saves one, waits too.
    {
        let mut readings = fake.readings.lock().expect("readings");
        readings.network.saved.push(SavedNetwork {
            name: "Hotel".into(),
            profile: "uuid-hotel".into(),
            kind: SavedKind::Wifi,
            autoconnect: true,
            open: true,
            ..SavedNetwork::default()
        });
    }
    service.refresh();
    service.settle();
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::OpenNetworks { ask: true }),
        "the network saved later is put in step"
    );
    let network = read(&service, NETWORK);
    assert_eq!(network["saved"][1]["name"], json!("Hotel"));
    assert_eq!(network["saved"][1]["autoconnect"], json!(false));

    // The person lets them join by themselves again: each follows its own setting.
    service
        .invoke(
            NETWORK,
            "set-ask-before-open-networks",
            &self::arguments(json!({"on": false})),
        )
        .expect("taken");
    service.settle();
    assert!(!policy.ask_before_open_networks());
    assert_eq!(
        fake.changes().last(),
        Some(&Change::OpenNetworks { ask: false })
    );
    let network = read(&service, NETWORK);
    assert_eq!(network["ask-before-open-networks"], json!(false));
    assert_eq!(network["saved"][0]["autoconnect"], json!(true));
}

/// A modem lists the operators it reaches, stands on the one chosen, and the network answers the
/// codes the person sends, asking on when it wants a reply.
#[test]
fn a_modem_lists_its_operators_and_the_network_answers_a_code() {
    let fake = Fake::phone();
    *fake.operators.lock().expect("operators") = vec![
        os_actions::Operator {
            code: "26202".into(),
            name: "Feldnetz".into(),
            availability: os_actions::Availability::Available,
        },
        os_actions::Operator {
            code: "26201".into(),
            name: "Stadtnetz".into(),
            availability: os_actions::Availability::Current,
        },
    ];
    let mut service = service(&fake);
    assert_eq!(read(&service, MODEM)["modems"][0]["operators"], json!([]));

    service
        .invoke(MODEM, "scan-operators", &Map::new())
        .expect("taken");
    assert_eq!(
        read(&service, MODEM)["pending"],
        json!(["scan-operators"]),
        "the scan is pending until the modem answers"
    );
    service.settle();
    let modem = &read(&service, MODEM)["modems"][0];
    let operators: Vec<(&str, &str)> = modem["operators"]
        .as_array()
        .expect("operators")
        .iter()
        .map(|operator| {
            (
                operator["name"].as_str().expect("a name"),
                operator["state"].as_str().expect("a state"),
            )
        })
        .collect();
    assert_eq!(
        operators,
        [("Stadtnetz", "current"), ("Feldnetz", "available")],
        "the operator it stands on first"
    );
    assert_eq!(read(&service, MODEM)["pending"], json!([]));

    // Choosing one by hand, and letting the modem choose.
    service
        .invoke(
            MODEM,
            "select-operator",
            &arguments(json!({"operator": "26202"})),
        )
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::ModemRegister {
            modem: "/mm/modem0".into(),
            operator: Some("26202".into()),
        })
    );
    service
        .invoke(MODEM, "select-operator", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(
        fake.changes().last(),
        Some(&Change::ModemRegister {
            modem: "/mm/modem0".into(),
            operator: None,
        })
    );

    // A code the network answers outright.
    *fake.answers.lock().expect("answers") = vec![os_actions::CodeAnswer {
        text: "Balance: 10.00".into(),
        awaiting: false,
    }];
    service
        .invoke(MODEM, "send-code", &arguments(json!({"code": "*100#"})))
        .expect("taken");
    service.settle();
    let modem = &read(&service, MODEM)["modems"][0];
    assert_eq!(modem["code"]["text"], json!("Balance: 10.00"));
    assert_eq!(modem["code"]["awaiting"], json!(false));

    // A menu, which waits for the person's answer and then ends.
    *fake.answers.lock().expect("answers") = vec![
        os_actions::CodeAnswer {
            text: "1 balance, 2 bundle".into(),
            awaiting: true,
        },
        os_actions::CodeAnswer {
            text: "Bundle: 5 GB".into(),
            awaiting: false,
        },
    ];
    service
        .invoke(MODEM, "send-code", &arguments(json!({"code": "*101#"})))
        .expect("taken");
    service.settle();
    assert_eq!(
        read(&service, MODEM)["modems"][0]["code"]["awaiting"],
        json!(true)
    );
    service
        .invoke(MODEM, "answer-code", &arguments(json!({"answer": "2"})))
        .expect("taken");
    service.settle();
    let modem = &read(&service, MODEM)["modems"][0];
    assert_eq!(modem["code"]["text"], json!("Bundle: 5 GB"));
    assert_eq!(modem["code"]["awaiting"], json!(false));
    assert_eq!(
        fake.asked(),
        ["*100#", "*101#", "answer 2"],
        "the network was given what the person sent"
    );

    service
        .invoke(MODEM, "cancel-code", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(read(&service, MODEM)["modems"][0]["code"], Value::Null);
    assert_eq!(fake.asked().last().map(String::as_str), Some("cancel"));
}

/// A code the network refuses is the modem resource's problem, and leaves nothing behind.
#[test]
fn a_code_the_network_refuses_says_why() {
    let fake = Fake::phone();
    let mut service = service(&fake);
    *fake.refuse.lock().expect("refuse") = Some("a SIM lock holds the modem".into());
    service
        .invoke(MODEM, "send-code", &arguments(json!({"code": "*100#"})))
        .expect("taken");
    service.settle();
    assert_eq!(
        read(&service, MODEM)["problem"],
        json!("a SIM lock holds the modem")
    );
    assert_eq!(read(&service, MODEM)["modems"][0]["code"], Value::Null);
}
