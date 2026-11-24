//! The `windows-apps` resource: this computer's Windows programs and appliance, beside the
//! compatibility layer's own `windows-compat` reading.
//!
//! What reads them is the session's compatibility service, which the session hands over as a
//! [`WindowsManager`]; the resource's reading is one projection of it for the app and workspace
//! reading, and its actions run through the same service the Windows Apps manager used to reach
//! over its own connection.

use tilo_actions::ResourceSpec;
use tilo_service::offer;
use tilo_system_resources::windows_apps::{WINDOWS_APPS, WindowsApps, WindowsAppsAction};

use crate::i18n::name;

/// The `windows-apps` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.windows-apps", label = "Windows Apps")]
pub struct WindowsAppsReading;

/// The `windows-apps` resource reading `windows`.
#[must_use]
pub fn resource(_windows: &WindowsApps) -> ResourceSpec {
    offer::<WindowsAppsAction>(tilo_resource::typed_resource::<WindowsAppsReading>(
        WINDOWS_APPS,
        name("so.tilo.system.windows-apps", "Windows Apps"),
    ))
}

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
    /// Why it did not happen.
    fn change(&self, workspace: &str, action: &WindowsAppsAction) -> Result<(), String>;
}
