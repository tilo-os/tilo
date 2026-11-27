//! The device's night light and its installed apps as it publishes them.

use serde::{Deserialize, Serialize};

/// The night light as a view of another language reads and changes it: whether the screen is warm,
/// and the verb that switches it.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct NightLightReading {
    /// Whether the screen is warm.
    pub on: bool,
}

/// The screen's night light.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.session.night-light")]
pub enum NightLightEffect {
    /// Turn the night light on or off
    #[effect(kind = "undoable", label = "session-night-light-toggle")]
    Toggle {
        /// Whether the screen is warm
        #[param("Whether the screen is warm")]
        on: Option<bool>,
    },
}

/// The applications installed on this device, as `session:apps` publishes them: what the
/// launcher lists and opens.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Apps {
    pub installed: Vec<App>,
    /// Why this device runs none of a workspace's apps, in the words a person reads; empty when
    /// it can run them.
    #[serde(default)]
    pub refused: String,
}

/// An application installed on this device: its package id, which `open-app` names, its name in
/// the person's language, its glyph, and its category when it named one.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct App {
    pub key: String,
    pub name: String,
    /// The glyph it shows, by the platform icon's kebab-case name.
    pub icon: String,
    /// `work`, `media`, `play` or `system`; empty when it named none.
    pub category: String,
}

/// The night light as a view of another language reads it (`device.night_light`).
#[must_use]
pub(crate) fn night_light_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<NightLightReading, NightLightEffect>(
        "night-light",
        "Night light",
    )
}

/// The apps as a view of another language reads them (`device.apps`).
#[must_use]
pub(crate) fn apps_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Apps, tilo_resource::NoEffect>("apps", "Apps")
}
