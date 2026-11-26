//! What the connectivity resources read: plain data, the same for quick
//! settings, Settings, the top bar, a script and an agent.

use serde::{Deserialize, Serialize};

/// The `network` resource: the radios, the links and the networks in
/// reach.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Network {
    pub wifi: Wifi,
    /// The wired link; `None` without an Ethernet adapter.
    pub wired: Option<Wired>,
    pub mobile: Mobile,
    pub airplane_mode: bool,
    /// What `NetworkManager` found when it last checked the internet.
    pub connectivity: Connectivity,
    /// The connection this machine's traffic leaves on: what the top bar
    /// draws in its network glyph. `None` when the device could not be read,
    /// which is not the same as offline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
    /// The networks this device keeps, in the order it lists them.
    pub saved: Vec<SavedNetwork>,
    /// Whether this device asks before it joins an open Wi-Fi network it keeps: on, none of
    /// them joins by itself.
    pub ask_before_open_networks: bool,
    /// Actions taken and not yet answered by the device, by id.
    pub pending: Vec<String>,
    /// Why the last change did not happen, until the next one.
    pub problem: Option<String>,
}

/// Wi-Fi.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
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
    /// Why the last search for networks failed, until the next one: what
    /// `NetworkManager` said, or that it could not be reached.
    pub scan_problem: Option<String>,
}

impl Wifi {
    /// The network this device is connected through.
    #[must_use]
    pub fn connected(&self) -> Option<&WifiNetwork> {
        self.networks.iter().find(|network| network.connected)
    }
}

/// One Wi-Fi network in reach.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct WifiNetwork {
    /// The network as the resource it is (`wifi-network:Home`): what its own actions
    /// (`join`, `leave`) are called on. The service sets it as it publishes the network.
    #[serde(rename = "$address")]
    pub address: tilo_resource::Handle<crate::WifiNetworkRow>,
    pub name: String,
    /// Signal from 0 to 4.
    pub signal: u8,
    /// It asks for a password (or more) to join.
    pub secure: bool,
    /// This device has joined it before and joins without asking.
    pub known: bool,
    pub connected: bool,
    /// Why joining it failed last time, until the next attempt: a wrong
    /// password, or the network refusing the device.
    pub problem: Option<String>,
}

/// The connection this machine's traffic leaves on.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Link {
    pub kind: LinkKind,
    /// What the connection is called: the network's name on Wi-Fi, the
    /// profile's on a cable. `None` while nothing is connected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Signal strength from 0 to 4 on Wi-Fi, `0` on a cable and offline.
    pub signal: u8,
}

/// What kind of connection it is.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum LinkKind {
    /// Nothing is connected.
    #[default]
    Offline,
    Wired,
    Wifi,
}

impl From<&os_actions::LinkStatus> for Link {
    fn from(link: &os_actions::LinkStatus) -> Self {
        Self {
            kind: match link.kind {
                os_actions::LinkKind::Offline => LinkKind::Offline,
                os_actions::LinkKind::Wired => LinkKind::Wired,
                os_actions::LinkKind::Wifi => LinkKind::Wifi,
            },
            name: link.name.clone(),
            signal: link.signal,
        }
    }
}

/// The wired link.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Wired {
    pub connected: bool,
    pub device: Option<String>,
    pub speed: Option<String>,
    pub ipv4: Option<String>,
}

/// A network this device keeps: what it joins by itself and how it is set
/// up. Its id is `NetworkManager`'s, which a rename keeps.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct SavedNetwork {
    /// The network as the resource it is (`saved-network:<id>`): what its own actions
    /// (`join`, `forget`, `set-autoconnect`) are called on. The service sets it as it
    /// publishes the network.
    #[serde(rename = "$address")]
    pub address: tilo_resource::Handle<crate::SavedNetworkRow>,
    pub name: String,
    /// `NetworkManager`'s id of the profile, which its address carries and which the network's
    /// per-profile actions take: a view reads it here, since `id` is the resource's own.
    pub profile: String,
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
    /// A Wi-Fi network that joins without a password.
    pub open: bool,
    /// How its addresses are set; `None` when the device could not read them.
    pub ipv4: Option<SavedIpv4>,
    /// Whether a wake packet over it wakes this device; `None` when the device could not read it.
    pub wake: Option<NetworkWake>,
    /// Who a work or school network knows this device as; `None` when the network is not one.
    pub enterprise: Option<SavedEnterprise>,
}

/// Who a work or school network knows this device as, and the certificate it checks the network
/// with.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct SavedEnterprise {
    /// The identity the network knows this device as.
    pub identity: String,
    /// The certificate that verifies the network, as a path; empty when it is not checked.
    pub ca_certificate: String,
}

impl From<&os_actions::network_settings::EnterpriseIdentity> for SavedEnterprise {
    fn from(identity: &os_actions::network_settings::EnterpriseIdentity) -> Self {
        Self {
            identity: identity.identity.clone(),
            ca_certificate: identity.ca_certificate.clone(),
        }
    }
}

/// Whether a saved network wakes this device from sleep, as the device reports it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct NetworkWake {
    /// The network is set to wake this device.
    pub on: bool,
    /// The interface it wakes through, when it is up.
    pub interface: Option<String>,
    /// Whether the interface's driver can wake the device; `None` when it does not say.
    pub supported: Option<bool>,
    /// Whether the interface wakes the device now; `None` when it does not say.
    pub applied: Option<bool>,
    /// Why waking does not work as set.
    pub problem: Option<String>,
}

impl From<&os_actions::network_wake::NetworkWakeStatus> for NetworkWake {
    fn from(status: &os_actions::network_wake::NetworkWakeStatus) -> Self {
        Self {
            on: status.configured,
            interface: status.interface.clone(),
            supported: status.supported,
            applied: status.applied,
            problem: status.problem.clone(),
        }
    }
}

/// How a saved network's addresses are set.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct SavedIpv4 {
    /// The network hands out an address; the entries below are then unread.
    pub automatic: bool,
    /// Addresses with their prefixes.
    pub addresses: Vec<String>,
    pub gateway: Option<String>,
    /// The name servers it asks.
    pub dns: Vec<String>,
}

/// What kind of connection a saved network is.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
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
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
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
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Mobile {
    /// The device has a modem.
    pub available: bool,
    pub on: bool,
    /// The profile mobile data connects with; `None` before one is made.
    pub profile: Option<MobileProfile>,
}

/// The mobile data profile: what the operator's network is reached with.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
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
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
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
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
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
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Accessory {
    /// The accessory as the resource it is (`bluetooth-accessory:<hardware address>`): what its
    /// own actions (`connect`, `disconnect`, `forget`) are called on. The service sets it as it
    /// publishes the accessory.
    #[serde(rename = "$address")]
    pub handle: tilo_resource::Handle<crate::BluetoothAccessoryRow>,
    pub name: String,
    pub address: String,
    /// Battery percentage; `None` when it reports none.
    pub battery: Option<u8>,
    /// Signal while the device is seen, in dBm; `None` when it is paired
    /// but not currently in reach.
    pub signal: Option<i16>,
    pub connected: bool,
}

/// A Bluetooth device in reach that is not paired.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Nearby {
    /// The device as the resource it is (`bluetooth-nearby:<hardware address>`): what its own
    /// `pair` is called on.
    #[serde(rename = "$address")]
    pub handle: tilo_resource::Handle<crate::BluetoothNearbyRow>,
    pub name: String,
    pub address: String,
    /// Signal during discovery, in dBm.
    pub signal: Option<i16>,
}

/// The `vpn` resource: the VPN profiles this device has, and the kinds it
/// can set up besides `WireGuard`.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Vpn {
    pub profiles: Vec<VpnProfile>,
    /// The VPN kinds installed as plugins, besides `WireGuard`.
    pub plugins: Vec<VpnPluginKind>,
    /// The public key a key pair made here stands by, to be given to a
    /// server before a tunnel is set up; the private one never leaves the
    /// device.
    pub generated_key: Option<String>,
    pub pending: Vec<String>,
    pub problem: Option<String>,
}

/// One kind of VPN this device can set up.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct VpnPluginKind {
    /// The plugin's service type.
    pub service: String,
    pub name: String,
}

/// One VPN profile, with the settings its own page changes.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct VpnProfile {
    /// The profile as the resource it is (`vpn-profile:<id>`): what its own actions
    /// (`connect`, `disconnect`, `edit`, `forget`) are called on.
    #[serde(rename = "$address")]
    pub address: tilo_resource::Handle<crate::VpnProfileRow>,
    pub name: String,
    /// `NetworkManager`'s id of it, which a rename keeps.
    pub id: String,
    /// What kind of tunnel (`wireguard`, `vpn`).
    pub kind: String,
    /// The plugin's service type, for a kind that is a plugin.
    pub service: Option<String>,
    pub connected: bool,
    /// It comes up by itself when the device starts.
    pub autoconnect: bool,
    /// Every connection goes through the tunnel, not only what it routes.
    pub all_traffic: bool,
    /// This device's addresses in the tunnel.
    pub addresses: Vec<String>,
    /// The name servers the tunnel asks.
    pub dns: Vec<String>,
    pub mtu: Option<u32>,
    /// The server's address, as the tunnel dials it.
    pub endpoint: Option<String>,
    /// What the tunnel routes.
    pub allowed_ips: Vec<String>,
    /// The server's public key, for `WireGuard`.
    pub peer_public_key: Option<String>,
    /// Seconds between keepalives; `None` sends none.
    pub keepalive: Option<u32>,
}

/// The `tethering` resource: sharing this device's connection.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
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
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
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
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
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
            ..Self::default()
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
            signal: device.rssi,
            connected: device.connected,
            ..Self::default()
        }
    }
}

impl From<&os_actions::bluez::DeviceInfo> for Nearby {
    fn from(device: &os_actions::bluez::DeviceInfo) -> Self {
        Self {
            name: device.name.clone(),
            address: device.address.clone(),
            signal: device.rssi,
            ..Self::default()
        }
    }
}

impl From<&os_actions::IpConfiguration> for SavedIpv4 {
    fn from(config: &os_actions::IpConfiguration) -> Self {
        Self {
            automatic: config.automatic,
            addresses: config.addresses.clone(),
            gateway: config.gateway.clone(),
            dns: config.dns.clone(),
        }
    }
}

impl From<&os_actions::SavedNetworkProfile> for SavedNetwork {
    fn from(profile: &os_actions::SavedNetworkProfile) -> Self {
        Self {
            name: profile.name.clone(),
            profile: profile.uuid.clone(),
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
            ..Self::default()
        }
    }
}

impl From<&os_actions::network_settings::VpnPlugin> for VpnPluginKind {
    fn from(plugin: &os_actions::network_settings::VpnPlugin) -> Self {
        Self {
            service: plugin.service.clone(),
            name: plugin.name.clone(),
        }
    }
}

/// The `modem` resource: the modems this device has and their SIMs, with
/// what each reports of the network it stands on. The mobile data connection
/// itself — on or off, its access point, roaming and metering — is the
/// `network` resource's.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Modem {
    /// The device has a modem.
    pub available: bool,
    /// The modems, each with its SIMs.
    pub modems: Vec<ModemUnit>,
    pub pending: Vec<String>,
    pub problem: Option<String>,
}

/// One modem and its SIM slots.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct ModemUnit {
    /// `ModemManager`'s path of it.
    pub path: String,
    pub name: String,
    pub state: ModemState,
    /// Why the modem cannot be used, when it cannot.
    pub failure: Option<ModemFailure>,
    pub lock: SimLock,
    pub retries: Retries,
    /// Whether a SIM asks for its PIN when the device starts; `None` when
    /// the modem does not say.
    pub pin_enabled: Option<bool>,
    /// Signal strength in percent; `None` while the modem reports none.
    pub signal: Option<u8>,
    pub technology: Option<Technology>,
    pub operator: Option<String>,
    /// The operators the last scan found, the one it stands on first; empty until one is asked
    /// for.
    pub operators: Vec<Operator>,
    /// The last code sent to the network and what it answered; `None` until one is.
    pub code: Option<Code>,
    pub registration: Registration,
    /// The network types it may use now, as a key (`5g-4g-3g`).
    pub network_type: Option<String>,
    /// The network types it can be set to, each a key.
    pub network_types: Vec<String>,
    pub sims: Vec<ModemSim>,
    /// The numbers the SIMs are known by.
    pub numbers: Vec<String>,
    /// Bytes since `ModemManager` started counting.
    pub received: Option<u64>,
    pub sent: Option<u64>,
}

impl ModemUnit {
    /// No SIM lock and not failed: the modem can connect.
    #[must_use]
    pub fn usable(&self) -> bool {
        self.failure.is_none() && self.lock == SimLock::None
    }

    /// The SIM mobile data uses: the active one, else the first.
    #[must_use]
    pub fn active_sim(&self) -> Option<&ModemSim> {
        self.sims
            .iter()
            .find(|sim| sim.active)
            .or_else(|| self.sims.first())
    }
}

/// An operator the modem can reach.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Operator {
    /// Its MCC and MNC (`26201`), which the modem is told to stand on it by.
    pub code: String,
    pub name: String,
    pub state: OperatorState,
}

/// Whether a modem may use an operator it reaches.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum OperatorState {
    #[default]
    Unknown,
    Available,
    /// The network the modem stands on now.
    Current,
    /// This SIM may not use it.
    Forbidden,
}

impl OperatorState {
    /// The order operators are listed in: the one the modem stands on, then those it may use.
    const fn rank(self) -> u8 {
        match self {
            Self::Current => 0,
            Self::Available => 1,
            Self::Unknown => 2,
            Self::Forbidden => 3,
        }
    }
}

/// The operators a scan found, the one the modem stands on first, then by name.
#[must_use]
pub fn ordered_operators(mut operators: Vec<Operator>) -> Vec<Operator> {
    operators.sort_by(|a, b| {
        a.state
            .rank()
            .cmp(&b.state.rank())
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.code.cmp(&b.code))
    });
    operators
}

impl From<os_actions::Availability> for OperatorState {
    fn from(availability: os_actions::Availability) -> Self {
        match availability {
            os_actions::Availability::Unknown => Self::Unknown,
            os_actions::Availability::Available => Self::Available,
            os_actions::Availability::Current => Self::Current,
            os_actions::Availability::Forbidden => Self::Forbidden,
        }
    }
}

impl From<&os_actions::Operator> for Operator {
    fn from(operator: &os_actions::Operator) -> Self {
        Self {
            code: operator.code.clone(),
            name: operator.name.clone(),
            state: OperatorState::from(operator.availability),
        }
    }
}

/// What the network answered a code, and whether it waits for the person's answer.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Code {
    /// What the network said: a balance, a bundle or a menu.
    pub text: String,
    /// The session waits for the person to answer.
    pub awaiting: bool,
}

impl From<&os_actions::CodeAnswer> for Code {
    fn from(answer: &os_actions::CodeAnswer) -> Self {
        Self {
            text: answer.text.clone(),
            awaiting: answer.awaiting,
        }
    }
}

/// One SIM a modem holds.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct ModemSim {
    /// `ModemManager`'s path of the SIM.
    pub path: String,
    pub slot: Option<u32>,
    /// The SIM mobile data uses now.
    pub active: bool,
    pub operator: Option<String>,
    /// What it is called: its operator's name, or its slot.
    pub label: String,
    pub kind: SimKind,
}

/// How many tries a locked SIM has left.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Retries {
    pub pin: Option<u32>,
    pub puk: Option<u32>,
}

/// What a modem is doing (`MMModemState`).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ModemState {
    Failed,
    #[default]
    Unknown,
    Initializing,
    /// A SIM lock holds it.
    Locked,
    Disabled,
    Enabled,
    Searching,
    Registered,
    Connected,
    Disconnecting,
    Connecting,
}

/// Why a modem cannot be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ModemFailure {
    SimMissing,
    SimError,
    /// An eSIM with no profile installed.
    EsimWithoutProfiles,
    Other,
}

/// What a SIM asks for before the modem can be used.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum SimLock {
    #[default]
    None,
    Pin,
    /// Blocked after too many wrong PINs; only its PUK lifts it.
    Puk,
    /// A lock this device cannot lift (a network or carrier lock).
    Other,
}

/// How the modem reaches the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Technology {
    Gsm,
    Gprs,
    Edge,
    Cdma,
    Umts,
    Hspa,
    HspaPlus,
    EvDo,
    Lte,
    Nr5g,
}

/// Which network the modem stands on.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Registration {
    #[default]
    Idle,
    Home,
    Roaming,
    Searching,
    Denied,
    EmergencyOnly,
    Unknown,
}

/// What kind of SIM it is.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum SimKind {
    Physical,
    /// An eSIM; `available` says whether one of its profiles is installed.
    Esim,
    EsimWithoutProfile,
    #[default]
    Unknown,
}

impl From<&os_actions::modem::SimKind> for SimKind {
    fn from(kind: &os_actions::modem::SimKind) -> Self {
        match kind {
            os_actions::modem::SimKind::Physical => Self::Physical,
            os_actions::modem::SimKind::Esim {
                profiles: Some(false),
            } => Self::EsimWithoutProfile,
            os_actions::modem::SimKind::Esim { .. } => Self::Esim,
            os_actions::modem::SimKind::Unknown => Self::Unknown,
        }
    }
}

impl From<os_actions::modem::ModemState> for ModemState {
    fn from(state: os_actions::modem::ModemState) -> Self {
        use os_actions::modem::ModemState as Mm;
        match state {
            Mm::Failed => Self::Failed,
            Mm::Unknown => Self::Unknown,
            Mm::Initializing => Self::Initializing,
            Mm::Locked => Self::Locked,
            Mm::Disabled | Mm::Disabling => Self::Disabled,
            Mm::Enabling | Mm::Enabled => Self::Enabled,
            Mm::Searching => Self::Searching,
            Mm::Registered => Self::Registered,
            Mm::Disconnecting => Self::Disconnecting,
            Mm::Connecting => Self::Connecting,
            Mm::Connected => Self::Connected,
        }
    }
}

impl From<os_actions::modem::ModemFailure> for ModemFailure {
    fn from(failure: os_actions::modem::ModemFailure) -> Self {
        use os_actions::modem::ModemFailure as Mm;
        match failure {
            Mm::SimMissing => Self::SimMissing,
            Mm::SimError => Self::SimError,
            Mm::EsimWithoutProfiles => Self::EsimWithoutProfiles,
            Mm::Other => Self::Other,
        }
    }
}

impl From<os_actions::modem::SimLock> for SimLock {
    fn from(lock: os_actions::modem::SimLock) -> Self {
        use os_actions::modem::SimLock as Mm;
        match lock {
            Mm::None => Self::None,
            Mm::Pin => Self::Pin,
            Mm::Puk => Self::Puk,
            Mm::Other => Self::Other,
        }
    }
}

impl From<os_actions::modem::Technology> for Technology {
    fn from(technology: os_actions::modem::Technology) -> Self {
        use os_actions::modem::Technology as Mm;
        match technology {
            Mm::Gsm => Self::Gsm,
            Mm::Gprs => Self::Gprs,
            Mm::Edge => Self::Edge,
            Mm::Cdma => Self::Cdma,
            Mm::Umts => Self::Umts,
            Mm::Hspa => Self::Hspa,
            Mm::HspaPlus => Self::HspaPlus,
            Mm::EvDo => Self::EvDo,
            Mm::Lte => Self::Lte,
            Mm::Nr5g => Self::Nr5g,
        }
    }
}

impl From<os_actions::modem::Registration> for Registration {
    fn from(registration: os_actions::modem::Registration) -> Self {
        use os_actions::modem::Registration as Mm;
        match registration {
            Mm::Idle => Self::Idle,
            Mm::Home => Self::Home,
            Mm::Roaming => Self::Roaming,
            Mm::Searching => Self::Searching,
            Mm::Denied => Self::Denied,
            Mm::EmergencyOnly => Self::EmergencyOnly,
            Mm::Unknown => Self::Unknown,
        }
    }
}

impl From<&os_actions::modem::SimCard> for ModemSim {
    fn from(sim: &os_actions::modem::SimCard) -> Self {
        Self {
            path: sim.path.clone(),
            slot: sim.slot,
            active: sim.active,
            operator: sim.operator.clone(),
            label: sim.label(),
            kind: SimKind::from(&sim.kind),
        }
    }
}

impl From<&os_actions::modem::ModemInfo> for ModemUnit {
    fn from(modem: &os_actions::modem::ModemInfo) -> Self {
        Self {
            path: modem.path.clone(),
            name: modem.name.clone(),
            state: ModemState::from(modem.state),
            failure: modem.failure.map(ModemFailure::from),
            lock: SimLock::from(modem.lock),
            retries: Retries {
                pin: modem.retries.pin,
                puk: modem.retries.puk,
            },
            pin_enabled: modem.pin_enabled,
            signal: modem.signal,
            technology: modem.technology.map(Technology::from),
            operator: modem.operator.clone(),
            operators: Vec::new(),
            code: None,
            registration: Registration::from(modem.registration),
            network_type: modem.modes.map(os_actions::NetworkModes::key),
            network_types: modem
                .supported_modes
                .iter()
                .copied()
                .map(os_actions::NetworkModes::key)
                .collect(),
            sims: modem.sims.iter().map(ModemSim::from).collect(),
            numbers: modem.numbers.clone(),
            received: modem.traffic.map(|traffic| traffic.received),
            sent: modem.traffic.map(|traffic| traffic.sent),
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_link_says_which_connection_the_traffic_leaves_on() {
        let wifi = os_actions::LinkStatus {
            kind: os_actions::LinkKind::Wifi,
            name: Some("Home".to_owned()),
            signal: 3,
        };
        assert_eq!(
            Link::from(&wifi),
            Link {
                kind: LinkKind::Wifi,
                name: Some("Home".to_owned()),
                signal: 3,
            }
        );
        let offline = Link::from(&os_actions::LinkStatus::offline());
        assert_eq!(offline.kind, LinkKind::Offline);
        assert_eq!(
            offline.name, None,
            "nothing is connected, so nothing is named"
        );
    }
}
