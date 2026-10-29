//! Mounting the panel inside the desktop's own process.

use std::rc::Rc;

use ui::application::{AppComponent, prepare_private_surface};
use ui::launch::LaunchError;

use crate::host::{Lent, PanelHost};
use crate::resource::QuickSettingsApp;

/// The panel as a component the desktop draws, its readings and effects
/// reaching `host`. Its record is kept in memory and its window is private:
/// the panel is this device's and is gone with the session, so nothing of it
/// is written to disk or offered beyond this device.
///
/// # Errors
/// The component could not be prepared.
pub fn prepare(
    host: Rc<dyn PanelHost>,
    device: &str,
    person: &str,
    instance: u64,
) -> Result<AppComponent, LaunchError> {
    prepare_private_surface(
        ui::framework::definition::<QuickSettingsApp>(),
        device,
        person,
        "quick-settings",
        instance,
        |context| context.lend(Lent(host)),
    )
}
