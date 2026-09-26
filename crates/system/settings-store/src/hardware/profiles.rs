//! Display layouts remembered per set of attached monitors.
//!
//! A layout belongs to the monitors it was arranged for, not to the ports
//! they happened to use: the office dock restores the office arrangement
//! whichever DP port the dock enumerates first, and a different monitor
//! on a familiar port never inherits a stranger's position or mode. Each
//! attached sink contributes a token — its EDID make, model and serial, or
//! its connector label when it has no EDID — and the sorted tokens form the
//! set's key. Two identical monitors (same make, model and serial, or both
//! reporting no serial) are told apart by their connector.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{DisplayConfig, MonitorIdentity, OutputDescriptor};

/// One attached sink as set matching sees it.
#[derive(Clone, Copy, Debug)]
pub struct AttachedOutput<'a> {
    /// Collision-free persistence key (see [`OutputDescriptor::connector`]).
    pub connector: &'a str,
    /// Raw DRM connector label, e.g. `DP-2`.
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

/// The monitors attached right now, and the key their layout is saved
/// under.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplaySet {
    pub key: String,
    /// Persistence key of each attached output → its identity token.
    pub members: BTreeMap<String, String>,
}

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

/// An EDID identity token, or `None` when the EDID names nothing that
/// could recognise the monitor again.
fn monitor_token(monitor: &MonitorIdentity) -> Option<String> {
    if monitor.make.is_empty() && monitor.model.is_empty() && monitor.serial.is_empty() {
        return None;
    }
    Some(format!(
        "edid:{}|{}|{}",
        monitor.make, monitor.model, monitor.serial
    ))
}

/// A layout together with the identity of each output it arranged, so it
/// can follow a monitor that comes back on another port.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayProfile {
    /// Persistence key at save time → identity token.
    pub members: BTreeMap<String, String>,
    pub config: DisplayConfig,
}

/// Every remembered layout, keyed by [`DisplaySet::key`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayProfiles {
    pub profiles: BTreeMap<String, DisplayProfile>,
}

impl DisplayProfiles {
    const SECTION: &'static str = "display_profiles";

    pub fn load() -> Self {
        serde_json::from_value(serde_json::Value::Object(crate::load_section(
            Self::SECTION,
        )))
        .unwrap_or_default()
    }

    /// # Errors
    ///
    /// Returns an error if `self` can't be serialised or the section can't
    /// be written.
    pub fn save(&self) -> std::io::Result<()> {
        let value = serde_json::to_value(self).map_err(std::io::Error::other)?;
        crate::update_section(
            Self::SECTION,
            value.as_object().cloned().unwrap_or_default(),
        )
    }

    /// The layout saved for exactly this set of monitors, with each output
    /// re-keyed to where that monitor is attached now. `None` for a set
    /// never arranged before: the compositor then lays it out itself.
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

    /// Remember `config` as the layout for `set`. Entries for outputs that
    /// are not part of the set are dropped: they describe another set.
    pub fn remember(&mut self, set: &DisplaySet, config: &DisplayConfig) {
        let mut kept = DisplayConfig::default();
        for (connector, output) in &config.outputs {
            if set.members.contains_key(connector) {
                kept.outputs.insert(connector.clone(), output.clone());
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::OutputConfig;

    fn monitor(make: &str, model: &str, serial: &str) -> MonitorIdentity {
        MonitorIdentity {
            make: make.into(),
            model: model.into(),
            serial: serial.into(),
            physical_size_mm: None,
        }
    }

    fn attached<'a>(
        connector: &'a str,
        monitor: Option<&'a MonitorIdentity>,
    ) -> AttachedOutput<'a> {
        AttachedOutput {
            connector,
            connector_label: connector,
            monitor,
        }
    }

    fn at(x: i32) -> OutputConfig {
        OutputConfig {
            x,
            ..OutputConfig::default()
        }
    }

    #[test]
    fn the_key_names_the_monitors_not_the_ports_they_use() {
        let panel = monitor("BOE", "NE135FBM-N41", "");
        let office = monitor("Dell", "DELL U2720Q", "7XK2P13");
        let docked_on_dp2 = DisplaySet::of([
            attached("eDP-1", Some(&panel)),
            attached("DP-2", Some(&office)),
        ]);
        let docked_on_dp5 = DisplaySet::of([
            attached("DP-5", Some(&office)),
            attached("eDP-1", Some(&panel)),
        ]);
        assert_eq!(docked_on_dp2.key, docked_on_dp5.key);

        let other = monitor("LG", "LG HDR 4K", "104NTAB1C123");
        let at_home = DisplaySet::of([
            attached("eDP-1", Some(&panel)),
            attached("DP-2", Some(&other)),
        ]);
        assert_ne!(
            docked_on_dp2.key, at_home.key,
            "another monitor on the same port is another set"
        );
    }

    #[test]
    fn identical_monitors_are_told_apart_by_connector() {
        let twin = monitor("Dell", "DELL P2419H", "");
        let set = DisplaySet::of([attached("DP-1", Some(&twin)), attached("DP-2", Some(&twin))]);
        assert_ne!(set.members["DP-1"], set.members["DP-2"]);
        assert_eq!(set.key.matches('+').count(), 1);
    }

    #[test]
    fn a_sink_without_edid_is_known_by_its_connector() {
        let blank = MonitorIdentity::default();
        let set = DisplaySet::of([
            attached("Virtual-1", Some(&blank)),
            attached("HDMI-A-1", None),
        ]);
        assert_eq!(set.key, "connector:HDMI-A-1+connector:Virtual-1");
    }

    #[test]
    fn plugging_the_office_dock_restores_the_office_layout_on_new_ports() {
        let panel = monitor("BOE", "NE135FBM-N41", "");
        let left = monitor("Dell", "DELL U2720Q", "AAA");
        let right = monitor("Dell", "DELL U2720Q", "BBB");
        let saved_set = DisplaySet::of([
            attached("eDP-1", Some(&panel)),
            attached("DP-5", Some(&left)),
            attached("DP-6", Some(&right)),
        ]);
        let mut config = DisplayConfig::default();
        config.outputs.insert("eDP-1".into(), at(2560));
        config.outputs.insert("DP-5".into(), at(0));
        config.outputs.insert(
            "DP-6".into(),
            OutputConfig {
                mirror_of: Some("DP-5".into()),
                ..at(0)
            },
        );
        config.outputs.insert("HDMI-A-1".into(), at(9999));
        let mut profiles = DisplayProfiles::default();
        profiles.remember(&saved_set, &config);

        // The dock's MST ports enumerate the other way round next boot.
        let replugged = DisplaySet::of([
            attached("eDP-1", Some(&panel)),
            attached("DP-7", Some(&right)),
            attached("DP-8", Some(&left)),
        ]);
        let restored = profiles.layout_for(&replugged).expect("office layout");
        assert_eq!(restored.outputs["eDP-1"].x, 2560);
        assert_eq!(restored.outputs["DP-8"].x, 0);
        assert_eq!(restored.outputs["DP-7"].mirror_of.as_deref(), Some("DP-8"));
        assert_eq!(restored.outputs.len(), 3, "a stray entry never follows");
    }

    #[test]
    fn an_unknown_set_has_no_saved_layout() {
        let panel = monitor("BOE", "NE135FBM-N41", "");
        let office = monitor("Dell", "DELL U2720Q", "7XK2P13");
        let stranger = monitor("Samsung", "LS27A600U", "H4ZR");
        let mut profiles = DisplayProfiles::default();
        let mut config = DisplayConfig::default();
        config.outputs.insert("DP-2".into(), at(-3840));
        profiles.remember(
            &DisplaySet::of([
                attached("eDP-1", Some(&panel)),
                attached("DP-2", Some(&office)),
            ]),
            &config,
        );
        let visitor = DisplaySet::of([
            attached("eDP-1", Some(&panel)),
            attached("DP-2", Some(&stranger)),
        ]);
        assert_eq!(profiles.layout_for(&visitor), None);
    }
}
