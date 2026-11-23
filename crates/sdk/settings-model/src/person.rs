//! The person's settings as one resource of the workspace whose only member is them, which every
//! device of theirs takes part in: each leaf of a section they hold, under `tilo.<section>.<leaf>`
//! ([`key`]), its value the JSON it is as text, so two devices' changes merge leaf by leaf. Its
//! one action, [`PersonSettings::set_leaves`], is how every device changes it: a device invokes
//! the action at its daemon and writes nothing of the resource itself.

use tilo_resource::{JsonSchema, Map, Model, Refusal};

/// The app whose resource the person's settings are.
pub const APP: &str = "so.tilo.settings";
/// The resource of the person's workspace that holds them.
pub const RESOURCE: &str = "person";
/// The kind the resource is published as.
pub const KIND: &str = "tilo.settings.person";
/// The action that sets its leaves.
pub const SET_LEAVES: &str = "set-leaves";

/// The largest value of the person's, as JSON text, that their devices carry. An app's entry in
/// [`crate::APP_PERSONAL_SECTION`] is one value, so its whole object must fit.
pub const MAX_PERSONAL_VALUE_BYTES: usize = 64 * 1024;

/// One leaf as [`SET_LEAVES`] carries it: a section's leaf and its value as JSON text, `None` for
/// one that goes.
#[derive(
    Model, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema,
)]
pub struct Leaf {
    pub section: String,
    pub leaf: String,
    #[serde(default)]
    pub value: Option<String>,
}

/// The key `section`'s `leaf` is kept under: `tilo.<section>.<leaf>`.
///
/// # Errors
/// The section is empty or holds `.`, or the leaf is empty.
pub fn key(section: &str, leaf: &str) -> Result<String, String> {
    if section.is_empty() || section.contains('.') {
        return Err(format!("`{section}` is no settings section"));
    }
    if leaf.is_empty() {
        return Err("a setting has a leaf".to_owned());
    }
    Ok(format!("tilo.{section}.{leaf}"))
}

/// The section and leaf `key` names, as [`key`] made it.
#[must_use]
pub fn leaf_of(key: &str) -> Option<(&str, &str)> {
    key.strip_prefix("tilo.")?.split_once('.')
}

/// The person's settings as their resource holds them.
#[derive(Model, Default)]
pub struct PersonSettings {
    /// Every leaf, under the key [`key`] makes of its section and leaf, as JSON text.
    pub leaves: Map<String>,
}

#[tilo_resource::resource(kind = "tilo.settings.person")]
impl PersonSettings {
    /// Set the person's settings on their other devices: each leaf's value, none for one that
    /// goes.
    #[action]
    pub fn set_leaves(
        &mut self,
        #[param("Each leaf, with its value as JSON text; none for one that goes")] leaves: Vec<
            Leaf,
        >,
    ) -> Result<(), Refusal> {
        for leaf in leaves {
            let key = key(&leaf.section, &leaf.leaf).map_err(Refusal::new)?;
            match leaf.value {
                Some(value) => {
                    if value.len() > MAX_PERSONAL_VALUE_BYTES {
                        return Err(Refusal::new(format!(
                            "{key} is too large for the person's devices"
                        )));
                    }
                    if serde_json::from_str::<serde_json::Value>(&value).is_err() {
                        return Err(Refusal::new(format!("{key} holds no JSON")));
                    }
                    self.leaves.insert(key, value);
                }
                None => {
                    self.leaves.remove(&key);
                }
            }
        }
        Ok(())
    }
}

/// The resource's kind, hosted by the settings: what applies its action.
#[must_use]
pub fn kind() -> tilo_resource::ResourceKind {
    tilo_resource::ResourceKind::of::<PersonSettings>().hosted_by(APP, "Settings")
}
