//! Which view draws each swappable part of the shell: the person's, and it
//! follows them to their other devices.
//!
//! A part of the shell that can be replaced — the window menu, the bar, the
//! switcher — goes by its own name, and the person binds the part to a view
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
pub fn device_key(device: settings_model::DeviceClass) -> &'static str {
    match device {
        settings_model::DeviceClass::Native => "native",
        settings_model::DeviceClass::Web => "web",
    }
}
