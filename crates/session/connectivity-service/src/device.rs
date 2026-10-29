//! The device under the service: what it reads and what it changes. The
//! session's is [`OsDevice`] (`NetworkManager` and `BlueZ` through
//! `os-actions`); a test lends one of its own.

use crate::model::{
    Accessory, Bluetooth, Connectivity, Mobile, Network, Tethering, Vpn, VpnProfile, Wifi,
    WifiNetwork, Wired, ordered,
};

/// A change to make on the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Wifi(bool),
    Airplane(bool),
    Wired(bool),
    MobileData(bool),
    Hotspot(bool),
    /// Look for networks again.
    Scan,
    /// Join a network, with its password when it asks for one.
    Join {
        network: String,
        password: Option<String>,
    },
    Leave {
        network: String,
    },
    Bluetooth(bool),
    Connect {
        address: String,
    },
    Disconnect {
        address: String,
    },
    Vpn {
        profile: String,
        on: bool,
    },
}

/// What the device reads now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Readings {
    pub network: Network,
    pub bluetooth: Bluetooth,
    pub vpn: Vpn,
    pub tethering: Tethering,
}

/// The device: reads and changes. Called on the service's worker, never
/// on the session's loop.
pub trait Device: Send + Sync {
    /// Read everything.
    fn read(&self) -> Readings;

    /// Make `change`.
    ///
    /// # Errors
    /// Why the device did not make it.
    fn change(&self, change: &Change) -> Result<(), String>;
}

/// The device as `NetworkManager` and `BlueZ` report it.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsDevice;

/// `result` as an option, saying why it is missing in the journal.
fn read_or_log<T>(what: &str, result: Result<T, String>) -> Option<T> {
    result
        .map_err(|error| tracing::debug!(%error, what, "a connectivity reading failed"))
        .ok()
}

impl Device for OsDevice {
    fn read(&self) -> Readings {
        let radios = read_or_log("radios", os_actions::get_radio_state()).unwrap_or_default();
        let networks = if radios.wifi_enabled {
            read_or_log("wifi networks", os_actions::list_wifi_networks())
                .unwrap_or_default()
                .iter()
                .map(WifiNetwork::from)
                .collect()
        } else {
            Vec::new()
        };
        let wired = read_or_log("wired", os_actions::get_wired())
            .filter(|wired| wired.device.is_some())
            .map(|wired| Wired::from(&wired));
        let connectivity = read_or_log("connectivity", os_actions::get_connectivity())
            .map_or(Connectivity::Unknown, Connectivity::from);
        let profiles = read_or_log("vpn profiles", os_actions::list_vpn_connections())
            .unwrap_or_default()
            .iter()
            .map(VpnProfile::from)
            .collect();
        let adapter = os_actions::bluez::has_adapter();
        let bluetooth_on =
            adapter && read_or_log("bluetooth", os_actions::get_bluetooth()).unwrap_or(false);
        let accessories = if bluetooth_on {
            read_or_log("bluetooth accessories", os_actions::list_bt_devices())
                .unwrap_or_default()
                .iter()
                .map(Accessory::from)
                .collect()
        } else {
            Vec::new()
        };
        Readings {
            network: Network {
                wifi: Wifi {
                    available: radios.wifi_available,
                    on: radios.wifi_enabled,
                    hardware_blocked: radios.wifi_hardware_blocked,
                    networks: ordered(networks),
                },
                wired,
                mobile: Mobile {
                    available: radios.wwan_available,
                    on: radios.wwan_enabled,
                },
                airplane_mode: radios.airplane_mode,
                connectivity,
                ..Network::default()
            },
            bluetooth: Bluetooth {
                available: adapter,
                on: bluetooth_on,
                accessories,
                ..Bluetooth::default()
            },
            vpn: Vpn {
                profiles,
                ..Vpn::default()
            },
            tethering: Tethering {
                available: radios.wifi_available,
                hotspot: radios.hotspot_active,
                ..Tethering::default()
            },
        }
    }

    fn change(&self, change: &Change) -> Result<(), String> {
        use os_actions::{Dispatcher as _, RadioChange, RealDispatcher};
        match change {
            Change::Wifi(on) => radio(RadioChange::Wifi(*on)),
            Change::Airplane(on) => radio(RadioChange::Airplane(*on)),
            Change::Wired(on) => os_actions::set_wired(*on),
            Change::MobileData(on) => os_actions::set_mobile_broadband(*on),
            Change::Hotspot(on) => os_actions::set_hotspot(*on),
            Change::Scan => os_actions::scan_wifi(),
            Change::Join { network, password } => {
                os_actions::connect_wifi(network, password.as_deref())
            }
            Change::Leave { network } => os_actions::disconnect_wifi(network),
            Change::Bluetooth(on) => RealDispatcher.set_bluetooth(*on),
            Change::Connect { address } => os_actions::bt_connect(address),
            Change::Disconnect { address } => os_actions::bt_disconnect(address),
            Change::Vpn { profile, on } => os_actions::set_vpn_connection_uuid(profile, *on),
        }
    }
}

/// Make a radio change and check the radios show it: a switch may hold a
/// radio off although the change was taken.
fn radio(change: os_actions::RadioChange) -> Result<(), String> {
    use os_actions::Dispatcher as _;
    let radios = match os_actions::RealDispatcher.apply_radio_change(change)? {
        Some(radios) => radios,
        None => os_actions::get_radio_state()?,
    };
    change.outcome(&radios)
}
