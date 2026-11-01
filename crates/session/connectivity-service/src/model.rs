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
    /// The networks this device keeps, in the order it lists them.
    pub saved: Vec<SavedNetwork>,
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

/// A network this device keeps: what it joins by itself and how it is set
/// up. Its id is `NetworkManager`'s, which a rename keeps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct SavedNetwork {
    pub name: String,
    /// `NetworkManager`'s id of the profile; the `profile` every action on it
    /// takes.
    pub id: String,
    pub kind: SavedKind,
    /// It joins by itself when it is in reach.
    pub autoconnect: bool,
    /// When this device last used it, in seconds since the Unix epoch.
    pub last_used: Option<u64>,
    /// Which saved network wins when several are in reach: higher first.
    pub priority: i32,
    pub metered: Metered,
    /// The proxy every connection through it uses, `host:port`.
    pub proxy: Option<String>,
    /// The Wi-Fi profile keeps its name hidden.
    pub hidden: bool,
}

/// What kind of connection a saved network is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SavedKind {
    #[default]
    Wifi,
    Wired,
    /// Mobile broadband.
    Mobile,
    /// Anything else `NetworkManager` keeps that is not a VPN or a bridge.
    Other,
}

impl From<&str> for SavedKind {
    fn from(kind: &str) -> Self {
        match kind {
            "802-11-wireless" => Self::Wifi,
            "802-3-ethernet" => Self::Wired,
            "gsm" | "cdma" => Self::Mobile,
            _ => Self::Other,
        }
    }
}

/// Whether a connection counts as metered for the things that pay for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Metered {
    /// `NetworkManager` decides.
    #[default]
    Automatic,
    Metered,
    Unmetered,
}

impl From<os_actions::MeteredPolicy> for Metered {
    fn from(policy: os_actions::MeteredPolicy) -> Self {
        match policy {
            os_actions::MeteredPolicy::Automatic => Self::Automatic,
            os_actions::MeteredPolicy::Metered => Self::Metered,
            os_actions::MeteredPolicy::Unmetered => Self::Unmetered,
        }
    }
}

impl From<Metered> for os_actions::MeteredPolicy {
    fn from(metered: Metered) -> Self {
        match metered {
            Metered::Automatic => Self::Automatic,
            Metered::Metered => Self::Metered,
            Metered::Unmetered => Self::Unmetered,
        }
    }
}

/// Mobile data.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Mobile {
    /// The device has a modem.
    pub available: bool,
    pub on: bool,
    /// The profile mobile data connects with; `None` before one is made.
    pub profile: Option<MobileProfile>,
}

/// The mobile data profile: what the operator's network is reached with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct MobileProfile {
    /// `NetworkManager`'s id of the profile.
    pub id: String,
    pub name: String,
    /// The access point name typed in; `None` while it is chosen from the
    /// provider database for the SIM.
    pub apn: Option<String>,
    /// It may connect on another operator's network.
    pub roaming: bool,
    pub metered: Metered,
    /// It connects by itself whenever the modem can.
    pub autoconnect: bool,
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
    /// The devices in reach that are not paired, strongest signal first.
    pub nearby: Vec<Nearby>,
    /// This device is looking for accessories now.
    pub discovering: bool,
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

/// A Bluetooth device in reach that is not paired.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Nearby {
    pub name: String,
    pub address: String,
    /// Signal during discovery, in dBm.
    pub signal: Option<i16>,
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
    /// The hotspot's network name; `None` before one is configured.
    pub name: Option<String>,
    pub band: Band,
    /// The devices sharing this connection now.
    pub clients: Vec<Client>,
    pub pending: Vec<String>,
    pub problem: Option<String>,
}

/// Which band the hotspot uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Band {
    #[default]
    Automatic,
    /// The long-range band everything has.
    TwoPointFour,
    /// The fast band, where the device has it.
    Five,
}

impl From<os_actions::HotspotBand> for Band {
    fn from(band: os_actions::HotspotBand) -> Self {
        match band {
            os_actions::HotspotBand::Automatic => Self::Automatic,
            os_actions::HotspotBand::TwoPointFourGhz => Self::TwoPointFour,
            os_actions::HotspotBand::FiveGhz => Self::Five,
        }
    }
}

impl From<Band> for os_actions::HotspotBand {
    fn from(band: Band) -> Self {
        match band {
            Band::Automatic => Self::Automatic,
            Band::TwoPointFour => Self::TwoPointFourGhz,
            Band::Five => Self::FiveGhz,
        }
    }
}

impl std::str::FromStr for Band {
    type Err = String;

    /// A band as it is typed: nothing or `automatic`, `two-point-four` or
    /// `five`.
    fn from_str(band: &str) -> Result<Self, Self::Err> {
        match band.trim().to_ascii_lowercase().as_str() {
            "" | "automatic" | "auto" => Ok(Self::Automatic),
            "two-point-four" | "2.4" | "2.4ghz" => Ok(Self::TwoPointFour),
            "five" | "5" | "5ghz" => Ok(Self::Five),
            other => Err(format!(
                "{other} is not a band: automatic, two-point-four or five"
            )),
        }
    }
}

/// A device sharing the hotspot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Client {
    pub address: String,
    /// Signal in dBm.
    pub signal: Option<i32>,
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

impl From<&os_actions::bluez::DeviceInfo> for Accessory {
    fn from(device: &os_actions::bluez::DeviceInfo) -> Self {
        Self {
            name: device.name.clone(),
            address: device.address.clone(),
            battery: device.battery.filter(|percentage| *percentage > 0),
            connected: device.connected,
        }
    }
}

impl From<&os_actions::NearbyBtDevice> for Nearby {
    fn from(device: &os_actions::NearbyBtDevice) -> Self {
        Self {
            name: device.name.clone(),
            address: device.address.clone(),
            signal: device.rssi,
        }
    }
}

impl From<&os_actions::SavedNetworkProfile> for SavedNetwork {
    fn from(profile: &os_actions::SavedNetworkProfile) -> Self {
        Self {
            name: profile.name.clone(),
            id: profile.uuid.clone(),
            kind: SavedKind::from(profile.kind.as_str()),
            autoconnect: profile.autoconnect,
            last_used: profile.last_used,
            ..Self::default()
        }
    }
}

impl From<&os_actions::modem::MobileProfile> for MobileProfile {
    fn from(profile: &os_actions::modem::MobileProfile) -> Self {
        Self {
            id: profile.uuid.clone(),
            name: profile.name.clone(),
            apn: profile.apn.clone(),
            roaming: profile.roaming,
            metered: Metered::from(profile.metered),
            autoconnect: profile.autoconnect,
        }
    }
}

impl From<&os_actions::network::HotspotClient> for Client {
    fn from(client: &os_actions::network::HotspotClient) -> Self {
        Self {
            address: client.mac.clone(),
            signal: client.signal_dbm,
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
