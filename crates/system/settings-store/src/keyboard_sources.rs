//! Keyboard assignments retain absent devices' choices independently of discovery.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::hardware::RichInputConfig;

/// The shared physical-keyboard default and the separate software keyboard.
pub const ALL: &str = "all";
pub const OSK: &str = "osk";

/// Editing a configured source changes its options, not the source being typed.
#[cfg_attr(
    feature = "model",
    derive(tilo_resource::Model, tilo_resource::JsonSchema)
)]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", default)]
pub struct SourceOptions {
    pub model: Option<String>,
    pub compose_key: Option<String>,
    pub caps_lock: Option<String>,
    /// Distinguish an explicitly disabled option from an inherited one.
    pub customised: bool,
}

crate::read_through_json_value!(SourceOptions);

/// An ordered source assignment. Device keys are discovery identities, never indices.
#[cfg_attr(
    feature = "model",
    derive(tilo_resource::Model, tilo_resource::JsonSchema)
)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct KeyboardProfile {
    pub sources: Vec<String>,
    pub active: String,
    #[serde(default)]
    pub options: BTreeMap<String, SourceOptions>,
}

crate::read_through_json_value!(KeyboardProfile);

impl KeyboardProfile {
    #[must_use]
    pub fn from_input(input: &RichInputConfig) -> Self {
        Self {
            sources: input.keyboard_layouts.clone(),
            active: input.keyboard_layout.clone(),
            options: BTreeMap::new(),
        }
    }

    /// Invalid physical assignments fall back through the existing migration rules.
    #[must_use]
    pub fn effective(&self, input: &RichInputConfig) -> RichInputConfig {
        let mut result = input.clone();
        result.keyboard_layouts.clone_from(&self.sources);
        result.keyboard_layout.clone_from(&self.active);
        result.normalise_sources();
        if let Some(options) = self.options.get(&result.keyboard_layout) {
            if let Some(model) = options
                .model
                .as_ref()
                .filter(|model| matches!(model.as_str(), "pc104" | "pc105" | "jp106" | "kr106"))
            {
                result.keyboard_model.clone_from(model);
            }
            if options.customised {
                result.compose_key.clone_from(&options.compose_key);
                result.caps_lock.clone_from(&options.caps_lock);
            }
        }
        result
    }
}

/// Decode profiles individually: one obsolete assignment cannot erase its siblings.
///
/// # Errors
/// Returns the deserializer error when the surrounding value cannot be read.
pub fn lenient_profiles<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, KeyboardProfile>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter())
        .filter(|(key, _)| {
            !key.is_empty() && key.len() <= 1024 && !key.chars().any(char::is_control)
        })
        .filter_map(|(key, value)| {
            serde_json::from_value(value.clone())
                .ok()
                .map(|profile| (key.clone(), profile))
        })
        .collect())
}

/// Return the configured assignment, falling back without creating or losing an override.
#[must_use]
pub fn effective(input: &RichInputConfig, scope: &str) -> RichInputConfig {
    if scope != ALL
        && let Some(profile) = input.keyboard_assignments.get(scope)
    {
        return profile.effective(input);
    }
    let mut profile = KeyboardProfile::from_input(input);
    if let Some(defaults) = input.keyboard_assignments.get(ALL) {
        profile.options.clone_from(&defaults.options);
    }
    profile.effective(input)
}

/// Resolve only a currently attached typing keyboard from the compositor's leased snapshot.
#[must_use]
pub fn active_scope() -> String {
    crate::hardware::InputHardwareSnapshot::load()
        .and_then(|snapshot| {
            snapshot.active_keyboard.filter(|key| {
                snapshot.devices.iter().any(|device| {
                    device.kind == crate::hardware::InputDeviceKind::Keyboard && &device.key == key
                })
            })
        })
        .unwrap_or_else(|| ALL.into())
}

/// Effective physical sources follow the keyboard in use, without modifying defaults.
#[must_use]
pub fn current(input: &RichInputConfig) -> RichInputConfig {
    effective(input, &active_scope())
}

/// Serial/physical location distinguishes identical keyboards without using event numbers.
#[must_use]
pub fn device_key(base: &str, serial: &str, physical: &str) -> String {
    let location = if serial.trim().is_empty() {
        physical.trim()
    } else {
        serial.trim()
    };
    if location.is_empty() {
        base.to_owned()
    } else {
        format!("{base}@{location}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_assignments_keep_defaults_and_disconnected_profiles() {
        let mut input = RichInputConfig {
            keyboard_layouts: vec!["us".into(), "fr".into()],
            ..Default::default()
        };
        input.keyboard_assignments.insert(
            "external".into(),
            KeyboardProfile {
                sources: vec!["de".into(), "us".into()],
                active: "de".into(),
                options: BTreeMap::from([(
                    "de".into(),
                    SourceOptions {
                        model: Some("pc104".into()),
                        compose_key: Some("ralt".into()),
                        caps_lock: Some("escape".into()),
                        customised: true,
                    },
                )]),
            },
        );
        let default = effective(&input, ALL);
        assert_eq!(default.keyboard_layouts, ["us", "fr"]);
        let assigned = effective(&input, "external");
        assert_eq!(assigned.keyboard_layouts, ["de", "us"]);
        assert_eq!(assigned.keyboard_layout, "de");
        assert_eq!(assigned.keyboard_model, "pc104");
        assert_eq!(assigned.compose_key.as_deref(), Some("ralt"));
        assert_eq!(effective(&input, "unassigned"), default);
        let restored: RichInputConfig =
            serde_json::from_value(serde_json::to_value(&input).unwrap()).unwrap();
        assert_eq!(restored.keyboard_assignments, input.keyboard_assignments);
    }

    #[test]
    fn keyboard_assignments_decode_valid_siblings_and_stable_identities() {
        let section = serde_json::json!({"keyboard_assignments": {
            "broken": {"sources": 9}, "usb": {"sources": ["de"], "active": "de"}
        }});
        let input = RichInputConfig::from_section(section.as_object().unwrap());
        assert!(!input.keyboard_assignments.contains_key("broken"));
        assert_eq!(effective(&input, "usb").keyboard_layout, "de");
        assert_eq!(
            device_key("keyboard", "serial", "usb1"),
            device_key("keyboard", "serial", "usb2")
        );
        assert_ne!(
            device_key("keyboard", "", "usb1"),
            device_key("keyboard", "", "usb2")
        );
    }
}
