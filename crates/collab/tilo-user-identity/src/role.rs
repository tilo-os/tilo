//! What a key is to the person, and the capabilities each kind gets.
//!
//! Every path that certifies a key grants one of these — an enrollment token
//! redeeming a home peer, a page pairing a browser, a hosting node's
//! certificate — so a device of a kind carries the same capabilities whichever
//! path admitted it, and no path keeps its own copy of a set.

use crate::DeviceCapabilities;

/// What a certificate makes the key it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceRole {
    /// One of the person's own devices — a laptop, a phone, a page's browser:
    /// it owns workspaces, hosts nothing and admits no devices.
    Personal,
    /// A machine that keeps the person's workspaces for them, at home or at a
    /// hosting provider: it owns and reads them all, and admits no devices.
    Host,
}

impl DeviceRole {
    /// The capabilities a certificate for this kind of device carries.
    #[must_use]
    pub fn capabilities(self) -> DeviceCapabilities {
        match self {
            Self::Personal => DeviceCapabilities::member(),
            Self::Host => DeviceCapabilities::host(),
        }
    }

    /// Whether `capabilities` have the shape of this kind: a host hosts and
    /// reads every workspace, one of the person's own devices does neither.
    #[must_use]
    pub fn admits(self, capabilities: &DeviceCapabilities) -> bool {
        let hosts = capabilities.can_act_as_host;
        let reads_all = capabilities.can_read_all_workspaces;
        match self {
            Self::Personal => !hosts && !reads_all,
            Self::Host => hosts && reads_all,
        }
    }

    /// The name this kind is asked for by: `personal-device` or `home-peer`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Personal => "personal-device",
            Self::Host => "home-peer",
        }
    }

    /// The kind named by [`Self::name`].
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "personal-device" => Some(Self::Personal),
            "home-peer" => Some(Self::Host),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_carries_its_own_capabilities_and_admits_only_those() {
        for role in [DeviceRole::Personal, DeviceRole::Host] {
            let capabilities = role.capabilities();
            assert!(role.admits(&capabilities), "{role:?} admits its own set");
            assert_eq!(DeviceRole::parse(role.name()), Some(role));
            assert!(DeviceRole::parse("administrator").is_none());
        }
        assert!(!DeviceRole::Host.admits(&DeviceRole::Personal.capabilities()));
        assert!(!DeviceRole::Personal.admits(&DeviceRole::Host.capabilities()));
    }
}
