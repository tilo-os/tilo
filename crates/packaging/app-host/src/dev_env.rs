//! The single gate for developer-only environment overrides in the app host.
//!
//! `TILO_LAUNCHER_BIN` names the binary that builds every app's sandbox.
//! (`TILO_APPS_SYSTEM_DIR` and `TILO_APPS_SYSTEM_PAYLOAD_DIR`, which re-point
//! the image's system app store, are gated where they are read, in
//! `package_store::layout`.) The daemon's environment is seeded from the
//! user's session (on systemd, `~/.config/environment.d/`), so a shipped
//! build must not let it replace any of these. [`DEV_ENV_OVERRIDES`] is true
//! for debug builds and for builds with the non-default `dev-overrides`
//! feature; a release build ignores the variables, with a one-time stderr
//! warning naming the variable (never its value).
//!
//! Same contract as the launcher's `dev_env`: read through [`dev_override`];
//! the pure [`dev_override_from`] takes the gate, so the release branch is
//! unit-tested from a debug run.

use std::ffi::OsString;
use std::sync::Mutex;

/// Whether this build honours the developer environment overrides.
pub const DEV_ENV_OVERRIDES: bool = cfg!(any(debug_assertions, feature = "dev-overrides"));

/// Names the `tilo-launcher` binary instead of `/usr/bin/tilo-launcher`.
pub const LAUNCHER_BIN_ENV: &str = "TILO_LAUNCHER_BIN";

/// Read the developer override `name`, honouring it only when this build's
/// gate is open.
pub fn dev_override(name: &'static str) -> Option<OsString> {
    dev_override_from(DEV_ENV_OVERRIDES, name, std::env::var_os(name))
}

/// [`dev_override`] as a function of the gate and the raw value. When the
/// gate is closed and the variable is set, warns once and returns `None`.
pub fn dev_override_from(
    enabled: bool,
    name: &'static str,
    value: Option<OsString>,
) -> Option<OsString> {
    if enabled {
        return value;
    }
    if value.is_some() {
        warn_ignored_once(name);
    }
    None
}

/// The warning printed for an ignored override; takes only the name.
pub fn ignored_warning(name: &str) -> String {
    format!(
        "tilo-app-host: ignoring {name}: developer environment overrides are disabled in \
         this release build (use a debug build, build with `--features dev-overrides`, or \
         a dev VM image)"
    )
}

static WARNED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

fn first_ignore(name: &'static str) -> bool {
    let mut warned = WARNED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if warned.contains(&name) {
        return false;
    }
    warned.push(name);
    true
}

#[allow(
    clippy::print_stderr,
    reason = "a one-time operator warning; tracing may not be initialised yet"
)]
fn warn_ignored_once(name: &'static str) {
    if first_ignore(name) {
        eprintln!("{}", ignored_warning(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_overrides_pass_only_through_an_open_gate_and_warn_once() {
        // Phase `the_gate_tracks_the_build_profile_and_feature`.
        {
            assert_eq!(
                DEV_ENV_OVERRIDES,
                cfg!(any(debug_assertions, feature = "dev-overrides"))
            );
        }
        // Phase `a_closed_gate_ignores_every_override`.
        {
            let name = LAUNCHER_BIN_ENV;
            assert_eq!(
                dev_override_from(false, name, Some("/attacker".into())),
                None
            );
            assert_eq!(
                dev_override_from(true, name, Some("/dev".into())),
                Some(OsString::from("/dev"))
            );
        }
        // Phase `the_warning_names_the_variable_and_how_to_enable_it`.
        {
            let warning = ignored_warning(LAUNCHER_BIN_ENV);
            assert!(warning.contains(LAUNCHER_BIN_ENV));
            assert!(warning.contains("--features dev-overrides"));
        }
        // Phase `each_variable_warns_only_once`.
        {
            const NAME: &str = "TILO_APP_HOST_WARN_ONCE_TEST";
            assert!(first_ignore(NAME));
            assert!(!first_ignore(NAME));
        }
    }
}
