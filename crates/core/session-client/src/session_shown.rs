//! The device record that names what this device's session shows, which programs on every
//! platform read, while only a Unix session keeps the device's records ([`crate::device_records`]).

use serde_json::Value;

/// The device record what this device's session shows: its workspaces in its own order and the
/// one in front, under `active`. It is the device's alone, and what any program the person runs
/// reads to know the workspace their session runs in.
pub const SHOWN: &str = "so.tilo.session-shown";

/// The workspace [`SHOWN`]'s value has in front; `None` when it names none.
#[must_use]
pub fn shown_workspace(shown: &Value) -> Option<&str> {
    shown
        .get("active")
        .and_then(Value::as_str)
        .filter(|workspace| !workspace.is_empty())
}
