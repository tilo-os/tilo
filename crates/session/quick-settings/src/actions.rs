//! Which parts of the device this panel can change.
//!
//! The panel changes the device by asking the session's services; what it
//! can ask for is what those services published ([`QsActionAvailability`]).
//! An effect whose control this machine has none of is published
//! unavailable, with why ([`crate::effect::statuses`]).

use crate::QuickSettingsState;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag independently records whether one Quick Settings backend is present"
)]
pub struct QsActionAvailability {
    pub brightness: bool,
    pub volume: bool,
    pub wifi: bool,
    pub power_profile: bool,
    pub vpn: bool,
    pub bluetooth: bool,
    pub airplane_mode: bool,
}

impl QsActionAvailability {
    /// Which parts of the device this machine has, as the session's
    /// readings say: a control no service published anything to change is
    /// not offered.
    #[must_use]
    pub fn of(state: &QuickSettingsState) -> Self {
        Self {
            brightness: state.can_set_brightness(),
            // The audio service is what carries a volume change out, so its
            // reading of an output is what makes the volume available.
            volume: state.volume_available,
            wifi: state.network.wifi.available,
            power_profile: !state.power_profiles.is_empty(),
            vpn: !state.vpn.profiles.is_empty(),
            bluetooth: state.bluetooth.available,
            airplane_mode: state.network.wifi.available || state.network.mobile.available,
        }
    }
}
