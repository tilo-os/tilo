//! The device facts the published resource surface is built from.
//!
//! Live resources (an audio stream's volume, a display's mode, the battery's
//! charge cap) are published from what the machine reports, and a control
//! bound to one is enabled exactly when its resource is published. A capture
//! shows fixed hardware, so it puts the same questions to the same fixture
//! the panes show: its controls are enabled, as they would be on that
//! hardware, and nothing else about the publish path differs.

use media::audio::AudioGraph;
use os_actions::ChargeCapCapability;
use settings_store::hardware::DisplayHardwareSnapshot;

/// Where the live device facts come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeviceFacts {
    fixed: bool,
}

impl DeviceFacts {
    /// What this machine reports.
    pub(crate) const LIVE: Self = Self { fixed: false };

    /// The fixed hardware a capture shows.
    #[cfg(not(feature = "thin"))]
    pub(crate) const FIXED: Self = Self { fixed: true };

    /// The audio devices and streams.
    pub(crate) fn audio_graph(self) -> AudioGraph {
        #[cfg(not(feature = "thin"))]
        if self.fixed {
            return crate::shots::audio_graph();
        }
        media::audio::enumerate()
    }

    /// The outputs the compositor publishes.
    pub(crate) fn display_snapshot(self) -> Option<DisplayHardwareSnapshot> {
        #[cfg(not(feature = "thin"))]
        if self.fixed {
            return Some(crate::shots::display_snapshot());
        }
        DisplayHardwareSnapshot::load()
    }

    /// The battery's charge-limit control, when it has one.
    pub(crate) fn charge_capability(self) -> Option<ChargeCapCapability> {
        #[cfg(not(feature = "thin"))]
        if self.fixed {
            return Some(crate::shots::charge_capability());
        }
        os_actions::charge_capability()
    }
}
