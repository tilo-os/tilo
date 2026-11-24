//! Which controls the Quick Settings panel shows, as the device publishes it.

/// What the Quick Settings resource offers: the controls the panel shows.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.session.quick-settings")]
pub enum LayoutAction {
    /// Show a control on the panel
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    ShowControl {
        /// The control's name, as the panel lists it
        #[param(
            "The control: `wifi`, `bluetooth`, `vpn`, `night_light`, `airplane_mode`, `mobile_data`, `do_not_disturb`, `rotation_lock` or `power_mode`"
        )]
        control: String,
    },
    /// Take a control off the panel. Wi-Fi stays: it is how a desktop without a network gets online
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    HideControl {
        /// The control's name, as the panel lists it
        #[param("The control, as Show control names it")]
        control: String,
    },
    /// Move a control to another place on the panel
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    MoveControl {
        /// The control's name, as the panel lists it
        #[param("The control, as Show control names it")]
        control: String,
        /// The control it stands in front of; none for the last place
        #[param("The control it stands in front of; none for the last place")]
        before: Option<String>,
    },
    /// Put the panel's controls back as they were at first
    #[effect(kind = "outside")]
    #[effect(own_device)]
    ResetControls,
}

/// Which controls the panel shows, as a view of another language reads it
/// (`device.quick_settings`): the controls, in the order they stand, each named as the actions
/// below name them.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct QuickSettingsView {
    #[serde(default)]
    pub controls: Vec<String>,
}

/// The panel's controls as a view of another language reads them (`device.quick_settings`).
#[must_use]
pub(crate) fn controls_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<QuickSettingsView, LayoutAction>(
        "quick-settings",
        "Quick settings",
    )
}
