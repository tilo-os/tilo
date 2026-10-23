//! The single gate for developer-only environment overrides in the
//! packaging stack: the store, the launcher, the app host and `app`.
//!
//! A handful of environment variables re-point these at trust or policy
//! inputs other than the image's: the system app store, the official
//! registry's owner key, the launcher and install binaries, the sandbox's
//! policy files. Each is load-bearing for tests, the headless harnesses and
//! the developer session — and each lets whoever sets the session
//! environment (on a systemd desktop, anything that can write
//! `~/.config/environment.d/`) replace what the sandbox trusts.
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
use tilo_lock::MutexRecover as _;

/// Whether this build honours developer environment overrides.
pub const DEV_ENV_OVERRIDES: bool = cfg!(any(debug_assertions, feature = "dev-overrides"));

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
    let mut warned = WARNED.lock_recover();
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
        {
            assert_eq!(
                DEV_ENV_OVERRIDES,
                cfg!(any(debug_assertions, feature = "dev-overrides"))
            );
        }
        {
            assert_eq!(
                dev_override_from(true, "TILO_X", Some("/dev/store".into())),
                Some(OsString::from("/dev/store"))
            );
            assert_eq!(dev_override_from(true, "TILO_X", None), None);
        }
        {
            assert_eq!(
                dev_override_from(false, "TILO_X", Some("/attacker/controlled".into())),
                None
            );
        }
        {
            let warning = ignored_warning("TILO_X");
            assert!(warning.contains("TILO_X"));
            assert!(warning.contains("--features dev-overrides"));
            assert!(warning.contains("debug build"));
            assert!(warning.contains("dev VM image"));
        }
        {
            const NAME: &str = "TILO_DEV_ENV_WARN_ONCE_TEST";
            assert!(first_ignore(NAME));
            assert!(!first_ignore(NAME));
        }
    }
}
