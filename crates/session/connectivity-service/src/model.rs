//! What the connectivity resources read: plain data, the same for quick
//! settings, Settings, the top bar, a script and an agent.

use serde::{Deserialize, Serialize};

/// The `network` resource: the radios, the links and the networks in
/// reach.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Network {
    pub wifi: Wifi,
    /// The wired link; `None` without an Ethernet adapter.
    pub wired: Option<Wired>,
    pub mobile: Mobile,
    pub airplane_mode: bool,
    /// What `NetworkManager` found when it last checked the internet.
    pub connectivity: Connectivity,
    /// Actions taken and not yet answered by the device, by id.
    pub pending: Vec<String>,
    /// Why the last change did not happen, until the next one.
    pub problem: Option<String>,
}

/// Wi-Fi.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Wifi {
    /// The device has a Wi-Fi radio.
    pub available: bool,
    /// The radio is really on: switched on, and no kill switch holds it.
    pub on: bool,
    /// A physical switch holds it off; nothing here turns it on.
    pub hardware_blocked: bool,
    /// The networks in reach, the connected one first, then the strongest.
    pub networks: Vec<WifiNetwork>,
}

impl Wifi {
    /// The network this device is connected through.
    #[must_use]
    pub fn connected(&self) -> Option<&WifiNetwork> {
        self.networks.iter().find(|network| network.connected)
    }
}

/// One Wi-Fi network in reach.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct WifiNetwork {
    pub name: String,
    /// Signal from 0 to 4.
    pub signal: u8,
    /// It asks for a password (or more) to join.
    pub secure: bool,
    /// This device has joined it before and joins without asking.
    pub known: bool,
    pub connected: bool,
}

/// The wired link.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Wired {
    pub connected: bool,
    pub device: Option<String>,
    pub speed: Option<String>,
    pub ipv4: Option<String>,
}

/// Mobile data.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Mobile {
    /// The device has a modem.
    pub available: bool,
    pub on: bool,
}

/// What `NetworkManager` found when it checked the internet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Connectivity {
    #[default]
    Unknown,
    Offline,
    /// A captive portal asks to sign in.
    Portal,
    Limited,
    Full,
}

/// The `bluetooth` resource.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Bluetooth {
    /// The device has a Bluetooth adapter.
    pub available: bool,
    pub on: bool,
    /// The paired accessories, connected first.
    pub accessories: Vec<Accessory>,
    pub pending: Vec<String>,
    pub problem: Option<String>,
}

/// One paired Bluetooth accessory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Accessory {
    pub name: String,
    pub address: String,
    /// Battery percentage; `None` when it reports none.
    pub battery: Option<u8>,
    pub connected: bool,
}

/// The `vpn` resource: the VPN profiles this device has.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Vpn {
    pub profiles: Vec<VpnProfile>,
    pub pending: Vec<String>,
    pub problem: Option<String>,
}

/// One VPN profile.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct VpnProfile {
    pub name: String,
    /// `NetworkManager`'s id of it, which a rename keeps.
    pub id: String,
    /// What kind of tunnel (`wireguard`, `vpn`).
    pub kind: String,
    pub connected: bool,
}

/// The `tethering` resource: sharing this device's connection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Tethering {
    /// The device can offer a hotspot (it has Wi-Fi).
    pub available: bool,
    /// The hotspot is on.
    pub hotspot: bool,
    pub pending: Vec<String>,
    pub problem: Option<String>,
}

impl From<&os_actions::WifiNetworkInfo> for WifiNetwork {
    fn from(network: &os_actions::WifiNetworkInfo) -> Self {
        Self {
            name: network.ssid.clone(),
            signal: network.signal,
            secure: network.secure,
            known: network.known,
            connected: network.connected,
        }
    }
}

impl From<&os_actions::WiredInfo> for Wired {
    fn from(wired: &os_actions::WiredInfo) -> Self {
        Self {
            connected: wired.connected,
            device: wired.device.clone(),
            speed: wired.speed.clone(),
            ipv4: wired.ipv4.clone(),
        }
    }
}

impl From<os_actions::ConnectivityState> for Connectivity {
    fn from(state: os_actions::ConnectivityState) -> Self {
        match state {
            os_actions::ConnectivityState::Unknown => Self::Unknown,
            os_actions::ConnectivityState::Offline => Self::Offline,
            os_actions::ConnectivityState::Portal => Self::Portal,
            os_actions::ConnectivityState::Limited => Self::Limited,
            os_actions::ConnectivityState::Full => Self::Full,
        }
    }
}

impl From<&os_actions::BtDeviceInfo> for Accessory {
    fn from(device: &os_actions::BtDeviceInfo) -> Self {
        Self {
            name: device.name.clone(),
            address: device.mac.clone(),
            battery: (device.battery > 0).then_some(device.battery),
            connected: device.connected,
        }
    }
}

impl From<&os_actions::VpnConnectionInfo> for VpnProfile {
    fn from(vpn: &os_actions::VpnConnectionInfo) -> Self {
        Self {
            name: vpn.name.clone(),
            id: vpn.uuid.clone(),
            kind: vpn.kind.clone(),
            connected: vpn.active,
        }
    }
}

/// `networks` ordered as they are shown: the connected one first, then by
/// signal, strongest first, then by name; one entry per name.
#[must_use]
pub fn ordered(mut networks: Vec<WifiNetwork>) -> Vec<WifiNetwork> {
    networks.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.signal.cmp(&a.signal))
            .then_with(|| a.name.cmp(&b.name))
    });
    let mut seen = std::collections::BTreeSet::new();
    networks.retain(|network| !network.name.is_empty() && seen.insert(network.name.clone()));
    networks
}
