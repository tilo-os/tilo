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

#[cfg(test)]
mod tests {
    use super::*;

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
