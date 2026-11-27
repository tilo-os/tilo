//! The `tilo-runtime` program: this computer's one Tilo daemon. The device daemon itself is the
//! core's (`tilo_runtime`); this program mounts on it what the core leaves optional
//! ([`capabilities`]), each through the daemon's one registration
//! ([`tilo_runtime::device::capabilities`]):
//!
//! - the device's image model, a provider on the daemon's dispatch;
//! - the language runtime, whose records, styles and rich text the workspace service reads and
//!   changes the built-in apps through;
//! - the device VPN, which carries a route through another of the person's devices and answers
//!   this one's exit on the daemon's endpoint;
//! - on Linux, the hold on suspend a request from another of the person's devices takes, and
//!   whether a Wi-Fi radio wakes its machine, as the OS actions read it.

use std::sync::Arc;

use tilo_runtime::device::capabilities::Capabilities;
#[cfg(unix)]
use tilo_runtime::device::capabilities::Mounting;
#[cfg(unix)]
use tilo_workspace_service::workspaces::providers::Reach;

#[cfg(target_os = "linux")]
mod stay_awake;
mod vpn;

/// What this program mounts on the daemon it runs.
#[must_use]
pub fn capabilities() -> Capabilities {
    Capabilities {
        // The language runtime: the daemon's own roles read and change the records of the apps
        // built into the build through it, and offer the styles the device's packages declare.
        tilolang: Some(Box::new(tilolang_workspace::host)),
        providers: {
            #[cfg(unix)]
            {
                vec![Box::new(image_model)]
            }
            #[cfg(not(unix))]
            {
                Vec::new()
            }
        },
        #[cfg(target_os = "linux")]
        stay_awake: Some(Box::new(|| Box::pin(stay_awake::hold()))),
        #[cfg(not(target_os = "linux"))]
        stay_awake: None,
        #[cfg(target_os = "linux")]
        radio_wakes: Some(os_actions::network_wake::wifi_magic_packet),
        #[cfg(not(target_os = "linux"))]
        radio_wakes: None,
        // A computer's session shows an app's moment as the notice the workspace sends; the
        // daemon of an app that runs it in its own process (a phone's) mounts the platform's.
        alarms: None,
        // The tunnel the person's other devices route through, and the route this one dials.
        vpn: Some(Arc::new(vpn::serving)),
    }
}

/// The device's image model, which apps ask by the addresses it publishes rather than through a
/// connection of their own: one copy on the daemon's dispatch. Its pictures come from the device's
/// content store, which only a Unix device keeps.
#[cfg(unix)]
fn image_model(at: &Mounting<'_>) {
    use tilo_vision_service::device::{APP, ImageModel, PROVIDER};
    if let Ok(model) = ImageModel::start(at.blob_socket, at.search.clone(), at.person) {
        at.providers
            .provide(PROVIDER, APP, Reach::ThisDevice, Arc::new(model));
    } else {
        tracing::warn!("this device runs no image model");
    }
}
