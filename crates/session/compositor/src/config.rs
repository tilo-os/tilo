//! What the person configured, as the compositor takes it.
//!
//! The compositor reads no settings store. The session reads the person's
//! settings, fills a [`Settings`] and hands it over at start and whenever
//! they change ([`crate::Compositor::apply_settings`]); every part applies
//! live. Each part can also be set on its own.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use smithay::utils::Transform;

use crate::backend::InputConfig;
use crate::backend::drm::devices::RequestedMode;
use crate::idle::IdleConfig;
use crate::output::{ModeRequest, OutputChange};

/// Everything the person configured that the compositor applies.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub display: DisplayConfig,
    pub input: InputConfig,
    pub keyboard: KeyboardLayout,
    pub idle: IdleConfig,
}

/// The keyboard layout, as XKB names it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyboardLayout {
    /// Layouts, comma-separated (`us,de`); empty for the default.
    pub layout: String,
    pub variant: String,
    pub model: String,
    /// Options, comma-separated (`grp:alt_shift_toggle,caps:escape`).
    pub options: String,
}

/// What the person configured for the outputs, by connector name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplayConfig {
    /// A chosen mode per connector (`eDP-1`).
    pub modes: HashMap<String, RequestedMode>,
    /// A chosen scale per connector; unlisted outputs run at 1.
    pub scales: HashMap<String, f64>,
    /// A chosen top-left corner in the logical layout per connector;
    /// unlisted outputs are packed left to right after the listed ones.
    pub positions: HashMap<String, (i32, i32)>,
    /// A chosen rotation per connector.
    pub transforms: HashMap<String, Transform>,
    /// Connectors the person turned off; they stay off when plugged in.
    pub disabled: HashSet<String>,
    /// Connectors that run at a variable refresh rate where the display
    /// can.
    pub variable_refresh: HashSet<String>,
    /// An ICC profile per connector, applied to everything it shows.
    pub color_profiles: HashMap<String, PathBuf>,
    /// Connectors driven in HDR, with what their displays report.
    pub hdr: HashMap<String, Hdr>,
    /// Night light's colour temperature in kelvin while it is on (see
    /// [`crate::Compositor::set_night_light`]); the session decides when
    /// (a switch, a schedule).
    pub night_light: Option<u32>,
}

/// An output driven in HDR (a PQ BT.2020 signal): where SDR white sits and
/// what the display reports in its EDID, sent to it as the mastering
/// display. Zero luminances are "unknown".
#[derive(Clone, Debug, PartialEq)]
pub struct Hdr {
    /// The luminance SDR white (sRGB 1.0) is shown at; 203 is the BT.2408
    /// reference.
    pub sdr_white_nits: f32,
    /// Red, green, blue chromaticities, in millionths (`x`, `y`).
    pub primaries: [(u32, u32); 3],
    pub white_point: (u32, u32),
    pub max_nits: u16,
    pub max_frame_average_nits: u16,
    /// The minimum luminance in 0.0001 cd/m².
    pub min_luminance_micro_nits: u32,
}

impl Default for Hdr {
    /// BT.2020 primaries, D65 white, luminances unknown.
    fn default() -> Self {
        Self {
            sdr_white_nits: 203.0,
            primaries: [(708_000, 292_000), (170_000, 797_000), (131_000, 46_000)],
            white_point: (312_700, 329_000),
            max_nits: 0,
            max_frame_average_nits: 0,
            min_luminance_micro_nits: 0,
        }
    }
}

impl DisplayConfig {
    /// The changes that bring `outputs` (name, on now) in line with this
    /// configuration. Outputs it says nothing about are left as they are.
    pub(crate) fn changes(&self, outputs: &[(String, bool)]) -> Vec<OutputChange> {
        outputs
            .iter()
            .filter_map(|(name, on)| {
                let enabled = !self.disabled.contains(name);
                let mode = self.modes.get(name).map(|mode| ModeRequest {
                    size: (
                        i32::try_from(mode.width).unwrap_or(i32::MAX),
                        i32::try_from(mode.height).unwrap_or(i32::MAX),
                    ),
                    refresh_mhz: (mode.refresh_mhz > 0)
                        .then(|| i32::try_from(mode.refresh_mhz).unwrap_or(i32::MAX)),
                });
                let change = OutputChange {
                    name: name.clone(),
                    enabled,
                    mode,
                    position: self.positions.get(name).copied(),
                    scale: self.scales.get(name).copied(),
                    transform: self.transforms.get(name).copied(),
                };
                let says_something = enabled != *on
                    || change.mode.is_some()
                    || change.position.is_some()
                    || change.scale.is_some()
                    || change.transform.is_some();
                says_something.then_some(change)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configuration_changes_only_what_it_names() {
        let mut config = DisplayConfig::default();
        config.scales.insert("eDP-1".into(), 1.5);
        config.disabled.insert("DP-1".into());
        config.modes.insert(
            "HDMI-A-1".into(),
            RequestedMode {
                width: 1920,
                height: 1080,
                refresh_mhz: 0,
            },
        );
        let outputs = [
            ("eDP-1".to_owned(), true),
            ("DP-1".to_owned(), true),
            ("HDMI-A-1".to_owned(), true),
            ("DP-2".to_owned(), true),
        ];
        let changes = config.changes(&outputs);
        assert_eq!(changes.len(), 3, "DP-2 is left alone: {changes:?}");
        assert_eq!(changes[0].scale, Some(1.5));
        assert!(!changes[1].enabled);
        assert_eq!(
            changes[2].mode,
            Some(ModeRequest {
                size: (1920, 1080),
                refresh_mhz: None
            })
        );
        // Taken off the list, an output that is off comes back on.
        let back = DisplayConfig::default().changes(&[("DP-1".to_owned(), false)]);
        assert_eq!(back, vec![OutputChange::enable("DP-1")]);
    }
}
