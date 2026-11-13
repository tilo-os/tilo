//! Which view draws each swappable part of the shell: the person's, and it
//! follows them to their other devices.
//!
//! A part of the shell that can be replaced — the window menu, the bar, the
//! switcher — is named by its slot, and the person binds the slot to a view
//! an app publishes ([`SECTION`]). A part that asks the person something on
//! the system's behalf is not in it: the lock screen and the questions a
//! workspace or a portal puts are drawn by the system, whatever the person
//! bound elsewhere.
//!
//! ```json
//! "shell_views": {
//!   "bindings": { "window-menu": { "app": "so.tilo.window-menu", "view": "window-menu" } },
//!   "on_device": { "web": { "window-menu": { "app": "so.tilo.window-menu", "view": "compact" } } }
//! }
//! ```

pub const SECTION: &str = "shell_views";

/// The key a kind of device's bindings are kept under, in the section's
/// `on_device`.
#[must_use]
pub fn device_key(device: crate::device_class::DeviceClass) -> &'static str {
    match device {
        crate::device_class::DeviceClass::Native => "native",
        crate::device_class::DeviceClass::Web => "web",
    }
}
