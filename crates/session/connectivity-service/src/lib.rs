//! The session's connectivity service.
//!
//! It publishes four resources, each a reading of the device and the
//! actions that change it:
//!
//! * `network`: Wi-Fi (the radio and the networks in reach, each a child
//!   offering to join or leave it), the wired link, mobile data, airplane
//!   mode and whether the internet answers;
//! * `bluetooth`: the radio and the paired accessories, each a child
//!   offering to connect or disconnect it;
//! * `vpn`: the VPN profiles, each a child offering to connect or
//!   disconnect it;
//! * `tethering`: the hotspot sharing this device's connection.
//!
//! Quick settings, Settings, the top bar, the palette, a script and an
//! agent read the same readings and invoke the same actions. An action the
//! device cannot do now (no adapter, a switch holding the radio off) is
//! published unavailable, with why. The device calls run on the service's
//! worker; while one runs its action is `pending` in the reading, and why a
//! change did not happen is its `problem` until the next.
//!
//! The readings follow the device: the session calls [`Connectivity::refresh`]
//! when `NetworkManager` or `BlueZ` say something changed
//! ([`Connectivity::follow`]), and after every change.

pub mod device;
pub mod effects;
pub mod model;

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::{DoorError, ResourceSpec};
use tilo_resource::Effect as _;
use tilo_service::{Jobs, Service, Started, Wake, Worker, decode, offer, reading, unavailable};

use device::{Change, Device, Readings};
use effects::{BluetoothAction, NetworkAction, TetheringAction, VpnAction};
use model::{Bluetooth, Network, Tethering, Vpn};

/// The resources' ids.
pub const NETWORK: &str = "network";
pub const BLUETOOTH: &str = "bluetooth";
pub const VPN: &str = "vpn";
pub const TETHERING: &str = "tethering";

/// Which resource a change belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Network,
    Bluetooth,
    Vpn,
    Tethering,
}

/// What lands from the worker.
enum Landed {
    /// A fresh reading.
    Read(Box<Readings>),
    /// A change was made (or not), and the device read after it.
    Changed {
        part: Part,
        action: String,
        result: Result<(), String>,
        readings: Box<Readings>,
    },
}

/// The connectivity service.
pub struct Connectivity {
    readings: Readings,
    worker: Worker<dyn Device, Landed>,
}

impl Connectivity {
    /// The service over `device`, reading it once at the start; `wake` is
    /// called whenever something landed for the session to take in.
    ///
    /// # Errors
    /// The worker's thread did not start.
    pub fn new(device: Arc<dyn Device>, wake: Option<Wake>) -> std::io::Result<Self> {
        let service = Self {
            readings: Readings::default(),
            worker: Worker::spawn("tilo-connectivity", device, wake)?,
        };
        service.refresh();
        Ok(service)
    }

    /// Read the device again.
    pub fn refresh(&self) {
        refresh(&self.worker.jobs());
    }

    /// Read the device again whenever `NetworkManager` or `BlueZ` report a
    /// change, for as long as the returned followers live.
    ///
    /// # Errors
    /// The followers did not start.
    pub fn follow(&self) -> Result<Follow, String> {
        let jobs = self.worker.jobs();
        let network = os_actions::network::follow_network_changes(move || refresh(&jobs))?;
        let jobs = self.worker.jobs();
        let bluetooth = os_actions::bluez::follow_changes(move || refresh(&jobs))?;
        Ok(Follow {
            _network: network,
            _bluetooth: bluetooth,
        })
    }

    /// What the device reads now.
    #[must_use]
    pub const fn readings(&self) -> &Readings {
        &self.readings
    }

    fn land(&mut self, landed: Landed) {
        match landed {
            Landed::Read(readings) => self.take(*readings),
            Landed::Changed {
                part,
                action,
                result,
                readings,
            } => {
                self.take(*readings);
                let (pending, problem) = self.status(part);
                if let Some(at) = pending.iter().position(|id| *id == action) {
                    pending.remove(at);
                }
                *problem = result.err();
            }
        }
    }

    /// Take a fresh reading, keeping what is the service's own: the actions
    /// pending and the last problem.
    fn take(&mut self, mut readings: Readings) {
        let old = &mut self.readings;
        readings.network.pending = std::mem::take(&mut old.network.pending);
        readings.network.problem = old.network.problem.take();
        readings.bluetooth.pending = std::mem::take(&mut old.bluetooth.pending);
        readings.bluetooth.problem = old.bluetooth.problem.take();
        readings.vpn.pending = std::mem::take(&mut old.vpn.pending);
        readings.vpn.problem = old.vpn.problem.take();
        readings.tethering.pending = std::mem::take(&mut old.tethering.pending);
        readings.tethering.problem = old.tethering.problem.take();
        self.readings = readings;
    }

    fn status(&mut self, part: Part) -> (&mut Vec<String>, &mut Option<String>) {
        let readings = &mut self.readings;
        match part {
            Part::Network => (&mut readings.network.pending, &mut readings.network.problem),
            Part::Bluetooth => (
                &mut readings.bluetooth.pending,
                &mut readings.bluetooth.problem,
            ),
            Part::Vpn => (&mut readings.vpn.pending, &mut readings.vpn.problem),
            Part::Tethering => (
                &mut readings.tethering.pending,
                &mut readings.tethering.problem,
            ),
        }
    }

    /// Make `change` on the worker, then read the device back.
    fn start(&mut self, part: Part, action: &str, change: Change) -> Started {
        self.status(part).0.push(action.to_owned());
        let action = action.to_owned();
        self.worker.run(move |device| {
            let result = device.change(&change);
            Landed::Changed {
                part,
                action,
                result,
                readings: Box::new(device.read()),
            }
        });
        Started::Accepted
    }

    fn network(&mut self, action: NetworkAction) -> Result<Started, DoorError> {
        let id = action.id();
        let network = &self.readings.network;
        let change = match action {
            NetworkAction::SetWifi { on } => Change::Wifi(on),
            NetworkAction::ToggleWifi => Change::Wifi(!network.wifi.on),
            NetworkAction::ScanWifi => Change::Scan,
            NetworkAction::JoinWifi { network: name } => {
                let known = network.wifi.networks.iter().find(|n| n.name == name);
                if known.is_some_and(|n| n.secure && !n.known) {
                    return Err(DoorError::Unavailable {
                        code: "password_needed".into(),
                        reason: format!("{name} asks for a password: join it with connect-wifi"),
                    });
                }
                Change::Join {
                    network: name,
                    password: None,
                }
            }
            NetworkAction::ConnectWifi { network, password } => Change::Join {
                network,
                password: Some(password),
            },
            NetworkAction::DisconnectWifi { network } => Change::Leave { network },
            NetworkAction::SetWired { on } => Change::Wired(on),
            NetworkAction::SetMobileData { on } => Change::MobileData(on),
            NetworkAction::SetAirplaneMode { on } => Change::Airplane(on),
            NetworkAction::ToggleAirplaneMode => Change::Airplane(!network.airplane_mode),
        };
        Ok(self.start(Part::Network, id, change))
    }

    fn bluetooth(&mut self, action: BluetoothAction) -> Started {
        let id = action.id();
        let change = match action {
            BluetoothAction::SetBluetooth { on } => Change::Bluetooth(on),
            BluetoothAction::ToggleBluetooth => Change::Bluetooth(!self.readings.bluetooth.on),
            BluetoothAction::Connect { address } => Change::Connect { address },
            BluetoothAction::Disconnect { address } => Change::Disconnect { address },
        };
        self.start(Part::Bluetooth, id, change)
    }
}

/// Following `NetworkManager` and `BlueZ`; it stops when dropped.
pub struct Follow {
    _network: os_actions::network::NetworkFollow,
    _bluetooth: os_actions::bluez::BluetoothFollow,
}

fn refresh(jobs: &Jobs<dyn Device, Landed>) {
    jobs.run(|device| Landed::Read(Box::new(device.read())));
}

/// A child resource's id: its kind and its key.
fn child_id(kind: &str, key: &str) -> Option<String> {
    (!key.is_empty() && !key.contains(tilo_actions::resource::KEY_SEP))
        .then(|| format!("{kind}:{key}"))
}

/// The `network` resource reading `network`.
#[must_use]
pub fn network_resource(network: &Network) -> ResourceSpec {
    let mut resource = offer::<NetworkAction>(ResourceSpec::new(
        NETWORK,
        "so.tilo.connectivity.network",
        "Network",
    ));
    if !network.wifi.available {
        for action in ["set-wifi", "toggle-wifi", "scan-wifi", "connect-wifi"] {
            unavailable(&mut resource, action, "no_wifi", "This device has no Wi-Fi");
        }
    } else if network.wifi.hardware_blocked {
        for action in ["set-wifi", "toggle-wifi"] {
            unavailable(
                &mut resource,
                action,
                "hardware_switch",
                "A switch on the device holds Wi-Fi off",
            );
        }
    } else if !network.wifi.on {
        for action in ["scan-wifi", "connect-wifi"] {
            unavailable(&mut resource, action, "wifi_off", "Wi-Fi is off");
        }
    }
    if network.wired.is_none() {
        unavailable(
            &mut resource,
            "set-wired",
            "no_wired",
            "This device has no wired network",
        );
    }
    if !network.mobile.available {
        unavailable(
            &mut resource,
            "set-mobile-data",
            "no_modem",
            "This device has no mobile data",
        );
    }
    if !network.wifi.available && !network.mobile.available {
        for action in ["set-airplane-mode", "toggle-airplane-mode"] {
            unavailable(
                &mut resource,
                action,
                "no_radio",
                "This device has no wireless radio",
            );
        }
    }
    let children = network.wifi.networks.iter().filter_map(|wifi| {
        let id = child_id("wifi-network", &wifi.name)?;
        let action = if wifi.connected {
            NetworkAction::DisconnectWifi {
                network: wifi.name.clone(),
            }
        } else {
            NetworkAction::JoinWifi {
                network: wifi.name.clone(),
            }
        };
        Some(
            ResourceSpec::new(id, "so.tilo.connectivity.wifi-network", wifi.name.clone())
                .with_action(action.bound()),
        )
    });
    resource.with_children(children)
}

/// The `bluetooth` resource reading `bluetooth`.
#[must_use]
pub fn bluetooth_resource(bluetooth: &Bluetooth) -> ResourceSpec {
    let mut resource = offer::<BluetoothAction>(ResourceSpec::new(
        BLUETOOTH,
        "so.tilo.connectivity.bluetooth",
        "Bluetooth",
    ));
    if !bluetooth.available {
        for action in ["set-bluetooth", "toggle-bluetooth"] {
            unavailable(
                &mut resource,
                action,
                "no_adapter",
                "This device has no Bluetooth",
            );
        }
    }
    let children = bluetooth.accessories.iter().filter_map(|accessory| {
        let id = child_id("bluetooth-accessory", &accessory.address)?;
        let action = if accessory.connected {
            BluetoothAction::Disconnect {
                address: accessory.address.clone(),
            }
        } else {
            BluetoothAction::Connect {
                address: accessory.address.clone(),
            }
        };
        Some(
            ResourceSpec::new(
                id,
                "so.tilo.connectivity.bluetooth-accessory",
                accessory.name.clone(),
            )
            .with_action(action.bound()),
        )
    });
    resource.with_children(children)
}

/// The `vpn` resource reading `vpn`.
#[must_use]
pub fn vpn_resource(vpn: &Vpn) -> ResourceSpec {
    let mut resource =
        offer::<VpnAction>(ResourceSpec::new(VPN, "so.tilo.connectivity.vpn", "VPN"));
    if vpn.profiles.is_empty() {
        unavailable(
            &mut resource,
            "set-vpn",
            "no_profile",
            "This device has no VPN profile",
        );
    }
    let children = vpn.profiles.iter().filter_map(|profile| {
        let id = child_id("vpn-profile", &profile.id)?;
        let action = VpnAction::SetVpn {
            profile: profile.id.clone(),
            on: !profile.connected,
        };
        Some(
            ResourceSpec::new(id, "so.tilo.connectivity.vpn-profile", profile.name.clone())
                .with_action(action.bound()),
        )
    });
    resource.with_children(children)
}

/// The `tethering` resource reading `tethering`.
#[must_use]
pub fn tethering_resource(tethering: &Tethering) -> ResourceSpec {
    let mut resource = offer::<TetheringAction>(ResourceSpec::new(
        TETHERING,
        "so.tilo.connectivity.tethering",
        "Tethering",
    ));
    if !tethering.available {
        unavailable(
            &mut resource,
            "set-hotspot",
            "no_wifi",
            "This device has no Wi-Fi to share over",
        );
    }
    resource
}

impl Service for Connectivity {
    fn resources(&self) -> Vec<ResourceSpec> {
        let readings = &self.readings;
        vec![
            network_resource(&readings.network),
            bluetooth_resource(&readings.bluetooth),
            vpn_resource(&readings.vpn),
            tethering_resource(&readings.tethering),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        let readings = &self.readings;
        match resource {
            NETWORK => Some(reading(&readings.network)),
            BLUETOOTH => Some(reading(&readings.bluetooth)),
            VPN => Some(reading(&readings.vpn)),
            TETHERING => Some(reading(&readings.tethering)),
            _ => None,
        }
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        let published = self.resources();
        // A child's action belongs to its parent's vocabulary.
        let parent = resource.split_once(':').map_or(resource, |(kind, _)| kind);
        match parent {
            NETWORK | "wifi-network" => {
                let action = decode::<NetworkAction>(&published, resource, action, arguments)?;
                self.network(action)
            }
            BLUETOOTH | "bluetooth-accessory" => {
                let action = decode::<BluetoothAction>(&published, resource, action, arguments)?;
                Ok(self.bluetooth(action))
            }
            VPN | "vpn-profile" => {
                let VpnAction::SetVpn { profile, on } =
                    decode::<VpnAction>(&published, resource, action, arguments)?;
                Ok(self.start(Part::Vpn, "set-vpn", Change::Vpn { profile, on }))
            }
            TETHERING => {
                let TetheringAction::SetHotspot { on } =
                    decode::<TetheringAction>(&published, resource, action, arguments)?;
                Ok(self.start(Part::Tethering, "set-hotspot", Change::Hotspot(on)))
            }
            _ => Err(DoorError::Unknown(tilo_actions::ActionAddress::new(
                resource, action,
            ))),
        }
    }

    fn poll(&mut self) -> bool {
        let landed = self.worker.landed();
        let changed = !landed.is_empty();
        for landed in landed {
            self.land(landed);
        }
        changed
    }

    fn settle(&mut self) {
        for landed in self.worker.settle() {
            self.land(landed);
        }
    }
}

#[cfg(test)]
mod tests;
