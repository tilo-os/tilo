//! `NetworkManager`'s answers as types, with no bus.
//!
//! The numbers are `NetworkManager`'s own enums (`NMState`,
//! `NMConnectivityState`, `NMMetered`); a value this crate does not know reads
//! as unknown rather than as a guess.

/// `NMConnectivityState`: what `NetworkManager` last concluded about reaching
/// the internet. Reading it never starts a check; see
/// [`crate::NmClient::check_connectivity_now`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NmConnectivity {
    /// Not checked, checking disabled, or a value this crate does not know.
    #[default]
    Unknown,
    /// No network path at all.
    None,
    /// A captive portal intercepts traffic until the person signs in.
    Portal,
    /// Connected, but the internet is not reachable.
    Limited,
    /// The internet is reachable.
    Full,
}

impl NmConnectivity {
    /// Map `NMConnectivityState` (`0..=4`).
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            1 => Self::None,
            2 => Self::Portal,
            3 => Self::Limited,
            4 => Self::Full,
            _ => Self::Unknown,
        }
    }
}

/// `NMState`: the daemon's overall networking state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NmState {
    /// `NetworkManager` is not answering, or reported a value this crate does
    /// not know.
    #[default]
    Unknown,
    Asleep,
    Disconnected,
    Disconnecting,
    Connecting,
    /// Only a local network is reachable.
    ConnectedLocal,
    /// A site-wide network is reachable, but not the internet.
    ConnectedSite,
    /// The internet is reachable.
    ConnectedGlobal,
}

impl NmState {
    /// Map `NMState` (`0, 10, 20, …, 70`).
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            10 => Self::Asleep,
            20 => Self::Disconnected,
            30 => Self::Disconnecting,
            40 => Self::Connecting,
            50 => Self::ConnectedLocal,
            60 => Self::ConnectedSite,
            70 => Self::ConnectedGlobal,
            _ => Self::Unknown,
        }
    }

    /// Whether any connection is up.
    #[must_use]
    pub const fn is_connected(self) -> bool {
        matches!(
            self,
            Self::ConnectedLocal | Self::ConnectedSite | Self::ConnectedGlobal
        )
    }
}

/// Map `NMMetered` (`0..=4`) to "does the person pay per byte".
///
/// `Yes` and `GuessYes` are metered, `No` and `GuessNo` are not, and
/// `Unknown` (or a value this crate does not know) says nothing.
#[must_use]
pub const fn metered_from_raw(raw: u32) -> Option<bool> {
    match raw {
        1 | 3 => Some(true),
        2 | 4 => Some(false),
        _ => None,
    }
}

/// What kind of link carries the primary connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// Ethernet and the virtual devices built on it (bridge, bond, team,
    /// VLAN). A phone tethered over USB is Ethernet too.
    Wired,
    Wifi,
    /// Mobile broadband, or a phone tethered over Bluetooth.
    Mobile,
    /// Anything else `NetworkManager` can make primary.
    Other,
}

impl LinkKind {
    /// Map a `NetworkManager` connection type (the setting name, e.g.
    /// `802-11-wireless`).
    #[must_use]
    pub fn from_connection_type(kind: &str) -> Self {
        match kind {
            "802-3-ethernet" | "ethernet" | "bridge" | "bond" | "team" | "vlan" | "macvlan"
            | "veth" | "pppoe" | "infiniband" => Self::Wired,
            "802-11-wireless" | "wifi" => Self::Wifi,
            "gsm" | "cdma" | "bluetooth" => Self::Mobile,
            _ => Self::Other,
        }
    }
}

/// The connection `NetworkManager` calls primary: the one holding the default
/// route (or, under a VPN, the route to the VPN endpoint). This is the link
/// the person's traffic actually leaves on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PrimaryLink {
    pub kind: LinkKind,
    /// The connection profile's name; on Wi-Fi usually the SSID.
    pub name: Option<String>,
    /// The kernel interface, e.g. `wlp2s0`.
    pub device: Option<String>,
}

/// One reading of `NetworkManager`'s root object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct NetworkState {
    pub state: NmState,
    pub connectivity: NmConnectivity,
    /// `NetworkManager`'s global `Metered`: the primary connection's metering,
    /// not a scan of every device. `None` when it does not say.
    pub metered: Option<bool>,
    /// `None` while nothing is connected.
    pub primary: Option<PrimaryLink>,
}

impl NetworkState {
    /// What a reader holds when `NetworkManager` cannot be asked: nothing is
    /// known, so nothing is allowed that needs knowing.
    #[must_use]
    pub fn unknown() -> Self {
        Self::default()
    }

    /// Whether optional, speculative traffic is fine: the internet is
    /// reachable and the primary connection is known not to be metered.
    /// Anything unknown answers no.
    #[must_use]
    pub fn allows_speculative_traffic(&self) -> bool {
        self.connectivity == NmConnectivity::Full && self.metered == Some(false)
    }
}

/// `NMActiveConnectionState`: where an active connection is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ActiveState {
    /// A value this crate does not know.
    #[default]
    Unknown,
    Activating,
    Activated,
    Deactivating,
    Deactivated,
}

impl ActiveState {
    /// Map `NMActiveConnectionState` (`0..=4`).
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            1 => Self::Activating,
            2 => Self::Activated,
            3 => Self::Deactivating,
            4 => Self::Deactivated,
            _ => Self::Unknown,
        }
    }
}

/// Whether an active connection going down for `reason`
/// (`NMActiveConnectionStateReason`) was asked for: someone deactivated it
/// (`USER_DISCONNECTED`, 2) or deleted its profile (`CONNECTION_REMOVED`,
/// 11). Every other reason is the network's, the server's or the VPN
/// service's doing.
#[must_use]
pub const fn ended_on_purpose(reason: u32) -> bool {
    matches!(reason, 2 | 11)
}

/// Whether a connection type (`connection.type`) is a VPN: `WireGuard`, or a
/// plugin's (`vpn`).
#[must_use]
pub fn is_vpn_type(kind: &str) -> bool {
    matches!(kind, "vpn" | "wireguard")
}

/// A VPN `NetworkManager` has active.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ActiveVpn {
    /// The profile's id.
    pub uuid: String,
    pub state: ActiveState,
}

/// What [`crate::NmClient::watch_vpns`] follows: the VPNs `NetworkManager`
/// has active, and how often each was turned off on purpose.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VpnReading {
    /// Whether `NetworkManager` answered; while it does not, nothing is
    /// known to be active.
    pub known: bool,
    pub active: Vec<ActiveVpn>,
    /// Per profile, how many times it went down on purpose since the watch
    /// started ([`ended_on_purpose`]). A count, so a reader that misses a
    /// reading still sees each one.
    pub ended_on_purpose: std::collections::BTreeMap<String, u64>,
}

/// `NMDeviceType`, as far as Tilo tells devices apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    Ethernet,
    Wifi,
    /// A mobile broadband modem (`ModemManager`'s).
    Modem,
    /// Anything else: loopback, bridges, tunnels, Wi-Fi P2P, …
    Other,
}

impl DeviceKind {
    /// Map `NMDeviceType`.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            1 => Self::Ethernet,
            2 => Self::Wifi,
            8 => Self::Modem,
            _ => Self::Other,
        }
    }
}

/// `NMDeviceState` `ACTIVATED`: the device's connection is up.
pub const DEVICE_ACTIVATED: u32 = 100;

/// A network device `NetworkManager` knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// Its object.
    pub path: String,
    /// The kernel interface (`wlp2s0`).
    pub interface: String,
    pub kind: DeviceKind,
    /// `NMDeviceState`.
    pub state: u32,
    /// The hardware address in use.
    pub hw_address: String,
    /// Its active connection's object; `None` while it has none.
    pub active: Option<String>,
}

impl Device {
    /// Whether its connection is up.
    #[must_use]
    pub const fn is_activated(&self) -> bool {
        self.state == DEVICE_ACTIVATED
    }
}

/// A Wi-Fi access point a device sees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessPoint {
    /// Its object.
    pub path: String,
    /// The device that sees it.
    pub device: String,
    /// The network's name; empty for a hidden network.
    pub ssid: String,
    /// Signal strength in percent.
    pub strength: u8,
    /// `NM80211ApFlags`.
    pub flags: u32,
    /// `NM80211ApSecurityFlags` of its WPA and RSN (WPA2/WPA3) elements.
    pub wpa_flags: u32,
    pub rsn_flags: u32,
    /// In MHz.
    pub frequency: u32,
    /// In kbit/s.
    pub max_bitrate: u32,
    /// Whether its device is connected through it.
    pub active: bool,
}

/// `NM80211ApFlags` `PRIVACY`: the network encrypts.
pub const AP_PRIVACY: u32 = 0x1;
/// `NM80211ApSecurityFlags` key management suites.
pub const AP_KEY_PSK: u32 = 0x100;
pub const AP_KEY_8021X: u32 = 0x200;
pub const AP_KEY_SAE: u32 = 0x400;
pub const AP_KEY_OWE: u32 = 0x800;
pub const AP_KEY_OWE_TRANSITION: u32 = 0x1000;
pub const AP_KEY_EAP_SUITE_B_192: u32 = 0x2000;

/// An active connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveConnection {
    /// Its object.
    pub path: String,
    /// The profile's name and id.
    pub id: String,
    pub uuid: String,
    /// The profile's type (`802-11-wireless`, `vpn`, …).
    pub kind: String,
    pub state: ActiveState,
    /// The devices' objects.
    pub devices: Vec<String>,
    /// The profile's object.
    pub profile: String,
}

/// What a device holds of one address family now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IpDetails {
    /// Each with its prefix (`192.0.2.10/24`).
    pub addresses: Vec<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
}

/// `NetworkManager`'s radio switches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one of NetworkManager's separate switches"
)]
pub struct Radios {
    pub wifi_enabled: bool,
    /// False while a hardware switch holds Wi-Fi off.
    pub wifi_hardware_enabled: bool,
    pub wwan_enabled: bool,
    pub wwan_hardware_enabled: bool,
}

/// Why an activation ended, for a person: `NMActiveConnectionStateReason`.
#[must_use]
pub const fn activation_failure(reason: u32) -> &'static str {
    match reason {
        2 => "the connection was disconnected",
        3 => "the device went away or was disconnected",
        4 => "the VPN service stopped",
        5 => "no usable IP configuration was received",
        6 => "the connection timed out",
        7 => "the VPN service did not start in time",
        8 => "the VPN service failed to start",
        9 => "the network needs a password or key that was not given",
        10 => "the login was refused",
        11 => "the saved network was removed",
        12 => "a connection it depends on failed",
        13 | 14 => "the device is not available",
        _ => "the connection failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_state_and_reason_mapping() {
        assert_eq!(
            [0, 1, 2, 3, 4, 5].map(ActiveState::from_raw),
            [
                ActiveState::Unknown,
                ActiveState::Activating,
                ActiveState::Activated,
                ActiveState::Deactivating,
                ActiveState::Deactivated,
                ActiveState::Unknown,
            ]
        );
        assert!(ended_on_purpose(2), "deactivated by someone");
        assert!(ended_on_purpose(11), "its profile deleted");
        for reason in [0, 1, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13, 14] {
            assert!(!ended_on_purpose(reason), "{reason}");
        }
        assert!(is_vpn_type("vpn") && is_vpn_type("wireguard"));
        assert!(!is_vpn_type("802-11-wireless"));
    }

    #[test]
    fn device_kinds_and_activation_failures_map() {
        assert_eq!(
            [1, 2, 8, 14, 0].map(DeviceKind::from_raw),
            [
                DeviceKind::Ethernet,
                DeviceKind::Wifi,
                DeviceKind::Modem,
                DeviceKind::Other,
                DeviceKind::Other,
            ]
        );
        assert!(activation_failure(9).contains("password"));
        assert_eq!(activation_failure(99), "the connection failed");
    }

    #[test]
    fn metered_mapping() {
        assert_eq!(
            [0, 1, 2, 3, 4, 5].map(metered_from_raw),
            [None, Some(true), Some(false), Some(true), Some(false), None],
            "NMMetered Unknown/Yes/No/GuessYes/GuessNo, then an unknown value"
        );
    }

    #[test]
    fn connectivity_mapping() {
        assert_eq!(
            [0, 1, 2, 3, 4, 9].map(NmConnectivity::from_raw),
            [
                NmConnectivity::Unknown,
                NmConnectivity::None,
                NmConnectivity::Portal,
                NmConnectivity::Limited,
                NmConnectivity::Full,
                NmConnectivity::Unknown,
            ]
        );
    }

    #[test]
    fn state_mapping() {
        assert_eq!(
            [0, 10, 20, 30, 40, 50, 60, 70, 71].map(NmState::from_raw),
            [
                NmState::Unknown,
                NmState::Asleep,
                NmState::Disconnected,
                NmState::Disconnecting,
                NmState::Connecting,
                NmState::ConnectedLocal,
                NmState::ConnectedSite,
                NmState::ConnectedGlobal,
                NmState::Unknown,
            ]
        );
        assert!(NmState::ConnectedLocal.is_connected());
        assert!(!NmState::Connecting.is_connected());
    }

    #[test]
    fn connection_types_map_to_link_kinds() {
        for (kind, want) in [
            ("802-3-ethernet", LinkKind::Wired),
            ("bridge", LinkKind::Wired),
            ("802-11-wireless", LinkKind::Wifi),
            ("gsm", LinkKind::Mobile),
            ("bluetooth", LinkKind::Mobile),
            ("wireguard", LinkKind::Other),
            ("", LinkKind::Other),
        ] {
            assert_eq!(LinkKind::from_connection_type(kind), want, "{kind}");
        }
    }

    #[test]
    fn speculation_needs_full_connectivity_and_an_unmetered_primary() {
        let full_unmetered = NetworkState {
            state: NmState::ConnectedGlobal,
            connectivity: NmConnectivity::Full,
            metered: Some(false),
            primary: None,
        };
        assert!(full_unmetered.allows_speculative_traffic());
        for (case, state) in [
            (
                "metered primary",
                NetworkState {
                    metered: Some(true),
                    ..full_unmetered.clone()
                },
            ),
            (
                "metering unknown",
                NetworkState {
                    metered: None,
                    ..full_unmetered.clone()
                },
            ),
            (
                "behind a portal",
                NetworkState {
                    connectivity: NmConnectivity::Portal,
                    ..full_unmetered
                },
            ),
            ("NetworkManager unreachable", NetworkState::unknown()),
        ] {
            assert!(!state.allows_speculative_traffic(), "{case}");
        }
    }
}
