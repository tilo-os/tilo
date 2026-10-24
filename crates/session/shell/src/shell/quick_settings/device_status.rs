//! Quick Settings' reading of the shell's one device status.

use crate::shell::chrome_state::QuickSettingsState;

impl QuickSettingsState {
    /// The output's level, `0.0..=1.0`; `0` until it has been read.
    pub fn volume_level(&self) -> f32 {
        self.status.volume.map_or(0.0, |volume| volume.level)
    }

    pub fn volume_muted(&self) -> bool {
        self.status.volume.is_some_and(|volume| volume.muted)
    }

    /// Show the output at `level`, as a slider being dragged or a readout
    /// shows it; mute stays as it is.
    pub fn set_volume_level(&mut self, level: f32) {
        self.status.volume = Some(os_actions::VolumeState {
            level,
            muted: self.volume_muted(),
        });
    }

    /// The battery as last read; absent until then.
    pub fn battery(&self) -> tilo_power_mgmt::BatterySnapshot {
        self.status
            .battery
            .unwrap_or_else(tilo_power_mgmt::BatterySnapshot::absent)
    }
}
