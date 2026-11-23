//! How a Linux VM's windows are named on the host: the `waypipe client` runs
//! behind an app keeper that prefixes every app id it announces with
//! [`host_app_id_prefix`], giving [`host_app_id`], so the shell can tell a VM
//! window from a native one and restore it. Claiming to be a VM's only lowers what
//! a window is trusted with, so the name needs no authentication.

use crate::GuestOs;

const PREFIX: &str = "vm.";

/// What every app id of `guest`'s windows starts with on the host.
#[must_use]
pub fn host_app_id_prefix(guest: GuestOs) -> String {
    format!("{PREFIX}{}.", guest.as_str())
}

/// `vm.<guest>.<app id>`; a window that has not named itself yet is
/// `vm.<guest>.window`.
#[must_use]
pub fn host_app_id(guest: GuestOs, app_id: Option<&str>) -> String {
    let app_id = app_id.map(str::trim).filter(|id| !id.is_empty());
    format!(
        "{}{}",
        host_app_id_prefix(guest),
        app_id.unwrap_or("window")
    )
}

#[must_use]
pub fn split_host_app_id(app_id: &str) -> Option<(GuestOs, &str)> {
    let rest = app_id.strip_prefix(PREFIX)?;
    let (guest, inner) = rest.split_once('.')?;
    let guest = GuestOs::parse(guest)?;
    (!inner.is_empty()).then_some((guest, inner))
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn vm_windows_carry_their_guest_in_the_app_id;

        phase an_app_id_round_trips_through_the_host_name;
        // phase: an app id round-trips through the host name
        {
            let id = host_app_id(GuestOs::Linux, Some("org.gnome.Calculator"));
            assert_eq!(id, "vm.linux.org.gnome.Calculator");
            assert_eq!(
                split_host_app_id(&id),
                Some((GuestOs::Linux, "org.gnome.Calculator"))
            );
            assert_eq!(host_app_id(GuestOs::Windows, None), "vm.windows.window");
            assert_eq!(
                host_app_id(GuestOs::Linux, Some("  ")),
                "vm.linux.window"
            );
        }

        phase native_ids_are_never_mistaken_for_vm_ones;
        // phase: native ids are never mistaken for VM ones
        {
            assert_eq!(split_host_app_id("org.gnome.Calculator"), None);
            assert_eq!(split_host_app_id("vm.beos.app"), None);
            assert_eq!(split_host_app_id("vm.linux."), None);
        }
    }
}
