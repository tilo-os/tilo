//! Mounting the bar where the desktop has no in-process apps of its own
//! to mount it among (a test's or a capture's chrome).

use ui::application::{AppComponent, prepare_private_surface};
use ui::launch::LaunchError;

use crate::link::{BarService, Link};

/// The bar as a component the desktop draws, showing what it publishes
/// on `link`. Its record is kept in memory and its window is private:
/// nothing of it is written to disk or offered beyond this device.
///
/// # Errors
/// The component could not be prepared.
pub fn prepare(link: Link, device: &str, person: &str) -> Result<AppComponent, LaunchError> {
    prepare_private_surface(
        crate::definition(),
        device,
        person,
        "topbar",
        0,
        |context| {
            context.lend(BarService { link });
        },
    )
}
