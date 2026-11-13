//! `NetworkManager`'s answers as types, with no bus.

/// `NMConnectivityState`: what `NetworkManager` last concluded about reaching
/// the internet. Reading it never starts a check; see
/// [`crate::NmClient::check_connectivity_now`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NmConnectivity {
    #[default]
    Unknown,
    None,
    /// A captive portal intercepts traffic until the person signs in.
    Portal,
    Limited,
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
    ConnectedLocal,
    ConnectedSite,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// Ethernet and the virtual devices built on it (bridge, bond, team,
    /// VLAN). A phone tethered over USB is Ethernet too.
    Wired,
    Wifi,
    Mobile,
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
    pub name: Option<String>,
    pub device: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct NetworkState {
    pub state: NmState,
    pub connectivity: NmConnectivity,
    /// `NetworkManager`'s global `Metered`: the primary connection's metering,
    /// not a scan of every device. `None` when it does not say.
    pub metered: Option<bool>,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ActiveState {
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ActiveVpn {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    Ethernet,
    Wifi,
    Modem,
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

pub const DEVICE_ACTIVATED: u32 = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub path: String,
    pub interface: String,
    pub kind: DeviceKind,
    pub state: u32,
    pub hw_address: String,
    pub active: Option<String>,
}

impl Device {
    #[must_use]
    pub const fn is_activated(&self) -> bool {
        self.state == DEVICE_ACTIVATED
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessPoint {
    pub path: String,
    pub device: String,
    pub ssid: String,
    /// Signal strength in percent.
    pub strength: u8,
    pub flags: u32,
    pub wpa_flags: u32,
    pub rsn_flags: u32,
    pub frequency: u32,
    pub max_bitrate: u32,
    pub active: bool,
}

pub const AP_PRIVACY: u32 = 0x1;
pub const AP_KEY_PSK: u32 = 0x100;
pub const AP_KEY_8021X: u32 = 0x200;
pub const AP_KEY_SAE: u32 = 0x400;
pub const AP_KEY_OWE: u32 = 0x800;
pub const AP_KEY_OWE_TRANSITION: u32 = 0x1000;
pub const AP_KEY_EAP_SUITE_B_192: u32 = 0x2000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveConnection {
    pub path: String,
    pub id: String,
    pub uuid: String,
    pub kind: String,
    pub state: ActiveState,
    pub devices: Vec<String>,
    pub profile: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IpDetails {
    /// Each with its prefix (`192.0.2.10/24`).
    pub addresses: Vec<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one of NetworkManager's separate switches"
)]
pub struct Radios {
    pub wifi_enabled: bool,
    pub wifi_hardware_enabled: bool,
    pub wwan_enabled: bool,
    pub wwan_hardware_enabled: bool,
}

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
