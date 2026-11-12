//! What a device is to the person: one of their own, or a home server that hosts for them.
//!
//! A token admits a device as one of these, and the certificate it becomes carries the matching
//! capabilities.

use tilo_user_identity::DeviceCapabilities;

/// What a token admits a device as. Each role has one capability preset, and
/// a token whose capabilities have the other role's shape is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EnrollmentRole {
    /// One of the person's own devices: it owns workspaces and does not host.
    PersonalDevice,
    /// An always-on home server: it hosts and reads every workspace.
    HomePeer,
}

impl EnrollmentRole {
    /// The capabilities a token minted for this role grants.
    #[must_use]
    pub fn preset(self) -> DeviceCapabilities {
        match self {
            Self::PersonalDevice => DeviceCapabilities::member(),
            Self::HomePeer => DeviceCapabilities::host(),
        }
    }

    /// Whether `capabilities` have the shape of this role: a home peer hosts
    /// and reads everything, a personal device does neither.
    #[must_use]
    pub fn admits(self, capabilities: &DeviceCapabilities) -> bool {
        let hosts = capabilities.can_act_as_host;
        let reads_all = capabilities.can_read_all_workspaces;
        match self {
            Self::PersonalDevice => !hosts && !reads_all,
            Self::HomePeer => hosts && reads_all,
        }
    }

    /// The stable name callers pass, `personal-device` or `home-peer`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PersonalDevice => "personal-device",
            Self::HomePeer => "home-peer",
        }
    }

    /// The role named by [`Self::as_str`].
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "personal-device" => Some(Self::PersonalDevice),
            "home-peer" => Some(Self::HomePeer),
            _ => None,
        }
    }
}
