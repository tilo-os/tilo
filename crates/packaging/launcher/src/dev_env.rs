//! The single gate for developer-only environment overrides in the launcher.
//!
//! A handful of environment variables re-point the launcher at trust or
//! policy inputs other than the image's: the system app store, the runtime
//! bundle root, the runtime trusted-publisher list, the JIT and
//! untrusted-parser policy files, the D-Bus filtering proxy binary, and the
//! switch that turns per-host egress filtering off. Each is load-bearing for
//! tests, the headless harnesses and the developer session — and each lets
//! whoever sets the session environment (on a systemd desktop, anything that
//! can write `~/.config/environment.d/`) replace what the sandbox trusts.
//!
//! So a shipped build does not honour them. [`DEV_ENV_OVERRIDES`] is true for
//! debug builds and for builds with the non-default `dev-overrides` feature,
//! false for the `--release` binaries the OS image installs. In a release
//! build each variable is ignored, with a one-time stderr warning naming it
//! (never its value).
//!
//! Consumers read overrides through [`dev_override`] rather than
//! `std::env::var_os`, so the gate is decided in one place; the pure
//! [`dev_override_from`] takes the gate as a parameter, so the disabled
//! branch is compiled and unit-tested in a debug build too.

use std::ffi::OsString;
use std::sync::Mutex;

/// Whether this build honours the developer environment overrides listed in
/// [`DEV_OVERRIDE_VARS`].
pub const DEV_ENV_OVERRIDES: bool = cfg!(any(debug_assertions, feature = "dev-overrides"));

/// Names the system app store (`/usr/share/tilo-apps`) instead.
pub const APPS_SYSTEM_DIR_ENV: &str = "TILO_APPS_SYSTEM_DIR";
/// Names the system payload root (`/usr/lib/tilo-apps`) instead. Read by
/// `tilo-app-host`, listed here so the compositor strips it too.
pub const APPS_SYSTEM_PAYLOAD_DIR_ENV: &str = "TILO_APPS_SYSTEM_PAYLOAD_DIR";
/// Names the runtime trusted-publisher list instead of the image's.
pub const TRUSTED_PUBLISHERS_FILE_ENV: &str = "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE";
/// `off` downgrades a host-scoped network grant to unrestricted egress.
pub const NET_GATE_ENV: &str = "TILO_LAUNCHER_NET_GATE";
/// Names the `xdg-dbus-proxy` binary that filters an app's session bus.
pub const DBUS_PROXY_BIN_ENV: &str = "TILO_LAUNCHER_DBUS_PROXY_BIN";

/// Every variable the launcher (and the app host beside it) treats as a
/// developer-only override. The compositor removes these from a launcher's
/// environment when its own gate is closed.
pub const DEV_OVERRIDE_VARS: &[&str] = &[
    APPS_SYSTEM_DIR_ENV,
    APPS_SYSTEM_PAYLOAD_DIR_ENV,
    runtime_bundle::RUNTIME_DIR_ENV,
    TRUSTED_PUBLISHERS_FILE_ENV,
    crate::seccomp::JIT_POLICY_PATH_ENV,
    crate::hardening::POLICY_PATH_ENV,
    NET_GATE_ENV,
    DBUS_PROXY_BIN_ENV,
];

/// Read the developer override `name`, honouring it only when this build's
/// gate is open.
pub fn dev_override(name: &'static str) -> Option<OsString> {
    dev_override_from(DEV_ENV_OVERRIDES, name, std::env::var_os(name))
}

/// [`dev_override`] as a function of the gate and the raw value, so the
/// release behaviour is assertable from a debug test run. When the gate is
/// closed and the variable is set, warns once per variable and returns
/// `None`.
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

/// The warning printed for an ignored override. Takes only the name, so the
/// value (a path that may be sensitive) cannot end up in the log.
pub fn ignored_warning(name: &str) -> String {
    format!(
        "tilo: ignoring {name}: developer environment overrides are disabled in this \
         release build (use a debug build, build with `--features dev-overrides`, or a \
         dev VM image)"
    )
}

static WARNED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// Whether this is the first time `name` has been ignored in this process.
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
        // Phase `an_open_gate_passes_the_value_through`.
        {
            assert_eq!(
                dev_override_from(true, APPS_SYSTEM_DIR_ENV, Some("/dev/store".into())),
                Some(OsString::from("/dev/store"))
            );
            assert_eq!(dev_override_from(true, APPS_SYSTEM_DIR_ENV, None), None);
        }
        // Phase `a_closed_gate_ignores_every_listed_override`.
        {
            for name in DEV_OVERRIDE_VARS {
                assert_eq!(
                    dev_override_from(false, name, Some("/attacker/controlled".into())),
                    None,
                    "{name} must be ignored when developer overrides are off"
                );
            }
        }
        // Phase `the_warning_names_the_variable_and_how_to_enable_it`.
        {
            let warning = ignored_warning(TRUSTED_PUBLISHERS_FILE_ENV);
            assert!(warning.contains(TRUSTED_PUBLISHERS_FILE_ENV));
            assert!(warning.contains("--features dev-overrides"));
            assert!(warning.contains("debug build"));
            assert!(warning.contains("dev VM image"));
        }
        // Phase `each_variable_warns_only_once`.
        {
            const NAME: &str = "TILO_DEV_ENV_WARN_ONCE_TEST";
            assert!(first_ignore(NAME));
            assert!(!first_ignore(NAME));
        }
    }
}
