//! Mounting the banners where the desktop has no in-process apps of its own
//! to mount them among (a test's or a capture's chrome).

use ui::application::{AppComponent, prepare_private_surface};
use ui::launch::LaunchError;

use crate::link::{BannerService, Link};

/// The banners as a component the desktop draws, showing what it publishes
/// on `link`. Their record is kept in memory and their window is private:
/// nothing of them is written to disk or offered beyond this device.
///
/// # Errors
/// The component could not be prepared.
pub fn prepare(link: Link, device: &str, person: &str) -> Result<AppComponent, LaunchError> {
    prepare_private_surface(
        crate::definition(),
        device,
        person,
        "notification-banners",
        0,
        |context| context.lend(BannerService { link }),
    )
}
