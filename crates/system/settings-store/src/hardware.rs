//! Typed, validated compositor-facing hardware preferences.
//!
//! Settings is not allowed to invent capabilities: callers pass a live
//! [`OutputDescriptor`] to [`DisplayConfig::validate_for`] and only the
//! resulting plan may be applied.  The file is durable intent; DRM/libinput
//! remain the authority for what the currently attached hardware can do.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputTransform {
    #[default]
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputMode {
    pub width: u32,
    pub height: u32,
    /// Milli-Hz keeps 59.94 and 60 Hz distinct.
    pub refresh_mhz: u32,
}

impl OutputMode {
    pub fn label(self) -> String {
        format!(
            "{} × {} · {:.2} Hz",
            self.width,
            self.height,
            self.refresh_mhz as f64 / 1000.0
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDescriptor {
    /// Connector name exposed by DRM, e.g. `eDP-1` or `HDMI-A-1`.
    pub connector: String,
    pub modes: Vec<OutputMode>,
    pub hdr: bool,
    pub vrr: bool,
    pub color_profiles: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    pub enabled: bool,
    pub primary: bool,
    /// Logical top-left in the shared infinite workspace plane.
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub transform: OutputTransform,
    pub mode: Option<OutputMode>,
    pub mirror_of: Option<String>,
    pub hdr: bool,
    pub vrr: bool,
    pub color_profile: Option<String>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            primary: false,
            x: 0,
            y: 0,
            scale: 1.0,
            transform: OutputTransform::Normal,
            mode: None,
            mirror_of: None,
            hdr: false,
            vrr: false,
            color_profile: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    pub outputs: BTreeMap<String, OutputConfig>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayConfigError {
    NoEnabledOutput,
    MultiplePrimary,
    UnknownMirrorTarget { output: String, target: String },
    MirrorCycle { output: String },
    UnsupportedMode { output: String },
    InvalidScale { output: String },
    UnsupportedHdr { output: String },
    UnsupportedVrr { output: String },
    UnsupportedColorProfile { output: String },
}

impl DisplayConfig {
    /// Validate intent against a fresh hardware snapshot. Unknown/disconnected
    /// configured outputs are retained for re-plug but ignored in this plan.
    pub fn validate_for(&self, attached: &[OutputDescriptor]) -> Result<(), DisplayConfigError> {
        let names: BTreeSet<&str> = attached.iter().map(|o| o.connector.as_str()).collect();
        let live: Vec<(&OutputDescriptor, &OutputConfig)> = attached
            .iter()
            .filter_map(|d| self.outputs.get(&d.connector).map(|c| (d, c)))
            .collect();
        if !attached.is_empty() && !live.iter().any(|(_, c)| c.enabled) {
            return Err(DisplayConfigError::NoEnabledOutput);
        }
        if live.iter().filter(|(_, c)| c.enabled && c.primary).count() > 1 {
            return Err(DisplayConfigError::MultiplePrimary);
        }
        for (desc, cfg) in live {
            if !cfg.scale.is_finite() || !(0.5..=4.0).contains(&cfg.scale) {
                return Err(DisplayConfigError::InvalidScale {
                    output: desc.connector.clone(),
                });
            }
            if let Some(mode) = cfg.mode
                && !desc.modes.contains(&mode)
            {
                return Err(DisplayConfigError::UnsupportedMode {
                    output: desc.connector.clone(),
                });
            }
            if cfg.hdr && !desc.hdr {
                return Err(DisplayConfigError::UnsupportedHdr {
                    output: desc.connector.clone(),
                });
            }
            if cfg.vrr && !desc.vrr {
                return Err(DisplayConfigError::UnsupportedVrr {
                    output: desc.connector.clone(),
                });
            }
            if let Some(profile) = &cfg.color_profile
                && !desc.color_profiles.contains(profile)
            {
                return Err(DisplayConfigError::UnsupportedColorProfile {
                    output: desc.connector.clone(),
                });
            }
            if let Some(target) = &cfg.mirror_of {
                if !names.contains(target.as_str()) {
                    return Err(DisplayConfigError::UnknownMirrorTarget {
                        output: desc.connector.clone(),
                        target: target.clone(),
                    });
                }
                let mut seen = BTreeSet::from([desc.connector.as_str()]);
                let mut cursor = target.as_str();
                while let Some(next) = self
                    .outputs
                    .get(cursor)
                    .and_then(|c| c.mirror_of.as_deref())
                {
                    if !seen.insert(cursor) || seen.contains(next) {
                        return Err(DisplayConfigError::MirrorCycle {
                            output: desc.connector.clone(),
                        });
                    }
                    cursor = next;
                }
            }
        }
        Ok(())
    }

    pub fn load() -> Self {
        serde_json::from_value(serde_json::Value::Object(super::load_section(
            "display_outputs",
        )))
        .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let value = serde_json::to_value(self).map_err(std::io::Error::other)?;
        super::update_section(
            "display_outputs",
            value.as_object().cloned().unwrap_or_default(),
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccelProfile {
    Flat,
    #[default]
    Adaptive,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RichInputConfig {
    pub pointer_speed: f64,
    pub natural_scroll: bool,
    pub tap_to_click: bool,
    pub tap_drag: bool,
    pub disable_while_typing: bool,
    pub middle_emulation: bool,
    pub left_handed: bool,
    pub accel_profile: AccelProfile,
    pub key_repeat: f64,
    pub layouts: Vec<String>,
    pub compose_key: Option<String>,
    pub touchpad_gestures: bool,
    pub pen_output: Option<String>,
}

impl Default for RichInputConfig {
    fn default() -> Self {
        Self {
            pointer_speed: 0.5,
            natural_scroll: false,
            tap_to_click: true,
            tap_drag: true,
            disable_while_typing: true,
            middle_emulation: true,
            left_handed: false,
            accel_profile: AccelProfile::Adaptive,
            key_repeat: 0.5,
            layouts: vec!["us".into()],
            compose_key: None,
            touchpad_gestures: true,
            pen_output: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutAction {
    Search,
    WorkspaceOverview,
    Lock,
    QuickSettings,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KeyChord {
    pub modifiers: BTreeSet<String>,
    pub key: String,
}

impl KeyChord {
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let mut parts: Vec<_> = text
            .split('+')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        let key = parts.pop().ok_or("shortcut is empty")?;
        if key.contains(char::is_whitespace) {
            return Err("key must be one key");
        }
        let mut modifiers = BTreeSet::new();
        for part in parts {
            let canonical = match part.to_ascii_lowercase().as_str() {
                "super" | "meta" | "cmd" => "Super",
                "ctrl" | "control" => "Ctrl",
                "alt" => "Alt",
                "shift" => "Shift",
                _ => return Err("unknown modifier"),
            };
            if !modifiers.insert(canonical.into()) {
                return Err("duplicate modifier");
            }
        }
        if modifiers.is_empty() && key.len() == 1 {
            return Err("printable shortcuts need a modifier");
        }
        Ok(Self {
            modifiers,
            key: key.to_ascii_lowercase(),
        })
    }

    pub fn display(&self) -> String {
        self.modifiers
            .iter()
            .cloned()
            .chain(std::iter::once(self.key.clone()))
            .collect::<Vec<_>>()
            .join("+")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortcutConfig {
    pub bindings: BTreeMap<ShortcutAction, KeyChord>,
}

impl Default for ShortcutConfig {
    fn default() -> Self {
        let bindings = [
            (ShortcutAction::Search, KeyChord::parse("Super+K").unwrap()),
            (
                ShortcutAction::WorkspaceOverview,
                KeyChord::parse("Super+Tab").unwrap(),
            ),
            (ShortcutAction::Lock, KeyChord::parse("Super+L").unwrap()),
            (
                ShortcutAction::QuickSettings,
                KeyChord::parse("Super+Q").unwrap(),
            ),
        ]
        .into_iter()
        .collect();
        Self { bindings }
    }
}

impl ShortcutConfig {
    pub fn set(&mut self, action: ShortcutAction, chord: KeyChord) -> Result<(), ShortcutAction> {
        if let Some((other, _)) = self
            .bindings
            .iter()
            .find(|(a, c)| **a != action && **c == chord)
        {
            return Err(*other);
        }
        self.bindings.insert(action, chord);
        Ok(())
    }
    pub fn load() -> Self {
        serde_json::from_value(serde_json::Value::Object(super::load_section("shortcuts")))
            .unwrap_or_default()
    }
    pub fn save(&self) -> std::io::Result<()> {
        let value = serde_json::to_value(self).map_err(std::io::Error::other)?;
        super::update_section("shortcuts", value.as_object().cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn display(name: &str) -> OutputDescriptor {
        OutputDescriptor {
            connector: name.into(),
            modes: vec![OutputMode {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
            }],
            hdr: false,
            vrr: false,
            color_profiles: vec![],
        }
    }
    #[test]
    fn rejects_modes_the_connector_does_not_advertise() {
        let mut cfg = DisplayConfig::default();
        cfg.outputs.insert(
            "DP-1".into(),
            OutputConfig {
                mode: Some(OutputMode {
                    width: 3840,
                    height: 2160,
                    refresh_mhz: 60_000,
                }),
                ..Default::default()
            },
        );
        assert!(matches!(
            cfg.validate_for(&[display("DP-1")]),
            Err(DisplayConfigError::UnsupportedMode { .. })
        ));
    }
    #[test]
    fn rejects_mirror_cycles() {
        let mut cfg = DisplayConfig::default();
        cfg.outputs.insert(
            "DP-1".into(),
            OutputConfig {
                mirror_of: Some("DP-2".into()),
                ..Default::default()
            },
        );
        cfg.outputs.insert(
            "DP-2".into(),
            OutputConfig {
                mirror_of: Some("DP-1".into()),
                ..Default::default()
            },
        );
        assert!(matches!(
            cfg.validate_for(&[display("DP-1"), display("DP-2")]),
            Err(DisplayConfigError::MirrorCycle { .. })
        ));
    }
    #[test]
    fn shortcut_parser_normalises_and_rejects_plain_letters() {
        assert_eq!(KeyChord::parse("meta + K").unwrap().display(), "Super+k");
        assert!(KeyChord::parse("k").is_err());
    }
    #[test]
    fn shortcut_conflicts_are_rejected_without_overwriting() {
        let mut cfg = ShortcutConfig::default();
        let old = cfg.bindings[&ShortcutAction::Lock].clone();
        assert_eq!(
            cfg.set(ShortcutAction::Lock, KeyChord::parse("Super+K").unwrap()),
            Err(ShortcutAction::Search)
        );
        assert_eq!(cfg.bindings[&ShortcutAction::Lock], old);
    }
}
