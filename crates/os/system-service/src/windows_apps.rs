//! The `windows-apps` resource: this computer's Windows programs and appliance, beside the
//! compatibility layer's own `windows-compat` reading.
//!
//! What reads them is the session's compatibility service, which the session hands over as a
//! [`WindowsManager`]; the resource's reading is one projection of it for the app and workspace
//! reading, and its actions run through the same service the Windows Apps manager used to reach
//! over its own connection.

use tilo_actions::ResourceSpec;
use tilo_service::offer;
use tilo_system_resources::windows_apps::{
    AppStatus, WINDOWS_APPS, WindowsApps, WindowsAppsAction, WindowsProblem, WindowsProgram,
    WindowsProgramAction, program_id,
};

use crate::i18n::name;

/// The `windows-apps` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.windows-apps", label = "Windows Apps")]
pub struct WindowsAppsReading;

/// The `windows-apps` resource reading `windows`, with every program it lists a row of its own:
/// search finds a program by its name, and a caller opens or stops a running one on the address
/// its row carries.
#[must_use]
pub fn resource(windows: &WindowsApps) -> ResourceSpec {
    let programs = windows.apps.iter().filter_map(|app| {
        let id = program_id(&app.id)?;
        let mut bindings = tilo_resource::RowBindings::new();
        if app.launch_available.yes() {
            bindings = bindings.bind(WindowsProgramAction::LAUNCH, NO_ARGUMENTS);
        }
        if app.status == AppStatus::Running {
            bindings = bindings.bind(WindowsProgramAction::STOP, NO_ARGUMENTS);
        }
        let row =
            tilo_resource::LiveRow::typed_with::<WindowsProgram>(id, app.name.clone(), &bindings);
        // The row opens *this* program: search lists it by the program's own name, and a
        // person or an agent chooses "Photoshop" rather than a second, anonymous "Open".
        let row = if app.launch_available.yes() {
            row.labelled(WindowsProgramAction::LAUNCH, app.name.clone())
        } else {
            row
        };
        Some(row.into_resource())
    });
    offer::<WindowsAppsAction>(tilo_resource::typed_resource::<WindowsAppsReading>(
        WINDOWS_APPS,
        name("so.tilo.system.windows-apps", "Windows Apps"),
    ))
    .with_children(programs)
}

/// No argument bound: a program's own action knows the program by the address it is called on.
const NO_ARGUMENTS: [(&str, serde_json::Value); 0] = [];

/// The `windows-apps` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<WindowsApps, WindowsAppsAction>(
        WINDOWS_APPS,
        "Windows Apps",
    )
}

/// What reads and changes this computer's Windows programs, as the session's compatibility
/// service answers it.
pub trait WindowsManager: Send + Sync {
    /// What the programs, the appliance and the layer read as now, for an app in `workspace`.
    fn reading(&self, workspace: &str) -> WindowsApps;

    /// Carry out `action` for an app in `workspace`.
    ///
    /// # Errors
    /// Why it did not happen, as the device names it.
    fn change(&self, workspace: &str, action: &WindowsAppsAction) -> Result<(), WindowsProblem>;
}
