//! The device under the service: what it reads and what it changes. The
//! session's is [`OsDevice`] (`NetworkManager` and `BlueZ` through
//! `os-actions`); a test lends one of its own.

use crate::model::{
    Accessory, Band, Bluetooth, Client, Connectivity, Metered, Mobile, MobileProfile, Nearby,
    Network, SavedNetwork, Tethering, Vpn, VpnProfile, Wifi, WifiNetwork, Wired, ordered,
};

/// A change to make on the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Wifi(bool),
    Airplane(bool),
    Wired(bool),
    MobileData(bool),
    Hotspot(bool),
    /// Name the hotspot, set its password and band, and turn it on.
    ConfigureHotspot {
        name: String,
        password: String,
        band: Band,
    },
    /// Look for networks again.
    Scan,
    /// Join a network, with its password when it asks for one; `hidden` when
    /// it does not broadcast its name.
    Join {
        network: String,
        password: Option<String>,
        hidden: bool,
    },
    Leave {
        network: String,
    },
    /// Join an enterprise (802.1X) network with its identity and the
    /// organization's certificates, and connect.
    Enterprise {
        config: os_actions::EnterpriseWifiConfiguration,
        password: Option<String>,
    },
    /// Forget a saved network.
    Forget {
        profile: String,
    },
    /// Join a saved network by itself, or stop.
    Autoconnect {
        profile: String,
        on: bool,
    },
    /// Send a saved network's traffic through a proxy, or straight out.
    Proxy {
        profile: String,
        server: Option<String>,
    },
    /// Type the mobile data profile's access point name, or choose it from
    /// the SIM's operator again.
    MobileApn {
        apn: Option<String>,
    },
    /// Let mobile data connect on another operator's network, or stop.
    MobileRoaming(bool),
    Bluetooth(bool),
    Connect {
        address: String,
    },
    Disconnect {
        address: String,
    },
    /// Look for Bluetooth devices, or stop.
    ScanBluetooth(bool),
    /// Pair, trust and connect the device at `address`.
    Pair {
        address: String,
    },
    /// Forget the device at `address`.
    ForgetBluetooth {
        address: String,
    },
    Vpn {
        profile: String,
        on: bool,
    },
    /// Add the VPN in `text`, a configuration file's contents.
    ImportVpn {
        kind: String,
        name: String,
        text: String,
    },
    ForgetVpn {
        profile: String,
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

/// The networks this device keeps, each with the settings a person changes.
fn saved_networks() -> Vec<SavedNetwork> {
    let profiles = read_or_log("saved networks", os_actions::list_saved_network_profiles())
        .unwrap_or_default();
    profiles
        .iter()
        .map(SavedNetwork::from)
        .map(with_settings)
        .collect()
}

/// `saved` with the per-profile settings `NetworkManager` has for it.
fn with_settings(mut saved: SavedNetwork) -> SavedNetwork {
    let Some(settings) = read_or_log(
        "saved network settings",
        os_actions::connection_profile_settings(&saved.id),
    ) else {
        return saved;
    };
    saved.priority = settings.priority.unwrap_or_default();
    if let Some(metered) = settings.metered {
        saved.metered = Metered::from(metered);
    }
    saved.proxy = settings.proxy.and_then(|proxy| proxy.manual);
    saved.hidden = settings.hidden.unwrap_or_default();
    saved
}

/// The hotspot's name and band, and who shares it while it is on.
fn tethering(available: bool, hotspot: bool) -> Tethering {
    let details = read_or_log("hotspot", os_actions::hotspot_details()).flatten();
    let clients = if hotspot {
        read_or_log("hotspot clients", os_actions::list_hotspot_clients())
            .unwrap_or_default()
            .iter()
            .map(Client::from)
            .collect()
    } else {
        Vec::new()
    };
    Tethering {
        available,
        hotspot,
        name: details
            .as_ref()
            .map(|details| details.ssid.clone())
            .filter(|name| !name.is_empty()),
        band: details.map_or(Band::Automatic, |details| Band::from(details.band)),
        clients,
        ..Tethering::default()
    }
}

/// The Bluetooth devices in reach that are not paired, strongest first.
fn nearby() -> Vec<Nearby> {
    read_or_log("bluetooth devices in reach", os_actions::nearby_bluetooth())
        .unwrap_or_default()
        .iter()
        .filter(|device| !device.paired)
        .map(Nearby::from)
        .collect()
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
            adapter && read_or_log("bluetooth", os_actions::bluez::is_powered()).unwrap_or(false);
        let accessories = if bluetooth_on {
            read_or_log("bluetooth accessories", os_actions::bluez::paired_devices())
                .unwrap_or_default()
                .iter()
                .map(Accessory::from)
                .collect()
        } else {
            Vec::new()
        };
        let discovering = bluetooth_on
            && read_or_log("bluetooth discovery", os_actions::scanning_bluetooth())
                .unwrap_or(false);
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
                    profile: read_or_log("mobile profile", os_actions::read_mobile_profile())
                        .flatten()
                        .map(|profile| MobileProfile::from(&profile)),
                },
                airplane_mode: radios.airplane_mode,
                connectivity,
                saved: saved_networks(),
                ..Network::default()
            },
            bluetooth: Bluetooth {
                available: adapter,
                on: bluetooth_on,
                accessories,
                nearby: if bluetooth_on { nearby() } else { Vec::new() },
                discovering,
                ..Bluetooth::default()
            },
            vpn: Vpn {
                profiles,
                ..Vpn::default()
            },
            tethering: tethering(radios.wifi_available, radios.hotspot_active),
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
            Change::ConfigureHotspot {
                name,
                password,
                band,
            } => os_actions::configure_hotspot(&os_actions::HotspotConfiguration {
                ssid: name.clone(),
                password: password.clone(),
                band: (*band).into(),
            }),
            Change::Scan => os_actions::scan_wifi(),
            Change::Join {
                network,
                password,
                hidden,
            } => {
                if *hidden {
                    os_actions::network_settings::connect_hidden_wifi(network, password.as_deref())
                } else {
                    os_actions::connect_wifi(network, password.as_deref())
                }
            }
            Change::Leave { network } => os_actions::disconnect_wifi(network),
            Change::Enterprise { config, password } => {
                os_actions::connect_enterprise_wifi(config, password.as_deref().unwrap_or_default())
            }
            Change::Forget { profile } => os_actions::forget_network_profile(profile),
            Change::Autoconnect { profile, on } => {
                os_actions::set_network_autoconnect(profile, *on)
            }
            Change::Proxy { profile, server } => os_actions::configure_connection_policy_patch(
                profile,
                &os_actions::ConnectionPolicyPatch {
                    proxy: Some(os_actions::ProxyConfiguration {
                        automatic_url: None,
                        manual: server.clone(),
                        browser_only: false,
                    }),
                    ..os_actions::ConnectionPolicyPatch::default()
                },
            ),
            Change::MobileApn { apn } => {
                let profile = os_actions::modem::mobile_profile_uuid()?;
                os_actions::set_mobile_apn(&profile, apn.as_deref())
            }
            Change::MobileRoaming(on) => {
                let profile = os_actions::modem::mobile_profile_uuid()?;
                os_actions::set_mobile_roaming(&profile, *on)
            }
            Change::Bluetooth(on) => RealDispatcher.set_bluetooth(*on),
            Change::Connect { address } => os_actions::bluez::connect_address(address),
            Change::Disconnect { address } => os_actions::bluez::disconnect_address(address),
            Change::ScanBluetooth(on) => os_actions::scan_bluetooth(*on),
            Change::Pair { address } => os_actions::pair_bluetooth_handoff(address),
            Change::ForgetBluetooth { address } => os_actions::forget_bluetooth(address),
            Change::Vpn { profile, on } => os_actions::set_vpn_connection_uuid(profile, *on),
            Change::ImportVpn { kind, name, text } => {
                os_actions::network_settings::import_vpn_text(kind, name, text).map(|_| ())
            }
            Change::ForgetVpn { profile } => os_actions::delete_vpn_connection_uuid(profile),
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
