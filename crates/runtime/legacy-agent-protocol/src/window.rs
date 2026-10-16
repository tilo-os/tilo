//! How a legacy VM's windows are named on the host.
//!
//! The host's `waypipe client` runs with [`GUEST_ENV`] set to the guest OS.
//! The compositor reads it from the connecting process and rewrites every
//! app id that connection announces to [`host_app_id`], so the shell can tell
//! a legacy window from a native one and name it after the guest. Claiming to
//! be legacy only ever lowers what a window is trusted with, so the marker
//! needs no authentication.

use crate::GuestOs;

/// Set on the host forwarder's environment: `linux` or `windows`.
pub const GUEST_ENV: &str = "TILO_LEGACY_GUEST";

const PREFIX: &str = "legacy.";

/// `legacy.<guest>.<app id>`; a window that has not named itself yet is
/// `legacy.<guest>.window`.
#[must_use]
pub fn host_app_id(guest: GuestOs, app_id: Option<&str>) -> String {
    let app_id = app_id.map(str::trim).filter(|id| !id.is_empty());
    format!("{PREFIX}{}.{}", guest.as_str(), app_id.unwrap_or("window"))
}

/// The guest and the app's own id, when `app_id` came from a legacy VM.
#[must_use]
pub fn split_host_app_id(app_id: &str) -> Option<(GuestOs, &str)> {
    let rest = app_id.strip_prefix(PREFIX)?;
    let (guest, inner) = rest.split_once('.')?;
    let guest = GuestOs::parse(guest)?;
    (!inner.is_empty()).then_some((guest, inner))
}

/// The guest named by a forwarder process's environment block
/// (`/proc/<pid>/environ`: `NAME=value` entries separated by NUL).
#[must_use]
pub fn guest_from_environ(environ: &[u8]) -> Option<GuestOs> {
    let needle = format!("{GUEST_ENV}=");
    environ
        .split(|byte| *byte == 0)
        .filter_map(|entry| entry.strip_prefix(needle.as_bytes()))
        .find_map(|value| GuestOs::parse(std::str::from_utf8(value).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_windows_carry_their_guest_in_the_app_id() {
        // phase: an app id round-trips through the host name
        {
            let id = host_app_id(GuestOs::Linux, Some("org.gnome.Calculator"));
            assert_eq!(id, "legacy.linux.org.gnome.Calculator");
            assert_eq!(
                split_host_app_id(&id),
                Some((GuestOs::Linux, "org.gnome.Calculator"))
            );
            assert_eq!(host_app_id(GuestOs::Windows, None), "legacy.windows.window");
            assert_eq!(
                host_app_id(GuestOs::Linux, Some("  ")),
                "legacy.linux.window"
            );
        }
        // phase: native ids are never mistaken for legacy ones
        {
            assert_eq!(split_host_app_id("org.gnome.Calculator"), None);
            assert_eq!(split_host_app_id("legacy.beos.app"), None);
            assert_eq!(split_host_app_id("legacy.linux."), None);
        }
        // phase: the forwarder is recognised from its environment block
        {
            assert_eq!(
                guest_from_environ(b"HOME=/h\0TILO_LEGACY_GUEST=linux\0X=1\0"),
                Some(GuestOs::Linux)
            );
            assert_eq!(guest_from_environ(b"TILO_LEGACY_GUEST=dos\0"), None);
            assert_eq!(guest_from_environ(b"XTILO_LEGACY_GUEST=linux\0"), None);
        }
    }
}
