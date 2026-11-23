//! Display layouts remembered per set of attached monitors: the office dock restores the office
//! placement whichever DP port it enumerates first, and a different monitor on a familiar port
//! never inherits a stranger's position or mode.
//!
//! Each attached sink contributes a token (its EDID make, model and serial, or its connector label
//! when it has no EDID) and the sorted tokens form the set's key; two identical monitors are told
//! apart by their connector. Each monitor also keeps the modes and scales it has shown in a kept
//! layout, so a display tool's change to one of them needs no confirmation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{DisplayConfig, MonitorIdentity, OutputDescriptor, OutputMode, OutputTransform};

#[derive(Clone, Copy, Debug)]
pub struct AttachedOutput<'a> {
    pub connector: &'a str,
    pub connector_label: &'a str,
    pub monitor: Option<&'a MonitorIdentity>,
}

impl<'a> From<&'a OutputDescriptor> for AttachedOutput<'a> {
    fn from(descriptor: &'a OutputDescriptor) -> Self {
        Self {
            connector: &descriptor.connector,
            connector_label: descriptor.label(),
            monitor: descriptor.monitor.as_ref(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", default)]
pub struct DisplaySet {
    pub key: String,
    pub members: BTreeMap<String, String>,
}

crate::read_through_json_value!(DisplaySet);

#[cfg(feature = "model")]
tilo_resource::json_model!(DisplaySet);

impl DisplaySet {
    pub fn of<'a>(outputs: impl IntoIterator<Item = AttachedOutput<'a>>) -> Self {
        let outputs: Vec<AttachedOutput<'a>> = outputs.into_iter().collect();
        let identities: Vec<Option<String>> = outputs
            .iter()
            .map(|output| output.monitor.and_then(monitor_token))
            .collect();
        let mut members = BTreeMap::new();
        let mut tokens = Vec::with_capacity(outputs.len());
        for (output, identity) in outputs.iter().zip(&identities) {
            let token = match identity {
                Some(identity)
                    if identities
                        .iter()
                        .filter(|other| other.as_ref() == Some(identity))
                        .count()
                        == 1 =>
                {
                    identity.clone()
                }
                Some(identity) => format!("{identity}@{}", output.connector_label),
                None => format!("connector:{}", output.connector_label),
            };
            members.insert(output.connector.to_owned(), token.clone());
            tokens.push(token);
        }
        tokens.sort();
        Self {
            key: tokens.join("+"),
            members,
        }
    }

    pub fn of_descriptors(descriptors: &[OutputDescriptor]) -> Self {
        Self::of(descriptors.iter().map(AttachedOutput::from))
    }
}

fn monitor_token(monitor: &MonitorIdentity) -> Option<String> {
    if monitor.make.is_empty() && monitor.model.is_empty() && monitor.serial.is_empty() {
        return None;
    }
    Some(format!(
        "edid:{}|{}|{}",
        monitor.make, monitor.model, monitor.serial
    ))
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayProfile {
    pub members: BTreeMap<String, String>,
    pub config: DisplayConfig,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorHistory {
    pub modes: Vec<OutputMode>,
    pub scales: Vec<f64>,
    pub last: Option<MonitorLast>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonitorLast {
    pub scale: f64,
    pub transform: OutputTransform,
}

impl MonitorHistory {
    const LIMIT: usize = 32;

    pub fn has_mode(&self, mode: OutputMode) -> bool {
        self.modes.contains(&mode)
    }

    pub fn has_scale(&self, scale: f64) -> bool {
        self.scales.iter().any(|known| same_scale(*known, scale))
    }

    fn note(&mut self, mode: Option<OutputMode>, scale: f64) {
        if let Some(mode) = mode.filter(|mode| !self.has_mode(*mode)) {
            if self.modes.len() >= Self::LIMIT {
                self.modes.remove(0);
            }
            self.modes.push(mode);
        }
        if scale.is_finite() && scale > 0.0 && !self.has_scale(scale) {
            if self.scales.len() >= Self::LIMIT {
                self.scales.remove(0);
            }
            self.scales.push(scale);
        }
    }
}

pub fn same_scale(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayProfiles {
    pub profiles: BTreeMap<String, DisplayProfile>,
    pub monitors: BTreeMap<String, MonitorHistory>,
}

impl DisplayProfiles {
    const SECTION: &'static str = "display_profiles";

    pub fn load() -> Self {
        serde_json::from_value(serde_json::Value::Object(crate::load_section(
            Self::SECTION,
        )))
        .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let value = serde_json::to_value(self).map_err(std::io::Error::other)?;
        crate::update_section(
            Self::SECTION,
            value.as_object().cloned().unwrap_or_default(),
        )
    }

    pub fn layout_for(&self, set: &DisplaySet) -> Option<DisplayConfig> {
        let profile = self.profiles.get(&set.key)?;
        let mut current_by_token = BTreeMap::<&str, &str>::new();
        for (connector, token) in &set.members {
            current_by_token.insert(token, connector);
        }
        let renamed: BTreeMap<&str, &str> = profile
            .members
            .iter()
            .filter_map(|(saved, token)| {
                current_by_token
                    .get(token.as_str())
                    .map(|current| (saved.as_str(), *current))
            })
            .collect();
        let mut config = DisplayConfig::default();
        for (saved, output) in &profile.config.outputs {
            let Some(current) = renamed.get(saved.as_str()) else {
                continue;
            };
            let mut output = output.clone();
            output.mirror_of = output
                .mirror_of
                .as_deref()
                .and_then(|target| renamed.get(target))
                .map(|target| (*target).to_owned());
            config.outputs.insert((*current).to_owned(), output);
        }
        Some(config)
    }

    pub fn last_shown_in(&self, set: &DisplaySet) -> BTreeMap<String, MonitorLast> {
        set.members
            .iter()
            .filter_map(|(connector, token)| {
                Some((connector.clone(), self.monitors.get(token)?.last?))
            })
            .collect()
    }

    pub fn remember(&mut self, set: &DisplaySet, config: &DisplayConfig) {
        let mut kept = DisplayConfig::default();
        for (connector, output) in &config.outputs {
            let Some(token) = set.members.get(connector) else {
                continue;
            };
            if output.enabled {
                let history = self.monitors.entry(token.clone()).or_default();
                history.note(output.mode, output.scale);
                if output.scale.is_finite() && output.scale > 0.0 {
                    history.last = Some(MonitorLast {
                        scale: output.scale,
                        transform: output.transform,
                    });
                }
            }
            kept.outputs.insert(connector.clone(), output.clone());
        }
        self.profiles.insert(
            set.key.clone(),
            DisplayProfile {
                members: set.members.clone(),
                config: kept,
            },
        );
    }
}

impl DisplayConfig {
    pub fn load_effective_remembering(set: &DisplaySet) -> (Self, BTreeMap<String, MonitorLast>) {
        let profiles = DisplayProfiles::load();
        let committed = profiles.layout_for(set).unwrap_or_default();
        (
            Self::effective_over(set, committed),
            profiles.last_shown_in(set),
        )
    }
}
