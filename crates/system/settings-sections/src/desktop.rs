//! The desktop's own sections: how the shell is navigated, which app opens
//! each kind of file, the clock and the displays' arrangement. The session
//! keeps them like every other section, so an agent reads and changes them
//! whether or not Settings is open.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use settings_store::person_displays::Arrangement;
use ui::tilo_resource;

/// The shell's desktop navigation preferences, as the person's settings
/// hold them.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
pub struct DesktopControls {
    /// Add Super+Ctrl+h/j/k/l aliases for moving between windows and strips,
    /// beside the arrow keys.
    #[serde(default)]
    pub vim_mode: bool,
}

impl ui::storage::SettingsSchema for DesktopControls {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(settings_store::desktop_controls::DESKTOP_CONTROLS_SECTION);
}

#[ui::tilo_resource::resource(
    kind = "settings.desktop-controls",
    label = "Desktop controls",
    version = 2,
    migrate = crate::leaves_moved_out
)]
impl DesktopControls {}

/// The shell's desktop preferences this device holds: a small laptop panel
/// and a large desk display want different answers.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
pub struct DesktopControlsDevice {
    /// Hide the top bar until the pointer reaches for it; shown in the
    /// Appearance pane.
    #[serde(default)]
    pub hide_top_bar: bool,
}

impl ui::storage::SettingsSchema for DesktopControlsDevice {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared(
        settings_store::desktop_controls::DESKTOP_CONTROLS_DEVICE_SECTION,
    );
}

#[ui::tilo_resource::resource(
    kind = "settings.desktop-controls-device",
    label = "Desktop controls on this device"
)]
impl DesktopControlsDevice {}

/// The person's default apps, as their settings hold them.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
pub struct DefaultApps {
    /// The app each kind of file or link opens with, by the kind's id
    /// (`ext:pdf`, `mime:text/html`, `scheme:https`).
    #[serde(default)]
    #[model(readonly)]
    pub handlers: BTreeMap<String, String>,
}

impl ui::storage::SettingsSchema for DefaultApps {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(desktop_policy::PERSONAL_DEFAULTS_SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.default-apps", label = "Default apps")]
impl DefaultApps {}

/// This device's clock settings: whether its time zone follows its
/// location. The session's zone follower reads the same leaf
/// ([`settings_store::clock::AUTOMATIC_ZONE`]).
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    ui::tilo_resource::Model,
)]
#[model(defaults)]
pub struct ClockPrefs {
    /// The zone follows the device's location.
    #[serde(default)]
    pub automatic_zone: bool,
}

impl ui::storage::SettingsSchema for ClockPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared(settings_store::clock::SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.clock", label = "Clock")]
impl ClockPrefs {}

/// How the person's displays are arranged, wherever they connect them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(default)]
pub struct SharedDisplayPrefs {
    #[model(readonly)]
    pub arrangement: Arrangement,
}

impl ui::storage::SettingsSchema for SharedDisplayPrefs {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(settings_store::person_displays::ARRANGEMENT_SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.display-arrangement", label = "Display arrangement")]
impl SharedDisplayPrefs {
    /// Arrange the person's displays: each display entry given takes the
    /// place of the stored one where it differs, at a revision above every
    /// stored one, so edits made elsewhere to other displays are kept.
    #[action(id = "arrange-displays")]
    pub fn arrange_displays(
        &mut self,
        #[param("The arrangement, as its display entries")] arrangement: serde_json::Value,
    ) -> Result<(), tilo_resource::Refusal> {
        let arrangement = Arrangement::from_value(&arrangement).ok_or_else(|| {
            tilo_resource::Refusal::new("the arrangement is malformed or out of bounds")
        })?;
        self.arrangement = self.arrangement.with_changes(arrangement.displays);
        Ok(())
    }
}

/// Whether a display nearby offers itself as this device's input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(default)]
pub struct AutomaticDisplayPrefs {
    /// A device administrator's choice: changed only through its verb.
    #[model(readonly)]
    pub automatic_display_input: bool,
}

impl Default for AutomaticDisplayPrefs {
    fn default() -> Self {
        Self {
            automatic_display_input: settings_store::continuity_privacy::ON_UNLESS_SET,
        }
    }
}

impl ui::storage::SettingsSchema for AutomaticDisplayPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("continuity");
}

#[ui::tilo_resource::resource(
    kind = "settings.automatic-display",
    label = "Automatic display input"
)]
impl AutomaticDisplayPrefs {
    /// Opt this device into automatic grants to its enrolled online input
    /// devices, or out. The separate Continuity master and Display/input
    /// switches must also be on.
    #[action(id = "set-automatic-display-input")]
    pub fn set_automatic_display_input(&mut self, on: bool) -> Result<(), tilo_resource::Refusal> {
        self.automatic_display_input = on;
        Ok(())
    }
}
