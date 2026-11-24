//! Each tablet's pen buttons, eraser, pad buttons, rings and strips as a reading a view shows:
//! the id of what each one does now, in the same words the verbs that set them take.
//!
//! The section (`settings.input-device`) keeps each tablet's mapping as its own typed record,
//! whose enum fields a view cannot turn back into the ids the verbs take; this is the same
//! stored record read as those ids, so a pane names the choice each control is on and changes it
//! through the section's own verbs. It is a reading beside the section, never a second copy:
//! every change goes through the section.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The resource the reading is published as.
pub const RESOURCE: &str = "settings/tablet-choices";
/// Its kind, as the reading's type is named and a view reads `settings.tablet_choices`.
pub const KIND: &str = "settings.tablet-choices";

/// What the controls of one tablet or pad do now, each as the choice id its verb takes.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct TabletChoiceIds {
    /// The pen's barrel button nearest the tip.
    pub lower_button: String,
    /// The barrel button further from the tip.
    pub upper_button: String,
    /// A third barrel button, on pens that have one.
    pub third_button: String,
    /// The eraser end of the pen.
    pub eraser: String,
    /// Pad button number, as a key, to its choice: `show-map`, `none`, or a shortcut.
    pub pad_buttons: BTreeMap<String, String>,
    /// Ring number, as a key, to what it does in each mode of its group, by mode.
    pub pad_rings: BTreeMap<String, Vec<String>>,
    /// Strip number, as a key, to what it does in each mode, like [`Self::pad_rings`].
    pub pad_strips: BTreeMap<String, Vec<String>>,
}

/// Every tablet the person's settings keep a mapping for.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct TabletChoices {
    /// The device key its settings are kept under, to what its controls do.
    pub tablets: BTreeMap<String, TabletChoiceIds>,
}

impl TabletChoices {
    /// The choices `stored` keeps: the input device section of the settings document, as it is
    /// written there.
    #[must_use]
    pub fn of(stored: &Value) -> Self {
        let tablets = stored
            .get("tablets")
            .and_then(Value::as_object)
            .map(|tablets| {
                tablets
                    .iter()
                    .filter_map(|(device, config)| {
                        let config =
                            serde_json::from_value::<settings_store::hardware::TabletConfig>(
                                config.clone(),
                            )
                            .ok()?;
                        Some((device.clone(), TabletChoiceIds::of(&config)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { tablets }
    }
}

impl TabletChoiceIds {
    /// What `config`'s controls do now, by the ids the verbs take.
    #[must_use]
    pub fn of(config: &settings_store::hardware::TabletConfig) -> Self {
        use crate::prefs::tablet_choices::{
            dial_id, eraser_id, pad_button_id, shortcut_id, stylus_button_id,
        };
        let mut pad_buttons: BTreeMap<String, String> = config
            .pad_buttons
            .iter()
            .map(|(button, shortcut)| (button.to_string(), shortcut_id(*shortcut)))
            .collect();
        if let Some(button) = config.pad_map_button {
            pad_buttons.insert(button.to_string(), pad_button_id(config, button));
        }
        let dials = |held: &BTreeMap<u32, Vec<settings_store::hardware::PadDialAction>>| {
            held.iter()
                .map(|(index, modes)| {
                    (
                        index.to_string(),
                        modes
                            .iter()
                            .map(|action| dial_id(*action).to_owned())
                            .collect(),
                    )
                })
                .collect()
        };
        Self {
            lower_button: stylus_button_id(config.lower_button),
            upper_button: stylus_button_id(config.upper_button),
            third_button: stylus_button_id(config.third_button),
            eraser: eraser_id(config.eraser),
            pad_buttons,
            pad_rings: dials(&config.pad_rings),
            pad_strips: dials(&config.pad_strips),
        }
    }
}

/// The tablet choices as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "settings.tablet-choices", label = "Tablet controls")]
pub struct TabletChoicesReading;

/// The reading, as the settings service publishes it beside the section. It offers no verb of
/// its own: a change is the section's.
#[must_use]
pub fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<TabletChoices, tilo_resource::NoEffect>(
        KIND,
        "Tablet controls",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use settings_store::hardware::{
        PadDialAction, ShortcutAction, StylusButtonAction, TabletConfig,
    };

    #[test]
    fn a_tablets_controls_read_as_the_ids_their_verbs_take() {
        let mut config = TabletConfig::default();
        config.upper_button = StylusButtonAction::Shortcut(ShortcutAction::Search);
        config.eraser = settings_store::hardware::EraserAction::Shortcut(ShortcutAction::Lock);
        config
            .pad_buttons
            .insert(2, ShortcutAction::FocusPreviousWindow);
        config.pad_map_button = Some(4);
        config.pad_rings.insert(
            0,
            vec![
                PadDialAction::Zoom,
                PadDialAction::Nothing,
                PadDialAction::Volume,
            ],
        );
        config.pad_strips.insert(0, vec![PadDialAction::Brightness]);
        let stored = serde_json::json!({
            "tablets": { "056a:0374:Wacom": serde_json::to_value(&config).unwrap() },
        });
        let choices = TabletChoices::of(&stored);
        let ids = choices.tablets.get("056a:0374:Wacom").expect("the tablet");
        assert_eq!(ids.lower_button, "default");
        assert_eq!(ids.upper_button, "shortcut:search");
        assert_eq!(ids.eraser, "shortcut:lock");
        assert_eq!(
            ids.pad_buttons.get("2").map(String::as_str),
            Some("shortcut:focus-previous-window"),
            "the shortcut catalogue's own id, which its verb accepts"
        );
        assert_eq!(
            ids.pad_buttons.get("4").map(String::as_str),
            Some("show-map")
        );
        assert_eq!(
            ids.pad_rings.get("0").map(Vec::as_slice),
            Some(["zoom".to_owned(), "nothing".to_owned(), "volume".to_owned()].as_slice()),
            "every mode the ring has, in order"
        );
        assert_eq!(
            ids.pad_strips.get("0").map(Vec::as_slice),
            Some(["brightness".to_owned()].as_slice())
        );
    }

    #[test]
    fn a_settings_document_with_no_tablets_reads_none() {
        assert_eq!(
            TabletChoices::of(&serde_json::json!({})),
            TabletChoices::default()
        );
    }
}
